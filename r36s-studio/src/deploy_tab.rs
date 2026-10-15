use crate::connection::ConnectionManager;
use crate::protocol::{ClientCommand, ServerResponse};
use egui::{Color32, ProgressBar, RichText, ScrollArea, Ui};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct DeployTab {
    selected_file: Option<PathBuf>,
    target_folder: String,
    is_uploading: Arc<AtomicBool>,
}

impl DeployTab {
    pub fn new() -> Self {
        Self {
            selected_file: None,
            target_folder: "ports".to_string(),
            is_uploading: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_dropped_file(&mut self, path: PathBuf) {
        self.selected_file = Some(path);
    }

    pub fn render(&mut self, ui: &mut Ui, conn: &ConnectionManager) {
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.heading("📦 Envio e Deploy de Jogos para o R36S");
                ui.label("Transmita ROMs, JARs, Scripts .sh ou Ports diretamente pelo cabo USB OTG.");
                ui.label(RichText::new("💡 Dica: Você também pode arrastar e soltar (drag & drop) arquivos diretamente nesta janela!").small().color(Color32::from_rgb(120, 200, 255)));

                ui.add_space(8.0);

                // Seleção de Arquivo Local
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("📂 Selecionar Arquivo no PC...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                self.selected_file = Some(path);
                            }
                        }

                        if let Some(path) = &self.selected_file {
                            ui.label(RichText::new(path.to_string_lossy()).strong());
                            if ui.button("❌").clicked() {
                                self.selected_file = None;
                            }
                        } else {
                            ui.colored_label(Color32::GRAY, "Nenhum arquivo selecionado.");
                        }
                    });
                });

                ui.add_space(8.0);

                // Destino no R36S
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Pasta de Destino no R36S (/roms/...):");
                        egui::ComboBox::from_label("")
                            .selected_text(&self.target_folder)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.target_folder, "ports".to_string(), "ports/ (PortMaster & Scripts .sh)");
                                ui.selectable_value(&mut self.target_folder, "j2me".to_string(), "j2me/ (Jogos Java .jar)");
                                ui.selectable_value(&mut self.target_folder, "gba".to_string(), "gba/ (Game Boy Advance)");
                                ui.selectable_value(&mut self.target_folder, "snes".to_string(), "snes/ (Super Nintendo)");
                                ui.selectable_value(&mut self.target_folder, "psx".to_string(), "psx/ (PlayStation 1)");
                                ui.selectable_value(&mut self.target_folder, "nds".to_string(), "nds/ (Nintendo DS)");
                                ui.selectable_value(&mut self.target_folder, "megadrive".to_string(), "megadrive/ (Mega Drive)");
                            });
                    });
                });

                ui.add_space(8.0);

                // Barra de Progresso e Status
                let uploading = self.is_uploading.load(Ordering::Relaxed);
                let progress = *conn.deploy_progress.lock().unwrap();

                if uploading {
                    ui.add(ProgressBar::new(progress).show_percentage().animate(true));
                }

                if let Some(msg) = &*conn.deploy_status.lock().unwrap() {
                    let color = if msg.starts_with("ERRO") || msg.starts_with("Falha") {
                        Color32::from_rgb(255, 80, 80)
                    } else if msg.starts_with("Sucesso") {
                        Color32::from_rgb(80, 220, 100)
                    } else {
                        Color32::from_rgb(100, 200, 255)
                    };
                    ui.colored_label(color, msg);
                }

                ui.add_space(8.0);

                let is_connected = matches!(*conn.status.lock().unwrap(), crate::connection::ConnectionStatus::Connected { .. });
                let can_upload = self.selected_file.is_some() && !uploading && is_connected;

                if !is_connected {
                    ui.colored_label(Color32::from_rgb(255, 180, 80), "⚠ Conecte o R36S via cabo OTG para habilitar o envio de arquivos.");
                }

                ui.add_enabled_ui(can_upload, |ui| {
                    if ui.button(RichText::new("🚀 Iniciar Envio para o R36S").strong()).clicked() {
                        if let Some(file_path) = self.selected_file.clone() {
                            let target = self.target_folder.clone();
                            let is_up = Arc::clone(&self.is_uploading);
                            let prog = Arc::clone(&conn.deploy_progress);
                            let stat = Arc::clone(&conn.deploy_status);

                            is_up.store(true, Ordering::Relaxed);

                            thread::spawn(move || {
                                Self::perform_upload(file_path, target, is_up, prog, stat);
                            });
                        }
                    }
                });
            });
        });
    }

    fn perform_upload(
        file_path: PathBuf,
        target_folder: String,
        is_uploading: Arc<AtomicBool>,
        progress: Arc<std::sync::Mutex<f32>>,
        status: Arc<std::sync::Mutex<Option<String>>>,
    ) {
        let filename = file_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("file")
            .to_string();

        *status.lock().unwrap() = Some(format!("Calculando SHA-256 de {}...", filename));
        *progress.lock().unwrap() = 0.0;

        let mut file = match File::open(&file_path) {
            Ok(f) => f,
            Err(e) => {
                *status.lock().unwrap() = Some(format!("ERRO: Falha ao abrir arquivo: {}", e));
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }
        };

        let total_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];

        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) => {
                    *status.lock().unwrap() = Some(format!("ERRO: Falha ao ler arquivo local: {}", e));
                    is_uploading.store(false, Ordering::Relaxed);
                    return;
                }
            }
        }

        let sha256 = format!("{:x}", hasher.finalize());
        *status.lock().unwrap() = Some("Conectando ao R36S para transferência direta...".to_string());

        let target_addr: std::net::SocketAddr = "192.168.7.2:7365".parse().unwrap();
        let stream_res = TcpStream::connect_timeout(&target_addr, Duration::from_secs(4));
        let mut stream = match stream_res {
            Ok(s) => s,
            Err(e) => {
                *status.lock().unwrap() = Some(format!("ERRO: Não foi possível conectar ao R36S (timeout): {}", e));
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }
        };

        let _ = stream.set_read_timeout(Some(Duration::from_secs(12)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(12)));
        let _ = stream.set_nodelay(true);

        let mut reader = BufReader::new(stream.try_clone().unwrap());

        // 1. Envia DeployStart
        let start_cmd = ClientCommand::DeployStart {
            relative_path: target_folder,
            filename: filename.clone(),
            total_size,
            sha256: sha256.clone(),
        };

        let mut cmd_json = serde_json::to_string(&start_cmd).unwrap();
        cmd_json.push('\n');
        if let Err(e) = stream.write_all(cmd_json.as_bytes()) {
            *status.lock().unwrap() = Some(format!("ERRO: Falha ao enviar DeployStart: {}", e));
            is_uploading.store(false, Ordering::Relaxed);
            return;
        }

        let mut resp_str = String::new();
        if reader.read_line(&mut resp_str).is_err() || resp_str.is_empty() {
            *status.lock().unwrap() = Some("ERRO: R36S não respondeu ao comando de início de deploy".to_string());
            is_uploading.store(false, Ordering::Relaxed);
            return;
        }

        match serde_json::from_str::<ServerResponse>(&resp_str) {
            Ok(ServerResponse::DeployProgress { .. }) => {}
            Ok(ServerResponse::DeployError { error }) => {
                *status.lock().unwrap() = Some(format!("ERRO rejeitado pelo R36S: {}", error));
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }
            _ => {
                *status.lock().unwrap() = Some("ERRO: Resposta inesperada do R36S ao iniciar deploy".to_string());
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }
        }

        // 2. Reabre e envia chunks sequenciais
        let mut file = match File::open(&file_path) {
            Ok(f) => f,
            Err(e) => {
                *status.lock().unwrap() = Some(format!("ERRO ao reabrir arquivo: {}", e));
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }
        };

        let mut sent_bytes: u64 = 0;
        let mut chunk_idx: u32 = 0;

        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break, // Fim do arquivo
                Ok(bytes) => bytes,
                Err(e) => {
                    *status.lock().unwrap() = Some(format!("ERRO de leitura local durante upload: {}", e));
                    is_uploading.store(false, Ordering::Relaxed);
                    return;
                }
            };

            let b64 = base64_encode(&buf[..n]);
            let chunk_cmd = ClientCommand::DeployChunk {
                chunk_index: chunk_idx,
                data_base64: b64,
            };

            let mut chunk_json = serde_json::to_string(&chunk_cmd).unwrap();
            chunk_json.push('\n');
            if let Err(e) = stream.write_all(chunk_json.as_bytes()) {
                *status.lock().unwrap() = Some(format!("ERRO ao transmitir chunk {}: {}", chunk_idx, e));
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }

            resp_str.clear();
            if reader.read_line(&mut resp_str).is_err() || resp_str.is_empty() {
                *status.lock().unwrap() = Some(format!("ERRO: Conexão interrompida no chunk {}", chunk_idx));
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }

            match serde_json::from_str::<ServerResponse>(&resp_str) {
                Ok(ServerResponse::DeployProgress { .. }) => {}
                Ok(ServerResponse::DeployError { error }) => {
                    *status.lock().unwrap() = Some(format!("ERRO no chunk {}: {}", chunk_idx, error));
                    is_uploading.store(false, Ordering::Relaxed);
                    return;
                }
                _ => {
                    *status.lock().unwrap() = Some(format!("ERRO: Resposta inválida para o chunk {}", chunk_idx));
                    is_uploading.store(false, Ordering::Relaxed);
                    return;
                }
            }

            sent_bytes += n as u64;
            chunk_idx += 1;

            let p = if total_size > 0 {
                sent_bytes as f32 / total_size as f32
            } else {
                1.0
            };
            *progress.lock().unwrap() = p;
            *status.lock().unwrap() = Some(format!(
                "Enviando: {:.1} MB / {:.1} MB ({:.0}%)",
                sent_bytes as f32 / 1_000_000.0,
                total_size as f32 / 1_000_000.0,
                p * 100.0
            ));
        }

        // 3. Envia DeployFinish
        let finish_cmd = ClientCommand::DeployFinish;
        let mut finish_json = serde_json::to_string(&finish_cmd).unwrap();
        finish_json.push('\n');
        if let Err(e) = stream.write_all(finish_json.as_bytes()) {
            *status.lock().unwrap() = Some(format!("ERRO ao enviar finalização: {}", e));
            is_uploading.store(false, Ordering::Relaxed);
            return;
        }

        resp_str.clear();
        if reader.read_line(&mut resp_str).is_err() || resp_str.is_empty() {
            *status.lock().unwrap() = Some("ERRO: R36S não confirmou gravação final".to_string());
            is_uploading.store(false, Ordering::Relaxed);
            return;
        }

        match serde_json::from_str::<ServerResponse>(&resp_str) {
            Ok(ServerResponse::DeploySuccess { path, sha256 }) => {
                let short_hash = if sha256.len() >= 8 { &sha256[0..8] } else { &sha256 };
                *status.lock().unwrap() = Some(format!(
                    "Sucesso! Arquivo {} gravado em {} (SHA-256: {})",
                    filename, path, short_hash
                ));
                *progress.lock().unwrap() = 1.0;
            }
            Ok(ServerResponse::DeployError { error }) => {
                *status.lock().unwrap() = Some(format!("ERRO na validação final: {}", error));
            }
            _ => {
                *status.lock().unwrap() = Some("ERRO: Resposta inesperada na finalização do deploy".to_string());
            }
        }

        is_uploading.store(false, Ordering::Relaxed);
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as u32;
        let b1 = if i + 1 < data.len() { data[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] as u32 } else { 0 };

        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if i + 1 < data.len() {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < data.len() {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}
