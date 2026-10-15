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
                return Ok("EmulationStation reiniciado com sucesso via systemd".to_string());
            }
        }

        let pkill_res = Command::new("pkill").arg("-9").arg("-f").arg("emulationstation").output();
        if let Ok(pkill_out) = pkill_res {
            if pkill_out.status.success() {
                return Ok("Processo EmulationStation finalizado para re-spawn".to_string());
            }
        }

        Err("Não foi possível reiniciar o EmulationStation".to_string())
    }

    pub fn fetch_logs(log_type: &str, max_lines: usize) -> String {
        let lines_to_fetch = if max_lines == 0 || max_lines > 500 {
            120
        } else {
            max_lines
        };

        match log_type {
            "emulationstation" | "es" => {
                let candidates = [
                    "/home/ark/.emulationstation/es_log.txt",
                    "/home/ark/.emulationstation/es_log.txt.bak",
                    "/tmp/es_log.txt",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return content;
                        }
                    }
                }
                "Nenhum log do EmulationStation encontrado em /home/ark/.emulationstation/.".to_string()
            }
            "retroarch" => {
                let candidates = [
                    "/home/ark/.config/retroarch/retroarch.log",
                    "/roms/ports/retroarch.log",
                    "/tmp/retroarch.log",
                ];
                for path in candidates {
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return content;
                        }
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
                    if Path::new(path).exists() {
                        if let Ok(content) = Self::tail_file(path, lines_to_fetch) {
                            return content;
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
                            return content;
                        }
                    }
                }
                "Nenhum log de Port/Instalador encontrado.".to_string()
            }
            "dmesg" => {
                if let Ok(output) = Command::new("dmesg").output() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    return Self::tail_string(&text, lines_to_fetch);
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
                "Sem registros recentes do r36s-agent no journalctl.".to_string()
            }
            _ => format!("Tipo de log desconhecido: {}", log_type),
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
