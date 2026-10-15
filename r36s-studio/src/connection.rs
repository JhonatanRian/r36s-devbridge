use crate::protocol::*;
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Searching { usb_detected: bool, message: String },
    Connecting { target: String, stage: String },
    Connected { target: String, latency_ms: f32, agent_version: String },
    Error { message: String },
}

#[derive(Clone)]
pub struct DecodedFrame {
    pub frame_index: u32,
    pub width: u16,
    pub height: u16,
    pub rgba_data: Vec<u8>,
    pub timestamp: Instant,
}

pub struct ConnectionManager {
    pub status: Arc<Mutex<ConnectionStatus>>,
    pub metrics: Arc<Mutex<Option<SystemMetrics>>>,
    pub current_frame: Arc<Mutex<Option<DecodedFrame>>>,
    pub last_logs: Arc<Mutex<Option<(String, String)>>>,
    pub deploy_status: Arc<Mutex<Option<String>>>,
    pub deploy_progress: Arc<Mutex<f32>>,
    pub is_streaming: Arc<AtomicBool>,
    pub current_fps: Arc<Mutex<f32>>,
    pub throughput_kbps: Arc<Mutex<f32>>,
    pub diag_logs: Arc<Mutex<Vec<String>>>,
    pub last_process_result: Arc<Mutex<Option<(bool, String)>>>,
    pub total_bytes_rx: Arc<AtomicU64>,
    pub total_bytes_tx: Arc<AtomicU64>,
    cmd_sender: Sender<ClientCommand>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        let status = Arc::new(Mutex::new(ConnectionStatus::Searching {
            usb_detected: false,
            message: "Procurando interface USB do R36S...".to_string(),
        }));
        let metrics = Arc::new(Mutex::new(None));
        let current_frame = Arc::new(Mutex::new(None));
        let last_logs = Arc::new(Mutex::new(None));
        let deploy_status = Arc::new(Mutex::new(None));
        let deploy_progress = Arc::new(Mutex::new(0.0));
        let is_streaming = Arc::new(AtomicBool::new(false));
        let current_fps = Arc::new(Mutex::new(0.0));
        let throughput_kbps = Arc::new(Mutex::new(0.0));
        let diag_logs = Arc::new(Mutex::new(Vec::new()));
        let last_process_result = Arc::new(Mutex::new(None));
        let total_bytes_rx = Arc::new(AtomicU64::new(0));
        let total_bytes_tx = Arc::new(AtomicU64::new(0));

        let (cmd_tx, cmd_rx) = channel::<ClientCommand>();

        let mgr = Self {
            status: Arc::clone(&status),
            metrics: Arc::clone(&metrics),
            current_frame: Arc::clone(&current_frame),
            last_logs: Arc::clone(&last_logs),
            deploy_status: Arc::clone(&deploy_status),
            deploy_progress: Arc::clone(&deploy_progress),
            is_streaming: Arc::clone(&is_streaming),
            current_fps: Arc::clone(&current_fps),
            throughput_kbps: Arc::clone(&throughput_kbps),
            diag_logs: Arc::clone(&diag_logs),
            last_process_result: Arc::clone(&last_process_result),
            total_bytes_rx: Arc::clone(&total_bytes_rx),
            total_bytes_tx: Arc::clone(&total_bytes_tx),
            cmd_sender: cmd_tx,
        };

        // Thread medidora de throughput KB/s
        let rx_cnt = Arc::clone(&total_bytes_rx);
        let kbps_lock = Arc::clone(&throughput_kbps);
        thread::spawn(move || {
            let mut last_rx = 0u64;
            loop {
                thread::sleep(Duration::from_millis(1000));
                let cur = rx_cnt.load(Ordering::Relaxed);
                let diff = cur.saturating_sub(last_rx);
                last_rx = cur;
                let kbps = (diff as f32) / 1024.0;
                let mut lock = kbps_lock.lock().unwrap();
                *lock = kbps;
            }
        });

