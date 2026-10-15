use crate::protocol::*;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Searching { usb_detected: bool, message: String },
    Connecting { target: String },
    Connected { target: String, latency_ms: f32 },
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

        let (cmd_tx, cmd_rx) = channel::<ClientCommand>();

        let mgr = Self {
            status: Arc::clone(&status),
            metrics: Arc::clone(&metrics),
            current_frame: Arc::clone(&current_frame),
            last_logs: Arc::clone(&last_logs),
            deploy_status: Arc::clone(&deploy_status),
            deploy_progress: Arc::clone(&deploy_progress),
            is_streaming: Arc::clone(&is_streaming),
            cmd_sender: cmd_tx,
        };

        // Thread de controle e telemetria
        let status_cl = Arc::clone(&status);
        let metrics_cl = Arc::clone(&metrics);
        let logs_cl = Arc::clone(&last_logs);
        let deploy_st_cl = Arc::clone(&deploy_status);
        let is_streaming_cl = Arc::clone(&is_streaming);
        thread::spawn(move || {
            Self::control_worker(
                cmd_rx,
                status_cl,
                metrics_cl,
                logs_cl,
                deploy_st_cl,
                is_streaming_cl,
            );
        });

        // Thread de streaming de vídeo
        let frame_cl = Arc::clone(&current_frame);
        let is_streaming_vid = Arc::clone(&is_streaming);
        let status_vid = Arc::clone(&status);
        thread::spawn(move || {
            Self::video_worker(frame_cl, is_streaming_vid, status_vid);
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
        }
    }

    fn check_usb_device() -> bool {
        // Varre interfaces de rede USB no Linux
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
                        return true;
                    }
                }
            }
        }
        false
    }

    fn control_worker(
        cmd_rx: Receiver<ClientCommand>,
        status: Arc<Mutex<ConnectionStatus>>,
        metrics: Arc<Mutex<Option<SystemMetrics>>>,
        logs: Arc<Mutex<Option<(String, String)>>>,
        deploy_status: Arc<Mutex<Option<String>>>,
        is_streaming: Arc<AtomicBool>,
    ) {
        let target_ip = "192.168.7.2";
        let target_addr: SocketAddr = format!("{}:{}", target_ip, CONTROL_PORT)
            .parse()
            .unwrap();

        loop {
            // Fase 1: Descoberta e tentativa de conexão
            let usb_found = Self::check_usb_device();
            {
                let mut st = status.lock().unwrap();
                *st = ConnectionStatus::Searching {
                    usb_detected: usb_found,
                    message: if usb_found {
                        format!("Interface USB detectada. Conectando em {}...", target_ip)
                    } else {
                        "Procurando R36S... Conecte o cabo na porta OTG".to_string()
                    },
                };
            }

            match TcpStream::connect_timeout(&target_addr, Duration::from_millis(800)) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(1500)));

                    {
                        let mut st = status.lock().unwrap();
                        *st = ConnectionStatus::Connected {
                            target: target_ip.to_string(),
                            latency_ms: 1.0,
                        };
                    }

                    // Se streaming estiver ativo, reinicia stream no agente
                    if is_streaming.load(Ordering::Relaxed) {
                        let _ = Self::send_raw_cmd(&stream, &ClientCommand::StartStream { target_fps: 30 });
                    }

                    Self::run_control_session(
                        stream,
                        &cmd_rx,
                        &status,
                        &metrics,
                        &logs,
                        &deploy_status,
                    );
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(800));
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
    ) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut last_metrics_req = Instant::now();
        let mut response_buf = String::new();

        loop {
            // Verifica comandos pendentes na fila
            while let Ok(cmd) = cmd_rx.try_recv() {
                if Self::send_raw_cmd(&stream, &cmd).is_err() {
                    return;
                }
                response_buf.clear();
                if reader.read_line(&mut response_buf).is_ok() {
                    Self::process_response(
                        &response_buf,
                        metrics,
                        logs,
                        deploy_status,
                    );
                }
            }

            // Coleta periódica de métricas a cada 1s
            if last_metrics_req.elapsed() >= Duration::from_millis(1000) {
                last_metrics_req = Instant::now();
                let start_ping = Instant::now();
                if Self::send_raw_cmd(&stream, &ClientCommand::GetMetrics).is_err() {
                    return;
                }

                response_buf.clear();
                match reader.read_line(&mut response_buf) {
                    Ok(n) if n > 0 => {
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
                        );
                    }
                    _ => return, // Desconectou
                }
            }

            thread::sleep(Duration::from_millis(30));
        }
    }

    fn send_raw_cmd(mut writer: &TcpStream, cmd: &ClientCommand) -> Result<(), std::io::Error> {
        let mut json = serde_json::to_string(cmd).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        json.push('\n');
        writer.write_all(json.as_bytes())?;
        writer.flush()
    }

    fn process_response(
        raw: &str,
        metrics: &Arc<Mutex<Option<SystemMetrics>>>,
        logs: &Arc<Mutex<Option<(String, String)>>>,
        deploy_status: &Arc<Mutex<Option<String>>>,
    ) {
        if let Ok(resp) = serde_json::from_str::<ServerResponse>(raw.trim()) {
            match resp {
                ServerResponse::Metrics(data) => {
                    let mut m = metrics.lock().unwrap();
                    *m = Some(data);
                }
                ServerResponse::LogsResponse { log_type, content } => {
                    let mut l = logs.lock().unwrap();
                    *l = Some((log_type, content));
                }
                ServerResponse::DeployProgress { status, message } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(format!("[{}] {}", status, message));
                }
                ServerResponse::DeploySuccess { path, sha256 } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(format!("Sucesso! Arquivo gravado em {} (SHA-256: {})", path, &sha256[0..8]));
                }
                ServerResponse::DeployError { error } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(format!("ERRO no Deploy: {}", error));
                }
                ServerResponse::KillResult { message, .. } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(message);
                }
                ServerResponse::RestartESResult { message, .. } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(message);
                }
                _ => {}
            }
        }
    }

    fn video_worker(
        current_frame: Arc<Mutex<Option<DecodedFrame>>>,
        is_streaming: Arc<AtomicBool>,
        status: Arc<Mutex<ConnectionStatus>>,
    ) {
        let target_ip = "192.168.7.2";
        let target_addr: SocketAddr = format!("{}:{}", target_ip, VIDEO_PORT)
            .parse()
            .unwrap();

        let mut header_buf = [0u8; 20];
        let mut comp_buf = Vec::with_capacity(640 * 480 * 2);
        let mut last_raw_rgb565 = vec![0u8; 640 * 480 * 2];

        loop {
            // Só conecta o canal de vídeo se estivermos com streaming ativo
            if !is_streaming.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            // Só tenta conectar se o controle principal estiver conectado
            let is_connected = {
                let st = status.lock().unwrap();
                matches!(*st, ConnectionStatus::Connected { .. })
            };

            if !is_connected {
                thread::sleep(Duration::from_millis(200));
                continue;
            }

            match TcpStream::connect_timeout(&target_addr, Duration::from_millis(800)) {
                Ok(mut stream) => {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(2000)));

                    while is_streaming.load(Ordering::Relaxed) {
                        if stream.read_exact(&mut header_buf).is_err() {
                            break;
                        }

                        if let Some(header) = VideoFrameHeader::from_bytes(&header_buf) {
                            let size = header.compressed_size as usize;
                            if size > 640 * 480 * 4 {
                                break;
                            }

                            comp_buf.resize(size, 0);
                            if stream.read_exact(&mut comp_buf).is_err() {
                                break;
                            }

                            // Se flag == 1 (unchanged), mantém o último frame sem reprocessar
                            if header.flags == 1 {
                                continue;
                            }

                            // Descompacta LZ4
                            if let Ok(decompressed) = lz4_flex::decompress(&comp_buf, (header.width as usize) * (header.height as usize) * 2) {
                                last_raw_rgb565 = decompressed;
                            } else if header.format == FORMAT_RGB565_LZ4 {
                                // Fallback se descompressão rápida com tamanho exato falhar
                                if let Ok(decompressed) = lz4_flex::decompress_size_prepended(&comp_buf) {
                                    last_raw_rgb565 = decompressed;
                                }
                            }

                            // Converte RGB565 para RGBA8888
                            let rgba = Self::rgb565_to_rgba(&last_raw_rgb565, header.width as usize, header.height as usize);

                            let frame = DecodedFrame {
                                frame_index: header.frame_index,
                                width: header.width,
                                height: header.height,
                                rgba_data: rgba,
                                timestamp: Instant::now(),
                            };

                            let mut lock = current_frame.lock().unwrap();
                            *lock = Some(frame);
                        }
                    }
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    fn rgb565_to_rgba(rgb565: &[u8], width: usize, height: usize) -> Vec<u8> {
        let total_pixels = width * height;
        let mut rgba = vec![255u8; total_pixels * 4];

        for i in 0..total_pixels {
            if i * 2 + 1 >= rgb565.len() {
                break;
            }
            let low = rgb565[i * 2] as u16;
            let high = rgb565[i * 2 + 1] as u16;
            let pixel = low | (high << 8);

            let r = ((pixel >> 11) & 0x1F) as u8;
            let g = ((pixel >> 5) & 0x3F) as u8;
            let b = (pixel & 0x1F) as u8;

            let out_idx = i * 4;
            rgba[out_idx] = (r * 255) / 31;
            rgba[out_idx + 1] = (g * 255) / 63;
            rgba[out_idx + 2] = (b * 255) / 31;
            rgba[out_idx + 3] = 255;
        }

        rgba
    }
}
