use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct ProcessManager;

impl ProcessManager {
    pub fn kill(pid: u32, signal: i32) -> Result<(), String> {
        if pid <= 1 {
            return Err("Operação não permitida para PID de sistema (0 ou 1)".to_string());
        }
        if pid == std::process::id() {
            return Err("Operação não permitida: auto-finalização do daemon".to_string());
        }

        // Restringe a sinais seguros: SIGHUP (1), SIGINT (2), SIGKILL (9), SIGTERM (15)
        if signal != 1 && signal != 2 && signal != 9 && signal != 15 {
            return Err(format!("Sinal não permitido: {}. Apenas 1 (HUP), 2 (INT), 9 (KILL), 15 (TERM) são aceitos.", signal));
        }

        // Não permite matar processos vitais do ArkOS/kernel
        if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{}/cmdline", pid)) {
            let lower = cmdline.to_lowercase();
            if lower.contains("systemd") || lower.contains("udevd") || lower.contains("dbus") {
                return Err(format!(
                    "Operação negada: o processo {} é um serviço essencial do sistema",
                    pid
                ));
            }
        }

        unsafe {
            let res = libc::kill(pid as libc::pid_t, signal);
            if res == 0 {
                Ok(())
            } else {
                let err = std::io::Error::last_os_error();
                Err(format!("Falha ao enviar sinal {}: {}", signal, err))
            }
        }
    }

    pub fn restart_emulationstation() -> Result<String, String> {
        let res = Command::new("systemctl")
            .args(["restart", "emulationstation.service"])
            .output();

        if let Ok(output) = res {
            if output.status.success() {
                // Aguarda 1 segundo e valida se o serviço está ativo
                std::thread::sleep(std::time::Duration::from_millis(1000));
                let is_active = Command::new("systemctl")
                    .args(["is-active", "--quiet", "emulationstation.service"])
                    .status();
                if let Ok(st) = is_active {
                    if st.success() {
                        return Ok(
                            "EmulationStation reiniciado com sucesso via systemd".to_string()
                        );
                    }
                }
            }
        }

        // Fallback: finaliza o processo para que o supervisor do ArkOS (emulationstation.sh) recarregue
        let _ = Command::new("pkill")
            .arg("-15")
            .arg("-f")
            .arg("/usr/bin/emulationstation/emulationstation")
            .output();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let check_pid = Command::new("pgrep")
            .arg("-f")
            .arg("/usr/bin/emulationstation/emulationstation")
            .output();
        if let Ok(out) = check_pid {
            if !out.stdout.is_empty() {
                return Ok("Processo EmulationStation re-spawned com sucesso".to_string());
            }
        }

        Err("Não foi possível confirmar o reinício do EmulationStation".to_string())
    }

    pub fn fetch_logs(log_type: &str, max_lines: usize) -> String {
        let lines_to_fetch = if max_lines == 0 || max_lines > 500 {
            120
        } else {
            max_lines
        };

        match log_type {
            "active_game" | "active" | "game" => {
                // 1. Tenta identificar processo de jogo/port ativo e inspecionar os descritores de arquivo stdout/stderr
                let mut active_procs = Vec::new();
                if let Ok(entries) = std::fs::read_dir("/proc") {
                    for entry in entries.flatten() {
                        if let Ok(name) = entry.file_name().into_string() {
                            if let Ok(pid) = name.parse::<u32>() {
                                let cmdline_path = entry.path().join("cmdline");
                                if let Ok(cmdline) = std::fs::read_to_string(cmdline_path) {
                                    let clean_cmd = cmdline.replace('\0', " ").trim().to_string();
                                    let lower = clean_cmd.to_lowercase();
                                    let is_game = lower.contains("retroarch")
                                        || lower.contains("drastic")
                                        || lower.contains("ppsspp")
                                        || lower.contains("mupen64plus")
                                        || lower.contains("/roms/ports/")
                                        || lower.contains("java")
                                        || lower.contains("dialog");
                                    let is_excluded = lower.contains("r36s-agent") || lower.contains("emulationstation");

                                    if is_game && !is_excluded {
                                        active_procs.push(format!("PID {}: {}", pid, clean_cmd));
                                        // Verifica se stdout ou stderr foram redirecionados para arquivo
                                        for fd in ["1", "2"] {
                                            let fd_path = entry.path().join("fd").join(fd);
                                            if let Ok(target) = std::fs::read_link(fd_path) {
                                                if let Some(target_str) = target.to_str() {
                                                    if target_str.ends_with(".log") || target_str.ends_with(".txt") {
                                                        if let Ok(content) = Self::tail_file(target_str, lines_to_fetch) {
                                                            return format!("[PID {} - {}]\n{}", pid, target_str, content);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // 2. Fallbacks conhecidos de logs de ports/jogos
                let candidates = [
                    "/dev/shm/runcommand.log",
                    "/dev/shm/run.log",
                    "/tmp/run.log",
                    "/tmp/retroarch.log",
                    "/home/ark/.config/retroarch/retroarch.log",
                    "/roms/ports/log.txt",
                    "/roms2/ports/log.txt",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            if !content.trim().is_empty() {
                                return format!("[Arquivo: {}]\n{}", path, content);
                            }
                        }
                    }
                }

                let mut diag = String::from("=== Diagnóstico em Tempo Real (Jogo / Port Ativo) ===\n");
                if active_procs.is_empty() {
                    diag.push_str("Processos de jogo/port ativos: Nenhum em execução no momento.\n");
                } else {
                    diag.push_str("Processos de jogo/port ativos:\n");
                    for p in &active_procs {
                        diag.push_str(&format!("  - {}\n", p));
                    }
                }
                if let Ok(clients) = std::fs::read_to_string("/sys/kernel/debug/dri/0/clients") {
                    diag.push_str("\n[Clientes DRM /dev/dri/card0]\n");
                    diag.push_str(&clients);
                }
                if let Ok(summary) = std::fs::read_to_string("/sys/kernel/debug/dri/0/summary") {
                    diag.push_str("\n[Estado do Vídeo VOP/KMS]\n");
                    diag.push_str(&summary);
                }
                diag
            }
            "emulationstation" | "es" => {
                let candidates = [
                    "/home/ark/.emulationstation/es_log.txt",
                    "/home/ark/.emulationstation/es_log.txt.bak",
                    "/dev/shm/es_log.txt",
                    "/tmp/es_log.txt",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            if !content.trim().is_empty() {
                                return format!("[Arquivo: {}]\n{}", path, content);
                            }
                        }
                    }
                }
                let mut diag = String::from("=== Estado do EmulationStation (es_log.txt desativado por padrão no ArkOS) ===\n");
                if let Ok(out) = Command::new("systemctl").args(["status", "emulationstation.service", "--no-pager"]).output() {
                    diag.push_str(&String::from_utf8_lossy(&out.stdout));
                }
                if let Ok(clients) = std::fs::read_to_string("/sys/kernel/debug/dri/0/clients") {
                    diag.push_str("\n[Clientes DRM /dev/dri/card0]\n");
                    diag.push_str(&clients);
                }
                diag
            }
            "retroarch" => {
                let candidates = [
                    "/dev/shm/runcommand.log",
                    "/dev/shm/run.log",
                    "/home/ark/.config/retroarch/retroarch.log",
                    "/dev/shm/retroarch.log",
                    "/roms/ports/retroarch.log",
                    "/tmp/retroarch.log",
                    "/tmp/run.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            if !content.trim().is_empty() {
                                return format!("[Arquivo: {}]\n{}", path, content);
                            }
                        }
                    }
                }
                let mut diag = String::from("=== Diagnóstico RetroArch / Vídeo KMS ===\n");
                diag.push_str("Nota: O ArkOS executa o RetroArch sem arquivo de log em disco por padrão.\n");
                if let Ok(clients) = std::fs::read_to_string("/sys/kernel/debug/dri/0/clients") {
                    diag.push_str("\n[Clientes DRM /dev/dri/card0]\n");
                    diag.push_str(&clients);
                }
                if let Ok(summary) = std::fs::read_to_string("/sys/kernel/debug/dri/0/summary") {
                    diag.push_str("\n[Resumo VOP / Planos KMS]\n");
                    diag.push_str(&summary);
                }
                diag
            }
            "run" | "runcommand" => {
                let candidates = [
                    "/dev/shm/runcommand.log",
                    "/dev/shm/run.log",
                    "/tmp/run.log",
                    "/var/log/run.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            if !content.trim().is_empty() {
                                return format!("[Arquivo: {}]\n{}", path, content);
                            }
                        }
                    }
                }
                "Nenhum log em /dev/shm/runcommand.log ou /dev/shm/run.log no momento.".to_string()
            }
            "syslog" | "system" => {
                let candidates = [
                    "/var/log/syslog",
                    "/var/log/messages",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            if !content.trim().is_empty() {
                                return format!("[Arquivo: {}]\n{}", path, content);
                            }
                        }
                    }
                }
                let mut diag = String::from("=== Diagnóstico do Sistema ArkOS ===\n");
                if let Ok(out) = Command::new("systemctl").args(["--failed", "--no-pager"]).output() {
                    diag.push_str("[Serviços com falha]\n");
                    diag.push_str(&String::from_utf8_lossy(&out.stdout));
                }
                if let Ok(out) = Command::new("df").args(["-h", "/", "/roms", "/boot"]).output() {
                    diag.push_str("\n[Uso de Disco]\n");
                    diag.push_str(&String::from_utf8_lossy(&out.stdout));
                }
                if let Ok(out) = Command::new("ip").args(["-brief", "addr"]).output() {
                    diag.push_str("\n[Interfaces de Rede]\n");
                    diag.push_str(&String::from_utf8_lossy(&out.stdout));
                }
                diag
            }
            "java" | "freej2me" => {
                let candidates = [
                    "/roms/ports/java.log",
                    "/roms2/ports/java.log",
                    "/home/ark/java.log",
                    "/tmp/java.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return format!("[Arquivo: {}]\n{}", path, content);
                        }
                    }
                }
                "Nenhum log Java/FreeJ2ME encontrado.".to_string()
            }
            "port" | "installer" => {
                let candidates = [
                    "/roms/ports/r36s-devbridge/install.log",
                    "/roms2/ports/r36s-devbridge/install.log",
                    "/roms/ports/r36s-usb-ssh.log",
                    "/tmp/port.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return format!("[Arquivo: {}]\n{}", path, content);
                        }
                    }
                }
                "Nenhum log de Port/Instalador encontrado.".to_string()
            }
            "ppsspp" | "psp" => {
                let candidates = [
                    "/home/ark/.config/ppsspp/PSP/SYSTEM/ppsspp.log",
                    "/roms/psp/ppsspp.log",
                    "/tmp/ppsspp.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return format!("[Arquivo: {}]\n{}", path, content);
                        }
                    }
                }
                "Nenhum log do PPSSPP encontrado em /home/ark/.config/ppsspp/.".to_string()
            }
            "drastic" | "nds" => {
                let candidates = [
                    "/home/ark/.drastic/log.txt",
                    "/roms/nds/drastic.log",
                    "/tmp/drastic.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return format!("[Arquivo: {}]\n{}", path, content);
                        }
                    }
                }
                "Nenhum log do DraStic encontrado em /home/ark/.drastic/.".to_string()
            }
            "mupen64plus" | "n64" => {
                let candidates = [
                    "/home/ark/.config/mupen64plus/mupen64plus.log",
                    "/tmp/mupen64plus.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return format!("[Arquivo: {}]\n{}", path, content);
                        }
                    }
                }
                "Nenhum log do Mupen64Plus encontrado.".to_string()
            }
            "dmesg" => {
                match Command::new("dmesg").output() {
                    Ok(output) if output.status.success() => {
                        let text = String::from_utf8_lossy(&output.stdout);
                        Self::tail_string(&text, lines_to_fetch)
                    }
                    Ok(output) => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        format!("dmesg retornou erro (código {:?}): {}", output.status.code(), err.trim())
                    }
                    Err(e) => format!("Falha ao invocar dmesg: {}", e),
                }
            }
            "journalctl" | "agent" | "daemon" => {
                if Path::new("/var/log/r36s-agent.log").exists() {
                    if let Ok(content) = Self::tail_file("/var/log/r36s-agent.log", lines_to_fetch) {
                        return format!("[/var/log/r36s-agent.log]\n{}", content);
                    }
                }
                match Command::new("journalctl")
                    .args(["-u", "r36s-agent", "-n", &lines_to_fetch.to_string(), "--no-pager"])
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        let text = String::from_utf8_lossy(&output.stdout);
                        if !text.trim().is_empty() {
                            text.into_owned()
                        } else {
                            "Sem registros recentes do r36s-agent no journalctl ou /var/log/r36s-agent.log.".to_string()
                        }
                    }
                    Ok(output) => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        format!("journalctl retornou erro (código {:?}): {}", output.status.code(), err.trim())
                    }
                    Err(e) => format!("Falha ao invocar journalctl: {}", e),
                }
            }
            _ => format!("Tipo de log desconhecido: {}. Tipos suportados: active_game, run, emulationstation, retroarch, agent, dmesg, syslog, port, java, ppsspp, drastic, mupen64plus", log_type),
        }
    }

    /// Lê eficientemente apenas os últimos bytes do arquivo sem carregar o arquivo inteiro na memória RAM
    fn tail_file(path: &str, count: usize) -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        let file_len = metadata.len();

        let max_read = 64 * 1024; // 64 KB no máximo
        let start_pos = if file_len > max_read {
            file_len - max_read
        } else {
            0
        };

        file.seek(SeekFrom::Start(start_pos))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        let text = String::from_utf8_lossy(&buffer);
        Ok(Self::tail_string(&text, count))
    }

    fn tail_string(text: &str, count: usize) -> String {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= count {
            text.to_string()
        } else {
            lines[lines.len() - count..].join("\n")
        }
    }
}

