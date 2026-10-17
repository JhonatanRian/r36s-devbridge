use crate::deploy::DeployManager;
use crate::framebuffer::FramebufferReader;
use crate::metrics::MetricsCollector;
use crate::process::ProcessManager;
use crate::protocol::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub struct AgentServer {
    streaming_enabled: Arc<AtomicBool>,
    target_fps: Arc<AtomicU32>,
    active_control_id: Arc<AtomicU32>,
    active_video_session: Arc<AtomicU32>,
    metrics_collector: Arc<MetricsCollector>,
    fb_reader: Arc<Mutex<FramebufferReader>>,
    gamepad: Arc<crate::gamepad::GamepadService>,
}

impl AgentServer {
    pub fn new() -> Self {
        eprintln!("[r36s-agent] Inicializando estruturas do agente...");
        let reader = FramebufferReader::new();
        let metrics = Arc::new(MetricsCollector::new());
        metrics.set_fb_info(reader.width, reader.height, reader.bpp, reader.format);
        let gamepad = Arc::new(crate::gamepad::GamepadService::new());

        Self {
            streaming_enabled: Arc::new(AtomicBool::new(false)),
            target_fps: Arc::new(AtomicU32::new(30)),
            active_control_id: Arc::new(AtomicU32::new(0)),
            active_video_session: Arc::new(AtomicU32::new(0)),
            metrics_collector: metrics,
            fb_reader: Arc::new(Mutex::new(reader)),
            gamepad,
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
        let metrics_video = Arc::clone(&self.metrics_collector);
        let active_video_session = Arc::clone(&self.active_video_session);
        thread::spawn(move || {
            Self::video_stream_worker(
                streaming_flag,
                target_fps,
                fb_reader_cl,
                metrics_video,
                active_video_session,
            );
        });

        // Watchdog seguro para manter interface usb0 UP com IP estático 192.168.7.2 sem overhead
        thread::spawn(|| {
            let _ = std::process::Command::new("systemctl")
                .args(["start", "ssh.service"])
                .output();
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
                    let control_id_gen = Arc::clone(&self.active_control_id);
                    let session_id = control_id_gen.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
                    let metrics = Arc::clone(&self.metrics_collector);
                    let gamepad = Arc::clone(&self.gamepad);
                    thread::spawn(move || {
                        Self::handle_control_client(
                            stream,
                            streaming_flag,
                            target_fps,
                            control_id_gen,
                            session_id,
                            metrics,
                            gamepad,
                        );
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
        active_control_id: Arc<AtomicU32>,
        session_id: u32,
        metrics: Arc<MetricsCollector>,
        gamepad: Arc<crate::gamepad::GamepadService>,
    ) {
        let peer_addr = stream.peer_addr().ok();
        println!("[r36s-agent] Cliente de controle conectado (sessão {}): {:?}", session_id, peer_addr);

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
                        Ok((path, hash)) => {
                            if path.contains("r36s-devbridge/payload.zip") {
                                let payload_path = path.clone();
                                thread::spawn(move || {
                                    thread::sleep(Duration::from_millis(800));
                                    let script = format!(
                                        "TMP_U=$(mktemp -d) && unzip -q -o '{}' -d $TMP_U && cp -f $TMP_U/r36s-agent /usr/local/bin/r36s-agent && chmod 755 /usr/local/bin/r36s-agent && rm -rf $TMP_U && systemctl restart r36s-agent",
                                        payload_path
                                    );
                                    let _ = std::process::Command::new("/bin/sh")
                                        .arg("-c")
                                        .arg(&script)
                                        .output();
                                });
                            }
                            ServerResponse::DeploySuccess { path, sha256: hash }
                        }
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
                    ClientCommand::SetGamepadMonitor { enabled } => {
                        gamepad.set_listening(enabled);
                        ServerResponse::GamepadAck {
                            success: true,
                            message: format!("Monitoramento de controles: {}", if enabled { "ATIVADO" } else { "DESATIVADO" }),
                        }
                    }
                    ClientCommand::GetGamepadState => {
                        let state = gamepad.get_state();
                        ServerResponse::GamepadStateResponse(state)
                    }
                    ClientCommand::InjectGamepadInput { action } => {
                        match gamepad.inject_action(&action) {
                            Ok(_) => ServerResponse::GamepadAck {
                                success: true,
                                message: "Comando de controle injetado com sucesso".to_string(),
                            },
                            Err(e) => ServerResponse::GamepadAck {
                                success: false,
                                message: format!("Falha ao injetar comando: {}", e),
                            },
                        }
                    }
                },
                Err(err) => ServerResponse::Error {
                    message: format!("Comando inválido: {}", err),
                },
            };

            response.write_to(&mut writer);
            line.clear();
        }

        // Somente desativa o streaming se esta ainda for a sessão de controle ativa mais recente
        if active_control_id.load(Ordering::SeqCst) == session_id {
            streaming_flag.store(false, Ordering::Relaxed);
        }
        println!("[r36s-agent] Cliente de controle desconectado (sessão {}).", session_id);
    }

    fn video_stream_worker(
        streaming_flag: Arc<AtomicBool>,
        target_fps: Arc<AtomicU32>,
        fb_reader: Arc<Mutex<FramebufferReader>>,
        metrics: Arc<MetricsCollector>,
        active_video_session: Arc<AtomicU32>,
    ) {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", VIDEO_PORT))
            .expect("Falha ao abrir porta de vídeo");

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let session_id = active_video_session.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
                    println!("[r36s-agent] Cliente de vídeo conectado (sessão {}): {:?}", session_id, stream.peer_addr());
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));

                    // Garante que o streaming esteja ativo ao conectar no socket 7366
                    streaming_flag.store(true, Ordering::Relaxed);

                    // Reseta sessão para forçar keyframe no início de cada nova conexão
                    {
                        let mut reader = fb_reader.lock().unwrap();
                        reader.reset_session();
                    }

                    let streaming_flag_cl = Arc::clone(&streaming_flag);
                    let target_fps_cl = Arc::clone(&target_fps);
                    let fb_reader_cl = Arc::clone(&fb_reader);
                    let metrics_cl = Arc::clone(&metrics);
                    let active_session_cl = Arc::clone(&active_video_session);

                    thread::spawn(move || {
                        let mut frame_index: u32 = 0;
                        let mut next_deadline = Instant::now();
                        let mut comp_buffer = Vec::with_capacity(lz4_flex::block::get_maximum_output_size(640 * 480 * 4));

                        let diag_enabled = std::env::var_os("DEVBRIDGE_DIAG").is_some();
                        let mut diag_last_report = Instant::now();
                        let mut diag_frames = 0u64;
                        let mut diag_unchanged = 0u64;
                        let mut diag_skips = 0u64;
                        let mut diag_cap_us = 0u64;
                        let mut diag_send_us = 0u64;

                        loop {
                            // Se uma nova conexão de vídeo chegou no socket, esta sessão é preempcionada e encerra imediatamente
                            if active_session_cl.load(Ordering::SeqCst) != session_id {
                                println!("[r36s-agent] Sessão de vídeo {} preempcionada por nova conexão.", session_id);
                                break;
                            }

                            if !streaming_flag_cl.load(Ordering::Relaxed) {
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
                                next_deadline = Instant::now();
                                continue;
                            }

                            let fps = target_fps_cl.load(Ordering::Relaxed).clamp(5, 60);
                            let frame_duration = Duration::from_millis(1000 / fps as u64);

                            // Controle anti-buffer-bloat TCP: se a fila de saída do socket tiver mais de 64KB pendentes,
                            // o receptor ou a rede ainda estão drenando o frame anterior. Pulamos o envio deste ciclo para
                            // evitar enfileiramento no kernel e garantir que o cliente receba sempre o frame mais atual (< 16ms).
                            let mut pending_bytes: libc::c_int = 0;
                            let _ = unsafe { libc::ioctl(stream.as_raw_fd(), libc::TIOCOUTQ, &mut pending_bytes) };
                            if pending_bytes > 64 * 1024 {
                                if diag_enabled {
                                    diag_skips += 1;
                                }
                                thread::sleep(Duration::from_millis(4));
                                continue;
                            }

                            let t_cap = if diag_enabled { Some(Instant::now()) } else { None };

                            // Captura e compressão in-place no buffer pré-alocado (zero alocações por frame)
                            let (width, height, bpp, format, flags, comp_len) = {
                                let mut reader = fb_reader_cl.lock().unwrap();
                                let (w, h, fmt, flg, len) = reader.capture_frame(&mut comp_buffer);
                                (w, h, reader.bpp, fmt, flg, len)
                            }; // O Mutex fb_reader_cl é liberado aqui antes do envio pela rede!
                            metrics_cl.set_fb_info(width, height, bpp, format);

                            if let Some(t) = t_cap {
                                diag_cap_us += t.elapsed().as_micros() as u64;
                                diag_frames += 1;
                                if flags == FRAME_FLAG_UNCHANGED {
                                    diag_unchanged += 1;
                                }
                            }

                            frame_index = frame_index.wrapping_add(1);

                            let header = VideoFrameHeader::new(
                                frame_index,
                                width,
                                height,
                                format,
                                flags,
                                comp_len as u32,
                            );

                            let t_send = if diag_enabled { Some(Instant::now()) } else { None };

                            // Envia cabeçalho (20 bytes)
                            let header_bytes = header.to_bytes();
                            if stream.write_all(&header_bytes).is_err() {
                                break;
                            }

                            // Se payload comprimido não estiver vazio (flags != 1 e comp_len > 0), envia dados
                            if comp_len > 0 {
                                if stream.write_all(&comp_buffer[..comp_len]).is_err() {
                                    break;
                                }
                            }

                            if let Some(t) = t_send {
                                diag_send_us += t.elapsed().as_micros() as u64;
                            }

                            // Relatório diagnóstico agregado a cada 5s sem poluir logs por quadro
                            if diag_enabled && diag_last_report.elapsed() >= Duration::from_secs(5) {
                                let el_secs = diag_last_report.elapsed().as_secs_f64();
                                let fps_eff = diag_frames as f64 / el_secs;
                                let avg_cap_ms = if diag_frames > 0 { (diag_cap_us as f64 / diag_frames as f64) / 1000.0 } else { 0.0 };
                                let avg_send_ms = if diag_frames > 0 { (diag_send_us as f64 / diag_frames as f64) / 1000.0 } else { 0.0 };
                                eprintln!(
                                    "[DEVBRIDGE_DIAG] 5s: {:.1} FPS (total: {}, unchanged: {}, tcp_skips: {}) | avg cap: {:.2}ms, send: {:.2}ms",
                                    fps_eff, diag_frames, diag_unchanged, diag_skips, avg_cap_ms, avg_send_ms
                                );
                                diag_last_report = Instant::now();
                                diag_frames = 0;
                                diag_unchanged = 0;
                                diag_skips = 0;
                                diag_cap_us = 0;
                                diag_send_us = 0;
                            }

                            // Cadência monotônica precisa: desconta o tempo gasto de captura/envio
                            let now = Instant::now();
                            if next_deadline > now {
                                thread::sleep(next_deadline - now);
                                next_deadline += frame_duration;
                            } else {
                                next_deadline = now + frame_duration;
                            }
                        }
                        println!("[r36s-agent] Cliente de vídeo desconectado (sessão {}).", session_id);
                    });
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