        // Thread de controle e telemetria
        let status_cl = Arc::clone(&status);
        let metrics_cl = Arc::clone(&metrics);
        let logs_cl = Arc::clone(&last_logs);
        let deploy_st_cl = Arc::clone(&deploy_status);
        let is_streaming_cl = Arc::clone(&is_streaming);
        let diag_cl = Arc::clone(&diag_logs);
        let proc_res_cl = Arc::clone(&last_process_result);
        let bytes_tx_cl = Arc::clone(&total_bytes_tx);
        let bytes_rx_cl = Arc::clone(&total_bytes_rx);

        thread::spawn(move || {
            Self::control_worker(
                cmd_rx,
                status_cl,
                metrics_cl,
                logs_cl,
                deploy_st_cl,
                is_streaming_cl,
                diag_cl,
                proc_res_cl,
                bytes_tx_cl,
                bytes_rx_cl,
            );
        });

        // Thread de streaming de vídeo
        let frame_cl = Arc::clone(&current_frame);
        let is_streaming_vid = Arc::clone(&is_streaming);
        let status_vid = Arc::clone(&status);
        let fps_cl = Arc::clone(&current_fps);
        let vid_rx = Arc::clone(&total_bytes_rx);
        let vid_diag = Arc::clone(&diag_logs);

        thread::spawn(move || {
            Self::video_worker(frame_cl, is_streaming_vid, status_vid, fps_cl, vid_rx, vid_diag);
        });

