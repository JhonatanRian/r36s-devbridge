use std::fs;
use std::process::Command;

pub struct ProcessManager;

impl ProcessManager {
    pub fn kill(pid: u32, signal: i32) -> Result<(), String> {
        if pid <= 1 {
            return Err("Operação não permitida para PID 0 ou 1".to_string());
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
        // Tenta systemctl se estiver disponível
        let res = Command::new("systemctl")
            .args(["restart", "emulationstation"])
            .output();

        if let Ok(output) = res {
            if output.status.success() {
                return Ok("EmulationStation reiniciado via systemd".to_string());
            }
        }

        // Fallback para pkill/killall
        let _ = Command::new("pkill").arg("-9").arg("emulationstation").output();
        Ok("Sinal enviado para reiniciar EmulationStation".to_string())
    }

    pub fn fetch_logs(log_type: &str, max_lines: usize) -> String {
        let lines_to_fetch = if max_lines == 0 || max_lines > 500 {
            100
        } else {
            max_lines
        };

        match log_type {
            "retroarch" => {
                let candidates = [
                    "/home/ark/.config/retroarch/retroarch.log",
                    "/roms/ports/retroarch.log",
                    "/tmp/retroarch.log",
                ];
                for path in candidates {
                    if let Ok(content) = fs::read_to_string(path) {
                        return Self::tail_lines(&content, lines_to_fetch);
                    }
                }
                "Nenhum log do RetroArch encontrado.".to_string()
            }
            "java" | "freej2me" => {
                let candidates = [
                    "/roms/ports/java.log",
                    "/roms2/ports/java.log",
                    "/home/ark/java.log",
                    "/tmp/java.log",
                ];
                for path in candidates {
                    if let Ok(content) = fs::read_to_string(path) {
                        return Self::tail_lines(&content, lines_to_fetch);
                    }
                }
                "Nenhum log Java/FreeJ2ME encontrado.".to_string()
            }
            "dmesg" => {
                if let Ok(output) = Command::new("dmesg").output() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    return Self::tail_lines(&text, lines_to_fetch);
                }
                "Falha ao executar dmesg.".to_string()
            }
            "journalctl" | "agent" => {
                if let Ok(output) = Command::new("journalctl")
                    .args(["-u", "r36s-agent", "-n", &lines_to_fetch.to_string(), "--no-pager"])
                    .output()
                {
                    let text = String::from_utf8_lossy(&output.stdout);
                    if !text.trim().is_empty() {
                        return text.into_owned();
                    }
                }
                "Sem registros recentes do r36s-agent.".to_string()
            }
            _ => format!("Tipo de log desconhecido: {}", log_type),
        }
    }

    fn tail_lines(text: &str, count: usize) -> String {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= count {
            text.to_string()
        } else {
            lines[lines.len() - count..].join("\n")
        }
    }
}
