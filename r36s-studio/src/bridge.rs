use crate::connection::DecodedFrame;
use crate::hub::{encode_frame_to_png, discover_client_socket_path, LockError, OwnerLock, OwnerRole, DEFAULT_LOCK_PATH};
use crate::ipc::*;
use crate::protocol::*;
use crate::sequence::SequenceStep;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const DEFAULT_CONSOLE_IP: &str = "192.168.7.2";
pub const DEFAULT_TCP_TIMEOUT_MS: u64 = 4000;

#[derive(Debug)]
pub enum BridgeError {
    StudioActiveIpcFailed(String),
    LockError(LockError),
    IpcError(IpcError),
    NetworkError(std::io::Error),
    ProtocolError(String),
    HeadlessError(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::StudioActiveIpcFailed(msg) => {
                write!(f, "O Studio está ativo, mas o IPC local falhou: {}. Fallback TCP proibido.", msg)
            }
            BridgeError::LockError(e) => write!(f, "Erro de lock de coordenação: {}", e),
            BridgeError::IpcError(e) => write!(f, "Erro IPC [{}]: {}", e.name, e.message),
            BridgeError::NetworkError(e) => write!(f, "Erro de rede no cliente Bridge: {}", e),
            BridgeError::ProtocolError(msg) => write!(f, "Erro de protocolo: {}", msg),
            BridgeError::HeadlessError(msg) => write!(f, "Erro no modo headless: {}", msg),
        }
    }
}

impl std::error::Error for BridgeError {}

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    pub lock_path: PathBuf,
    pub socket_path_override: Option<PathBuf>,
    pub console_ip: String,
    pub control_port: u16,
    pub video_port: u16,
    pub timeout_ms: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            lock_path: PathBuf::from(DEFAULT_LOCK_PATH),
            socket_path_override: None,
            console_ip: DEFAULT_CONSOLE_IP.to_string(),
            control_port: CONTROL_PORT,
            video_port: VIDEO_PORT,
            timeout_ms: DEFAULT_TCP_TIMEOUT_MS,
        }
    }
}

pub struct BridgeClient {
    config: BridgeConfig,
}

impl BridgeClient {
    pub fn new() -> Self {
        Self::with_config(BridgeConfig::default())
    }

    pub fn with_config(config: BridgeConfig) -> Self {
        Self { config }
    }

    pub fn ping(&self) -> Result<(), BridgeError> {
        let req = IpcRequest {
            version: IPC_VERSION.to_string(),
            id: format!("ping-{}", now_epoch_ms()),
            operation: "Ping".to_string(),
            payload: serde_json::json!({}),
        };
        self.execute_request(req)?;
        Ok(())
    }

    pub fn get_diagnostics(&self) -> Result<DiagnosticsResult, BridgeError> {
        let req = IpcRequest {
            version: IPC_VERSION.to_string(),
            id: format!("diag-{}", now_epoch_ms()),
            operation: "GetDiagnostics".to_string(),
            payload: serde_json::json!({}),
        };

        if self.is_studio_active() {
            let resp = self.call_local_ipc(&req)?;
            if resp.ok {
                let res = serde_json::from_value(resp.result.unwrap_or_default())
                    .map_err(|e| BridgeError::ProtocolError(format!("JSON inválido: {}", e)))?;
                Ok(res)
            } else {
                Err(BridgeError::IpcError(resp.error.unwrap_or_else(|| {
                    IpcError::new(IpcErrorCode::InternalError, "Erro desconhecido")
                })))
            }
        } else {
            self.execute_headless_diagnostics()
        }
    }

    pub fn get_screenshot_png(&self) -> Result<ScreenshotPngResult, BridgeError> {
        let req = IpcRequest {
            version: IPC_VERSION.to_string(),
            id: format!("shot-{}", now_epoch_ms()),
            operation: "GetScreenshotPng".to_string(),
            payload: serde_json::json!({}),
        };

        if self.is_studio_active() {
            let resp = self.call_local_ipc(&req)?;
            if resp.ok {
                let res = serde_json::from_value(resp.result.unwrap_or_default())
                    .map_err(|e| BridgeError::ProtocolError(format!("JSON inválido: {}", e)))?;
                Ok(res)
            } else {
                Err(BridgeError::IpcError(resp.error.unwrap_or_else(|| {
                    IpcError::new(IpcErrorCode::InternalError, "Erro desconhecido")
                })))
            }
        } else {
            self.execute_headless_screenshot()
        }
    }

    pub fn send_command(&self, cmd: ClientCommand) -> Result<SendCommandResult, BridgeError> {
        let req = IpcRequest {
            version: IPC_VERSION.to_string(),
            id: format!("cmd-{}", now_epoch_ms()),
            operation: "SendCommand".to_string(),
            payload: serde_json::to_value(&cmd)
                .map_err(|e| BridgeError::ProtocolError(format!("Erro ao serializar comando: {}", e)))?,
        };

        if self.is_studio_active() {
            let resp = self.call_local_ipc(&req)?;
            if resp.ok {
                let res = serde_json::from_value(resp.result.unwrap_or_default())
                    .map_err(|e| BridgeError::ProtocolError(format!("JSON inválido: {}", e)))?;
                Ok(res)
            } else {
                Err(BridgeError::IpcError(resp.error.unwrap_or_else(|| {
                    IpcError::new(IpcErrorCode::InternalError, "Erro desconhecido")
                })))
            }
        } else {
            self.execute_headless_command(cmd)
        }
    }

    pub fn run_macro(&self, steps: Vec<SequenceStep>) -> Result<RunMacroResult, BridgeError> {
        let req = IpcRequest {
            version: IPC_VERSION.to_string(),
            id: format!("macro-{}", now_epoch_ms()),
            operation: "RunMacro".to_string(),
            payload: serde_json::to_value(&steps)
                .map_err(|e| BridgeError::ProtocolError(format!("Erro ao serializar macro: {}", e)))?,
        };

        if self.is_studio_active() {
            let resp = self.call_local_ipc(&req)?;
            if resp.ok {
                let res = serde_json::from_value(resp.result.unwrap_or_default())
                    .map_err(|e| BridgeError::ProtocolError(format!("JSON inválido: {}", e)))?;
                Ok(res)
            } else {
                Err(BridgeError::IpcError(resp.error.unwrap_or_else(|| {
                    IpcError::new(IpcErrorCode::InternalError, "Erro desconhecido")
                })))
            }
        } else {
            self.execute_headless_macro(steps)
        }
    }

    pub fn deploy_file(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
        executable: bool,
    ) -> Result<(String, String), BridgeError> {
        let file_bytes = std::fs::read(local_path)
            .map_err(|e| BridgeError::ProtocolError(format!("Falha ao ler arquivo local {:?}: {}", local_path, e)))?;
        let total_size = file_bytes.len() as u64;

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&file_bytes);
        let sha256_hash = format!("{:x}", hasher.finalize());

