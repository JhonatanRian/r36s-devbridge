use crate::protocol::*;
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
    Searching {
        usb_detected: bool,
        message: String,
    },
    Connecting {
        target: String,
        stage: String,
    },
    Connected {
        target: String,
        latency_ms: f32,
        agent_version: String,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Off,
    Listen,
    Send,
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
    pub current_frame: Arc<Mutex<Option<Arc<DecodedFrame>>>>,
    pub last_video_frame: Arc<Mutex<Option<Instant>>>,
    pub last_logs: Arc<Mutex<Option<(String, String)>>>,
    pub deploy_status: Arc<Mutex<Option<String>>>,
    pub deploy_progress: Arc<Mutex<f32>>,
    pub is_streaming: Arc<AtomicBool>,
    pub video_connected: Arc<AtomicBool>,
    pub current_fps: Arc<Mutex<f32>>,
    pub throughput_kbps: Arc<Mutex<f32>>,
    pub diag_logs: Arc<Mutex<Vec<String>>>,
    pub last_process_result: Arc<Mutex<Option<(bool, String)>>>,
    pub total_bytes_rx: Arc<AtomicU64>,
    pub total_bytes_tx: Arc<AtomicU64>,
    pub gamepad_supported: Arc<AtomicBool>,
    pub gamepad_listening: Arc<AtomicBool>,
    pub gamepad_input_enabled: Arc<AtomicBool>,
    pub gamepad_state: Arc<Mutex<GamepadState>>,
    pub egui_ctx: Arc<Mutex<Option<egui::Context>>>,
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
        let last_video_frame = Arc::new(Mutex::new(None));
        let last_logs = Arc::new(Mutex::new(None));
        let deploy_status = Arc::new(Mutex::new(None));
        let deploy_progress = Arc::new(Mutex::new(0.0));
        let is_streaming = Arc::new(AtomicBool::new(false));
        let video_connected = Arc::new(AtomicBool::new(false));
        let current_fps = Arc::new(Mutex::new(0.0));
        let throughput_kbps = Arc::new(Mutex::new(0.0));
        let diag_logs = Arc::new(Mutex::new(Vec::new()));
        let last_process_result = Arc::new(Mutex::new(None));
        let total_bytes_rx = Arc::new(AtomicU64::new(0));
        let total_bytes_tx = Arc::new(AtomicU64::new(0));
        let gamepad_supported = Arc::new(AtomicBool::new(false));
        let gamepad_listening = Arc::new(AtomicBool::new(true));
        let gamepad_input_enabled = Arc::new(AtomicBool::new(false));
        let gamepad_state = Arc::new(Mutex::new(GamepadState::default()));
        let egui_ctx = Arc::new(Mutex::new(None));

        let (cmd_tx, cmd_rx) = channel::<ClientCommand>();

        let mgr = Self {
            status: Arc::clone(&status),
            metrics: Arc::clone(&metrics),
            current_frame: Arc::clone(&current_frame),
            last_video_frame: Arc::clone(&last_video_frame),
            last_logs: Arc::clone(&last_logs),
            deploy_status: Arc::clone(&deploy_status),
            deploy_progress: Arc::clone(&deploy_progress),
            is_streaming: Arc::clone(&is_streaming),
            video_connected: Arc::clone(&video_connected),
            current_fps: Arc::clone(&current_fps),
            throughput_kbps: Arc::clone(&throughput_kbps),
            diag_logs: Arc::clone(&diag_logs),
            last_process_result: Arc::clone(&last_process_result),
            total_bytes_rx: Arc::clone(&total_bytes_rx),
            total_bytes_tx: Arc::clone(&total_bytes_tx),
            gamepad_supported: Arc::clone(&gamepad_supported),
            gamepad_listening: Arc::clone(&gamepad_listening),
            gamepad_input_enabled: Arc::clone(&gamepad_input_enabled),
            gamepad_state: Arc::clone(&gamepad_state),
            egui_ctx: Arc::clone(&egui_ctx),
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
        let gp_listening_cl = Arc::clone(&gamepad_listening);
        let gp_supported_cl = Arc::clone(&gamepad_supported);
        let gp_input_cl = Arc::clone(&gamepad_input_enabled);
        let gp_state_cl = Arc::clone(&gamepad_state);

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
                gp_listening_cl,
                gp_state_cl,
                gp_supported_cl,
                gp_input_cl,
            );
        });

        // Thread de streaming de vídeo
        let frame_cl = Arc::clone(&current_frame);
        let is_streaming_vid = Arc::clone(&is_streaming);
        let status_vid = Arc::clone(&status);
        let vid_rx = Arc::clone(&total_bytes_rx);
        let vid_diag = Arc::clone(&diag_logs);
        let vid_conn = Arc::clone(&video_connected);
        let received = Arc::clone(&last_video_frame);
        let egui_ctx_vid = Arc::clone(&egui_ctx);

        thread::spawn(move || {
            Self::video_worker(
                frame_cl,
                is_streaming_vid,
                status_vid,
                vid_rx,
                vid_diag,
                vid_conn,
                received,
                egui_ctx_vid,
            );
        });

        mgr
    }

    pub fn set_egui_ctx(&self, ctx: egui::Context) {
        *self.egui_ctx.lock().unwrap() = Some(ctx);
    }

    pub fn send_command(&self, cmd: ClientCommand) {
        let _ = self.cmd_sender.send(cmd);
    }

    pub fn toggle_streaming(&self) {
        let current = self.is_streaming.load(Ordering::Relaxed);
        let new_state = !current;
        self.is_streaming.store(new_state, Ordering::Relaxed);
        if new_state {
            self.send_command(ClientCommand::StartStream { target_fps: 60 });
        } else {
            self.send_command(ClientCommand::StopStream);
            let mut fps = self.current_fps.lock().unwrap();
            *fps = 0.0;
        }
    }

    pub fn set_input_mode(&self, mode: InputMode) {
        let send = mode == InputMode::Send;
        let listen = mode == InputMode::Listen;
        self.gamepad_input_enabled.store(send, Ordering::Relaxed);
        self.gamepad_listening.store(listen, Ordering::Relaxed);
        // Disable the previous mode before enabling its replacement.
        if send {
            self.send_command(ClientCommand::SetGamepadMonitor { enabled: false });
            self.send_command(ClientCommand::SetGamepadInput { enabled: true });
        } else {
            self.send_command(ClientCommand::SetGamepadInput { enabled: false });
            self.send_command(ClientCommand::SetGamepadMonitor { enabled: listen });
        }
    }
    pub fn set_gamepad_listening(&self, enabled: bool) {
        self.set_input_mode(if enabled {
            InputMode::Listen
        } else {
            InputMode::Off
        });
    }
    pub fn send_gamepad_action(&self, action: GamepadInputAction) {
        if self.gamepad_input_enabled.load(Ordering::Relaxed) {
            self.send_command(ClientCommand::InjectGamepadInput { action });
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
                        let device = match entry.path().join("device").canonicalize() {
                            Ok(device) => device,
                            Err(_) => continue,
                        };
                        let known_gadget = device.ancestors().any(|parent| {
                            fs::read_to_string(parent.join("idVendor"))
                                .map_or(false, |v| v.trim() == "0525")
                                && fs::read_to_string(parent.join("idProduct"))
                                    .map_or(false, |v| v.trim() == "a4a2")
                        });
                        if !known_gadget {
                            continue;
                        }
                        let ip_error = Self::ensure_host_ip(&name_str, diag).err();
                        return (true, name_str.to_string(), ip_error);
                    }
                }
            }
        }
        (false, String::new(), None)
    }

    fn host_has_ip(ifname: &str) -> Result<bool, String> {
        if let Ok(output) = std::process::Command::new("ip")
            .args(["-4", "-o", "addr", "show", "dev", ifname])
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.contains("192.168.7.1/") {
                    return Ok(true);
                }
            }
        }

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
        let mac = fs::read_to_string(format!("/sys/class/net/{ifname}/address"))
            .map_err(|error| error.to_string())?;
        let mac = mac.trim();
        let profile = format!("DevBridge R36S {}", mac.replace(':', ""));
        let active = std::process::Command::new("nmcli")
            .args(["-g", "GENERAL.CONNECTION", "device", "show", ifname])
            .output()
            .map_err(|error| error.to_string())?;
        if String::from_utf8_lossy(&active.stdout).trim() == profile && Self::host_has_ip(ifname)? {
            return Ok(());
        }
        let exists = std::process::Command::new("nmcli")
            .args(["connection", "show", "id", &profile])
            .output()
            .map_err(|error| error.to_string())?
            .status
            .success();
        if !exists {
            let output = std::process::Command::new("nmcli")
                .args([
                    "--wait",
                    "5",
                    "connection",
                    "add",
                    "type",
                    "ethernet",
                    "ifname",
                    ifname,
                    "con-name",
                    &profile,
                    "802-3-ethernet.mac-address",
                    mac,
                    "ipv4.method",
                    "manual",
                    "ipv4.addresses",
                    "192.168.7.1/24",
                    "ipv4.never-default",
                    "yes",
                    "ipv6.method",
                    "disabled",
                    "connection.autoconnect",
                    "yes",
                    "connection.autoconnect-priority",
                    "100",
                ])
                .output()
                .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(format!(
                    "Falha ao criar perfil USB: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        let output = std::process::Command::new("nmcli")
            .args([
                "--wait",
                "5",
                "connection",
                "up",
                "id",
                &profile,
                "ifname",
                ifname,
            ])
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() || !Self::host_has_ip(ifname)? {
            return Err(format!(
                "Falha ao ativar IP USB persistente: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Self::add_diag(
            diag,
            &format!("Perfil USB persistente {profile}: 192.168.7.1/24."),
        );
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
        gamepad_listening: Arc<AtomicBool>,
        gamepad_state: Arc<Mutex<GamepadState>>,
        gamepad_supported: Arc<AtomicBool>,
        gamepad_input: Arc<AtomicBool>,
    ) {
        let target_ip = "192.168.7.2";
        let target_addr: SocketAddr = format!("{}:{}", target_ip, CONTROL_PORT).parse().unwrap();

        loop {
            gamepad_supported.store(false, Ordering::Relaxed);
            gamepad_input.store(false, Ordering::Relaxed);
            *gamepad_state.lock().unwrap() = GamepadState::default();
            *metrics.lock().unwrap() = None;
            while cmd_rx.try_recv().is_ok() {} // Never replay input from a previous session.
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
                    message: format!(
                        "USB {ifname} detectado, mas o IP do PC não foi configurado: {error}"
                    ),
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

            match TcpStream::connect_timeout(&target_addr, Duration::from_millis(1500)) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(4500)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(4500)));

                    // Commands from a lost session must never re-enable injection on a new one.
                    while cmd_rx.try_recv().is_ok() {}
                    gamepad_listening.store(true, Ordering::Relaxed);
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
                    let mut reader = BufReader::new(stream.try_clone().unwrap());

                    if Self::send_raw_cmd(&stream, &handshake_cmd, &bytes_tx).is_ok() {
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                            bytes_rx.fetch_add(line.len() as u64, Ordering::Relaxed);
                            if let Ok(ServerResponse::HandshakeAck {
                                agent_version,
                                compatible,
                                capabilities,
                                fb_width,
                                fb_height,
                                fb_bpp,
                                fb_format,
                                ..
                            }) = serde_json::from_str(&line)
                            {
                                agent_ver = agent_version;
                                handshake_ok = compatible;
                                gamepad_supported.store(
                                    capabilities.iter().any(|c| c == "gamepad-v2"),
                                    Ordering::Relaxed,
                                );
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
                        *status.lock().unwrap() = ConnectionStatus::Error {
                            message: "Handshake inválido ou protocolo incompatível".into(),
                        };
                        Self::add_diag(
                            &diag,
                            "Falha no handshake inicial com o daemon; tentando novamente...",
                        );
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

                    // Se streaming já estiver ativo, solicita início no agente e consome o ACK para não dessincronizar o reader
                    if is_streaming.load(Ordering::Relaxed) {
                        if Self::send_raw_cmd(
                            &stream,
                            &ClientCommand::StartStream { target_fps: 60 },
                            &bytes_tx,
                        )
                        .is_ok()
                        {
                            let mut ack_line = String::new();
                            if let Ok(n) = reader.read_line(&mut ack_line) {
                                bytes_rx.fetch_add(n as u64, Ordering::Relaxed);
                            }
                        }
                    }

                    // Se monitoramento de gamepad já estiver ativo na reconexão, habilita no agente
                    if gamepad_supported.load(Ordering::Relaxed)
                        && gamepad_listening.load(Ordering::Relaxed)
                    {
                        if Self::send_raw_cmd(
                            &stream,
                            &ClientCommand::SetGamepadMonitor { enabled: true },
                            &bytes_tx,
                        )
                        .is_ok()
                        {
                            let mut ack_line = String::new();
                            if let Ok(n) = reader.read_line(&mut ack_line) {
                                bytes_rx.fetch_add(n as u64, Ordering::Relaxed);
                            }
                        }
                    }

                    Self::run_control_session(
                        stream,
                        reader,
                        &cmd_rx,
                        &status,
                        &metrics,
                        &logs,
                        &deploy_status,
                        &proc_result,
                        &diag,
                        &bytes_tx,
                        &bytes_rx,
                        &gamepad_listening,
                        &gamepad_state,
                        &gamepad_supported,
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
        mut reader: BufReader<TcpStream>,
        cmd_rx: &Receiver<ClientCommand>,
        status: &Arc<Mutex<ConnectionStatus>>,
        metrics: &Arc<Mutex<Option<SystemMetrics>>>,
        logs: &Arc<Mutex<Option<(String, String)>>>,
        deploy_status: &Arc<Mutex<Option<String>>>,
        proc_result: &Arc<Mutex<Option<(bool, String)>>>,
        diag: &Arc<Mutex<Vec<String>>>,
        bytes_tx: &Arc<AtomicU64>,
        bytes_rx: &Arc<AtomicU64>,
        gamepad_listening: &Arc<AtomicBool>,
        gamepad_state: &Arc<Mutex<GamepadState>>,
        gamepad_supported: &Arc<AtomicBool>,
    ) {
        let mut last_metrics_req = Instant::now();
        let mut last_gamepad_req = Instant::now();
        let mut response_buf = String::new();

        loop {
            // Processa comandos pendentes na fila
            while let Ok(cmd) = cmd_rx.try_recv() {
                if Self::send_raw_cmd(&stream, &cmd, bytes_tx).is_err() {
                    return;
                }
                response_buf.clear();
                if matches!(reader.read_line(&mut response_buf), Ok(n) if n > 0) {
                    bytes_rx.fetch_add(response_buf.len() as u64, Ordering::Relaxed);
                    Self::process_response(
                        &response_buf,
                        metrics,
                        logs,
                        deploy_status,
                        proc_result,
                        diag,
                        gamepad_state,
                    );
                } else {
                    return;
                }
            }

            // Polling de gamepad se gamepad_listening estiver ativo (~35ms = ~28 Hz)
            if gamepad_supported.load(Ordering::Relaxed)
                && gamepad_listening.load(Ordering::Relaxed)
                && last_gamepad_req.elapsed() >= Duration::from_millis(35)
            {
                last_gamepad_req = Instant::now();
                if Self::send_raw_cmd(&stream, &ClientCommand::GetGamepadState, bytes_tx).is_err() {
                    return;
                }
                response_buf.clear();
                match reader.read_line(&mut response_buf) {
                    Ok(n) if n > 0 => {
                        bytes_rx.fetch_add(n as u64, Ordering::Relaxed);
                        Self::process_response(
                            &response_buf,
                            metrics,
                            logs,
                            deploy_status,
                            proc_result,
                            diag,
                            gamepad_state,
                        );
                    }
                    _ => return, // Desconectou
                }
            }

            // Coleta periódica de métricas a cada 1s
            if last_metrics_req.elapsed() >= Duration::from_millis(1000) {
                last_metrics_req = Instant::now();
                let start_ping = Instant::now();
                if Self::send_raw_cmd(&stream, &ClientCommand::Ping, bytes_tx).is_err() {
                    return;
                }
                response_buf.clear();
                if reader.read_line(&mut response_buf).unwrap_or(0) == 0 {
                    return;
                }
                bytes_rx.fetch_add(response_buf.len() as u64, Ordering::Relaxed);
                if !matches!(
                    serde_json::from_str::<ServerResponse>(&response_buf),
                    Ok(ServerResponse::Pong)
                ) {
                    return;
                }
                let ping_ms = start_ping.elapsed().as_secs_f32() * 1000.0;
                if Self::send_raw_cmd(&stream, &ClientCommand::GetMetrics, bytes_tx).is_err() {
                    return;
                }

                response_buf.clear();
                match reader.read_line(&mut response_buf) {
                    Ok(n) if n > 0 => {
                        bytes_rx.fetch_add(n as u64, Ordering::Relaxed);
                        let latency = ping_ms;
                        {
                            let mut st = status.lock().unwrap();
                            if let ConnectionStatus::Connected {
                                ref mut latency_ms, ..
                            } = *st
                            {
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
                            gamepad_state,
                        );
                    }
                    _ => return, // Desconectou
                }
            }

            let sleep_ms = if gamepad_listening.load(Ordering::Relaxed) {
                10
            } else {
                30
            };
            thread::sleep(Duration::from_millis(sleep_ms));
        }
    }

    fn send_raw_cmd(
        mut writer: &TcpStream,
        cmd: &ClientCommand,
        bytes_tx: &Arc<AtomicU64>,
    ) -> Result<(), std::io::Error> {
        let mut json = serde_json::to_string(cmd)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
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
        gamepad_state: &Arc<Mutex<GamepadState>>,
    ) {
        match serde_json::from_str::<ServerResponse>(raw.trim()) {
            Ok(resp) => match resp {
                ServerResponse::Metrics(data) => {
                    let mut m = metrics.lock().unwrap();
                    *m = Some(data);
                }
                ServerResponse::LogsResponse { log_type, content } => {
                    let mut l = logs.lock().unwrap();
                    *l = Some((log_type, content));
                }
                ServerResponse::DeployProgress {
                    status,
                    message,
                    chunk_index,
                } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(format!("[Chunk {} - {}] {}", chunk_index, status, message));
                }
                ServerResponse::DeploySuccess { path, sha256 } => {
                    let mut d = deploy_status.lock().unwrap();
                    let short_sha = if sha256.len() >= 8 {
                        &sha256[0..8]
                    } else {
                        &sha256
                    };
                    *d = Some(format!(
                        "Sucesso! Arquivo gravado em {} (SHA: {})",
                        path, short_sha
                    ));
                }
                ServerResponse::DeployError { error } => {
                    let mut d = deploy_status.lock().unwrap();
                    *d = Some(format!("ERRO no Deploy: {}", error));
                }
                ServerResponse::KillResult {
                    pid,
                    success,
                    message,
                } => {
                    let mut p = proc_result.lock().unwrap();
                    *p = Some((success, format!("[PID {}] {}", pid, message)));
                }
                ServerResponse::RestartESResult { success, message } => {
                    let mut p = proc_result.lock().unwrap();
                    *p = Some((success, message));
                }
                ServerResponse::GamepadStateResponse(mut state) => {
                    let mut g = gamepad_state.lock().unwrap();
                    let mut pending = std::mem::take(&mut g.transitions);
                    pending.append(&mut state.transitions);
                    if pending.len() > 256 {
                        pending.drain(..pending.len() - 256);
                        state.overflow = true;
                    }
                    state.transitions = pending;
                    *g = state;
                }
                ServerResponse::GamepadAck { success, message } => {
                    if !success {
                        Self::add_diag(diag, &format!("Aviso Gamepad: {}", message));
                        let mut state = gamepad_state.lock().unwrap();
                        state.error = Some(message);
                        state.input_enabled = false;
                    }
                }
                _ => {}
            },
            Err(e) => {
                Self::add_diag(
                    diag,
                    &format!("Erro ao deserializar resposta do daemon: {}", e),
                );
            }
        }
    }

    fn video_worker(
        current_frame: Arc<Mutex<Option<Arc<DecodedFrame>>>>,
        is_streaming: Arc<AtomicBool>,
        status: Arc<Mutex<ConnectionStatus>>,
        bytes_rx: Arc<AtomicU64>,
        diag: Arc<Mutex<Vec<String>>>,
        video_connected: Arc<AtomicBool>,
        received: Arc<Mutex<Option<Instant>>>,
        egui_ctx: Arc<Mutex<Option<egui::Context>>>,
    ) {
        let target_ip = "192.168.7.2";
        let target_addr: SocketAddr = format!("{}:{}", target_ip, VIDEO_PORT).parse().unwrap();

        let mut header_buf = [0u8; 20];
        let mut comp_buf = Vec::with_capacity(640 * 480 * 4 + 65536);

        loop {
            if !is_streaming.load(Ordering::Relaxed) {
                *current_frame.lock().unwrap() = None;
                *received.lock().unwrap() = None;
                video_connected.store(false, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            let is_connected = {
                let st = status.lock().unwrap();
                matches!(*st, ConnectionStatus::Connected { .. })
            };

            if !is_connected {
                *current_frame.lock().unwrap() = None;
                *received.lock().unwrap() = None;
                video_connected.store(false, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(200));
                continue;
            }

            match TcpStream::connect_timeout(&target_addr, Duration::from_millis(1500)) {
                Ok(mut stream) => {
                    video_connected.store(true, Ordering::Relaxed);
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(5000)));
                    Self::add_diag(&diag, "Canal de vídeo TCP conectado.");

                    while is_streaming.load(Ordering::Relaxed) {
                        if stream.read_exact(&mut header_buf).is_err() {
                            break;
                        }
                        bytes_rx.fetch_add(20, Ordering::Relaxed);

                        if let Some(header) = VideoFrameHeader::from_bytes(&header_buf) {
                            let size = header.compressed_size as usize;
                            let max_allowed =
                                ((header.width as usize) * (header.height as usize) * 4) + 65536;
                            if header.width == 0
                                || header.height == 0
                                || max_allowed > 16 * 1024 * 1024
                                || !matches!(
                                    header.format,
                                    FORMAT_RGB565_LZ4 | FORMAT_BGRA8888_LZ4 | FORMAT_RGBA8888_LZ4
                                )
                                || header.flags > 3
                                || (matches!(header.flags, 1 | 2) && size != 0)
                                || size > max_allowed
                                || (size == 0 && header.flags == 0)
                            {
                                Self::add_diag(&diag, &format!("Aviso: Pacote de vídeo inválido (tam {size}, max {max_allowed})"));
                                break;
                            }

                            // Cálculo de bytes esperados após descompressão baseado no formato real
                            let bytes_per_pixel = if header.format == FORMAT_RGB565_LZ4 {
                                2
                            } else {
                                4
                            };
                            let expected_uncompressed = (header.width as usize)
                                * (header.height as usize)
                                * bytes_per_pixel;

                            // Flag == 1: Quadro idêntico ao anterior (tela inalterada)
                            if header.flags == 1 {
                                *received.lock().unwrap() = Some(Instant::now());
                                continue;
                            }

                            // Flag == 2: Framebuffer temporariamente em transição ou indisponível
                            if header.flags == 2 {
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
                                        FORMAT_RGB565_LZ4 => Self::rgb565_to_rgba(
                                            &decomp_buf,
                                            header.width as usize,
                                            header.height as usize,
                                        ),
                                        FORMAT_BGRA8888_LZ4 => Self::bgra_to_rgba(&decomp_buf),
                                        _ => {
                                            let mut buf = decomp_buf;
                                            Self::sanitize_rgba_alpha(&mut buf);
                                            buf
                                        }
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
                                        *lock = Some(Arc::new(frame));
                                        *received.lock().unwrap() = Some(Instant::now());
                                    }

                                    if let Some(ctx) = egui_ctx.lock().unwrap().as_ref() {
                                        ctx.request_repaint();
                                    }
                                }
                                Err(e) => {
                                    Self::add_diag(
                                        &diag,
                                        &format!("Erro na descompressão LZ4: {}", e),
                                    );
                                }
                            }
                        }
                    }
                    video_connected.store(false, Ordering::Relaxed);
                    Self::add_diag(&diag, "Canal de vídeo desconectado.");
                }
                Err(_) => {
                    video_connected.store(false, Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
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
            chunk[3] = 255; // Garante opacidade absoluta: evita tela preta/escurecida pelo alpha do display driver
        }
        rgba
    }

    pub fn sanitize_rgba_alpha(rgba: &mut [u8]) {
        for chunk in rgba.chunks_exact_mut(4) {
            chunk[3] = 255;
        }
    }
}

fn chrono_time() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sec = now % 60;
    let min = (now / 60) % 60;
    let hr = (now / 3600) % 24;
    format!("{:02}:{:02}:{:02}", hr, min, sec)
}
