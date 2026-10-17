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
                                Self::process_response(
                                    &ack_line,
                                    &metrics,
                                    &logs,
                                    &deploy_status,
                                    &proc_result,
                                    &diag,
                                    &gamepad_state,
                                    &status,
                                    &Arc::new(Mutex::new(None)),
                                );
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

    pub fn run_control_session(
        stream: TcpStream,
        reader: BufReader<TcpStream>,
        cmd_rx: &Receiver<ClientCommand>,
        status: &Arc<Mutex<ConnectionStatus>>,
        metrics: &Arc<Mutex<Option<SystemMetrics>>>,
        logs: &Arc<Mutex<Option<(String, String)>>>,
        deploy_status: &Arc<Mutex<Option<String>>>,
        proc_result: &Arc<Mutex<Option<(bool, String)>>>,
        diag: &Arc<Mutex<Vec<String>>>,
        bytes_tx: &Arc<AtomicU64>,
        bytes_rx: &Arc<AtomicU64>,
        _gamepad_listening: &Arc<AtomicBool>,
        gamepad_state: &Arc<Mutex<GamepadState>>,
        _gamepad_supported: &Arc<AtomicBool>,
    ) {
        let is_running = Arc::new(AtomicBool::new(true));
        let pending_ping_start = Arc::new(Mutex::new(None));

        let is_running_reader = Arc::clone(&is_running);
        let metrics_reader = Arc::clone(metrics);
        let logs_reader = Arc::clone(logs);
        let deploy_status_reader = Arc::clone(deploy_status);
        let proc_result_reader = Arc::clone(proc_result);
        let diag_reader = Arc::clone(diag);
        let gamepad_state_reader = Arc::clone(gamepad_state);
        let status_reader = Arc::clone(status);
        let bytes_rx_reader = Arc::clone(bytes_rx);
        let pending_ping_reader = Arc::clone(&pending_ping_start);

        let reader_handle = thread::spawn(move || {
            let mut reader = reader;
            let mut line = String::new();
            while is_running_reader.load(Ordering::Relaxed) {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        break;
                    }
                    Ok(n) => {
                        bytes_rx_reader.fetch_add(n as u64, Ordering::Relaxed);
                        Self::process_response(
                            &line,
                            &metrics_reader,
                            &logs_reader,
                            &deploy_status_reader,
                            &proc_result_reader,
                            &diag_reader,
                            &gamepad_state_reader,
                            &status_reader,
                            &pending_ping_reader,
                        );
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::Interrupted {
                            break;
                        }
                    }
                }
            }
            is_running_reader.store(false, Ordering::Relaxed);
        });

        let mut last_metrics_req = Instant::now();
        while is_running.load(Ordering::Relaxed) {
            match cmd_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(cmd) => {
                    if Self::send_raw_cmd(&stream, &cmd, bytes_tx).is_err() {
                        is_running.store(false, Ordering::Relaxed);
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    is_running.store(false, Ordering::Relaxed);
                    break;
                }
            }

            while let Ok(cmd) = cmd_rx.try_recv() {
                if Self::send_raw_cmd(&stream, &cmd, bytes_tx).is_err() {
                    is_running.store(false, Ordering::Relaxed);
                    break;
                }
            }

            if !is_running.load(Ordering::Relaxed) {
                break;
            }

            // Coleta periódica de métricas a cada 1s (com cálculo de RTT por Ping/Pong)
            if last_metrics_req.elapsed() >= Duration::from_millis(1000) {
                last_metrics_req = Instant::now();
                *pending_ping_start.lock().unwrap() = Some(Instant::now());
                if Self::send_raw_cmd(&stream, &ClientCommand::Ping, bytes_tx).is_err() {
                    is_running.store(false, Ordering::Relaxed);
                    break;
                }
                if Self::send_raw_cmd(&stream, &ClientCommand::GetMetrics, bytes_tx).is_err() {
                    is_running.store(false, Ordering::Relaxed);
                    break;
                }
            }
        }

        is_running.store(false, Ordering::Relaxed);
        let _ = stream.shutdown(std::net::Shutdown::Both);
        let _ = reader_handle.join();
    }

    pub fn send_raw_cmd(
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

    pub fn process_response(
        raw: &str,
        metrics: &Arc<Mutex<Option<SystemMetrics>>>,
        logs: &Arc<Mutex<Option<(String, String)>>>,
        deploy_status: &Arc<Mutex<Option<String>>>,
        proc_result: &Arc<Mutex<Option<(bool, String)>>>,
        diag: &Arc<Mutex<Vec<String>>>,
        gamepad_state: &Arc<Mutex<GamepadState>>,
        status: &Arc<Mutex<ConnectionStatus>>,
        pending_ping_start: &Arc<Mutex<Option<Instant>>>,
    ) {
        match serde_json::from_str::<ServerResponse>(raw.trim()) {
            Ok(resp) => match resp {
                ServerResponse::Pong => {
                    let latency = {
                        let mut p = pending_ping_start.lock().unwrap();
                        p.take().map(|t| t.elapsed().as_secs_f32() * 1000.0)
                    };
                    if let Some(latency) = latency {
                        let mut st = status.lock().unwrap();
                        if let ConnectionStatus::Connected {
                            ref mut latency_ms, ..
                        } = *st
                        {
                            *latency_ms = latency;
                        }
                    }
                }
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
        let mut raw_reference_buffer = Vec::new();
        let mut decomp_buf = vec![0u8; 640 * 480 * 4];

        loop {
            if !is_streaming.load(Ordering::Relaxed) {
                *current_frame.lock().unwrap() = None;
                *received.lock().unwrap() = None;
                video_connected.store(false, Ordering::Relaxed);
                raw_reference_buffer.clear();
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
                raw_reference_buffer.clear();
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
                                || header.flags > 4
                                || (matches!(header.flags, 1 | 2) && size != 0)
                                || size > max_allowed
                                || (size == 0 && (header.flags == 0 || header.flags == 4))
                            {
                                Self::add_diag(&diag, &format!("Aviso: Pacote de vídeo inválido (tam {size}, max {max_allowed}, flags {})", header.flags));
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

                            if decomp_buf.len() < expected_uncompressed {
                                decomp_buf.resize(expected_uncompressed, 0);
                            }

                            // Descompacta LZ4 diretamente no buffer pré-alocado
                            match lz4_flex::block::decompress_into(
                                &comp_buf,
                                &mut decomp_buf[..expected_uncompressed],
                            ) {
                                Ok(decomp_len) => {
                                    if decomp_len != expected_uncompressed {
                                        Self::add_diag(
                                            &diag,
                                            &format!(
                                                "Erro na descompressão LZ4: tamanho descompactado divergente (esperado {}, obtido {})",
                                                expected_uncompressed, decomp_len
                                            ),
                                        );
                                        continue;
                                    }

                                    // Processa keyframe vs delta XOR
                                    if header.flags == FRAME_FLAG_DELTA {
                                        if raw_reference_buffer.len() == expected_uncompressed {
                                            Self::apply_xor_delta(
                                                &mut raw_reference_buffer,
                                                &decomp_buf[..expected_uncompressed],
                                            );
                                        } else {
                                            // Dessincronia de referência: ignora até o próximo keyframe
                                            continue;
                                        }
                                    } else {
                                        // Keyframe (flags == 0 ou mock/outro): substitui buffer de referência
                                        if raw_reference_buffer.len() != expected_uncompressed {
                                            raw_reference_buffer.resize(expected_uncompressed, 0);
                                        }
                                        raw_reference_buffer
                                            .copy_from_slice(&decomp_buf[..expected_uncompressed]);
                                    }

                                    // Converte para RGBA8888 a partir do buffer de referência reconstruído
                                    let rgba = match header.format {
                                        FORMAT_RGB565_LZ4 => Self::rgb565_to_rgba(
                                            &raw_reference_buffer,
                                            header.width as usize,
                                            header.height as usize,
                                        ),
                                        FORMAT_BGRA8888_LZ4 => {
                                            let mut buf = raw_reference_buffer.clone();
                                            Self::bgra_to_rgba_inplace(&mut buf);
                                            buf
                                        }
                                        _ => {
                                            let mut buf = raw_reference_buffer.clone();
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
                    raw_reference_buffer.clear();
                    Self::add_diag(&diag, "Canal de vídeo desconectado.");
                }
                Err(_) => {
                    video_connected.store(false, Ordering::Relaxed);
                    raw_reference_buffer.clear();
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

    pub fn bgra_to_rgba_inplace(buffer: &mut [u8]) {
        for chunk in buffer.chunks_exact_mut(4) {
            chunk.swap(0, 2);
            chunk[3] = 255;
        }
    }

    #[allow(dead_code)]
    pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
        let mut rgba = bgra.to_vec();
        Self::bgra_to_rgba_inplace(&mut rgba);
        rgba
    }

    pub fn sanitize_rgba_alpha(rgba: &mut [u8]) {
        for chunk in rgba.chunks_exact_mut(4) {
            chunk[3] = 255;
        }
    }

    pub fn apply_xor_delta(reference: &mut [u8], delta: &[u8]) {
        let len = reference.len().min(delta.len());
        let chunks_delta = delta[..len].chunks_exact(16);
        let mut chunks_ref = reference[..len].chunks_exact_mut(16);

        for (cr, cd) in (&mut chunks_ref).zip(chunks_delta) {
            let ur = u128::from_ne_bytes(cr.try_into().unwrap());
            let ud = u128::from_ne_bytes(cd.try_into().unwrap());
            cr.copy_from_slice(&(ur ^ ud).to_ne_bytes());
        }

        let remainder_ref = chunks_ref.into_remainder();
        let rem_start = len - remainder_ref.len();
        for i in 0..remainder_ref.len() {
            remainder_ref[i] ^= delta[rem_start + i];
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