        let clean_remote = remote_path.trim().to_string();
        let (relative_path, filename) = if let Some(idx) = clean_remote.rfind('/') {
            (clean_remote[..idx].to_string(), clean_remote[idx + 1..].to_string())
        } else {
            ("".to_string(), clean_remote.clone())
        };

        if filename.is_empty() {
            return Err(BridgeError::ProtocolError("Nome de arquivo remoto vazio".to_string()));
        }

        let start_cmd = ClientCommand::DeployStart {
            relative_path,
            filename,
            total_size,
            sha256: sha256_hash,
            executable: Some(executable),
        };

        if self.is_studio_active() {
            let start_res = self.send_command(start_cmd)?;
            match start_res.response {
                Some(ServerResponse::DeployProgress { .. }) => {}
                Some(ServerResponse::DeployError { error }) => {
                    return Err(BridgeError::ProtocolError(format!("Erro ao iniciar deploy: {}", error)));
                }
                other => {
                    return Err(BridgeError::ProtocolError(format!("Resposta inesperada no DeployStart: {:?}", other)));
                }
            }

            let chunk_size = 32 * 1024;
            let mut chunk_idx = 0;
            for chunk in file_bytes.chunks(chunk_size) {
                let data_base64 = base64_encode(chunk);
                let chunk_cmd = ClientCommand::DeployChunk {
                    chunk_index: chunk_idx,
                    data_base64,
                };
                let chunk_res = self.send_command(chunk_cmd)?;
                match chunk_res.response {
                    Some(ServerResponse::DeployProgress { .. }) => {}
                    Some(ServerResponse::DeployError { error }) => {
                        return Err(BridgeError::ProtocolError(format!("Erro no chunk {}: {}", chunk_idx, error)));
                    }
                    other => {
                        return Err(BridgeError::ProtocolError(format!("Resposta inesperada no chunk {}: {:?}", chunk_idx, other)));
                    }
                }
                chunk_idx += 1;
            }

            let finish_res = self.send_command(ClientCommand::DeployFinish)?;
            match finish_res.response {
                Some(ServerResponse::DeploySuccess { path, sha256 }) => Ok((path, sha256)),
                Some(ServerResponse::DeployError { error }) => {
                    Err(BridgeError::ProtocolError(format!("Erro ao finalizar deploy: {}", error)))
                }
                other => {
                    Err(BridgeError::ProtocolError(format!("Resposta inesperada no DeployFinish: {:?}", other)))
                }
            }
        } else {
            self.execute_headless_deploy(start_cmd, &file_bytes)
        }
    }

    fn is_studio_active(&self) -> bool {
        OwnerLock::is_studio_active(&self.config.lock_path)
    }

    fn execute_request(&self, req: IpcRequest) -> Result<IpcResponse, BridgeError> {
        if self.is_studio_active() {
            self.call_local_ipc(&req)
        } else {
            Err(BridgeError::HeadlessError("Operação não implementada em modo headless".into()))
        }
    }

    fn call_local_ipc(&self, req: &IpcRequest) -> Result<IpcResponse, BridgeError> {
        let sock_path = self
            .config
            .socket_path_override
            .clone()
            .or_else(discover_client_socket_path)
            .ok_or_else(|| {
                BridgeError::StudioActiveIpcFailed(
                    "Studio está registrado como ativo mas nenhum socket Unix foi encontrado".into(),
                )
            })?;

        let stream = UnixStream::connect(&sock_path).map_err(|e| {
            BridgeError::StudioActiveIpcFailed(format!(
                "Falha ao conectar no socket Unix {:?}: {}",
                sock_path, e
            ))
        })?;

        let _ = stream.set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(self.config.timeout_ms)));

        let mut writer = stream.try_clone().map_err(BridgeError::NetworkError)?;
        let line = req
            .to_json_line()
            .map_err(|e| BridgeError::ProtocolError(e.to_string()))?;

        writer
            .write_all(line.as_bytes())
            .map_err(|e| BridgeError::StudioActiveIpcFailed(format!("Erro ao enviar requisição: {}", e)))?;
        writer
            .flush()
            .map_err(|e| BridgeError::StudioActiveIpcFailed(format!("Erro ao fazer flush: {}", e)))?;

        let mut reader = BufReader::new(stream);
        let mut resp_line = String::new();
        reader
            .read_line(&mut resp_line)
            .map_err(|e| BridgeError::StudioActiveIpcFailed(format!("Erro ao ler resposta: {}", e)))?;

        let resp: IpcResponse = serde_json::from_str(&resp_line)
            .map_err(|e| BridgeError::StudioActiveIpcFailed(format!("JSON inválido retornado pelo Hub: {}", e)))?;

        Ok(resp)
    }

    fn execute_headless_diagnostics(&self) -> Result<DiagnosticsResult, BridgeError> {
        let _lock = OwnerLock::acquire(&self.config.lock_path, OwnerRole::Headless)
            .map_err(BridgeError::LockError)?;

        if self.is_studio_active() {
            return Err(BridgeError::StudioActiveIpcFailed(
                "Studio foi iniciado concorrentemente durante a aquisição do lock headless".into(),
            ));
        }

        let addr: SocketAddr = format!("{}:{}", self.config.console_ip, self.config.control_port)
            .parse()
            .map_err(|e| BridgeError::ProtocolError(format!("Endereço TCP inválido: {}", e)))?;

        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(self.config.timeout_ms))
            .map_err(BridgeError::NetworkError)?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(self.config.timeout_ms)));

        let hs = ClientCommand::Handshake {
            client_version: "0.2.9".into(),
            client_id: "headless-bridge".into(),
        };
        Self::send_tcp_command(&mut stream, &hs)?;

        let mut reader = BufReader::new(stream.try_clone().map_err(BridgeError::NetworkError)?);
        let ack = Self::read_tcp_response(&mut reader)?;
        let mut sandbox_supported = false;
        if let ServerResponse::HandshakeAck { capabilities, .. } = ack {
            sandbox_supported = capabilities.iter().any(|c| c == "sandbox-v1");
        }

        Self::send_tcp_command(&mut stream, &ClientCommand::GetMetrics)?;
        let metrics_resp = Self::read_tcp_response(&mut reader)?;
        let metrics = if let ServerResponse::Metrics(m) = metrics_resp {
            Some(m)
        } else {
            None
        };

        let mut sandbox_running = false;
        let mut sandbox_pid = None;
        let mut sandbox_exit_code = None;
        let mut sandbox_term_signal = None;
        let mut sandbox_uptime_ms = None;
        let mut sandbox_stdout_tail = None;
        let mut sandbox_stderr_tail = None;
        let mut sandbox_restore_error = None;
        let mut sandbox_recent_logs = None;
        let mut sandbox_phase = if sandbox_supported {
            Some("idle".to_string())
        } else {
            None
        };

        if sandbox_supported {
            let _ = Self::send_tcp_command(&mut stream, &ClientCommand::GetSandboxStatus);
            if let Ok(ServerResponse::SandboxStatus {
                running,
                pid,
                exit_code,
                term_signal,
                uptime_ms,
                stdout_tail,
                stderr_tail,
                restore_error,
            }) = Self::read_tcp_response(&mut reader)
            {
                sandbox_running = running;
                sandbox_pid = pid;
                sandbox_exit_code = exit_code;
                sandbox_term_signal = term_signal;
                sandbox_uptime_ms = Some(uptime_ms);
                sandbox_stdout_tail = Some(stdout_tail.clone());
                sandbox_stderr_tail = Some(stderr_tail.clone());
                sandbox_restore_error = restore_error;
                let mut logs = String::new();
                if !stdout_tail.is_empty() {
                    logs.push_str(&stdout_tail);
                }
                if !stderr_tail.is_empty() {
                    if !logs.is_empty() {
                        logs.push('\n');
                    }
                    logs.push_str(&stderr_tail);
                }
                sandbox_recent_logs = if logs.is_empty() { None } else { Some(logs) };
                sandbox_phase = Some(if running {
                    "running".to_string()
                } else if exit_code.is_some() || term_signal.is_some() {
                    "terminated".to_string()
                } else {
                    "idle".to_string()
                });
            }
        }

        let diag = DiagnosticsResult {
            timestamp_ms: now_epoch_ms(),
            is_connected: true,
            connection_state: format!("Headless TCP to {}", addr),
            fps: 0.0,
            session_generation: 1,
            metrics: metrics.clone(),
            telemetry: metrics.map(|m| TelemetrySnapshot::from_metrics(m, now_epoch_ms())),
            gamepad: None,
            sandbox_supported,
            sandbox_running,
            sandbox_pid,
            sandbox_exit_code,
            sandbox_uptime_ms,
            sandbox_recent_logs,
            sandbox_term_signal,
            sandbox_stdout_tail,
            sandbox_stderr_tail,
            sandbox_restore_error,
            sandbox_phase,
        };

        let _ = stream.shutdown(std::net::Shutdown::Both);
        Ok(diag)
    }

    fn execute_headless_screenshot(&self) -> Result<ScreenshotPngResult, BridgeError> {
        let _lock = OwnerLock::acquire(&self.config.lock_path, OwnerRole::Headless)
            .map_err(BridgeError::LockError)?;

        if self.is_studio_active() {
            return Err(BridgeError::StudioActiveIpcFailed(
                "Studio foi iniciado concorrentemente durante a aquisição do lock headless".into(),
            ));
        }

        let ctrl_addr: SocketAddr = format!("{}:{}", self.config.console_ip, self.config.control_port)
            .parse()
            .map_err(|e| BridgeError::ProtocolError(format!("Endereço TCP inválido: {}", e)))?;

        let vid_addr: SocketAddr = format!("{}:{}", self.config.console_ip, self.config.video_port)
            .parse()
            .map_err(|e| BridgeError::ProtocolError(format!("Endereço TCP inválido: {}", e)))?;

        let mut ctrl_stream = TcpStream::connect_timeout(&ctrl_addr, Duration::from_millis(self.config.timeout_ms))
            .map_err(BridgeError::NetworkError)?;
        let _ = ctrl_stream.set_nodelay(true);
        let _ = ctrl_stream.set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)));
        let _ = ctrl_stream.set_write_timeout(Some(Duration::from_millis(self.config.timeout_ms)));

        let hs = ClientCommand::Handshake {
            client_version: "0.2.9".into(),
            client_id: "headless-bridge-shot".into(),
        };
        Self::send_tcp_command(&mut ctrl_stream, &hs)?;

        let mut reader = BufReader::new(ctrl_stream.try_clone().map_err(BridgeError::NetworkError)?);
        let _ack = Self::read_tcp_response(&mut reader)?;

        // Abrir vídeo
        let mut vid_stream = TcpStream::connect_timeout(&vid_addr, Duration::from_millis(self.config.timeout_ms))
            .map_err(BridgeError::NetworkError)?;
        let _ = vid_stream.set_nodelay(true);
        let _ = vid_stream.set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)));

        Self::send_tcp_command(&mut ctrl_stream, &ClientCommand::StartStream { target_fps: 60 })?;

        let deadline = Instant::now() + Duration::from_millis(self.config.timeout_ms);
        let (header, rgba) = match Self::read_next_valid_frame(&mut vid_stream, deadline) {
            Ok(res) => res,
            Err(e) => {
                let _ = Self::send_tcp_command(&mut ctrl_stream, &ClientCommand::StopStream);
                let _ = vid_stream.shutdown(std::net::Shutdown::Both);
                let _ = ctrl_stream.shutdown(std::net::Shutdown::Both);
                return Err(e);
            }
        };

        let frame = DecodedFrame {
            frame_index: header.frame_index,
            width: header.width,
            height: header.height,
            rgba_data: rgba,
            timestamp: Instant::now(),
        };

        // Encerrar stream e conexões
        let _ = Self::send_tcp_command(&mut ctrl_stream, &ClientCommand::StopStream);
        let _ = vid_stream.shutdown(std::net::Shutdown::Both);
        let _ = ctrl_stream.shutdown(std::net::Shutdown::Both);

        // Codificar PNG no PC
        let png_bytes = encode_frame_to_png(&frame)
            .map_err(|e| BridgeError::HeadlessError(format!("Falha ao codificar PNG no host: {}", e)))?;
        let png_base64 = base64_encode(&png_bytes);

        Ok(ScreenshotPngResult {
            width: header.width as u32,
            height: header.height as u32,
            frame_index: header.frame_index as u64,
            captured_at_ms: now_epoch_ms(),
            png_base64,
        })
    }

    fn execute_headless_command(&self, cmd: ClientCommand) -> Result<SendCommandResult, BridgeError> {
        let _lock = OwnerLock::acquire(&self.config.lock_path, OwnerRole::Headless)
            .map_err(BridgeError::LockError)?;

        if self.is_studio_active() {
            return Err(BridgeError::StudioActiveIpcFailed(
                "Studio foi iniciado concorrentemente durante a aquisição do lock headless".into(),
            ));
        }

        let addr: SocketAddr = format!("{}:{}", self.config.console_ip, self.config.control_port)
            .parse()
            .map_err(|e| BridgeError::ProtocolError(format!("Endereço TCP inválido: {}", e)))?;

        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(self.config.timeout_ms))
            .map_err(BridgeError::NetworkError)?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(self.config.timeout_ms)));

        let hs = ClientCommand::Handshake {
            client_version: "0.2.9".into(),
            client_id: "headless-bridge-cmd".into(),
        };
        Self::send_tcp_command(&mut stream, &hs)?;

        let mut reader = BufReader::new(stream.try_clone().map_err(BridgeError::NetworkError)?);
        let _ack = Self::read_tcp_response(&mut reader)?;

        Self::send_tcp_command(&mut stream, &cmd)?;
        let resp = Self::read_tcp_response(&mut reader)?;

        let _ = stream.shutdown(std::net::Shutdown::Both);

        Ok(SendCommandResult {
            status: "Completed".into(),
            message: None,
            response: Some(resp),
        })
    }

    fn execute_headless_deploy(
        &self,
        start_cmd: ClientCommand,
        file_bytes: &[u8],
    ) -> Result<(String, String), BridgeError> {
        let _lock = OwnerLock::acquire(&self.config.lock_path, OwnerRole::Headless)
            .map_err(BridgeError::LockError)?;

        if self.is_studio_active() {
            return Err(BridgeError::StudioActiveIpcFailed(
                "Studio foi iniciado concorrentemente durante a aquisição do lock headless".into(),
            ));
        }

        let addr: SocketAddr = format!("{}:{}", self.config.console_ip, self.config.control_port)
            .parse()
            .map_err(|e| BridgeError::ProtocolError(format!("Endereço TCP inválido: {}", e)))?;

        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(self.config.timeout_ms))
            .map_err(BridgeError::NetworkError)?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(15000)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(15000)));

        let hs = ClientCommand::Handshake {
            client_version: "0.2.9".into(),
            client_id: "headless-bridge-deploy".into(),
        };
        Self::send_tcp_command(&mut stream, &hs)?;

        let mut reader = BufReader::new(stream.try_clone().map_err(BridgeError::NetworkError)?);
        let _ack = Self::read_tcp_response(&mut reader)?;

        Self::send_tcp_command(&mut stream, &start_cmd)?;
        let start_resp = Self::read_tcp_response(&mut reader)?;
        match start_resp {
            ServerResponse::DeployProgress { .. } => {}
            ServerResponse::DeployError { error } => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(BridgeError::ProtocolError(format!("Erro ao iniciar deploy: {}", error)));
            }
            other => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(BridgeError::ProtocolError(format!("Resposta inesperada no DeployStart: {:?}", other)));
            }
        }

        let chunk_size = 32 * 1024;
        let mut chunk_idx = 0;
        for chunk in file_bytes.chunks(chunk_size) {
            let data_base64 = base64_encode(chunk);
            let chunk_cmd = ClientCommand::DeployChunk {
                chunk_index: chunk_idx,
                data_base64,
            };
            Self::send_tcp_command(&mut stream, &chunk_cmd)?;
            let chunk_resp = Self::read_tcp_response(&mut reader)?;
            match chunk_resp {
                ServerResponse::DeployProgress { .. } => {}
                ServerResponse::DeployError { error } => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return Err(BridgeError::ProtocolError(format!("Erro no chunk {}: {}", chunk_idx, error)));
                }
                other => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return Err(BridgeError::ProtocolError(format!("Resposta inesperada no chunk {}: {:?}", chunk_idx, other)));
                }
            }
            chunk_idx += 1;
        }

        Self::send_tcp_command(&mut stream, &ClientCommand::DeployFinish)?;
        let finish_resp = Self::read_tcp_response(&mut reader)?;
        let _ = stream.shutdown(std::net::Shutdown::Both);

        match finish_resp {
            ServerResponse::DeploySuccess { path, sha256 } => Ok((path, sha256)),
            ServerResponse::DeployError { error } => {
                Err(BridgeError::ProtocolError(format!("Erro ao finalizar deploy: {}", error)))
            }
            other => {
                Err(BridgeError::ProtocolError(format!("Resposta inesperada no DeployFinish: {:?}", other)))
            }
        }
    }

    fn execute_headless_macro(&self, steps: Vec<SequenceStep>) -> Result<RunMacroResult, BridgeError> {
        let _lock = OwnerLock::acquire(&self.config.lock_path, OwnerRole::Headless)
            .map_err(BridgeError::LockError)?;

        if self.is_studio_active() {
            return Err(BridgeError::StudioActiveIpcFailed(
                "Studio foi iniciado concorrentemente durante a aquisição do lock headless".into(),
            ));
        }

        let addr: SocketAddr = format!("{}:{}", self.config.console_ip, self.config.control_port)
            .parse()
            .map_err(|e| BridgeError::ProtocolError(format!("Endereço TCP inválido: {}", e)))?;

        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(self.config.timeout_ms))
            .map_err(BridgeError::NetworkError)?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(self.config.timeout_ms)));

        let hs = ClientCommand::Handshake {
            client_version: "0.2.9".into(),
            client_id: "headless-bridge-macro".into(),
        };
        Self::send_tcp_command(&mut stream, &hs)?;

        let mut reader = BufReader::new(stream.try_clone().map_err(BridgeError::NetworkError)?);
        let _ack = Self::read_tcp_response(&mut reader)?;

        Self::send_tcp_command(&mut stream, &ClientCommand::SetGamepadInput { enabled: true })?;
        let resp = Self::read_tcp_response(&mut reader)?;
        if let ServerResponse::GamepadAck { success: false, ref message } = resp {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return Err(BridgeError::ProtocolError(format!(
                "Agente rejeitou habilitação de gamepad: {}",
                message
            )));
        }

        let mut steps_completed = 0;
        for step in steps {
            match step {
                SequenceStep::Pulse { mask, duration_ms } => {
                    let d = duration_ms.unwrap_or(80);
                    let cmd = ClientCommand::InjectGamepadInput {
                        action: GamepadInputAction::Pulse { mask, duration_ms: d },
                    };
                    Self::send_tcp_command(&mut stream, &cmd)?;
                    let resp = Self::read_tcp_response(&mut reader)?;
                    if let ServerResponse::GamepadAck { success: false, ref message } = resp {
                        let _ = Self::send_tcp_command(&mut stream, &ClientCommand::SetGamepadInput { enabled: false });
                        let _ = Self::read_tcp_response(&mut reader);
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return Err(BridgeError::ProtocolError(format!(
                            "Comando de gamepad rejeitado no passo {}: {}",
                            steps_completed, message
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(d as u64));
                }
                SequenceStep::Hold { mask } => {
                    let cmd = ClientCommand::InjectGamepadInput {
                        action: GamepadInputAction::Button { mask, pressed: true },
                    };
                    Self::send_tcp_command(&mut stream, &cmd)?;
                    let resp = Self::read_tcp_response(&mut reader)?;
                    if let ServerResponse::GamepadAck { success: false, ref message } = resp {
                        let _ = Self::send_tcp_command(&mut stream, &ClientCommand::SetGamepadInput { enabled: false });
                        let _ = Self::read_tcp_response(&mut reader);
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return Err(BridgeError::ProtocolError(format!(
                            "Comando de gamepad rejeitado no passo {}: {}",
                            steps_completed, message
                        )));
                    }
                }
                SequenceStep::Release { mask } => {
                    let cmd = ClientCommand::InjectGamepadInput {
                        action: GamepadInputAction::Button { mask, pressed: false },
                    };
                    Self::send_tcp_command(&mut stream, &cmd)?;
                    let resp = Self::read_tcp_response(&mut reader)?;
                    if let ServerResponse::GamepadAck { success: false, ref message } = resp {
                        let _ = Self::send_tcp_command(&mut stream, &ClientCommand::SetGamepadInput { enabled: false });
                        let _ = Self::read_tcp_response(&mut reader);
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return Err(BridgeError::ProtocolError(format!(
                            "Comando de gamepad rejeitado no passo {}: {}",
                            steps_completed, message
                        )));
                    }
                }
                SequenceStep::WaitMs(mut ms) => {
                    while ms > 0 {
                        let chunk = ms.min(400);
                        std::thread::sleep(Duration::from_millis(chunk));
                        ms -= chunk;
                        if ms > 0 {
                            Self::send_tcp_command(
                                &mut stream,
                                &ClientCommand::SetGamepadInput { enabled: true },
                            )?;
                            let resp = Self::read_tcp_response(&mut reader)?;
                            if let ServerResponse::GamepadAck { success: false, ref message } = resp {
                                let _ = Self::send_tcp_command(&mut stream, &ClientCommand::SetGamepadInput { enabled: false });
                                let _ = Self::read_tcp_response(&mut reader);
                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                return Err(BridgeError::ProtocolError(format!(
                                    "Falha ao renovar lease no passo {}: {}",
                                    steps_completed, message
                                )));
                            }
                        }
                    }
                }
                SequenceStep::Axis { left_x, left_y, right_x, right_y } => {
                    let cmd = ClientCommand::InjectGamepadInput {
                        action: GamepadInputAction::Axis { left_x, left_y, right_x, right_y },
                    };
                    Self::send_tcp_command(&mut stream, &cmd)?;
                    let resp = Self::read_tcp_response(&mut reader)?;
                    if let ServerResponse::GamepadAck { success: false, ref message } = resp {
                        let _ = Self::send_tcp_command(&mut stream, &ClientCommand::SetGamepadInput { enabled: false });
                        let _ = Self::read_tcp_response(&mut reader);
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return Err(BridgeError::ProtocolError(format!(
                            "Comando de gamepad rejeitado no passo {}: {}",
                            steps_completed, message
                        )));
                    }
                }
            }
            steps_completed += 1;
        }

        let _ = Self::send_tcp_command(&mut stream, &ClientCommand::SetGamepadInput { enabled: false });
        let _ = Self::read_tcp_response(&mut reader);
        let _ = stream.shutdown(std::net::Shutdown::Both);

        Ok(RunMacroResult {
            success: true,
            steps_completed,
            error: None,
        })
    }

    pub fn decode_video_payload(
        format: u8,
        width: u16,
        height: u16,
        comp_buf: &[u8],
    ) -> Result<Vec<u8>, BridgeError> {
        let expected_uncompressed = match format {
            FORMAT_RGB565_LZ4 => (width as usize) * (height as usize) * 2,
            FORMAT_BGRA8888_LZ4 | FORMAT_RGBA8888_LZ4 => (width as usize) * (height as usize) * 4,
            other => {
                return Err(BridgeError::ProtocolError(format!(
                    "Formato de vídeo desconhecido: {}",
                    other
                )));
            }
        };

        let decompressed = lz4_flex::decompress(comp_buf, expected_uncompressed)
            .map_err(|e| BridgeError::ProtocolError(format!("Falha na descompressão LZ4: {}", e)))?;

        let rgba = crate::connection::ConnectionManager::decode_raw_to_rgba(
            format,
            width as usize,
            height as usize,
            &decompressed,
        );

        Ok(rgba)
    }

    pub fn read_next_valid_frame<R: Read>(
        reader: &mut R,
        deadline: Instant,
    ) -> Result<(VideoFrameHeader, Vec<u8>), BridgeError> {
        let mut header_buf = [0u8; 20];
        loop {
            if Instant::now() >= deadline {
                return Err(BridgeError::ProtocolError(
                    "Timeout ao aguardar quadro de vídeo válido".into(),
                ));
            }

            match reader.read_exact(&mut header_buf) {
                Ok(()) => {}
                Err(_e) if Instant::now() >= deadline => {
                    return Err(BridgeError::ProtocolError(
                        "Timeout ao aguardar quadro de vídeo válido".into(),
                    ));
                }
                Err(e) => return Err(BridgeError::NetworkError(e)),
            }

            let header = VideoFrameHeader::from_bytes(&header_buf)
                .ok_or_else(|| BridgeError::ProtocolError("Cabeçalho de vídeo inválido".into()))?;

            if header.compressed_size > 4 * 1024 * 1024 {
                return Err(BridgeError::ProtocolError("Quadro comprimido excede 4 MiB".into()));
            }

            let mut comp_buf = vec![0u8; header.compressed_size as usize];
            if header.compressed_size > 0 {
                match reader.read_exact(&mut comp_buf) {
                    Ok(()) => {}
                    Err(_e) if Instant::now() >= deadline => {
                        return Err(BridgeError::ProtocolError(
                            "Timeout ao aguardar quadro de vídeo válido".into(),
                        ));
                    }
                    Err(e) => return Err(BridgeError::NetworkError(e)),
                }
            }

            // Ignorar quadros em transição ou vazios
            if (header.flags & FRAME_FLAG_FB_UNAVAILABLE) != 0 || header.compressed_size == 0 {
                continue;
            }

            // Ignorar deltas sem quadro base
            if (header.flags & FRAME_FLAG_DELTA) != 0 {
                continue;
            }

            let rgba = Self::decode_video_payload(header.format, header.width, header.height, &comp_buf)?;
            return Ok((header, rgba));
        }
    }

    fn send_tcp_command(stream: &mut TcpStream, cmd: &ClientCommand) -> Result<(), BridgeError> {
        let mut json = serde_json::to_string(cmd)
            .map_err(|e| BridgeError::ProtocolError(format!("Erro ao serializar comando: {}", e)))?;
        json.push('\n');
        stream.write_all(json.as_bytes()).map_err(BridgeError::NetworkError)?;
        stream.flush().map_err(BridgeError::NetworkError)
    }

    fn read_tcp_response(reader: &mut BufReader<TcpStream>) -> Result<ServerResponse, BridgeError> {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(BridgeError::NetworkError)?;
        serde_json::from_str(&line)
            .map_err(|e| BridgeError::ProtocolError(format!("JSON de resposta inválido: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionManager;
    use crate::hub::HubServer;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_bridge_routes_to_local_hub_when_studio_active() {
        let test_id = std::process::id();
        let lock_path = PathBuf::from(format!("/tmp/r36s_test_bridge_lock_{}.lock", test_id));
        let sock_path = PathBuf::from(format!("/tmp/r36s_test_bridge_sock_{}.sock", test_id));
        let _ = std::fs::remove_file(&lock_path);
        let _ = std::fs::remove_file(&sock_path);

        let studio_lock = OwnerLock::acquire(&lock_path, OwnerRole::Studio)
            .expect("Aquisição de lock do Studio");

        let (mgr, _) = ConnectionManager::new_mock();
        let mut server = HubServer::start_at_path(
            Arc::new(mgr),
            sock_path.clone(),
            Some(studio_lock),
        )
        .expect("Inicialização de servidor Hub");

        let config = BridgeConfig {
            lock_path: lock_path.clone(),
            socket_path_override: Some(sock_path.clone()),
            console_ip: "127.0.0.1".into(),
            control_port: 0,
            video_port: 0,
            timeout_ms: 2000,
        };
        let client = BridgeClient::with_config(config);

        // Deve rotear via IPC local com sucesso
        client.ping().expect("Ping via IPC local");

        let diag = client.get_diagnostics().expect("Diagnostics via IPC local");
        assert!(diag.is_connected);

        server.shutdown();
        let _ = std::fs::remove_file(&lock_path);
        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn test_bridge_fails_without_tcp_fallback_when_studio_active_hub_unresponsive() {
        let test_id = std::process::id();
        let lock_path = PathBuf::from(format!("/tmp/r36s_test_unresponsive_{}.lock", test_id));
        let sock_path = PathBuf::from(format!("/tmp/r36s_test_unresponsive_{}.sock", test_id));
        let _ = std::fs::remove_file(&lock_path);
        let _ = std::fs::remove_file(&sock_path);

        // Simular Studio ativo segurando lock
        let studio_lock = OwnerLock::acquire(&lock_path, OwnerRole::Studio)
            .expect("Aquisição de lock do Studio");

        // Mas o socket não existe ou não responde
        let config = BridgeConfig {
            lock_path: lock_path.clone(),
            socket_path_override: Some(sock_path.clone()),
            console_ip: "127.0.0.1".into(),
            control_port: 7365,
            video_port: 7366,
            timeout_ms: 500,
        };
        let client = BridgeClient::with_config(config);

        // Deve falhar com StudioActiveIpcFailed e JAMAIS tentar fallback TCP
        let res = client.get_diagnostics();
        match res {
            Err(BridgeError::StudioActiveIpcFailed(msg)) => {
                assert!(msg.contains("Falha ao conectar no socket Unix") || msg.contains("não foi encontrado"));
            }
            other => panic!("Esperado StudioActiveIpcFailed, obteve: {:?}", other),
        }

        drop(studio_lock);
        let _ = std::fs::remove_file(&lock_path);
    }

    #[test]
    fn test_headless_screenshot_and_command_with_mock_daemon() {
        use std::net::TcpListener;
        let test_id = std::process::id();
        let lock_path = PathBuf::from(format!("/tmp/r36s_test_headless_{}.lock", test_id));
        let _ = std::fs::remove_file(&lock_path);

        let ctrl_listener = TcpListener::bind("127.0.0.1:0").expect("Bind mock control");
        let vid_listener = TcpListener::bind("127.0.0.1:0").expect("Bind mock video");
        let ctrl_port = ctrl_listener.local_addr().unwrap().port();
        let vid_port = vid_listener.local_addr().unwrap().port();

        let server_thread = thread::spawn(move || {
            // 1. Conexão de controle
            let (mut ctrl_stream, _) = ctrl_listener.accept().expect("Accept control");
            let mut ctrl_reader = BufReader::new(ctrl_stream.try_clone().unwrap());

            // Handshake
            let mut line = String::new();
            ctrl_reader.read_line(&mut line).unwrap();
            let hs_ack = ServerResponse::HandshakeAck {
                agent_version: "0.2.9".into(),
                protocol_version: "0.2.0".into(),
                compatible: true,
                capabilities: vec!["sandbox-v1".into()],
                fb_width: 640,
                fb_height: 480,
                fb_bpp: 16,
                fb_format: FORMAT_RGB565_LZ4,
            };
            let mut ack_line = serde_json::to_string(&hs_ack).unwrap();
            ack_line.push('\n');
            ctrl_stream.write_all(ack_line.as_bytes()).unwrap();
            ctrl_stream.flush().unwrap();

            // StartStream
            line.clear();
            ctrl_reader.read_line(&mut line).unwrap();
            assert!(line.contains("StartStream"));

            // Conexão de vídeo
            let (mut vid_stream, _) = vid_listener.accept().expect("Accept video");

            // Enviar 1 quadro em RGB565 (formato padrão do DrmReader RK3326)
            // Vermelho puro RGB565 (0xF800): low byte = 0x00, high byte = 0xF8
            let mut rgb565 = vec![0u8; 640 * 480 * 2];
            for chunk in rgb565.chunks_exact_mut(2) {
                chunk[0] = 0x00;
                chunk[1] = 0xF8;
            }
            let comp = lz4_flex::compress(&rgb565);
            let header = VideoFrameHeader::new(1, 640, 480, FORMAT_RGB565_LZ4, 0, comp.len() as u32);
            vid_stream.write_all(&header.to_bytes()).unwrap();
            vid_stream.write_all(&comp).unwrap();
            vid_stream.flush().unwrap();

            // StopStream
            line.clear();
            ctrl_reader.read_line(&mut line).unwrap();
            assert!(line.contains("StopStream"));
        });

        let config = BridgeConfig {
            lock_path: lock_path.clone(),
            socket_path_override: None,
            console_ip: "127.0.0.1".into(),
            control_port: ctrl_port,
            video_port: vid_port,
            timeout_ms: 3000,
        };
        let client = BridgeClient::with_config(config);

        let shot = client.get_screenshot_png().expect("Headless screenshot");
        assert_eq!(shot.width, 640);
        assert_eq!(shot.height, 480);
        assert_eq!(shot.frame_index, 1);

        let png = base64_decode(&shot.png_base64).expect("PNG base64 decode");
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);

        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().expect("PNG read info");
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).expect("PNG decode next frame");
        assert_eq!(info.width, 640);
        assert_eq!(info.height, 480);
        // Vermelho puro convertido de RGB565 para RGBA8888 com Alpha = 255
        assert_eq!(&buf[0..4], &[255, 0, 0, 255]);

        server_thread.join().unwrap();
        let _ = std::fs::remove_file(&lock_path);
    }

    #[test]
    fn test_headless_screenshot_with_mock_daemon_bgra8888_mali_alpha_zero() {
        use std::net::TcpListener;
        let test_id = std::process::id();
        let lock_path = PathBuf::from(format!("/tmp/r36s_test_headless_bgra_{}.lock", test_id));
        let _ = std::fs::remove_file(&lock_path);

        let ctrl_listener = TcpListener::bind("127.0.0.1:0").expect("Bind mock control");
        let vid_listener = TcpListener::bind("127.0.0.1:0").expect("Bind mock video");
        let ctrl_port = ctrl_listener.local_addr().unwrap().port();
        let vid_port = vid_listener.local_addr().unwrap().port();

        let server_thread = thread::spawn(move || {
            let (mut ctrl_stream, _) = ctrl_listener.accept().expect("Accept control");
            let mut ctrl_reader = BufReader::new(ctrl_stream.try_clone().unwrap());

            let mut line = String::new();
            ctrl_reader.read_line(&mut line).unwrap();
            let hs_ack = ServerResponse::HandshakeAck {
                agent_version: "0.2.9".into(),
                protocol_version: "0.2.0".into(),
                compatible: true,
                capabilities: vec!["sandbox-v1".into()],
                fb_width: 640,
                fb_height: 480,
                fb_bpp: 32,
                fb_format: FORMAT_BGRA8888_LZ4,
            };
            let mut ack_line = serde_json::to_string(&hs_ack).unwrap();
            ack_line.push('\n');
            ctrl_stream.write_all(ack_line.as_bytes()).unwrap();
            ctrl_stream.flush().unwrap();

            line.clear();
            ctrl_reader.read_line(&mut line).unwrap();
            assert!(line.contains("StartStream"));

            let (mut vid_stream, _) = vid_listener.accept().expect("Accept video");

            // Mali GPU RK3326 grava Alpha=0x00 em buffers 32-bpp.
            // B=200, G=100, R=50, A=0
            let mut bgra = vec![0u8; 640 * 480 * 4];
            for chunk in bgra.chunks_exact_mut(4) {
                chunk[0] = 200; // Blue
                chunk[1] = 100; // Green
                chunk[2] = 50;  // Red
                chunk[3] = 0;   // Alpha 0x00 da Mali RK3326
            }
            let comp = lz4_flex::compress(&bgra);
            let header = VideoFrameHeader::new(1, 640, 480, FORMAT_BGRA8888_LZ4, 0, comp.len() as u32);
            vid_stream.write_all(&header.to_bytes()).unwrap();
            vid_stream.write_all(&comp).unwrap();
            vid_stream.flush().unwrap();

            line.clear();
            ctrl_reader.read_line(&mut line).unwrap();
            assert!(line.contains("StopStream"));
        });

        let config = BridgeConfig {
            lock_path: lock_path.clone(),
            socket_path_override: None,
            console_ip: "127.0.0.1".into(),
            control_port: ctrl_port,
            video_port: vid_port,
            timeout_ms: 3000,
        };
        let client = BridgeClient::with_config(config);

        let shot = client.get_screenshot_png().expect("Headless screenshot BGRA");
        assert_eq!(shot.width, 640);
        assert_eq!(shot.height, 480);
        assert_eq!(shot.frame_index, 1);

        let png = base64_decode(&shot.png_base64).expect("PNG base64 decode");
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().expect("PNG read info");
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).expect("PNG decode frame");
        assert_eq!(info.width, 640);
        assert_eq!(info.height, 480);
        // RGBA: Red deve ser 50, Green 100, Blue 200, e Alpha DEVE ser 255 (não 0)
        assert_eq!(&buf[0..4], &[50, 100, 200, 255]);

        server_thread.join().unwrap();
        let _ = std::fs::remove_file(&lock_path);
    }

    #[test]
    fn test_race_between_studio_and_headless_client() {
        let test_id = std::process::id();
        let lock_path = PathBuf::from(format!("/tmp/r36s_test_race_{}.lock", test_id));
        let _ = std::fs::remove_file(&lock_path);

        // 1. Headless client obtém lock
        let headless_lock = OwnerLock::acquire(&lock_path, OwnerRole::Headless)
            .expect("Aquisição de lock headless");

        // 2. Studio tenta iniciar simultaneamente -> deve ser rejeitado pelo lock
        let studio_attempt = OwnerLock::acquire(&lock_path, OwnerRole::Studio);
        assert!(matches!(studio_attempt, Err(LockError::AlreadyLocked { .. })));

        drop(headless_lock);

        // 3. Após liberação do headless, Studio adquire com sucesso
        let studio_lock = OwnerLock::acquire(&lock_path, OwnerRole::Studio)
            .expect("Studio adquire após headless liberar");
        assert_eq!(studio_lock.role(), OwnerRole::Studio);

        let _ = std::fs::remove_file(&lock_path);
    }

    #[test]
    fn test_decode_video_payload_multiformat() {
        // 1. RGB565 LZ4 (2x2 pixels)
        // Pixel 0: pure red (0xF800 -> low: 0x00, high: 0xF8)
        // Pixel 1: pure green (0x07E0 -> low: 0xE0, high: 0x07)
        // Pixel 2: pure blue (0x001F -> low: 0x1F, high: 0x00)
        // Pixel 3: white (0xFFFF -> low: 0xFF, high: 0xFF)
        let rgb565_raw = vec![
            0x00, 0xF8,
            0xE0, 0x07,
            0x1F, 0x00,
            0xFF, 0xFF,
        ];
        let comp_rgb565 = lz4_flex::compress(&rgb565_raw);
        let rgba_out = BridgeClient::decode_video_payload(FORMAT_RGB565_LZ4, 2, 2, &comp_rgb565)
            .expect("Decodificação RGB565");
        assert_eq!(rgba_out.len(), 16);
        // Red
        assert_eq!(rgba_out[0], 255);
        assert_eq!(rgba_out[1], 0);
        assert_eq!(rgba_out[2], 0);
        assert_eq!(rgba_out[3], 255);
        // Green
        assert_eq!(rgba_out[4], 0);
        assert_eq!(rgba_out[5], 255);
        assert_eq!(rgba_out[6], 0);
        assert_eq!(rgba_out[7], 255);
        // Blue
        assert_eq!(rgba_out[8], 0);
        assert_eq!(rgba_out[9], 0);
        assert_eq!(rgba_out[10], 255);
        assert_eq!(rgba_out[11], 255);

        // 2. BGRA8888 LZ4 (B, G, R, A=0x00) -> RGBA (R, G, B, A=255)
        let bgra_raw = vec![
            10, 20, 30, 0, // B=10, G=20, R=30, A=0
        ];
        let comp_bgra = lz4_flex::compress(&bgra_raw);
        let rgba_out_bgra = BridgeClient::decode_video_payload(FORMAT_BGRA8888_LZ4, 1, 1, &comp_bgra)
            .expect("Decodificação BGRA8888");
        assert_eq!(rgba_out_bgra[0], 30); // R
        assert_eq!(rgba_out_bgra[1], 20); // G
        assert_eq!(rgba_out_bgra[2], 10); // B
        assert_eq!(rgba_out_bgra[3], 255); // A corrigido de 0 para 255

        // 3. RGBA8888 LZ4 (R, G, B, A=0) -> RGBA (A=255)
        let rgba_raw = vec![
            50, 60, 70, 0,
        ];
        let comp_rgba = lz4_flex::compress(&rgba_raw);
        let rgba_out_rgba = BridgeClient::decode_video_payload(FORMAT_RGBA8888_LZ4, 1, 1, &comp_rgba)
            .expect("Decodificação RGBA8888");
        assert_eq!(rgba_out_rgba[0], 50);
        assert_eq!(rgba_out_rgba[1], 60);
        assert_eq!(rgba_out_rgba[2], 70);
        assert_eq!(rgba_out_rgba[3], 255);
    }

    #[test]
    fn test_read_next_valid_frame_discards_transitional_and_delta() {
        use std::io::Cursor;
        let mut stream_bytes = Vec::new();

        // 1. Frame FB_UNAVAILABLE com payload
        let dummy_payload = vec![1, 2, 3, 4];
        let h1 = VideoFrameHeader::new(1, 2, 2, FORMAT_RGB565_LZ4, FRAME_FLAG_FB_UNAVAILABLE, dummy_payload.len() as u32);
        stream_bytes.extend_from_slice(&h1.to_bytes());
        stream_bytes.extend_from_slice(&dummy_payload);

        // 2. Frame com compressed_size = 0
        let h2 = VideoFrameHeader::new(2, 2, 2, FORMAT_RGB565_LZ4, 0, 0);
        stream_bytes.extend_from_slice(&h2.to_bytes());

        // 3. Frame DELTA sem base
        let h3 = VideoFrameHeader::new(3, 2, 2, FORMAT_RGB565_LZ4, FRAME_FLAG_DELTA, dummy_payload.len() as u32);
        stream_bytes.extend_from_slice(&h3.to_bytes());
        stream_bytes.extend_from_slice(&dummy_payload);

        // 4. Frame válido (Keyframe) em RGB565
        let rgb565_raw = vec![0x00, 0xF8, 0xE0, 0x07, 0x1F, 0x00, 0xFF, 0xFF];
        let comp_valid = lz4_flex::compress(&rgb565_raw);
        let h4 = VideoFrameHeader::new(4, 2, 2, FORMAT_RGB565_LZ4, 0, comp_valid.len() as u32);
        stream_bytes.extend_from_slice(&h4.to_bytes());
        stream_bytes.extend_from_slice(&comp_valid);

        let mut cursor = Cursor::new(stream_bytes);
        let deadline = Instant::now() + Duration::from_secs(2);
        let (header, rgba) = BridgeClient::read_next_valid_frame(&mut cursor, deadline)
            .expect("Leitura de quadro válido após descarte");

        let f_index = header.frame_index;
        let flags = header.flags;
        assert_eq!(f_index, 4);
        assert_eq!(flags, 0);
        assert_eq!(rgba.len(), 16);
        assert_eq!(rgba[0], 255); // Red R
        assert_eq!(rgba[3], 255); // Red Alpha
    }

    #[test]
    fn test_headless_macro_lease_renewal_and_error_propagation() {
        use std::net::TcpListener;
        let test_id = std::process::id();
        let lock_path = PathBuf::from(format!("/tmp/r36s_test_macro_{}.lock", test_id));
        let _ = std::fs::remove_file(&lock_path);

        let listener = TcpListener::bind("127.0.0.1:0").expect("Bind mock macro listener");
        let port = listener.local_addr().unwrap().port();

        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("Accept macro conn");
            let mut reader = BufReader::new(stream.try_clone().unwrap());

            // 1. Handshake
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let ack = ServerResponse::HandshakeAck {
                agent_version: "0.2.9".into(),
                protocol_version: "0.2.0".into(),
                compatible: true,
                capabilities: vec!["gamepad-v2".into()],
                fb_width: 640,
                fb_height: 480,
                fb_bpp: 32,
                fb_format: FORMAT_BGRA8888_LZ4,
            };
            let mut s = serde_json::to_string(&ack).unwrap();
            s.push('\n');
            stream.write_all(s.as_bytes()).unwrap();
            stream.flush().unwrap();

            // 2. SetGamepadInput { enabled: true }
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("SetGamepadInput"));
            let mut g_ack = serde_json::to_string(&ServerResponse::GamepadAck {
                success: true,
                message: "Lease acquired".into(),
            }).unwrap();
            g_ack.push('\n');
            stream.write_all(g_ack.as_bytes()).unwrap();
            stream.flush().unwrap();

            // 3. Step 0: Pulse -> Sucesso
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("InjectGamepadInput"));
            stream.write_all(g_ack.as_bytes()).unwrap();
            stream.flush().unwrap();

            // 4. Step 1: WaitMs(500) -> Deve enviar SetGamepadInput { enabled: true }, NÃO Ping!
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(
                line.contains("SetGamepadInput") && !line.contains("Ping"),
                "Esperado SetGamepadInput para renovação de lease, mas recebeu: {}",
                line
            );
            stream.write_all(g_ack.as_bytes()).unwrap();
            stream.flush().unwrap();

            // 5. Step 2: Pulse -> Rejeitado pelo agente
            line.clear();
            reader.read_line(&mut line).unwrap();
            let mut g_reject = serde_json::to_string(&ServerResponse::GamepadAck {
                success: false,
                message: "Lease expired or input blocked".into(),
            }).unwrap();
            g_reject.push('\n');
            stream.write_all(g_reject.as_bytes()).unwrap();
            stream.flush().unwrap();

            // Cleanup: SetGamepadInput { enabled: false }
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("SetGamepadInput"));
            stream.write_all(g_ack.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let config = BridgeConfig {
            lock_path: lock_path.clone(),
            socket_path_override: None,
            console_ip: "127.0.0.1".into(),
            control_port: port,
            video_port: 0,
            timeout_ms: 2000,
        };
        let client = BridgeClient::with_config(config);

        let steps = vec![
            SequenceStep::Pulse { mask: 1, duration_ms: Some(10) },
            SequenceStep::WaitMs(500),
            SequenceStep::Pulse { mask: 2, duration_ms: Some(10) },
        ];

        let result = client.run_macro(steps);
        match result {
            Err(BridgeError::ProtocolError(msg)) => {
                assert!(msg.contains("Comando de gamepad rejeitado no passo 2"));
                assert!(msg.contains("Lease expired or input blocked"));
            }
            other => panic!("Esperado erro de rejeição no passo 2, obteve: {:?}", other),
        }

        server_thread.join().unwrap();
        let _ = std::fs::remove_file(&lock_path);
    }
}