pub const MAX_LOG_BYTES: usize = 64 * 1024; // 64 KiB combined stdout/stderr budget

#[derive(Debug)]
pub struct BoundedLogRing {
    max_bytes: usize,
    total_bytes: usize,
    chunks: VecDeque<(bool /* is_stderr */, Vec<u8>)>,
}

impl BoundedLogRing {
    pub fn new() -> Self {
        Self {
            max_bytes: MAX_LOG_BYTES,
            total_bytes: 0,
            chunks: VecDeque::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_capacity(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            total_bytes: 0,
            chunks: VecDeque::new(),
        }
    }

    pub fn push(&mut self, is_stderr: bool, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.chunks.push_back((is_stderr, data.to_vec()));
        self.total_bytes += data.len();

        while self.total_bytes > self.max_bytes && !self.chunks.is_empty() {
            if let Some((_, front)) = self.chunks.front_mut() {
                let excess = self.total_bytes - self.max_bytes;
                if front.len() <= excess {
                    self.total_bytes -= front.len();
                    self.chunks.pop_front();
                } else {
                    front.drain(..excess);
                    self.total_bytes -= excess;
                    break;
                }
            }
        }
    }

    pub fn tails(&self) -> (String, String) {
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        for (is_err, chunk) in &self.chunks {
            if *is_err {
                stderr_buf.extend_from_slice(chunk);
            } else {
                stdout_buf.extend_from_slice(chunk);
            }
        }
        (
            String::from_utf8_lossy(&stdout_buf).to_string(),
            String::from_utf8_lossy(&stderr_buf).to_string(),
        )
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.total_bytes = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxLifecycleState {
    Idle,
    Preparing,
    Running,
    Stopping,
    Restoring,
}

struct SandboxInner {
    state: SandboxLifecycleState,
    exclusive_drm: bool,
    menu_displaced: bool,
    pid: Option<u32>,
    start_time: Option<Instant>,
    exit_code: Option<i32>,
    term_signal: Option<i32>,
    restore_error: Option<String>,
    logs: Arc<Mutex<BoundedLogRing>>,
    stop_requested: bool,
}

pub type RestorerFn = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
pub type StopperFn = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
pub type DrmCheckerFn = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

pub fn get_sandbox_base_dir() -> PathBuf {
    if cfg!(debug_assertions) || std::env::var("DEVBRIDGE_MOCK_ROMS").is_ok() {
        std::env::temp_dir().join("r36s-sandbox-mock")
    } else {
        PathBuf::from("/tmp/r36s-sandbox")
    }
}

pub fn resolve_sandbox_executable_path(path_str: &str) -> Result<PathBuf, String> {
    if path_str.contains('\0') {
        return Err("Caminho contém byte NUL".to_string());
    }
    if path_str.contains("..") {
        return Err("Violação de segurança: '..' não permitido no caminho de sandbox".to_string());
    }

    let is_sandbox = path_str == "@sandbox"
        || path_str.starts_with("@sandbox/")
        || path_str.starts_with("@sandbox\\");

    if is_sandbox {
        let base_dir = get_sandbox_base_dir();
        let _ = fs::create_dir_all(&base_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&base_dir, fs::Permissions::from_mode(0o700));
        }

        let rel = if path_str == "@sandbox" {
            ""
        } else if let Some(stripped) = path_str.strip_prefix("@sandbox/") {
            stripped
        } else if let Some(stripped) = path_str.strip_prefix("@sandbox\\") {
            stripped
        } else {
            path_str
        };

        let clean_rel: PathBuf = rel
            .split(['/', '\\'])
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();

        Ok(base_dir.join(clean_rel))
    } else {
        if !path_str.starts_with('/') {
            return Err("O caminho do executável deve ser absoluto ou começar com @sandbox/".to_string());
        }
        Ok(PathBuf::from(path_str))
    }
}

pub struct SandboxManager {
    inner: Arc<Mutex<SandboxInner>>,
    stop_condvar: Arc<Condvar>,
    marker_path: PathBuf,
    restorer_override: Option<RestorerFn>,
    stopper_override: Option<StopperFn>,
    drm_checker_override: Option<DrmCheckerFn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStatusSnapshot {
    pub running: bool,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub term_signal: Option<i32>,
    pub uptime_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub restore_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStopOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub term_signal: Option<i32>,
    pub restore_error: Option<String>,
    pub error: Option<String>,
}

impl SandboxManager {
    pub fn new() -> Self {
        Self::with_marker_path(PathBuf::from("/run/r36s-sandbox-restore.marker"))
    }

    pub fn with_marker_path(marker_path: PathBuf) -> Self {
        let logs = Arc::new(Mutex::new(BoundedLogRing::new()));
        let mut initial_restore_error = None;

        if marker_path.exists() {
            eprintln!(
                "[r36s-agent] Marcador de restauração prévio encontrado em {:?}. Restaurando EmulationStation...",
                marker_path
            );
            match Self::restore_emulationstation_verified() {
                Ok(_) => {
                    let _ = std::fs::remove_file(&marker_path);
                    eprintln!("[r36s-agent] EmulationStation restaurado com sucesso na inicialização.");
                }
                Err(e) => {
                    let msg = format!("Falha na recuperação de inicialização: {}", e);
                    eprintln!("[r36s-agent] {}", msg);
                    initial_restore_error = Some(msg);
                }
            }
        }

        let inner = Arc::new(Mutex::new(SandboxInner {
            state: SandboxLifecycleState::Idle,
            exclusive_drm: false,
            menu_displaced: false,
            pid: None,
            start_time: None,
            exit_code: None,
            term_signal: None,
            restore_error: initial_restore_error,
            logs,
            stop_requested: false,
        }));

        Self {
            inner,
            stop_condvar: Arc::new(Condvar::new()),
            marker_path,
            restorer_override: None,
            stopper_override: None,
            drm_checker_override: None,
        }
    }

    #[cfg(test)]
    pub fn with_test_overrides(
        marker_path: PathBuf,
        stopper: Option<StopperFn>,
        restorer: Option<RestorerFn>,
        drm_checker: Option<DrmCheckerFn>,
    ) -> Self {
        let logs = Arc::new(Mutex::new(BoundedLogRing::new()));
        let mut initial_restore_error = None;

        if marker_path.exists() {
            let res = if let Some(ref r) = restorer {
                r()
            } else {
                Self::restore_emulationstation_verified()
            };
            match res {
                Ok(_) => {
                    let _ = std::fs::remove_file(&marker_path);
                }
                Err(e) => {
                    initial_restore_error = Some(format!("Startup recovery failed: {}", e));
                }
            }
        }

        let inner = Arc::new(Mutex::new(SandboxInner {
            state: SandboxLifecycleState::Idle,
            exclusive_drm: false,
            menu_displaced: false,
            pid: None,
            start_time: None,
            exit_code: None,
            term_signal: None,
            restore_error: initial_restore_error,
            logs,
            stop_requested: false,
        }));

        Self {
            inner,
            stop_condvar: Arc::new(Condvar::new()),
            marker_path,
            restorer_override: restorer,
            stopper_override: stopper,
            drm_checker_override: drm_checker,
        }
    }

    pub fn validate_request(
        path: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&str>,
    ) -> Result<PathBuf, String> {
        if path.contains('\0') {
            return Err("Caminho do executável contém byte NUL".to_string());
        }
        let resolved = resolve_sandbox_executable_path(path)?;
        if !resolved.is_file() {
            return Err(format!("Executável não encontrado ou não é arquivo regular: {}", resolved.display()));
        }
        let meta = resolved
            .metadata()
            .map_err(|e| format!("Não foi possível ler metadados de {}: {}", resolved.display(), e))?;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("Arquivo {} não possui permissão de execução", resolved.display()));
        }

        if let Ok(curr_exe) = std::env::current_exe() {
            if curr_exe == resolved {
                return Err("Não é permitido executar o próprio binário do daemon no sandbox".to_string());
            }
        }
        let lower_path = resolved.to_string_lossy().to_lowercase();
        if lower_path.ends_with("/r36s-agent")
            || lower_path.contains("systemd")
            || lower_path.contains("udevd")
            || lower_path.contains("dbus")
            || lower_path == "/sbin/init"
        {
            return Err(format!("Execução negada para binário crítico do sistema: {}", resolved.display()));
        }

        if let Some(dir) = cwd {
            if dir.contains('\0') {
                return Err("Diretório de trabalho (cwd) contém byte NUL".to_string());
            }
            let d = Path::new(dir);
            if !d.is_dir() {
                return Err(format!("Diretório de trabalho não existe ou não é diretório: {}", dir));
            }
        }

        if args.len() > 256 {
            return Err("Número excessivo de argumentos (máximo 256)".to_string());
        }
        for (i, arg) in args.iter().enumerate() {
            if arg.contains('\0') {
                return Err(format!("Argumento {} contém byte NUL", i));
            }
            if arg.len() > 65536 {
                return Err(format!("Argumento {} excede o limite de tamanho (64 KiB)", i));
            }
        }

        if env.len() > 256 {
            return Err("Número excessivo de variáveis de ambiente (máximo 256)".to_string());
        }
        let mut seen_keys = HashSet::new();
        for (k, v) in env {
            if k.contains('\0') || v.contains('\0') {
                return Err(format!("Variável de ambiente '{}' contém byte NUL", k));
            }
            if k.len() > 65536 || v.len() > 65536 {
                return Err(format!("Variável de ambiente '{}' excede 64 KiB", k));
            }
            if !seen_keys.insert(k.clone()) {
                return Err(format!("Chave de variável de ambiente duplicada: {}", k));
            }
        }

        Ok(resolved)
    }

    pub fn run(
        &self,
        path: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<String>,
        exclusive_drm: bool,
    ) -> Result<u32, String> {
        let resolved_path = Self::validate_request(&path, &args, &env, cwd.as_deref())?;

        let logs_arc = {
            let mut inner = self.inner.lock().unwrap();
            if inner.state != SandboxLifecycleState::Idle {
                return Err("Sandbox já está em execução ou em processo de encerramento".to_string());
            }
            inner.state = SandboxLifecycleState::Preparing;
            inner.exclusive_drm = exclusive_drm;
            inner.menu_displaced = false;
            inner.pid = None;
            inner.start_time = None;
            inner.exit_code = None;
            inner.term_signal = None;
            inner.restore_error = None;
            inner.stop_requested = false;
            inner.logs.lock().unwrap().clear();
            Arc::clone(&inner.logs)
        };

        if exclusive_drm {
            if let Err(e) = self.check_no_active_game() {
                let mut inner = self.inner.lock().unwrap();
                inner.state = SandboxLifecycleState::Idle;
                return Err(e);
            }

            if let Err(e) = self.write_restore_marker() {
                let mut inner = self.inner.lock().unwrap();
                inner.state = SandboxLifecycleState::Idle;
                return Err(format!("Falha ao gravar marcador atômico em {:?}: {}", self.marker_path, e));
            }

            let stop_res = if let Some(ref stopper) = self.stopper_override {
                stopper()
            } else {
                Self::stop_emulationstation()
            };

            if let Err(e) = stop_res {
                let mut inner = self.inner.lock().unwrap();
                self.clear_restore_marker();
                inner.state = SandboxLifecycleState::Idle;
                return Err(format!("Falha ao parar EmulationStation: {}", e));
            }

            {
                let mut inner = self.inner.lock().unwrap();
                inner.menu_displaced = true;
            }

            let drm_res = if let Some(ref checker) = self.drm_checker_override {
                checker()
            } else {
                Self::verify_drm_released(Duration::from_secs(4))
            };

            if let Err(e) = drm_res {
                eprintln!("[r36s-agent] Verificação de DRM falhou: {}. Restaurando EmulationStation...", e);
                let restore_res = self.restore_menu();
                let mut inner = self.inner.lock().unwrap();
                if let Err(ref re) = restore_res {
                    inner.restore_error = Some(re.clone());
                } else {
                    self.clear_restore_marker();
                }
                inner.state = SandboxLifecycleState::Idle;
                return Err(format!("Handoff DRM falhou: {}", e));
            }
        }

        let mut cmd = Command::new(&resolved_path);
        cmd.args(&args);

        let mut final_env = HashMap::new();
        final_env.insert(
            "SDL_GAMECONTROLLERCONFIG_FILE".to_string(),
            "/opt/inttools/gamecontrollerdb.txt".to_string(),
        );
        final_env.insert("XDG_RUNTIME_DIR".to_string(), "/run/user/1002".to_string());
        final_env.insert("HOME".to_string(), "/home/ark".to_string());

        for (k, v) in env {
            final_env.insert(k, v);
        }
        for (k, v) in final_env {
            cmd.env(k, v);
        }

        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        } else if let Some(parent) = resolved_path.parent() {
            if parent.is_dir() {
                cmd.current_dir(parent);
            }
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let err_msg = format!("Falha ao inicializar processo sandbox: {}", e);
                eprintln!("[r36s-agent] {}", err_msg);
                let displaced = {
                    let inner = self.inner.lock().unwrap();
                    inner.menu_displaced
                };
                if displaced {
                    let restore_res = self.restore_menu();
                    let mut inner = self.inner.lock().unwrap();
                    if let Err(ref re) = restore_res {
                        inner.restore_error = Some(re.clone());
                    } else {
                        self.clear_restore_marker();
                    }
                    inner.state = SandboxLifecycleState::Idle;
                } else {
                    let mut inner = self.inner.lock().unwrap();
                    inner.state = SandboxLifecycleState::Idle;
                }
                return Err(err_msg);
            }
        };

