use crate::deploy::DeployManager;
use crate::framebuffer::FramebufferReader;
use crate::metrics::MetricsCollector;
use crate::process::ProcessManager;
use crate::protocol::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct AgentServer {
    streaming_enabled: Arc<AtomicBool>,
    target_fps: Arc<AtomicU32>,
    metrics_collector: Arc<MetricsCollector>,
    fb_reader: Arc<Mutex<FramebufferReader>>,
}

impl AgentServer {
    pub fn new() -> Self {
        eprintln!("[r36s-agent] Inicializando estruturas do agente...");
        let reader = FramebufferReader::new();
        let metrics = Arc::new(MetricsCollector::new());
        metrics.set_fb_info(reader.width, reader.height, reader.bpp, reader.format);

        Self {
            streaming_enabled: Arc::new(AtomicBool::new(false)),
            target_fps: Arc::new(AtomicU32::new(30)),
            metrics_collector: metrics,
            fb_reader: Arc::new(Mutex::new(reader)),
        }
    }

    pub fn start(&self) {
        println!("[r36s-agent] Iniciando servidor DevBridge v{}...", PROTOCOL_VERSION);
        println!("[r36s-agent] Porta de Controle: {}", CONTROL_PORT);
        println!("[r36s-agent] Porta de Vídeo:    {}", VIDEO_PORT);

        // Thread para streaming de vídeo
        let streaming_flag = Arc::clone(&self.streaming_enabled);
        let target_fps = Arc::clone(&self.target_fps);
        let fb_reader_cl = Arc::clone(&self.fb_reader);
        thread::spawn(move || {
            Self::video_stream_worker(streaming_flag, target_fps, fb_reader_cl);
        });

        // Watchdog seguro para manter interface usb0 UP com IP estático 192.168.7.2 sem overhead
        thread::spawn(|| {
            loop {
                thread::sleep(Duration::from_secs(4));
                if std::path::Path::new("/sys/class/net/usb0").exists() {
                    // Verifica se já possui o IP correto antes de reexecutar ip
                    let has_ip = std::fs::read_to_string("/proc/net/dev").is_ok()
                        && std::process::Command::new("/sbin/ip")
                            .args(["-o", "-4", "addr", "show", "dev", "usb0"])
                            .output()
                            .map(|o| String::from_utf8_lossy(&o.stdout).contains("192.168.7.2"))
                            .unwrap_or(false);

                    if !has_ip {
                        let _ = std::process::Command::new("/sbin/ip")
                            .args(["link", "set", "dev", "usb0", "up"])
                            .output();
                        let _ = std::process::Command::new("/sbin/ip")
                            .args(["-4", "address", "replace", "192.168.7.2/24", "dev", "usb0"])
                            .output();
                    }
                }
            }
        });

        // Thread principal: Listener de comandos
        let listener = TcpListener::bind(format!("0.0.0.0:{}", CONTROL_PORT))
            .expect("Falha ao abrir porta de controle");
        println!("[r36s-agent] Porta de controle {} escutando.", CONTROL_PORT);

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
        let peer_addr = stream.peer_addr().ok();
        println!("[r36s-agent] Cliente de controle conectado: {:?}", peer_addr);

        // Validação de segurança: permitir somente loopback ou rede USB 192.168.7.*
        if let Some(ref addr) = peer_addr {
            let ip = addr.ip();
            let is_allowed = ip.is_loopback() || match ip {
                std::net::IpAddr::V4(v4) => {
                    let oct = v4.octets();
                    oct[0] == 192 && oct[1] == 168 && oct[2] == 7
                }
                _ => false,
            };

            if !is_allowed {
                eprintln!("[r36s-agent] Conexão rejeitada por segurança (IP não autorizado: {})", ip);
                return;
            }
        }

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
                    ClientCommand::Handshake { client_version, client_id } => {
                        println!("[r36s-agent] Handshake recebido: id={}, versão={}", client_id, client_version);
                        let (fb_w, fb_h, fb_bpp, fb_fmt) = metrics.get_fb_info();
                        ServerResponse::HandshakeAck {
                            agent_version: env!("CARGO_PKG_VERSION").to_string(),
                            protocol_version: PROTOCOL_VERSION.to_string(),
                            compatible: true,
                            fb_width: fb_w,
                            fb_height: fb_h,
                            fb_bpp: fb_bpp,
                            fb_format: fb_fmt,
                        }
                    }
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
                            chunk_index: 0,
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
                                chunk_index,
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

    fn video_stream_worker(
        streaming_flag: Arc<AtomicBool>,
        target_fps: Arc<AtomicU32>,
        fb_reader: Arc<Mutex<FramebufferReader>>,
    ) {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", VIDEO_PORT))
            .expect("Falha ao abrir porta de vídeo");

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    println!("[r36s-agent] Cliente de vídeo conectado: {:?}", stream.peer_addr());
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(150)));

                    // Reseta sessão para forçar keyframe no início de cada nova conexão
                    {
                        let mut reader = fb_reader.lock().unwrap();
                        reader.reset_session();
                    }

                    let mut frame_index: u32 = 0;
                    loop {
                        if !streaming_flag.load(Ordering::Relaxed) {
                            // Testa se o cliente ainda está conectado enquanto pausado
                            let mut probe_buf = [0u8; 1];
                            let _ = stream.set_nonblocking(true);
                            match stream.peek(&mut probe_buf) {
                                Ok(0) => break, // EOF / socket fechado pelo cliente
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    // Socket ainda aberto e sem dados prontos (normal)
                                }
                                Err(_) => break, // Erro de conexão
                                _ => {}
                            }
                            let _ = stream.set_nonblocking(false);
                            thread::sleep(Duration::from_millis(60));
                            continue;
                        }

                        let fps = target_fps.load(Ordering::Relaxed).clamp(5, 60);
                        let frame_duration = Duration::from_millis(1000 / fps as u64);

                        let (width, height, format, flags, compressed) = {
                            let mut reader = fb_reader.lock().unwrap();
                            reader.capture_frame()
                        };

                        frame_index = frame_index.wrapping_add(1);

                        let header = VideoFrameHeader::new(
                            frame_index,
                            width,
                            height,
                            format,
                            flags,
                            compressed.len() as u32,
                        );

                        // Envia cabeçalho (20 bytes)
                        let header_bytes = header.to_bytes();
                        if stream.write_all(&header_bytes).is_err() {
                            break;
                        }

                        // Se payload comprimido não estiver vazio (flags != 1), envia dados
                        if !compressed.is_empty() {
                            if stream.write_all(&compressed).is_err() {
                                break;
                            }
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
