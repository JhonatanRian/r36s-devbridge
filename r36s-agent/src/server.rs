use crate::deploy::DeployManager;
use crate::framebuffer::FramebufferReader;
use crate::metrics::MetricsCollector;
use crate::process::ProcessManager;
use crate::protocol::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct AgentServer {
    streaming_enabled: Arc<AtomicBool>,
    target_fps: Arc<AtomicU32>,
    metrics_collector: Arc<MetricsCollector>,
}

impl AgentServer {
    pub fn new() -> Self {
        Self {
            streaming_enabled: Arc::new(AtomicBool::new(false)),
            target_fps: Arc::new(AtomicU32::new(30)),
            metrics_collector: Arc::new(MetricsCollector::new()),
        }
    }

    pub fn start(&self) {
        println!("[r36s-agent] Iniciando servidor DevBridge...");
        println!("[r36s-agent] Porta de Controle: {}", CONTROL_PORT);
        println!("[r36s-agent] Porta de Vídeo:    {}", VIDEO_PORT);

        // Thread para streaming de vídeo
        let streaming_flag = Arc::clone(&self.streaming_enabled);
        let target_fps = Arc::clone(&self.target_fps);
        thread::spawn(move || {
            Self::video_stream_worker(streaming_flag, target_fps);
        });

        // Thread principal: Listener de comandos
        let listener = TcpListener::bind(format!("0.0.0.0:{}", CONTROL_PORT))
            .expect("Falha ao abrir porta de controle");

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let streaming_flag = Arc::clone(&self.streaming_enabled);
                    let target_fps = Arc::clone(&self.target_fps);
                    let metrics = Arc::clone(&self.metrics_collector);
                    thread::spawn(move || {
                        Self::handle_control_client(stream, streaming_flag, target_fps, metrics);
                    });
                }
                Err(e) => {
                    eprintln!("[r36s-agent] Erro de conexão de controle: {}", e);
                }
            }
        }
    }

    fn handle_control_client(
        stream: TcpStream,
        streaming_flag: Arc<AtomicBool>,
        target_fps: Arc<AtomicU32>,
        metrics: Arc<MetricsCollector>,
    ) {
        println!("[r36s-agent] Cliente de controle conectado: {:?}", stream.peer_addr());
        let _ = stream.set_nodelay(true);
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut deploy_mgr = DeployManager::new();

        let mut line = String::new();
        while let Ok(bytes_read) = reader.read_line(&mut line) {
            if bytes_read == 0 {
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                line.clear();
                continue;
            }

            let response = match serde_json::from_str::<ClientCommand>(trimmed) {
                Ok(cmd) => match cmd {
                    ClientCommand::Ping => ServerResponse::Pong,
                    ClientCommand::GetMetrics => {
                        let data = metrics.collect();
                        ServerResponse::Metrics(data)
                    }
                    ClientCommand::StartStream { target_fps: fps } => {
                        let fps = if fps == 0 || fps > 60 { 30 } else { fps };
                        target_fps.store(fps, Ordering::Relaxed);
                        streaming_flag.store(true, Ordering::Relaxed);
                        ServerResponse::StreamStatus { active: true, fps }
                    }
                    ClientCommand::StopStream => {
                        streaming_flag.store(false, Ordering::Relaxed);
                        ServerResponse::StreamStatus { active: false, fps: 0 }
                    }
                    ClientCommand::DeployStart {
                        relative_path,
                        filename,
                        total_size,
                        sha256,
                    } => match deploy_mgr.start(&relative_path, &filename, total_size, &sha256) {
                        Ok(msg) => ServerResponse::DeployProgress {
                            status: "in_progress".to_string(),
                            message: msg,
                        },
                        Err(e) => ServerResponse::DeployError { error: e },
                    },
                    ClientCommand::DeployChunk { chunk_index, data_base64 } => {
                        let chunk_bytes = match base64_decode(&data_base64) {
                            Ok(b) => b,
                            Err(e) => {
                                ServerResponse::DeployError {
                                    error: format!("Chunk {} base64 inválido: {}", chunk_index, e),
                                }
                                .write_to(&mut writer);
                                line.clear();
                                continue;
                            }
                        };
                        match deploy_mgr.append_chunk(chunk_index, &chunk_bytes) {
                            Ok(written) => ServerResponse::DeployProgress {
                                status: "chunk_ok".to_string(),
                                message: format!("Bytes recebidos: {}", written),
                            },
                            Err(e) => ServerResponse::DeployError { error: e },
                        }
                    }
                    ClientCommand::DeployFinish => match deploy_mgr.finish() {
                        Ok((path, hash)) => ServerResponse::DeploySuccess { path, sha256: hash },
                        Err(e) => ServerResponse::DeployError { error: e },
                    },
                    ClientCommand::FetchLogs { log_type, lines } => {
                        let content = ProcessManager::fetch_logs(&log_type, lines);
                        ServerResponse::LogsResponse { log_type, content }
                    }
                    ClientCommand::KillProcess { pid, signal } => {
                        match ProcessManager::kill(pid, signal) {
                            Ok(_) => ServerResponse::KillResult {
                                pid,
                                success: true,
                                message: format!("Processo {} finalizado com sinal {}", pid, signal),
                            },
                            Err(e) => ServerResponse::KillResult {
                                pid,
                                success: false,
                                message: e,
                            },
                        }
                    }
                    ClientCommand::RestartES => match ProcessManager::restart_emulationstation() {
                        Ok(msg) => ServerResponse::RestartESResult {
                            success: true,
                            message: msg,
                        },
                        Err(e) => ServerResponse::RestartESResult {
                            success: false,
                            message: e,
                        },
                    },
                },
                Err(err) => ServerResponse::Error {
                    message: format!("Comando inválido: {}", err),
                },
            };

            response.write_to(&mut writer);
            line.clear();
        }

        // Ao desconectar o controle, desativa streaming para economizar CPU
        streaming_flag.store(false, Ordering::Relaxed);
        println!("[r36s-agent] Cliente de controle desconectado.");
    }

    fn video_stream_worker(streaming_flag: Arc<AtomicBool>, target_fps: Arc<AtomicU32>) {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", VIDEO_PORT))
            .expect("Falha ao abrir porta de vídeo");

        let mut fb_reader = FramebufferReader::new();

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    println!("[r36s-agent] Cliente de vídeo conectado: {:?}", stream.peer_addr());
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));

                    let mut frame_index: u32 = 0;
                    loop {
                        if !streaming_flag.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            continue;
                        }

                        let fps = target_fps.load(Ordering::Relaxed).clamp(5, 60);
                        let frame_duration = Duration::from_millis(1000 / fps as u64);

                        let (width, height, format, flags, compressed) = fb_reader.capture_frame();
                        frame_index = frame_index.wrapping_add(1);

                        let header = VideoFrameHeader::new(
                            frame_index,
                            width,
                            height,
                            format,
                            flags,
                            compressed.len() as u32,
                        );

                        // Envia cabeçalho (20 bytes) + payload comprimido
                        let header_bytes = header.to_bytes();
                        if stream.write_all(&header_bytes).is_err() {
                            break;
                        }
                        if stream.write_all(&compressed).is_err() {
                            break;
                        }

                        thread::sleep(frame_duration);
                    }
                    println!("[r36s-agent] Cliente de vídeo desconectado.");
                }
                Err(e) => {
                    eprintln!("[r36s-agent] Erro no listener de vídeo: {}", e);
                }
            }
        }
    }
}

impl ServerResponse {
    pub fn write_to(&self, writer: &mut TcpStream) {
        if let Ok(mut json) = serde_json::to_string(self) {
            json.push('\n');
            let _ = writer.write_all(json.as_bytes());
            let _ = writer.flush();
        }
    }
}

// Implementação simples de base64 decode sem crates externas pesadas
fn base64_decode(input: &str) -> Result<Vec<u8>, &'static str> {
    const TABLE: [i8; 256] = {
        let mut t = [-1i8; 256];
        let mut i = 0u8;
        while i < 26 {
            t[(b'A' + i) as usize] = i as i8;
            t[(b'a' + i) as usize] = (i + 26) as i8;
            i += 1;
        }
        let mut j = 0u8;
        while j < 10 {
            t[(b'0' + j) as usize] = (j + 52) as i8;
            j += 1;
        }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };

    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;

    for &b in bytes {
        if b == b'=' || b == b'\r' || b == b'\n' || b == b' ' {
            continue;
        }
        let val = TABLE[b as usize];
        if val < 0 {
            return Err("Caractere base64 inválido");
        }
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}
