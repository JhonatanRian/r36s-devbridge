use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

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