        let pid = child.id();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.pid = Some(pid);
            inner.start_time = Some(Instant::now());
            inner.state = SandboxLifecycleState::Running;
        }

        if let Some(stdout_pipe) = child.stdout.take() {
            let logs_clone = Arc::clone(&logs_arc);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout_pipe);
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut ring = logs_clone.lock().unwrap();
                    ring.push(false, &buf[..n]);
                }
            });
        }

        if let Some(stderr_pipe) = child.stderr.take() {
            let logs_clone = Arc::clone(&logs_arc);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stderr_pipe);
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut ring = logs_clone.lock().unwrap();
                    ring.push(true, &buf[..n]);
                }
            });
        }

        let inner_clone = Arc::clone(&self.inner);
        let condvar_clone = Arc::clone(&self.stop_condvar);
        let restorer_opt = self.restorer_override.clone();
        let marker_path_clone = self.marker_path.clone();

        std::thread::spawn(move || {
            let pgid = pid as libc::pid_t;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let mut inner = inner_clone.lock().unwrap();
                        inner.exit_code = status.code();
                        inner.term_signal = status.signal();
                        break;
                    }
                    Ok(None) => {
                        let stop_requested = {
                            let inner = inner_clone.lock().unwrap();
                            inner.stop_requested
                        };
                        if stop_requested {
                            unsafe {
                                libc::kill(-pgid, libc::SIGTERM);
                            }
                            let start_wait = Instant::now();
                            let mut exited = false;
                            while start_wait.elapsed() < Duration::from_millis(3000) {
                                if let Ok(Some(status)) = child.try_wait() {
                                    let mut inner = inner_clone.lock().unwrap();
                                    inner.exit_code = status.code();
                                    inner.term_signal = status.signal();
                                    exited = true;
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            if !exited {
                                unsafe {
                                    libc::kill(-pgid, libc::SIGKILL);
                                }
                                if let Ok(status) = child.wait() {
                                    let mut inner = inner_clone.lock().unwrap();
                                    inner.exit_code = status.code();
                                    inner.term_signal = status.signal();
                                }
                            }
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => {
                        eprintln!("[r36s-agent] Erro ao aguardar término do processo {}: {}", pid, e);
                        break;
                    }
                }
            }

            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
                std::thread::sleep(Duration::from_millis(150));
                libc::kill(-pgid, libc::SIGKILL);
                let mut dummy = 0;
                while libc::waitpid(-pgid, &mut dummy, libc::WNOHANG) > 0 {}
            }

            std::thread::sleep(Duration::from_millis(100));

            let displaced = {
                let inner = inner_clone.lock().unwrap();
                inner.menu_displaced
            };

            if displaced {
                {
                    let mut inner = inner_clone.lock().unwrap();
                    inner.state = SandboxLifecycleState::Restoring;
                }
                let restore_res = if let Some(ref r) = restorer_opt {
                    r()
                } else {
                    Self::restore_emulationstation_verified()
                };

                let mut inner = inner_clone.lock().unwrap();
                if let Err(e) = restore_res {
                    eprintln!("[r36s-agent] Falha ao restaurar menu do EmulationStation: {}", e);
                    inner.restore_error = Some(e);
                } else {
                    let _ = std::fs::remove_file(&marker_path_clone);
                    inner.restore_error = None;
                }
            }

            {
                let mut inner = inner_clone.lock().unwrap();
                inner.state = SandboxLifecycleState::Idle;
                condvar_clone.notify_all();
            }
        });

        Ok(pid)
    }

    pub fn stop(
        &self,
        timeout_ms: u64,
    ) -> SandboxStopOutcome {
        let clamped_timeout = if timeout_ms == 0 {
            Duration::from_millis(3000)
        } else if timeout_ms < 500 {
            Duration::from_millis(500)
        } else if timeout_ms > 15000 {
            Duration::from_millis(15000)
        } else {
            Duration::from_millis(timeout_ms)
        };

        let mut inner = self.inner.lock().unwrap();
        if inner.state != SandboxLifecycleState::Running && inner.state != SandboxLifecycleState::Stopping {
            let success = inner.restore_error.is_none();
            return SandboxStopOutcome {
                success,
                exit_code: inner.exit_code,
                term_signal: inner.term_signal,
                restore_error: inner.restore_error.clone(),
                error: None,
            };
        }

        inner.stop_requested = true;
        inner.state = SandboxLifecycleState::Stopping;
        let pid = inner.pid;

        if let Some(p) = pid {
            unsafe {
                libc::kill(-(p as libc::pid_t), libc::SIGTERM);
            }
        }

        let start_wait = Instant::now();
        while inner.state != SandboxLifecycleState::Idle && start_wait.elapsed() < clamped_timeout {
            let remaining = clamped_timeout.saturating_sub(start_wait.elapsed());
            let (new_inner, _) = self.stop_condvar.wait_timeout(inner, remaining).unwrap();
            inner = new_inner;
        }

        if inner.state != SandboxLifecycleState::Idle {
            if let Some(p) = pid {
                unsafe {
                    libc::kill(-(p as libc::pid_t), libc::SIGKILL);
                }
            }
            let (new_inner, _) = self
                .stop_condvar
                .wait_timeout(inner, Duration::from_millis(2000))
                .unwrap();
            inner = new_inner;
        }

        let success = inner.restore_error.is_none() && inner.state == SandboxLifecycleState::Idle;
        SandboxStopOutcome {
            success,
            exit_code: inner.exit_code,
            term_signal: inner.term_signal,
            restore_error: inner.restore_error.clone(),
            error: if !success && inner.restore_error.is_none() {
                Some("Tempo esgotado aguardando finalização do processo sandbox".to_string())
            } else {
                None
            },
        }
    }

    pub fn status(&self) -> SandboxStatusSnapshot {
        let inner = self.inner.lock().unwrap();
        let running = inner.state == SandboxLifecycleState::Running
            || inner.state == SandboxLifecycleState::Stopping
            || inner.state == SandboxLifecycleState::Preparing
            || inner.state == SandboxLifecycleState::Restoring;
        let uptime_ms = if running {
            inner.start_time.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0)
        } else {
            0
        };
        let (stdout_tail, stderr_tail) = inner.logs.lock().unwrap().tails();

        SandboxStatusSnapshot {
            running,
            pid: inner.pid,
            exit_code: inner.exit_code,
            term_signal: inner.term_signal,
            uptime_ms,
            stdout_tail,
            stderr_tail,
            restore_error: inner.restore_error.clone(),
        }
    }

    fn check_no_active_game(&self) -> Result<(), String> {
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if let Ok(pid) = name.parse::<u32>() {
                        if pid == std::process::id() || pid <= 1 {
                            continue;
                        }
                        if let Ok(cmdline) = std::fs::read_to_string(entry.path().join("cmdline")) {
                            let clean = cmdline.replace('\0', " ").to_lowercase();
                            if (clean.contains("retroarch")
                                || clean.contains("drastic")
                                || clean.contains("ppsspp")
                                || clean.contains("mupen64plus")
                                || clean.contains("ffplay")
                                || clean.contains("/roms/ports/"))
                                && !clean.contains("r36s-agent")
                                && !clean.contains("emulationstation")
                            {
                                return Err(format!(
                                    "Jogo ou emulador já ativo no momento (PID {}: {}). Handoff DRM exclusivo recusado.",
                                    pid, clean.trim()
                                ));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn write_restore_marker(&self) -> std::io::Result<()> {
        let tmp = self.marker_path.with_extension("tmp");
        let content = format!("{}\n", std::process::id());
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &self.marker_path)?;
        Ok(())
    }

    fn clear_restore_marker(&self) {
        let _ = std::fs::remove_file(&self.marker_path);
    }

    fn stop_emulationstation() -> Result<(), String> {
        let res = Command::new("systemctl")
            .args(["stop", "emulationstation.service"])
            .output();
        match res {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr);
                Err(format!("systemctl stop falhou: {}", err.trim()))
            }
            Err(e) => Err(format!("Falha ao invocar systemctl stop: {}", e)),
        }
    }

    fn restore_menu(&self) -> Result<(), String> {
        if let Some(ref r) = self.restorer_override {
            r()
        } else {
            Self::restore_emulationstation_verified()
        }
    }

    pub fn restore_emulationstation_verified() -> Result<(), String> {
        let res = Command::new("systemctl")
            .args(["start", "emulationstation.service"])
            .output();

        let started = match res {
            Ok(out) => out.status.success(),
            Err(_) => false,
        };

        let start_check = Instant::now();
        while start_check.elapsed() < Duration::from_secs(4) {
            let is_active = Command::new("systemctl")
                .args(["is-active", "--quiet", "emulationstation.service"])
                .status();
            if let Ok(st) = is_active {
                if st.success() {
                    return Ok(());
                }
            }

            let check_pid = Command::new("pgrep")
                .arg("-f")
                .arg("emulationstation")
                .output();
            if let Ok(out) = check_pid {
                if !out.stdout.is_empty() {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }

        if started {
            Err("Serviço emulationstation foi iniciado mas não foi confirmado ativo a tempo".to_string())
        } else {
            Err("systemctl start emulationstation.service falhou".to_string())
        }
    }

    pub fn verify_drm_released(timeout: Duration) -> Result<(), String> {
        let mut card_rdevs = Vec::new();
        for card_path in &["/dev/dri/card0", "/dev/dri/card1"] {
            if let Ok(meta) = std::fs::metadata(card_path) {
                card_rdevs.push((card_path.to_string(), meta.rdev()));
            }
        }

        let start = Instant::now();
        while start.elapsed() < timeout {
            let mut graphical_running = false;
            let mut drm_holder: Option<u32> = None;

            if let Ok(entries) = std::fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    if let Ok(name) = entry.file_name().into_string() {
                        if let Ok(pid) = name.parse::<u32>() {
                            if pid == std::process::id() || pid <= 1 {
                                continue;
                            }

                            if let Ok(cmdline) = std::fs::read_to_string(entry.path().join("cmdline")) {
                                let clean = cmdline.replace('\0', " ").to_lowercase();
                                if clean.contains("emulationstation")
                                    || clean.contains("retroarch")
                                    || clean.contains("ffplay")
                                    || clean.contains("ppsspp")
                                    || clean.contains("drastic")
                                {
                                    graphical_running = true;
                                }
                            }

                            let fd_dir = entry.path().join("fd");
                            if let Ok(fd_entries) = std::fs::read_dir(fd_dir) {
                                for fd_entry in fd_entries.flatten() {
                                    if let Ok(target) = std::fs::read_link(fd_entry.path()) {
                                        let target_str = target.to_string_lossy();
                                        if target_str.contains("/dev/dri/card") {
                                            drm_holder = Some(pid);
                                            break;
                                        }
                                        if !card_rdevs.is_empty() {
                                            if let Ok(meta) = std::fs::metadata(fd_entry.path()) {
                                                for (_, rdev) in &card_rdevs {
                                                    if meta.rdev() == *rdev {
                                                        drm_holder = Some(pid);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if graphical_running || drm_holder.is_some() {
                        break;
                    }
                }
            }

            if !graphical_running && drm_holder.is_none() {
                return Ok(());
            }

            std::thread::sleep(Duration::from_millis(100));
        }

        Err("Tempo limite esgotado: EmulationStation ou descritores DRM ainda ativos".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn test_bounded_log_ring_budget_and_identity() {
        let mut ring = BoundedLogRing::with_capacity(1024); // 1 KiB for test
        ring.push(false, b"out1\n");
        ring.push(true, b"err1\n");
        ring.push(false, b"out2\n");

        let (stdout, stderr) = ring.tails();
        assert_eq!(stdout, "out1\nout2\n");
        assert_eq!(stderr, "err1\n");

        // High volume overflow test
        let large_payload = vec![b'A'; 2000];
        ring.push(false, &large_payload);

        let (stdout_after, stderr_after) = ring.tails();
        assert_eq!(stdout_after.len(), 1024);
        assert_eq!(stderr_after.len(), 0); // err1 was evicted due to budget
        assert!(stdout_after.chars().all(|c| c == 'A'));
    }

    #[test]
    fn test_validate_request_checks() {
        // Relative path
        assert!(SandboxManager::validate_request("bin/ls", &[], &[], None).is_err());
        // NUL byte
        assert!(SandboxManager::validate_request("/bin/ls\0", &[], &[], None).is_err());
        // Non existent
        assert!(SandboxManager::validate_request("/nonexistent_test_bin", &[], &[], None).is_err());
        // System critical binary
        assert!(SandboxManager::validate_request("/sbin/init", &[], &[], None).is_err());

        // File without exec bit
        let tmp_file = PathBuf::from("/tmp/test_noexec.bin");
        std::fs::write(&tmp_file, b"echo hello").unwrap();
        let mut perms = std::fs::metadata(&tmp_file).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&tmp_file, perms).unwrap();

        assert!(SandboxManager::validate_request(tmp_file.to_str().unwrap(), &[], &[], None).is_err());

        // Make executable
        let mut perms_exec = std::fs::metadata(&tmp_file).unwrap().permissions();
        perms_exec.set_mode(0o755);
        std::fs::set_permissions(&tmp_file, perms_exec).unwrap();

        assert!(SandboxManager::validate_request(tmp_file.to_str().unwrap(), &[], &[], None).is_ok());

        // Duplicate env keys
        let dup_env = vec![
            ("VAR".to_string(), "1".to_string()),
            ("VAR".to_string(), "2".to_string()),
        ];
        assert!(SandboxManager::validate_request(tmp_file.to_str().unwrap(), &[], &dup_env, None).is_err());

        // Invalid cwd
        assert!(SandboxManager::validate_request(tmp_file.to_str().unwrap(), &[], &[], Some("/nonexistent_cwd_dir")).is_err());

        let _ = std::fs::remove_file(tmp_file);

        // Sandbox paths
        let sb_base = get_sandbox_base_dir();
        let _ = std::fs::create_dir_all(&sb_base);
        let sb_bin = sb_base.join("my_test_app");
        std::fs::write(&sb_bin, b"#!/bin/sh\necho hi\n").unwrap();
        let mut perms_sb = std::fs::metadata(&sb_bin).unwrap().permissions();
        perms_sb.set_mode(0o755);
        std::fs::set_permissions(&sb_bin, perms_sb).unwrap();

        let resolved = SandboxManager::validate_request("@sandbox/my_test_app", &[], &[], None);
        assert!(resolved.is_ok());
        assert_eq!(resolved.unwrap(), sb_bin);

        // Path traversal in sandbox path
        assert!(SandboxManager::validate_request("@sandbox/../etc/passwd", &[], &[], None).is_err());

        let _ = std::fs::remove_file(&sb_bin);
    }

    #[test]
    fn test_concurrent_run_rejected_and_disconnect_ownership() {
        let test_id = std::process::id();
        let marker = PathBuf::from(format!("/tmp/test_marker_concurrency_{}.marker", test_id));
        let mgr = Arc::new(SandboxManager::with_test_overrides(
            marker.clone(),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(|| Ok(()))),
        ));

        // Create executable script
        let script = PathBuf::from(format!("/tmp/test_sleep_sandbox_{}.sh", test_id));
        let _ = std::fs::remove_file(&script);
        std::fs::write(&script, b"#!/bin/sh\nsleep 1\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let pid1 = mgr
            .run(
                script.to_str().unwrap().to_string(),
                vec![],
                vec![],
                None,
                false,
            )
            .expect("First run must succeed");

        // Concurrent attempt while running
        let err2 = mgr.run(
            script.to_str().unwrap().to_string(),
            vec![],
            vec![],
            None,
            false,
        );
        assert!(err2.is_err(), "Second launch while active must fail");

        // Stop the running sandbox
        let outcome = mgr.stop(2000);
        assert!(outcome.success);

        // After stopping, a new run must succeed
        let pid2 = mgr
            .run(
                script.to_str().unwrap().to_string(),
                vec![],
                vec![],
                None,
                false,
            )
            .expect("Run after stop must succeed");
        assert_ne!(pid1, pid2);

        mgr.stop(2000);
        let _ = std::fs::remove_file(script);
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn test_sandbox_environment_pgid_and_output_capture() {
        let test_id = std::process::id();
        let marker = PathBuf::from(format!("/tmp/test_marker_env_{}.marker", test_id));
        let mgr = SandboxManager::with_test_overrides(
            marker.clone(),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(|| Ok(()))),
        );

        let script = PathBuf::from(format!("/tmp/test_env_sandbox_{}.sh", test_id));
        let _ = std::fs::remove_file(&script);
        std::fs::write(
            &script,
            b"#!/bin/sh\necho \"CFG=$SDL_GAMECONTROLLERCONFIG_FILE\"\necho \"CUSTOM=$MY_VAR\"\necho \"HOME=$HOME\"\necho \"PGID=$(ps -o pgid= -p $$ | tr -d ' ')\"\necho \"PID=$$\"\nexit 0\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let custom_env = vec![
            ("MY_VAR".to_string(), "custom_val".to_string()),
            ("HOME".to_string(), "/custom/home".to_string()),
        ];

        let pid = mgr
            .run(
                script.to_str().unwrap().to_string(),
                vec![],
                custom_env,
                None,
                false,
            )
            .expect("Run script");

        // Wait for script to complete naturally
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(3000) {
            let status = mgr.status();
            if !status.running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let outcome = mgr.stop(1000);
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, Some(0));

        let status = mgr.status();
        assert!(status.stdout_tail.contains("CFG=/opt/inttools/gamecontrollerdb.txt"));
        assert!(status.stdout_tail.contains("CUSTOM=custom_val"));
        assert!(status.stdout_tail.contains("HOME=/custom/home"));
        // Check PGID matching PID
        assert!(status.stdout_tail.contains(&format!("PID={}", pid)));
        assert!(status.stdout_tail.contains(&format!("PGID={}", pid)));

        let _ = std::fs::remove_file(script);
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn test_crash_sigsegv_signal_reporting() {
        let test_id = std::process::id();
        let marker = PathBuf::from(format!("/tmp/test_marker_crash_{}.marker", test_id));
        let mgr = SandboxManager::with_test_overrides(
            marker.clone(),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(|| Ok(()))),
        );

        let script = PathBuf::from(format!("/tmp/test_crash_sandbox_{}.sh", test_id));
        let _ = std::fs::remove_file(&script);
        std::fs::write(&script, b"#!/bin/sh\nkill -SEGV $$\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        mgr.run(
            script.to_str().unwrap().to_string(),
            vec![],
            vec![],
            None,
            false,
        )
        .expect("Run crash script");

        std::thread::sleep(Duration::from_millis(100));

        let outcome = mgr.stop(3000);
        assert_eq!(outcome.term_signal, Some(11)); // 11 is SIGSEGV

        let status = mgr.status();
        assert_eq!(status.exit_code, None);
        assert_eq!(status.term_signal, Some(11));

        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn test_restoration_marker_and_fault_injected_error() {
        let marker = PathBuf::from("/tmp/test_marker_fault.marker");
        let _ = std::fs::remove_file(&marker);

        let restore_called = Arc::new(AtomicBool::new(false));
        let restore_called_cl = Arc::clone(&restore_called);

        let mgr = SandboxManager::with_test_overrides(
            marker.clone(),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(move || {
                restore_called_cl.store(true, Ordering::SeqCst);
                Err("Injected restoration failure".to_string())
            })),
            Some(Arc::new(|| Ok(()))),
        );

        let script = PathBuf::from("/tmp/test_quick_sandbox.sh");
        std::fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        mgr.run(
            script.to_str().unwrap().to_string(),
            vec![],
            vec![],
            None,
            true, // exclusive_drm triggers marker and menu restoration
        )
        .expect("Run exclusive sandbox");

        let outcome = mgr.stop(3000);
        assert!(!outcome.success, "Failure in restoration must report success = false");
        assert!(restore_called.load(Ordering::SeqCst));
        assert!(outcome.restore_error.is_some());
        assert!(outcome.restore_error.unwrap().contains("Injected restoration failure"));

        // Status must expose restore_error
        let status = mgr.status();
        assert!(status.restore_error.is_some());

        // Startup recovery test:
        // Create marker file, then initialize new SandboxManager: it must invoke recovery!
        std::fs::write(&marker, b"12345\n").unwrap();
        assert!(marker.exists());

        let recovery_called = Arc::new(AtomicBool::new(false));
        let recovery_called_cl = Arc::clone(&recovery_called);

        let _recovered_mgr = SandboxManager::with_test_overrides(
            marker.clone(),
            Some(Arc::new(|| Ok(()))),
            Some(Arc::new(move || {
                recovery_called_cl.store(true, Ordering::SeqCst);
                Ok(())
            })),
            Some(Arc::new(|| Ok(()))),
        );

        assert!(recovery_called.load(Ordering::SeqCst));
        assert!(!marker.exists(), "Marker must be cleared after verified recovery");

        let _ = std::fs::remove_file(script);
        let _ = std::fs::remove_file(marker);
    }
}


