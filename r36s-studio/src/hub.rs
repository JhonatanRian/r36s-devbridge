use crate::connection::{ConnectionManager, ConnectionStatus, DecodedFrame};
use crate::ipc::*;
use crate::protocol::*;
use crate::sequence::SequenceStep;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_LOCK_PATH: &str = "/tmp/r36s-devbridge.owner.lock";
pub const DEFAULT_FALLBACK_SOCKET: &str = "/tmp/r36s-devbridge.sock";
pub const SOCKET_FILENAME: &str = "r36s-devbridge.sock";
pub const MAX_CONCURRENT_WORKERS: usize = 8;
pub const IO_DEADLINE_MS: u64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OwnerRole {
    Studio,
    Headless,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LockInfo {
    pub pid: u32,
    pub role: OwnerRole,
    pub timestamp_ms: u64,
}

#[derive(Debug)]
pub enum LockError {
    AlreadyLocked { pid: Option<u32>, role: Option<OwnerRole> },
    SymlinkNotAllowed,
    UserMismatch { expected: u32, actual: u32 },
    Io(std::io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyLocked { pid, role } => {
                write!(
                    f,
                    "Lock já ocupado por outro processo (PID: {:?}, Papel: {:?})",
                    pid, role
                )
            }
            LockError::SymlinkNotAllowed => write!(f, "O arquivo de lock não pode ser um link simbólico"),
            LockError::UserMismatch { expected, actual } => {
                write!(
                    f,
                    "Dono do arquivo de lock incompatível (esperado UID {}, encontrado {})",
                    expected, actual
                )
            }
            LockError::Io(e) => write!(f, "Erro de I/O no arquivo de lock: {}", e),
        }
    }
}

impl std::error::Error for LockError {}

#[derive(Debug)]
pub struct OwnerLock {
    file: File,
    path: PathBuf,
    role: OwnerRole,
}

impl OwnerLock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn acquire(path: impl AsRef<Path>, role: OwnerRole) -> Result<Self, LockError> {
        let path = path.as_ref().to_path_buf();
        let uid = unsafe { libc::getuid() };

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        options.mode(0o600);

        let file = options.open(&path).map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                LockError::SymlinkNotAllowed
            } else {
                LockError::Io(e)
            }
        })?;

        let fd = file.as_raw_fd();
        let meta = file.metadata().map_err(LockError::Io)?;
        if meta.uid() != uid {
            return Err(LockError::UserMismatch {
                expected: uid,
                actual: meta.uid(),
            });
        }

        let lock_res = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if lock_res != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.raw_os_error() == Some(libc::EAGAIN) {
                let info = Self::read_lock_info(&file);
                return Err(LockError::AlreadyLocked {
                    pid: info.as_ref().map(|i| i.pid),
                    role: info.as_ref().map(|i| i.role),
                });
            }
            return Err(LockError::Io(err));
        }

        let _ = unsafe { libc::ftruncate(fd, 0) };
        let info = LockInfo {
            pid: std::process::id(),
            role,
            timestamp_ms: now_epoch_ms(),
        };
        if let Ok(json) = serde_json::to_string(&info) {
            let _ = (&file).write_all(json.as_bytes());
            let _ = (&file).flush();
        }

        Ok(Self { file, path, role })
    }

    pub fn acquire_studio() -> Result<Self, LockError> {
        Self::acquire(DEFAULT_LOCK_PATH, OwnerRole::Studio)
    }

    pub fn acquire_headless() -> Result<Self, LockError> {
        Self::acquire(DEFAULT_LOCK_PATH, OwnerRole::Headless)
    }

    fn read_lock_info(mut file: &File) -> Option<LockInfo> {
        let mut s = String::new();
        if file.read_to_string(&mut s).is_ok() {
            serde_json::from_str(&s).ok()
        } else {
            None
        }
    }

    pub fn is_studio_active(path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        if !path.exists() {
            return false;
        }

        let mut options = OpenOptions::new();
        options.read(true).write(true);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);

        match options.open(path) {
            Ok(file) => {
                let fd = file.as_raw_fd();
                let lock_res = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
                if lock_res == 0 {
                    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
                    false
                } else {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.raw_os_error() == Some(libc::EAGAIN) {
                        if let Some(info) = Self::read_lock_info(&file) {
                            info.role == OwnerRole::Studio
                        } else {
                            true
                        }
                    } else {
                        false
                    }
                }
            }
            Err(_) => false,
        }
    }

    pub fn role(&self) -> OwnerRole {
        self.role
    }
}

impl Drop for OwnerLock {
    fn drop(&mut self) {
        let fd = self.file.as_raw_fd();
        let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
}

impl FileIdentity {
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
}

pub fn resolve_server_socket_path() -> Result<PathBuf, String> {
    let uid = unsafe { libc::getuid() };

    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(&runtime_dir);
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_dir() && meta.uid() == uid {
                let candidate = p.join(SOCKET_FILENAME);
                return Ok(candidate);
            }
        }
    }

    Ok(PathBuf::from(DEFAULT_FALLBACK_SOCKET))
}

pub fn discover_client_socket_path() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() };

    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let candidate = PathBuf::from(runtime_dir).join(SOCKET_FILENAME);
        if candidate.exists() {
            if let Ok(meta) = std::fs::symlink_metadata(&candidate) {
                if meta.file_type().is_socket() && meta.uid() == uid {
                    return Some(candidate);
                }
            }
        }
    }

    let fallback = PathBuf::from(DEFAULT_FALLBACK_SOCKET);
    if fallback.exists() {
        if let Ok(meta) = std::fs::symlink_metadata(&fallback) {
            if meta.file_type().is_socket() && meta.uid() == uid {
                return Some(fallback);
            }
        }
    }

    None
}