        mgr
    }

    pub fn send_command(&self, cmd: ClientCommand) {
        let _ = self.cmd_sender.send(cmd);
    }

    pub fn toggle_streaming(&self) {
        let current = self.is_streaming.load(Ordering::Relaxed);
        let new_state = !current;
        self.is_streaming.store(new_state, Ordering::Relaxed);
        if new_state {
            self.send_command(ClientCommand::StartStream { target_fps: 30 });
        } else {
            self.send_command(ClientCommand::StopStream);
            let mut fps = self.current_fps.lock().unwrap();
            *fps = 0.0;
        }
    }

    fn add_diag(diag: &Arc<Mutex<Vec<String>>>, msg: &str) {
        let line = format!("[{}] {}", chrono_time(), msg);
        let mut d = diag.lock().unwrap();
        if d.len() > 100 {
            d.remove(0);
        }
        d.push(line);
    }

    fn check_usb_device(diag: &Arc<Mutex<Vec<String>>>) -> (bool, String, Option<String>) {
        if let Ok(entries) = fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str == "lo" {
                    continue;
                }
                let driver_path = entry.path().join("device/driver");
                if let Ok(target) = fs::read_link(driver_path) {
                    let drv_name = target.file_name().unwrap_or_default().to_string_lossy();
                    if drv_name == "rndis_host"
                        || drv_name == "cdc_ether"
                        || drv_name == "cdc_ncm"
                        || drv_name == "usbnet"
                    {
                        let ip_error = Self::ensure_host_ip(&name_str, diag).err();
                        return (true, name_str.to_string(), ip_error);
                    }
                }
            }
        }
        (false, String::new(), None)
    }

    fn host_has_ip(ifname: &str) -> Result<bool, String> {
        let output = std::process::Command::new("nmcli")
            .args(["-g", "IP4.ADDRESS", "dev", "show", ifname])
            .output()
            .map_err(|e| format!("nmcli indisponível: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "não foi possível consultar o IP de {ifname}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "192.168.7.1/24"))
    }

    fn ensure_host_ip(ifname: &str, diag: &Arc<Mutex<Vec<String>>>) -> Result<(), String> {
        if Self::host_has_ip(ifname)? {
            return Ok(());
        }
        Self::add_diag(diag, &format!("Interface {ifname} sem 192.168.7.1/24. Configurando IP no dispositivo USB..."));

        // Modifica a conexão ativa sem criar um perfil por MAC aleatório do gadget.
        let output = std::process::Command::new("nmcli")
            .args(["device", "modify", ifname, "ipv4.method", "manual", "ipv4.addresses", "192.168.7.1/24"])
            .output()
            .map_err(|e| format!("não foi possível executar nmcli: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "falha ao configurar o IP de {ifname}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if !Self::host_has_ip(ifname)? {
            return Err(format!("nmcli concluiu, mas {ifname} continua sem 192.168.7.1/24"));
        }
        Self::add_diag(diag, &format!("Interface {ifname} configurada em 192.168.7.1/24."));
        Ok(())
    }

    fn control_worker(
        cmd_rx: Receiver<ClientCommand>,
        status: Arc<Mutex<ConnectionStatus>>,
        metrics: Arc<Mutex<Option<SystemMetrics>>>,
        logs: Arc<Mutex<Option<(String, String)>>>,
        deploy_status: Arc<Mutex<Option<String>>>,
        is_streaming: Arc<AtomicBool>,
        diag: Arc<Mutex<Vec<String>>>,
        proc_result: Arc<Mutex<Option<(bool, String)>>>,
        bytes_tx: Arc<AtomicU64>,
        bytes_rx: Arc<AtomicU64>,
    ) {
        let target_ip = "192.168.7.2";
        let target_addr: SocketAddr = format!("{}:{}", target_ip, CONTROL_PORT)
            .parse()
            .unwrap();

        loop {
            // Fase 1: Descoberta USB
            let (usb_found, ifname, host_ip_error) = Self::check_usb_device(&diag);
            {
                let mut st = status.lock().unwrap();
                *st = ConnectionStatus::Searching {
                    usb_detected: usb_found,
                    message: if usb_found {
                        format!("Interface USB detectada ({})", ifname)
                    } else {
                        "Procurando R36S... Conecte o cabo na porta OTG".to_string()
                    },
                };
            }

            if !usb_found {
                thread::sleep(Duration::from_millis(800));
                continue;
            }
            if let Some(error) = host_ip_error {
                *status.lock().unwrap() = ConnectionStatus::Error {
                    message: format!("USB {ifname} detectado, mas o IP do PC não foi configurado: {error}"),
                };
                thread::sleep(Duration::from_secs(2));
                continue;
            }

            // Fase 2: Conexão TCP
            {
                let mut st = status.lock().unwrap();
                *st = ConnectionStatus::Connecting {
                    target: target_ip.to_string(),
                    stage: "Abrindo socket TCP...".to_string(),
                };
            }

            match TcpStream::connect_timeout(&target_addr, Duration::from_millis(900)) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(1500)));

                    // Fase 3: Handshake de protocolo
                    {
                        let mut st = status.lock().unwrap();
                        *st = ConnectionStatus::Connecting {
                            target: target_ip.to_string(),
                            stage: "Autenticando e verificando protocolo...".to_string(),
                        };
                    }

                    let handshake_cmd = ClientCommand::Handshake {
                        client_version: env!("CARGO_PKG_VERSION").to_string(),
                        client_id: "r36s-studio-pc".to_string(),
                    };

                    let mut handshake_ok = false;
                    let mut agent_ver = "desconhecida".to_string();

                    if Self::send_raw_cmd(&stream, &handshake_cmd, &bytes_tx).is_ok() {
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                            bytes_rx.fetch_add(line.len() as u64, Ordering::Relaxed);
                            if let Ok(ServerResponse::HandshakeAck { agent_version, fb_width, fb_height, fb_bpp, fb_format, .. }) = serde_json::from_str(&line) {
                                agent_ver = agent_version;
                                handshake_ok = true;
                                Self::add_diag(
                                    &diag,
                                    &format!(
                                        "Conectado ao R36S! Agente v{}, FB: {}x{} @ {} bpp (fmt {})",
                                        agent_ver, fb_width, fb_height, fb_bpp, fb_format
                                    ),
                                );
                            }
                        }
                    }

                    if !handshake_ok {
                        Self::add_diag(&diag, "Falha no handshake inicial com o daemon; tentando novamente...");
                        thread::sleep(Duration::from_millis(800));
                        continue;
                    }

                    {
                        let mut st = status.lock().unwrap();
                        *st = ConnectionStatus::Connected {
                            target: target_ip.to_string(),
                            latency_ms: 0.5,
                            agent_version: agent_ver,
                        };
                    }

                    // Se streaming já estiver ativo, solicita início no agente
                    if is_streaming.load(Ordering::Relaxed) {
                        let _ = Self::send_raw_cmd(&stream, &ClientCommand::StartStream { target_fps: 30 }, &bytes_tx);
                    }

                    Self::run_control_session(
                        stream,
                        &cmd_rx,
                        &status,
                        &metrics,
                        &logs,
                        &deploy_status,
                        &proc_result,
                        &diag,
                        &bytes_tx,
                        &bytes_rx,
                    );
                }
                Err(error) => {
                    let message = format!(
                        "USB detectado, mas 192.168.7.2:{} não respondeu: {}",
                        CONTROL_PORT, error
                    );
                    Self::add_diag(&diag, &message);
                    *status.lock().unwrap() = ConnectionStatus::Error { message };
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }

    fn run_control_session(
        stream: TcpStream,
        cmd_rx: &Receiver<ClientCommand>,
        status: &Arc<Mutex<ConnectionStatus>>,
        metrics: &Arc<Mutex<Option<SystemMetrics>>>,
        logs: &Arc<Mutex<Option<(String, String)>>>,
        deploy_status: &Arc<Mutex<Option<String>>>,
        proc_result: &Arc<Mutex<Option<(bool, String)>>>,
        diag: &Arc<Mutex<Vec<String>>>,
        bytes_tx: &Arc<AtomicU64>,
        bytes_rx: &Arc<AtomicU64>,
    ) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut last_metrics_req = Instant::now();
        let mut response_buf = String::new();

        loop {
            // Processa comandos pendentes na fila
            while let Ok(cmd) = cmd_rx.try_recv() {
                if Self::send_raw_cmd(&stream, &cmd, bytes_tx).is_err() {
                    return;
                }
                response_buf.clear();
                if reader.read_line(&mut response_buf).is_ok() {
                    bytes_rx.fetch_add(response_buf.len() as u64, Ordering::Relaxed);
                    Self::process_response(
                        &response_buf,
                        metrics,
                        logs,
                        deploy_status,
                        proc_result,
                        diag,
                    );
                }
            }

            // Coleta periódica de métricas a cada 1s
            if last_metrics_req.elapsed() >= Duration::from_millis(1000) {
                last_metrics_req = Instant::now();
                let start_ping = Instant::now();
                if Self::send_raw_cmd(&stream, &ClientCommand::GetMetrics, bytes_tx).is_err() {
                    return;
                }

                response_buf.clear();
                match reader.read_line(&mut response_buf) {
                    Ok(n) if n > 0 => {
                        bytes_rx.fetch_add(n as u64, Ordering::Relaxed);
                        let latency = start_ping.elapsed().as_secs_f32() * 1000.0;
                        {
                            let mut st = status.lock().unwrap();
                            if let ConnectionStatus::Connected { ref mut latency_ms, .. } = *st {
                                *latency_ms = latency;
                            }
                        }
                        Self::process_response(
                            &response_buf,
                            metrics,
                            logs,
                            deploy_status,
                            proc_result,
                            diag,
                        );
                    }
                    _ => return, // Desconectou
                }
            }

            thread::sleep(Duration::from_millis(30));
        }
    }

    fn send_raw_cmd(mut writer: &TcpStream, cmd: &ClientCommand, bytes_tx: &Arc<AtomicU64>) -> Result<(), std::io::Error> {
        let mut json = serde_json::to_string(cmd).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        json.push('\n');
        let b = json.as_bytes();
        bytes_tx.fetch_add(b.len() as u64, Ordering::Relaxed);
        writer.write_all(b)?;
        writer.flush()
    }

    fn process_response(
        raw: &str,
        metrics: &Arc<Mutex<Option<SystemMetrics>>>,
        logs: &Arc<Mutex<Option<(String, String)>>>,
        deploy_status: &Arc<Mutex<Option<String>>>,
        proc_result: &Arc<Mutex<Option<(bool, String)>>>,
        diag: &Arc<Mutex<Vec<String>>>,
    ) {
        match serde_json::from_str::<ServerResponse>(raw.trim()) {
            Ok(resp) => {
                match resp {
                    ServerResponse::Metrics(data) => {
                        let mut m = metrics.lock().unwrap();
                        *m = Some(data);
                    }
                ServerResponse::LogsResponse { log_type, content } => {
                    let mut l = logs.lock().unwrap();
                    *l = Some((log_type, content));
                }
                ServerResponse::DeployProgress { status, message, chunk_index } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(format!("[Chunk {} - {}] {}", chunk_index, status, message));
                }
                ServerResponse::DeploySuccess { path, sha256 } => {
                    let mut d = deploy_status.lock().unwrap();
                    let short_sha = if sha256.len() >= 8 { &sha256[0..8] } else { &sha256 };
                    *d = Some(format!("Sucesso! Arquivo gravado em {} (SHA: {})", path, short_sha));
                }
                ServerResponse::DeployError { error } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(format!("ERRO no Deploy: {}", error));
                }
                ServerResponse::KillResult { pid, success, message } => {
                    let mut p = proc_result.lock().unwrap();
                    *p = Some((success, format!("[PID {}] {}", pid, message)));
                }
                ServerResponse::RestartESResult { success, message } => {
                    let mut p = proc_result.lock().unwrap();
                    *p = Some((success, message));
                }
                _ => {}
            }
        }
        Err(e) => {
            Self::add_diag(diag, &format!("Erro ao deserializar resposta do daemon: {}", e));
        }
    }
}

    fn video_worker(
        current_frame: Arc<Mutex<Option<DecodedFrame>>>,
        is_streaming: Arc<AtomicBool>,
        status: Arc<Mutex<ConnectionStatus>>,
        current_fps: Arc<Mutex<f32>>,
        bytes_rx: Arc<AtomicU64>,
        diag: Arc<Mutex<Vec<String>>>,
    ) {
        let target_ip = "192.168.7.2";
        let target_addr: SocketAddr = format!("{}:{}", target_ip, VIDEO_PORT)
            .parse()
            .unwrap();

        let mut header_buf = [0u8; 20];
        let mut comp_buf = Vec::with_capacity(640 * 480 * 4);
        let mut frame_times: VecDeque<Instant> = VecDeque::with_capacity(70);

        loop {
            if !is_streaming.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            let is_connected = {
                let st = status.lock().unwrap();
                matches!(*st, ConnectionStatus::Connected { .. })
            };

            if !is_connected {
                thread::sleep(Duration::from_millis(200));
                continue;
            }

            match TcpStream::connect_timeout(&target_addr, Duration::from_millis(900)) {
                Ok(mut stream) => {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(2000)));
                    Self::add_diag(&diag, "Canal de vídeo TCP conectado.");

                    while is_streaming.load(Ordering::Relaxed) {
                        if stream.read_exact(&mut header_buf).is_err() {
                            break;
                        }
                        bytes_rx.fetch_add(20, Ordering::Relaxed);

                        if let Some(header) = VideoFrameHeader::from_bytes(&header_buf) {
                            let size = header.compressed_size as usize;
                            if size > 640 * 480 * 4 {
                                break;
                            }

                            // Cálculo de bytes esperados após descompressão baseado no formato real
                            let bytes_per_pixel = if header.format == FORMAT_RGB565_LZ4 { 2 } else { 4 };
                            let expected_uncompressed = (header.width as usize) * (header.height as usize) * bytes_per_pixel;

                            // Flag == 1: Quadro idêntico ao anterior (tela inalterada)
                            if header.flags == 1 {
                                let has_valid_frame = current_frame.lock().unwrap().is_some();
                                if has_valid_frame {
                                    let now = Instant::now();
                                    frame_times.push_back(now);
                                    Self::update_fps(&mut frame_times, &current_fps);
                                }
                                continue;
                            }

                            // Flag == 2: Framebuffer inacessível no console
                            if header.flags == 2 {
                                Self::add_diag(&diag, "Aviso: Framebuffer /dev/fb0 inacessível no R36S (verifique permissões ou modo do kernel)");
                                thread::sleep(Duration::from_millis(200));
                                continue;
                            }

                            // Flag == 3: Framebuffer mock em execução de testes
                            if header.flags == 3 {
                                // Frame mock (gerado sinteticamente para depuração)
                            }

                            comp_buf.resize(size, 0);
                            if stream.read_exact(&mut comp_buf).is_err() {
                                break;
                            }
                            bytes_rx.fetch_add(size as u64, Ordering::Relaxed);

                            // Descompacta LZ4 com o tamanho correto do formato do frame
                            match lz4_flex::decompress(&comp_buf, expected_uncompressed) {
                                Ok(decomp_buf) => {
                                    // Converte para RGBA8888 de acordo com o formato de entrada
                                    let rgba = match header.format {
                                        FORMAT_RGB565_LZ4 => Self::rgb565_to_rgba(&decomp_buf, header.width as usize, header.height as usize),
                                        FORMAT_BGRA8888_LZ4 => Self::bgra_to_rgba(&decomp_buf),
                                        _ => decomp_buf, // FORMAT_RGBA8888_LZ4 já é RGBA
                                    };

                                    let frame = DecodedFrame {
                                        frame_index: header.frame_index,
                                        width: header.width,
                                        height: header.height,
                                        rgba_data: rgba,
                                        timestamp: Instant::now(),
                                    };

                                    {
                                        let mut lock = current_frame.lock().unwrap();
                                        *lock = Some(frame);
                                    }

                                    let now = Instant::now();
                                    frame_times.push_back(now);
                                    Self::update_fps(&mut frame_times, &current_fps);
                                }
                                Err(e) => {
                                    Self::add_diag(&diag, &format!("Erro na descompressão LZ4: {}", e));
                                }
                            }
                        }
                    }
                    Self::add_diag(&diag, "Canal de vídeo desconectado.");
                    let mut fps = current_fps.lock().unwrap();
                    *fps = 0.0;
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    fn update_fps(frame_times: &mut VecDeque<Instant>, current_fps: &Arc<Mutex<f32>>) {
        let now = Instant::now();
        while let Some(&front) = frame_times.front() {
            if now.duration_since(front) > Duration::from_millis(1000) {
                frame_times.pop_front();
            } else {
                break;
            }
        }
        let fps = frame_times.len() as f32;
        let mut lock = current_fps.lock().unwrap();
        *lock = fps;
    }

    pub fn rgb565_to_rgba(rgb565: &[u8], width: usize, height: usize) -> Vec<u8> {
        let total_pixels = width * height;
        let mut rgba = vec![255u8; total_pixels * 4];

        for i in 0..total_pixels {
            if i * 2 + 1 >= rgb565.len() {
                break;
            }
            let low = rgb565[i * 2] as u16;
            let high = rgb565[i * 2 + 1] as u16;
            let pixel = low | (high << 8);

            let r = ((pixel >> 11) & 0x1F) as u32;
            let g = ((pixel >> 5) & 0x3F) as u32;
            let b = (pixel & 0x1F) as u32;

            let out_idx = i * 4;
            rgba[out_idx] = ((r * 255) / 31) as u8;
            rgba[out_idx + 1] = ((g * 255) / 63) as u8;
            rgba[out_idx + 2] = ((b * 255) / 31) as u8;
            rgba[out_idx + 3] = 255;
        }

        rgba
    }

    pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
        let mut rgba = bgra.to_vec();
        for chunk in rgba.chunks_exact_mut(4) {
            let b = chunk[0];
            let r = chunk[2];
            chunk[0] = r;
            chunk[2] = b;
        }
        rgba
    }
}

fn chrono_time() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let sec = now % 60;
    let min = (now / 60) % 60;
    let hr = (now / 3600) % 24;
    format!("{:02}:{:02}:{:02}", hr, min, sec)
}