pub fn prepare_server_socket_path(path: &Path) -> Result<(), String> {
    let uid = unsafe { libc::getuid() };

    if path.exists() {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|e| format!("Falha ao ler metadata do socket {:?}: {}", path, e))?;

        if !meta.file_type().is_socket() {
            return Err(format!(
                "Caminho {:?} já existe e não é um socket Unix",
                path
            ));
        }

        if meta.uid() != uid {
            return Err(format!(
                "Socket {:?} pertence ao usuário UID {}, esperado UID {}",
                path,
                meta.uid(),
                uid
            ));
        }

        // Tentar conectar para checar se está vivo
        match UnixStream::connect(path) {
            Ok(_) => {
                return Err(format!(
                    "Outro Hub ativo já está escutando no socket {:?}",
                    path
                ));
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::ConnectionRefused {
                    let _ = std::fs::remove_file(path);
                } else {
                    return Err(format!("Socket {:?} inacessível: {}", path, e));
                }
            }
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoStreamOwner {
    None,
    UserUi,
    IpcTemporary,
}

pub struct ScreenshotCache {
    frame_index: u64,
    result: Arc<ScreenshotPngResult>,
}

pub struct HubState {
    pub manager: Arc<ConnectionManager>,
    pub stream_owner: Arc<Mutex<VideoStreamOwner>>,
    pub temp_capture_cv: Arc<Condvar>,
    pub temp_capture_waiters: Arc<AtomicUsize>,
    pub screenshot_cache: Arc<Mutex<Option<ScreenshotCache>>>,
}

pub struct HubServer {
    socket_path: PathBuf,
    file_identity: Option<FileIdentity>,
    is_running: Arc<AtomicBool>,
    _worker_counter: Arc<AtomicUsize>,
    join_handle: Option<thread::JoinHandle<()>>,
    _owner_lock: Option<OwnerLock>,
    state: Arc<HubState>,
}

impl HubServer {
    pub fn start(
        manager: Arc<ConnectionManager>,
        lock: Option<OwnerLock>,
    ) -> Result<Self, String> {
        let socket_path = resolve_server_socket_path()?;
        Self::start_at_path(manager, socket_path, lock)
    }

    pub fn start_at_path(
        manager: Arc<ConnectionManager>,
        socket_path: PathBuf,
        lock: Option<OwnerLock>,
    ) -> Result<Self, String> {
        prepare_server_socket_path(&socket_path)?;

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| format!("Falha ao criar UnixListener em {:?}: {}", socket_path, e))?;

        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));

        let file_identity = FileIdentity::from_path(&socket_path);
        let is_running = Arc::new(AtomicBool::new(true));
        let worker_counter = Arc::new(AtomicUsize::new(0));

        let state = Arc::new(HubState {
            manager,
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        let listener_running = Arc::clone(&is_running);
        let listener_workers = Arc::clone(&worker_counter);
        let listener_state = Arc::clone(&state);

        let _ = listener.set_nonblocking(true);

        let join_handle = thread::Builder::new()
            .name("r36s-hub-listener".to_string())
            .spawn(move || {
                while listener_running.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if listener_workers.load(Ordering::Relaxed) >= MAX_CONCURRENT_WORKERS {
                                let mut s = stream;
                                let err_resp = IpcResponse::failure(
                                    "rejected",
                                    IpcError::new(
                                        IpcErrorCode::InternalError,
                                        "Limite máximo de requisições concorrentes atingido",
                                    ),
                                );
                                if let Ok(line) = err_resp.to_json_line() {
                                    let _ = s.write_all(line.as_bytes());
                                }
                                continue;
                            }

                            listener_workers.fetch_add(1, Ordering::Relaxed);
                            let worker_count = Arc::clone(&listener_workers);
                            let stream_state = Arc::clone(&listener_state);

                            thread::spawn(move || {
                                Self::handle_connection(stream, stream_state);
                                worker_count.fetch_sub(1, Ordering::Relaxed);
                            });
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(e) => {
                            if !listener_running.load(Ordering::Relaxed) {
                                break;
                            }
                            eprintln!("[r36s-hub] Erro no accept do socket Unix: {}", e);
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .map_err(|e| format!("Falha ao iniciar thread do Hub Unix: {}", e))?;

        Ok(Self {
            socket_path,
            file_identity,
            is_running,
            _worker_counter: worker_counter,
            join_handle: Some(join_handle),
            _owner_lock: lock,
            state,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn state(&self) -> &Arc<HubState> {
        &self.state
    }

    pub fn stream_owner(&self) -> Arc<Mutex<VideoStreamOwner>> {
        Arc::clone(&self.state.stream_owner)
    }

    pub fn set_stream_owner(&self, owner: VideoStreamOwner) {
        let mut lock = self.state.stream_owner.lock().unwrap();
        *lock = owner;
    }

    fn handle_connection(mut stream: UnixStream, state: Arc<HubState>) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(IO_DEADLINE_MS)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(IO_DEADLINE_MS)));

        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });

        let mut line = Vec::new();
        loop {
            line.clear();
            let read_res = reader.read_until(b'\n', &mut line);
            match read_res {
                Ok(0) => break, // EOF do cliente
                Err(_) => break,
                Ok(_) => {
                    if line.is_empty() || line == b"\n" || line == b"\r\n" {
                        continue;
                    }
                }
            }

            let req = match validate_request_raw(&line) {
                Ok(r) => r,
                Err(e) => {
                    let resp = IpcResponse::failure("unknown", e);
                    if let Ok(resp_line) = resp.to_json_line() {
                        let _ = stream.write_all(resp_line.as_bytes());
                        let _ = stream.flush();
                    }
                    continue;
                }
            };

            let response = Self::dispatch_request(&req, &state);
            if let Ok(resp_line) = response.to_json_line() {
                if resp_line.len() > MAX_RESPONSE_BYTES {
                    let err_resp = IpcResponse::failure(
                        req.id,
                        IpcError::new(
                            IpcErrorCode::PayloadTooLarge,
                            "Resposta gerada excede o limite máximo de 4 MiB",
                        ),
                    );
                    if let Ok(err_line) = err_resp.to_json_line() {
                        let _ = stream.write_all(err_line.as_bytes());
                        let _ = stream.flush();
                    }
                } else {
                    let _ = stream.write_all(resp_line.as_bytes());
                    let _ = stream.flush();
                }
            }
        }
    }

    pub fn dispatch_request(req: &IpcRequest, state: &Arc<HubState>) -> IpcResponse {
        match req.operation.as_str() {
            "Ping" => IpcResponse::success(&req.id, serde_json::json!({ "pong": true })),
            "GetDiagnostics" => Self::handle_get_diagnostics(&req.id, state),
            "GetScreenshotPng" => Self::handle_get_screenshot_png(&req.id, state),
            "SendCommand" => Self::handle_send_command(&req.id, &req.payload, state),
            "RunMacro" => Self::handle_run_macro(&req.id, &req.payload, state),
            _ => IpcResponse::failure(
                &req.id,
                IpcError::new(IpcErrorCode::UnknownOperation, "Operação não implementada"),
            ),
        }
    }

    pub fn handle_get_diagnostics(req_id: &str, state: &Arc<HubState>) -> IpcResponse {
        let mgr = &state.manager;
        let is_connected = {
            let st = mgr.status.lock().unwrap();
            matches!(*st, ConnectionStatus::Connected { .. })
        };

        let conn_state_str = match &*mgr.status.lock().unwrap() {
            ConnectionStatus::Connected { target, latency_ms, agent_version } => {
                format!("Connected (target: {}, latency: {:.1}ms, v{})", target, latency_ms, agent_version)
            }
            ConnectionStatus::Connecting { target, stage } => format!("Connecting ({}: {})", target, stage),
            ConnectionStatus::Searching { message, .. } => format!("Searching ({})", message),
            ConnectionStatus::Error { message } => format!("Error ({})", message),
        };

        let metrics = mgr.metrics.lock().unwrap().clone();
        let telemetry = metrics.as_ref().map(|m| {
            crate::protocol::TelemetrySnapshot::from_metrics(m.clone(), now_epoch_ms())
        });
        let gamepad = if mgr.gamepad_supported.load(Ordering::Relaxed) {
            Some(mgr.gamepad_state.lock().unwrap().clone())
        } else {
            None
        };

        let fps = *mgr.current_fps.lock().unwrap();
        let session_generation = mgr.session_generation.load(Ordering::Relaxed);

        let sandbox_supported = mgr.is_sandbox_supported();
        let (
            sandbox_running,
            sandbox_pid,
            sandbox_exit_code,
            sandbox_term_signal,
            sandbox_uptime_ms,
            sandbox_stdout_tail,
            sandbox_stderr_tail,
            sandbox_restore_error,
            sandbox_recent_logs,
            sandbox_phase,
        ) = if sandbox_supported {
            let st = mgr.sandbox_status.lock().unwrap();
            if let Some(ServerResponse::SandboxStatus {
                running,
                pid,
                exit_code,
                term_signal,
                uptime_ms,
                stdout_tail,
                stderr_tail,
                restore_error,
            }) = st.as_ref()
            {
                let mut logs = String::new();
                if !stdout_tail.is_empty() {
                    logs.push_str(stdout_tail);
                }
                if !stderr_tail.is_empty() {
                    if !logs.is_empty() {
                        logs.push('\n');
                    }
                    logs.push_str(stderr_tail);
                }
                let phase = if *running {
                    "running".to_string()
                } else if exit_code.is_some() || term_signal.is_some() {
                    "terminated".to_string()
                } else {
                    "idle".to_string()
                };
                (
                    *running,
                    *pid,
                    *exit_code,
                    *term_signal,
                    Some(*uptime_ms),
                    Some(stdout_tail.clone()),
                    Some(stderr_tail.clone()),
                    restore_error.clone(),
                    if logs.is_empty() { None } else { Some(logs) },
                    Some(phase),
                )
            } else {
                (false, None, None, None, None, None, None, None, None, Some("idle".to_string()))
            }
        } else {
            (false, None, None, None, None, None, None, None, None, None)
        };

        let diag = DiagnosticsResult {
            timestamp_ms: now_epoch_ms(),
            is_connected,
            connection_state: conn_state_str,
            fps,
            session_generation,
            metrics,
            telemetry,
            gamepad,
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

        match serde_json::to_value(&diag) {
            Ok(v) => IpcResponse::success(req_id, v),
            Err(e) => IpcResponse::failure(
                req_id,
                IpcError::new(IpcErrorCode::InternalError, format!("Erro de serialização: {}", e)),
            ),
        }
    }

    pub fn handle_get_screenshot_png(req_id: &str, state: &Arc<HubState>) -> IpcResponse {
        let mgr = &state.manager;

        // 1. Verificar conexão
        let is_connected = {
            let st = mgr.status.lock().unwrap();
            matches!(*st, ConnectionStatus::Connected { .. })
        };
        if !is_connected {
            return IpcResponse::failure(
                req_id,
                IpcError::new(
                    IpcErrorCode::NotConnected,
                    "Studio não está conectado ao console R36S",
                ),
            );
        }

        // 2. Tentar obter snapshot recente com mutex mantido apenas pelo clone
        let frame_opt = {
            let current = mgr.current_frame.lock().unwrap();
            current.clone()
        };

        if let Some(frame) = frame_opt {
            let now = Instant::now();
            let is_recent = now.duration_since(frame.timestamp) < Duration::from_millis(500);
            if is_recent {
                return Self::encode_and_cache_screenshot(req_id, frame, state);
            }
        }

        // 3. Se streaming está desativado ou quadro antigo, ativar captura temporária
        match Self::capture_temporary_frame(state, Duration::from_millis(2500)) {
            Ok(frame) => Self::encode_and_cache_screenshot(req_id, frame, state),
            Err(err) => IpcResponse::failure(req_id, err),
        }
    }

    fn encode_and_cache_screenshot(
        req_id: &str,
        frame: Arc<DecodedFrame>,
        state: &Arc<HubState>,
    ) -> IpcResponse {
        // Verificar cache por frame_index
        {
            let cache = state.screenshot_cache.lock().unwrap();
            if let Some(ref c) = *cache {
                if c.frame_index == frame.frame_index as u64 {
                    if let Ok(v) = serde_json::to_value(&*c.result) {
                        return IpcResponse::success(req_id, v);
                    }
                }
            }
        }

        // Codificar PNG fora do lock de vídeo
        let png_bytes = match encode_frame_to_png(&frame) {
            Ok(b) => b,
            Err(e) => {
                return IpcResponse::failure(
                    req_id,
                    IpcError::new(
                        IpcErrorCode::InternalError,
                        format!("Falha ao codificar PNG no host: {}", e),
                    ),
                );
            }
        };

        let base64_str = base64_encode(&png_bytes);
        let result = Arc::new(ScreenshotPngResult {
            width: frame.width as u32,
            height: frame.height as u32,
            frame_index: frame.frame_index as u64,
            captured_at_ms: now_epoch_ms(),
            png_base64: base64_str,
        });

        // Salvar no cache
        {
            let mut cache = state.screenshot_cache.lock().unwrap();
            *cache = Some(ScreenshotCache {
                frame_index: frame.frame_index as u64,
                result: Arc::clone(&result),
            });
        }

        match serde_json::to_value(&*result) {
            Ok(v) => IpcResponse::success(req_id, v),
            Err(e) => IpcResponse::failure(
                req_id,
                IpcError::new(IpcErrorCode::InternalError, format!("Erro de serialização: {}", e)),
            ),
        }
    }

    pub fn capture_temporary_frame(
        state: &Arc<HubState>,
        timeout: Duration,
    ) -> Result<Arc<DecodedFrame>, IpcError> {
        let mgr = &state.manager;

        // Registrar garçom
        state.temp_capture_waiters.fetch_add(1, Ordering::SeqCst);

        let initial_index = {
            let cur = mgr.current_frame.lock().unwrap();
            cur.as_ref().map(|f| f.frame_index).unwrap_or(0)
        };

        let mut became_stream_starter = false;
        {
            let mut owner = state.stream_owner.lock().unwrap();
            if *owner == VideoStreamOwner::None {
                *owner = VideoStreamOwner::IpcTemporary;
                became_stream_starter = true;
            }
        }

        if became_stream_starter {
            mgr.is_streaming.store(true, Ordering::SeqCst);
            mgr.send_command(ClientCommand::StartStream { target_fps: 60 });
        }

        let start = Instant::now();
        let mut new_frame: Option<Arc<DecodedFrame>> = None;

        while start.elapsed() < timeout {
            {
                let cur = mgr.current_frame.lock().unwrap();
                if let Some(ref f) = *cur {
                    if f.frame_index > initial_index {
                        new_frame = Some(Arc::clone(f));
                        break;
                    }
                }
            }
            thread::sleep(Duration::from_millis(25));
        }

        let remaining_waiters = state.temp_capture_waiters.fetch_sub(1, Ordering::SeqCst) - 1;

        // O comando StopStream só deve ser enviado se o dono atual for IpcTemporary
        // e não houver mais garçons concorrentes aguardando
        if remaining_waiters == 0 {
            let mut owner = state.stream_owner.lock().unwrap();
            if *owner == VideoStreamOwner::IpcTemporary {
                *owner = VideoStreamOwner::None;
                mgr.is_streaming.store(false, Ordering::SeqCst);
                mgr.send_command(ClientCommand::StopStream);
            }
        }

        match new_frame {
            Some(f) => Ok(f),
            None => Err(IpcError::new(
                IpcErrorCode::CaptureTimeout,
                "Tempo limite esgotado aguardando novo quadro do console",
            )),
        }
    }

    pub fn handle_send_command(
        req_id: &str,
        payload: &serde_json::Value,
        state: &Arc<HubState>,
    ) -> IpcResponse {
        let cmd: ClientCommand = match serde_json::from_value(payload.clone()) {
            Ok(c) => c,
            Err(e) => {
                return IpcResponse::failure(
                    req_id,
                    IpcError::new(
                        IpcErrorCode::InvalidRequest,
                        format!("Payload de comando inválido: {}", e),
                    ),
                );
            }
        };

        let mgr = &state.manager;
        let is_connected = {
            let st = mgr.status.lock().unwrap();
            matches!(*st, ConnectionStatus::Connected { .. })
        };
        if !is_connected {
            return IpcResponse::failure(
                req_id,
                IpcError::new(IpcErrorCode::NotConnected, "Console não conectado"),
            );
        }

        match &cmd {
            ClientCommand::RunSandbox { .. } => {
                if !mgr.is_sandbox_supported() {
                    return IpcResponse::failure(
                        req_id,
                        IpcError::new(
                            IpcErrorCode::CapabilityNotSupported,
                            "Capacidade de Sandbox não suportada pelo agente remoto",
                        ),
                    );
                }
            }
            ClientCommand::StopSandbox { .. } | ClientCommand::GetSandboxStatus => {
                if !mgr.is_sandbox_supported() {
                    return IpcResponse::failure(
                        req_id,
                        IpcError::new(
                            IpcErrorCode::CapabilityNotSupported,
                            "Capacidade de Sandbox não suportada pelo agente remoto",
                        ),
                    );
                }
            }
            ClientCommand::InjectGamepadInput { action } => {
                if let GamepadInputAction::Pulse { .. } = action {
                    if !mgr.is_gamepad_pulse_supported() {
                        return IpcResponse::failure(
                            req_id,
                            IpcError::new(
                                IpcErrorCode::CapabilityNotSupported,
                                "Pulsos atômicos não suportados pelo agente remoto",
                            ),
                        );
                    }
                }
            }
            ClientCommand::SetGamepadInput { .. }
            | ClientCommand::SetGamepadMonitor { .. }
            | ClientCommand::GetGamepadState
            | ClientCommand::DeployStart { .. }
            | ClientCommand::DeployChunk { .. }
            | ClientCommand::DeployFinish
            | ClientCommand::FetchLogs { .. }
            | ClientCommand::KillProcess { .. }
            | ClientCommand::RestartES
            | ClientCommand::Ping
            | ClientCommand::GetMetrics
            | ClientCommand::StartStream { .. }
            | ClientCommand::StopStream => {}
            _ => {
                return IpcResponse::failure(
                    req_id,
                    IpcError::new(
                        IpcErrorCode::CommandRejected,
                        "Comando não autorizado na whitelist do Hub",
                    ),
                );
            }
        }

        // Despachar comando e aguardar resposta correlacionada quando aplicável
        let sub = mgr.subscribe_responses();
        let target_gen = mgr.session_generation.load(Ordering::SeqCst);
        mgr.send_command(cmd.clone());

        let timeout = Duration::from_millis(IO_DEADLINE_MS);
        let start = Instant::now();

        let mut matched_response: Option<ServerResponse> = None;

        while start.elapsed() < timeout {
            let current_gen = mgr.session_generation.load(Ordering::SeqCst);
            if current_gen != target_gen {
                return IpcResponse::failure(
                    req_id,
                    IpcError::new(
                        IpcErrorCode::NotConnected,
                        "Conexão foi reiniciada durante a execução da operação",
                    ),
                );
            }

            match sub.recv_timeout(Duration::from_millis(50)) {
                Ok((gen, resp)) => {
                    if gen != target_gen {
                        continue;
                    }
                    if Self::matches_response(&cmd, &resp) {
                        matched_response = Some(resp);
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if let Some(resp) = matched_response {
            let res = SendCommandResult {
                status: "Completed".to_string(),
                message: None,
                response: Some(resp),
            };
            match serde_json::to_value(&res) {
                Ok(v) => IpcResponse::success(req_id, v),
                Err(e) => IpcResponse::failure(
                    req_id,
                    IpcError::new(IpcErrorCode::InternalError, format!("Erro de serialização: {}", e)),
                ),
            }
        } else {
            // Se timeout expirou, estado indeterminado
            IpcResponse::failure(
                req_id,
                IpcError::new(
                    IpcErrorCode::CommandIndeterminate,
                    "Prazo de resposta do agente expirou; estado da operação indeterminado",
                ),
            )
        }
    }

    fn matches_response(cmd: &ClientCommand, resp: &ServerResponse) -> bool {
        match (cmd, resp) {
            (ClientCommand::Ping, ServerResponse::Pong) => true,
            (ClientCommand::GetMetrics, ServerResponse::Metrics(_)) => true,
            (ClientCommand::DeployStart { .. }, ServerResponse::DeployProgress { .. }) => true,
            (ClientCommand::DeployChunk { .. }, ServerResponse::DeployProgress { .. }) => true,
            (ClientCommand::DeployFinish, ServerResponse::DeploySuccess { .. } | ServerResponse::DeployError { .. }) => true,
            (ClientCommand::FetchLogs { .. }, ServerResponse::LogsResponse { .. }) => true,
            (ClientCommand::InjectGamepadInput { .. }, ServerResponse::GamepadAck { .. }) => true,
            (
                ClientCommand::SetGamepadInput { .. },
                ServerResponse::GamepadStateResponse(_) | ServerResponse::GamepadAck { .. },
            ) => true,
            (
                ClientCommand::SetGamepadMonitor { .. },
                ServerResponse::GamepadStateResponse(_) | ServerResponse::GamepadAck { .. },
            ) => true,
            (ClientCommand::GetGamepadState, ServerResponse::GamepadStateResponse(_)) => true,
            (ClientCommand::RunSandbox { .. }, ServerResponse::SandboxStartResult { .. }) => true,
            (ClientCommand::StopSandbox { .. }, ServerResponse::SandboxStopResult { .. }) => true,
            (ClientCommand::GetSandboxStatus, ServerResponse::SandboxStatus { .. }) => true,
            (ClientCommand::KillProcess { .. }, ServerResponse::KillResult { .. }) => true,
            (ClientCommand::RestartES, ServerResponse::RestartESResult { .. }) => true,
            (ClientCommand::StartStream { .. }, ServerResponse::StreamStatus { .. }) => true,
            _ => false,
        }
    }

    pub fn handle_run_macro(
        req_id: &str,
        payload: &serde_json::Value,
        state: &Arc<HubState>,
    ) -> IpcResponse {
        let steps: Vec<SequenceStep> = match serde_json::from_value(payload.clone()) {
            Ok(s) => s,
            Err(e) => {
                return IpcResponse::failure(
                    req_id,
                    IpcError::new(
                        IpcErrorCode::InvalidRequest,
                        format!("Passos de macro inválidos: {}", e),
                    ),
                );
            }
        };

        let total_steps = steps.len();
        let mgr = &state.manager;
        match mgr.execute_input_sequence(steps) {
            Ok(handle) => {
                match handle.wait() {
                    Ok(()) => {
                        let res = RunMacroResult {
                            success: true,
                            steps_completed: total_steps,
                            error: None,
                        };
                        match serde_json::to_value(&res) {
                            Ok(v) => IpcResponse::success(req_id, v),
                            Err(e) => IpcResponse::failure(
                                req_id,
                                IpcError::new(
                                    IpcErrorCode::InternalError,
                                    format!("Erro de serialização: {}", e),
                                ),
                            ),
                        }
                    }
                    Err(e) => {
                        let failed_step = match e {
                            crate::sequence::SequenceError::CommandRejected { step, .. } => step,
                            crate::sequence::SequenceError::InvalidStep { step, .. } => step,
                            _ => 0,
                        };
                        IpcResponse::failure(
                            req_id,
                            IpcError::new(
                                IpcErrorCode::MacroFailed,
                                format!("Execução da macro falhou no passo {}: {}", failed_step, e),
                            ),
                        )
                    }
                }
            }
            Err(e) => IpcResponse::failure(
                req_id,
                IpcError::new(
                    IpcErrorCode::MacroFailed,
                    format!("Não foi possível iniciar macro: {}", e),
                ),
            ),
        }
    }

    pub fn shutdown(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);

        // Conectar rapidamente ao socket para acordar o accept se bloqueado
        let _ = UnixStream::connect(&self.socket_path);

        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }

        // Limpeza segura do arquivo de socket se a identidade for a mesma
        if let Some(rec) = self.file_identity {
            if let Some(cur) = FileIdentity::from_path(&self.socket_path) {
                if rec == cur {
                    let _ = std::fs::remove_file(&self.socket_path);
                }
            }
        }
    }
}

impl Drop for HubServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn encode_frame_to_png(frame: &DecodedFrame) -> Result<Vec<u8>, String> {
    let mut png_bytes = Vec::with_capacity(frame.width as usize * frame.height as usize * 4 + 1024);
    {
        let mut encoder = png::Encoder::new(
            &mut png_bytes,
            frame.width as u32,
            frame.height as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fastest);
        encoder.set_filter(png::Filter::NoFilter);

        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("Falha ao inicializar cabeçalho PNG: {}", e))?;

        writer
            .write_image_data(&frame.rgba_data)
            .map_err(|e| format!("Falha ao gravar dados do PNG: {}", e))?;
    }

    Ok(png_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_owner_lock_acquisition_and_duplicate_rejection() {
        let lock_path = format!("/tmp/r36s_test_lock_{}.lock", std::process::id());
        let _ = std::fs::remove_file(&lock_path);

        let lock1 = OwnerLock::acquire(&lock_path, OwnerRole::Studio).expect("Primeiro lock deve ser adquirido");
        assert_eq!(lock1.role(), OwnerRole::Studio);
        assert!(OwnerLock::is_studio_active(&lock_path));

        let lock2_res = OwnerLock::acquire(&lock_path, OwnerRole::Headless);
        match lock2_res {
            Err(LockError::AlreadyLocked { role, .. }) => {
                assert_eq!(role, Some(OwnerRole::Studio));
            }
            other => panic!("Esperado AlreadyLocked, obteve: {:?}", other),
        }

        drop(lock1);

        assert!(!OwnerLock::is_studio_active(&lock_path));

        let lock3 = OwnerLock::acquire(&lock_path, OwnerRole::Headless).expect("Lock após drop deve ser adquirido");
        assert_eq!(lock3.role(), OwnerRole::Headless);

        let _ = std::fs::remove_file(&lock_path);
    }

    #[test]
    fn test_hub_lifecycle_and_ping_request() {
        let sock_path = format!("/tmp/r36s_test_sock_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&sock_path);

        let (mgr, _) = ConnectionManager::new_mock();
        let mut server = HubServer::start_at_path(
            Arc::new(mgr),
            PathBuf::from(&sock_path),
            None,
        )
        .expect("Inicialização do HubServer");

        let mut stream = UnixStream::connect(&sock_path).expect("Conexão ao socket Unix do Hub");
        let req = IpcRequest {
            version: IPC_VERSION.to_string(),
            id: "test-ping-1".to_string(),
            operation: "Ping".to_string(),
            payload: serde_json::json!({}),
        };
        let line = req.to_json_line().unwrap_or_else(|_| {
            let mut s = serde_json::to_string(&req).unwrap();
            s.push('\n');
            s
        });
        stream.write_all(line.as_bytes()).expect("Envio de requisição");
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).expect("Leitura de resposta");
        assert!(resp_line.ends_with('\n'));

        let resp: IpcResponse = serde_json::from_str(&resp_line).expect("Parse de resposta JSON");
        assert_eq!(resp.id, "test-ping-1");
        assert!(resp.ok);
        assert_eq!(resp.result, Some(serde_json::json!({ "pong": true })));

        server.shutdown();
        assert!(!Path::new(&sock_path).exists(), "Socket deve ter sido removido na limpeza segura");
    }

    #[test]
    fn test_diagnostics_and_sandbox_availability_contract() {
        let (mgr, _) = ConnectionManager::new_mock();
        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        // 1. Diagnostics quando conectado com sandbox ativo
        let resp = HubServer::handle_get_diagnostics("diag-1", &state);
        assert!(resp.ok);
        let val = resp.result.unwrap();
        let diag: DiagnosticsResult = serde_json::from_value(val).unwrap();
        assert!(diag.is_connected);
        assert!(diag.sandbox_supported);
        assert_eq!(diag.sandbox_running, false);

        // 2. Diagnostics quando sandbox não é suportado
        state.manager.sandbox_supported.store(false, Ordering::SeqCst);
        let resp2 = HubServer::handle_get_diagnostics("diag-2", &state);
        assert!(resp2.ok);
        let val2 = resp2.result.unwrap();
        let diag2: DiagnosticsResult = serde_json::from_value(val2).unwrap();
        assert_eq!(diag2.sandbox_supported, false);
        assert_eq!(diag2.sandbox_running, false);
    }

    #[test]
    fn test_screenshot_png_with_frame_in_memory() {
        let (mgr, _) = ConnectionManager::new_mock();
        let mut rgba = vec![0u8; 640 * 480 * 4];
        for i in 0..640 * 480 {
            rgba[i * 4] = 255;
            rgba[i * 4 + 1] = 0;
            rgba[i * 4 + 2] = 0;
            rgba[i * 4 + 3] = 0; // Alpha Mali
        }

        let frame = Arc::new(DecodedFrame {
            frame_index: 101,
            width: 640,
            height: 480,
            rgba_data: rgba,
            timestamp: Instant::now(),
        });
        *mgr.current_frame.lock().unwrap() = Some(frame);

        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        let resp = HubServer::handle_get_screenshot_png("shot-1", &state);
        assert!(resp.ok, "Resposta deve ser sucesso: {:?}", resp.error);
        let val = resp.result.unwrap();
        let shot: ScreenshotPngResult = serde_json::from_value(val).unwrap();
        assert_eq!(shot.width, 640);
        assert_eq!(shot.height, 480);
        assert_eq!(shot.frame_index, 101);

        let decoded_png = base64_decode(&shot.png_base64).expect("Base64 válido");
        assert_eq!(&decoded_png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn test_send_command_rejection_and_correlation() {
        let (mgr, cmd_rx) = ConnectionManager::new_mock();
        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        // 1. Rejeição de comando sandbox quando não suportado
        state.manager.sandbox_supported.store(false, Ordering::SeqCst);
        let payload = serde_json::to_value(ClientCommand::RunSandbox {
            path: "/bin/test".into(),
            args: vec![],
            env: vec![],
            cwd: None,
            exclusive_drm: false,
        }).unwrap();
        let resp = HubServer::handle_send_command("cmd-sb", &payload, &state);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, IpcErrorCode::CapabilityNotSupported as u32);

        // 2. Resposta correlacionada para Ping -> Pong
        let state_cl = Arc::clone(&state);
        let handle = thread::spawn(move || {
            let cmd = cmd_rx.recv().expect("Recebe comando Ping");
            assert_eq!(cmd, ClientCommand::Ping);
            // Simular resposta Pong do agente
            let gen = state_cl.manager.session_generation.load(Ordering::SeqCst);
            state_cl.manager.broadcast_response(gen, &ServerResponse::Pong);
        });

        let ping_payload = serde_json::to_value(ClientCommand::Ping).unwrap();
        let ping_resp = HubServer::handle_send_command("cmd-ping", &ping_payload, &state);
        handle.join().unwrap();
        assert!(ping_resp.ok);
        let res: SendCommandResult = serde_json::from_value(ping_resp.result.unwrap()).unwrap();
        assert_eq!(res.response, Some(ServerResponse::Pong));
    }

    #[test]
    fn test_temporary_capture_when_stream_paused() {
        let (mgr, cmd_rx) = ConnectionManager::new_mock();
        mgr.is_streaming.store(false, Ordering::SeqCst);

        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        let state_cl = Arc::clone(&state);
        let handle = thread::spawn(move || {
            // Deve receber StartStream
            let cmd1 = cmd_rx.recv().expect("Recebe StartStream");
            assert!(matches!(cmd1, ClientCommand::StartStream { .. }));

            // Fornecer novo quadro após um instante
            thread::sleep(Duration::from_millis(50));
            let frame = Arc::new(DecodedFrame {
                frame_index: 888,
                width: 640,
                height: 480,
                rgba_data: vec![255; 640 * 480 * 4],
                timestamp: Instant::now(),
            });
            *state_cl.manager.current_frame.lock().unwrap() = Some(frame);

            // Deve receber StopStream ao terminar
            let cmd2 = cmd_rx.recv().expect("Recebe StopStream");
            assert!(matches!(cmd2, ClientCommand::StopStream));
        });

        let frame = HubServer::capture_temporary_frame(&state, Duration::from_millis(2000))
            .expect("Captura temporária");
        assert_eq!(frame.frame_index, 888);
        assert!(!state.manager.is_streaming.load(Ordering::SeqCst));

        handle.join().unwrap();
    }

    #[test]
    fn test_png_encoding_opaque_alpha() {
        let mut rgba = vec![0u8; 640 * 480 * 4];
        for i in 0..640 * 480 {
            rgba[i * 4] = 100;
            rgba[i * 4 + 1] = 150;
            rgba[i * 4 + 2] = 200;
            rgba[i * 4 + 3] = 0; // Alpha 0 do driver Mali
        }

        let frame = DecodedFrame {
            frame_index: 42,
            width: 640,
            height: 480,
            rgba_data: rgba,
            timestamp: Instant::now(),
        };

        let png_bytes = encode_frame_to_png(&frame).expect("Codificação PNG");
        assert!(png_bytes.len() > 100);
        assert_eq!(&png_bytes[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]); // Magic header PNG
    }

    #[test]
    fn test_set_gamepad_input_matches_gamepad_state_response() {
        let (mgr, cmd_rx) = ConnectionManager::new_mock();
        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        let state_cl = Arc::clone(&state);
        let handle = thread::spawn(move || {
            let cmd = cmd_rx.recv().expect("Recebe comando SetGamepadInput");
            assert!(matches!(cmd, ClientCommand::SetGamepadInput { enabled: true }));
            let gen = state_cl.manager.session_generation.load(Ordering::SeqCst);
            // Agente real responde GamepadStateResponse em caso de sucesso
            state_cl.manager.broadcast_response(
                gen,
                &ServerResponse::GamepadStateResponse(crate::protocol::GamepadState {
                    available: true,
                    input_enabled: true,
                    ..Default::default()
                }),
            );
        });

        let payload = serde_json::to_value(ClientCommand::SetGamepadInput { enabled: true }).unwrap();
        let resp = HubServer::handle_send_command("cmd-gamepad", &payload, &state);
        handle.join().unwrap();
        assert!(resp.ok);
        let res: SendCommandResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(matches!(res.response, Some(ServerResponse::GamepadStateResponse(_))));
    }

    #[test]
    fn test_run_macro_reports_actual_steps_completed() {
        let (mgr, cmd_rx) = ConnectionManager::new_mock();
        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        let state_cl = Arc::clone(&state);
        let responder = thread::spawn(move || {
            let gen = state_cl.manager.session_generation.load(Ordering::SeqCst);
            while let Ok(cmd) = cmd_rx.recv_timeout(Duration::from_millis(500)) {
                match cmd {
                    ClientCommand::SetGamepadInput { enabled } => {
                        state_cl.manager.broadcast_response(
                            gen,
                            &ServerResponse::GamepadStateResponse(crate::protocol::GamepadState {
                                available: true,
                                input_enabled: enabled,
                                ..Default::default()
                            }),
                        );
                    }
                    ClientCommand::InjectGamepadInput { .. } => {
                        state_cl.manager.broadcast_response(
                            gen,
                            &ServerResponse::GamepadAck {
                                success: true,
                                message: "OK".into(),
                            },
                        );
                    }
                    _ => {}
                }
            }
        });

        let steps = vec![
            SequenceStep::Pulse { mask: 1, duration_ms: Some(10) },
            SequenceStep::Hold { mask: 2 },
            SequenceStep::Release { mask: 2 },
        ];
        let payload = serde_json::to_value(&steps).unwrap();
        let resp = HubServer::handle_run_macro("macro-1", &payload, &state);
        let _ = responder.join();

        assert!(resp.ok, "Resposta deve ser ok: {:?}", resp.error);
        let res: RunMacroResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(res.success);
        assert_eq!(res.steps_completed, 3);
        assert_eq!(res.error, None);
    }

    #[test]
    fn test_capture_temporary_frame_waits_for_newer_frame_not_stale_cache() {
        let (mgr, cmd_rx) = ConnectionManager::new_mock();
        // Colocar frame antigo no cache
        let old_frame = Arc::new(DecodedFrame {
            frame_index: 50,
            width: 640,
            height: 480,
            rgba_data: vec![128; 640 * 480 * 4],
            timestamp: Instant::now(),
        });
        *mgr.current_frame.lock().unwrap() = Some(old_frame);
        mgr.is_streaming.store(false, Ordering::SeqCst);

        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::None)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        let state_cl = Arc::clone(&state);
        let handle = thread::spawn(move || {
            let cmd1 = cmd_rx.recv().expect("Recebe StartStream");
            assert!(matches!(cmd1, ClientCommand::StartStream { .. }));

            // Fornecer novo quadro após delay com frame_index maior
            thread::sleep(Duration::from_millis(50));
            let new_frame = Arc::new(DecodedFrame {
                frame_index: 51,
                width: 640,
                height: 480,
                rgba_data: vec![255; 640 * 480 * 4],
                timestamp: Instant::now(),
            });
            *state_cl.manager.current_frame.lock().unwrap() = Some(new_frame);

            // Deve receber StopStream
            let cmd2 = cmd_rx.recv().expect("Recebe StopStream");
            assert!(matches!(cmd2, ClientCommand::StopStream));
        });

        let frame = HubServer::capture_temporary_frame(&state, Duration::from_millis(2000))
            .expect("Captura temporária com frame fresco");
        assert_eq!(frame.frame_index, 51);
        handle.join().unwrap();
    }

    #[test]
    fn test_capture_temporary_frame_respects_user_ui_owner() {
        let (mgr, cmd_rx) = ConnectionManager::new_mock();
        mgr.is_streaming.store(true, Ordering::SeqCst);

        let state = Arc::new(HubState {
            manager: Arc::new(mgr),
            stream_owner: Arc::new(Mutex::new(VideoStreamOwner::UserUi)),
            temp_capture_cv: Arc::new(Condvar::new()),
            temp_capture_waiters: Arc::new(AtomicUsize::new(0)),
            screenshot_cache: Arc::new(Mutex::new(None)),
        });

        let state_cl = Arc::clone(&state);
        let handle = thread::spawn(move || {
            // Como UserUi já é o dono, capture_temporary_frame NÃO envia StartStream!
            thread::sleep(Duration::from_millis(30));
            let new_frame = Arc::new(DecodedFrame {
                frame_index: 99,
                width: 640,
                height: 480,
                rgba_data: vec![200; 640 * 480 * 4],
                timestamp: Instant::now(),
            });
            *state_cl.manager.current_frame.lock().unwrap() = Some(new_frame);

            // Também NÃO deve receber StopStream
            thread::sleep(Duration::from_millis(50));
        });

        let frame = HubServer::capture_temporary_frame(&state, Duration::from_millis(1000))
            .expect("Captura temporária sob posse UserUi");
        assert_eq!(frame.frame_index, 99);
        // Owner continua UserUi
        assert_eq!(*state.stream_owner.lock().unwrap(), VideoStreamOwner::UserUi);
        assert!(state.manager.is_streaming.load(Ordering::SeqCst));
        handle.join().unwrap();
        // Garante que nenhum StopStream foi enfileirado
        assert!(cmd_rx.try_recv().is_err());
    }
}
