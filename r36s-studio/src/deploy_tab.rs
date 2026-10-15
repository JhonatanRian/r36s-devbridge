use crate::connection::ConnectionManager;
use crate::protocol::ClientCommand;
use egui::{ProgressBar, RichText, Ui};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

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

    pub fn render(&mut self, ui: &mut Ui, conn: &ConnectionManager) {
        ui.vertical(|ui| {
            ui.heading("📦 Envio e Deploy de Jogos / Arquivos para o R36S");
            ui.label("Transmita ROMs, JARs, Scripts .sh ou Ports diretamente pelo cabo USB sem tirar o cartão MicroSD.");

            ui.add_space(8.0);

            // Seleção de Arquivo Local
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    if ui.button("📂 Selecionar Arquivo no PC...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.selected_file = Some(path);
                        }
                    }

                    if let Some(path) = &self.selected_file {
                        ui.label(RichText::new(path.to_string_lossy()).strong());
                    } else {
                        ui.colored_label(egui::Color32::GRAY, "Nenhum arquivo selecionado.");
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

            // Barra de Progresso e Botão de Envio
            let uploading = self.is_uploading.load(Ordering::Relaxed);
            let progress = *conn.deploy_progress.lock().unwrap();

            if uploading {
                ui.add(ProgressBar::new(progress).show_percentage().animate(true));
            }

            if let Some(msg) = &*conn.deploy_status.lock().unwrap() {
                ui.colored_label(egui::Color32::from_rgb(100, 200, 255), msg);
            }

            ui.add_space(8.0);

            let can_upload = self.selected_file.is_some() && !uploading;
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
    }

    fn perform_upload(
        file_path: PathBuf,
        target_folder: String,
        is_uploading: Arc<AtomicBool>,
        progress: Arc<std::sync::Mutex<f32>>,
        status: Arc<std::sync::Mutex<Option<String>>>,
    ) {
        use std::io::{BufRead, Write};
        use std::net::TcpStream;

        let filename = file_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("file")
            .to_string();

        *status.lock().unwrap() = Some(format!("Calculando SHA-256 de {}...", filename));

        let mut file = match File::open(&file_path) {
            Ok(f) => f,
            Err(e) => {
                *status.lock().unwrap() = Some(format!("Erro ao abrir arquivo: {}", e));
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
                    *status.lock().unwrap() = Some(format!("Erro lendo arquivo: {}", e));
                    is_uploading.store(false, Ordering::Relaxed);
                    return;
                }
            }
        }

        let sha256 = format!("{:x}", hasher.finalize());
        *status.lock().unwrap() = Some("Conectando ao R36S para transferência direta...".to_string());

        // Conecta direto na porta de controle
        let mut stream = match TcpStream::connect("192.168.7.2:7365") {
            Ok(s) => s,
            Err(e) => {
                *status.lock().unwrap() = Some(format!("Falha ao conectar no R36S (192.168.7.2:7365): {}", e));
                is_uploading.store(false, Ordering::Relaxed);
                return;
            }
        };

        // 1. Envia DeployStart
        let start_cmd = ClientCommand::DeployStart {
            relative_path: target_folder,
            filename: filename.clone(),
            total_size,
            sha256,
        };
        let _ = writeln!(stream, "{}", serde_json::to_string(&start_cmd).unwrap());
        let _ = stream.flush();

        // Aguarda resposta de start
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut resp = String::new();
        let _ = reader.read_line(&mut resp);

        // 2. Reabre e envia chunks
        let mut file = File::open(&file_path).unwrap();
        let mut sent_bytes: u64 = 0;
        let mut chunk_idx: u32 = 0;

        while let Ok(n) = file.read(&mut buf) {
            if n == 0 {
                break;
            }

            let b64 = base64_encode(&buf[..n]);
            let chunk_cmd = ClientCommand::DeployChunk {
                chunk_index: chunk_idx,
                data_base64: b64,
            };
            let _ = writeln!(stream, "{}", serde_json::to_string(&chunk_cmd).unwrap());
            let _ = stream.flush();

            resp.clear();
            let _ = reader.read_line(&mut resp);

            sent_bytes += n as u64;
            chunk_idx += 1;

            let p = if total_size > 0 {
                sent_bytes as f32 / total_size as f32
            } else {
                1.0
            };
            *progress.lock().unwrap() = p;
            *status.lock().unwrap() = Some(format!("Enviando: {:.1} MB / {:.1} MB ({:.0}%)", sent_bytes as f32 / 1_000_000.0, total_size as f32 / 1_000_000.0, p * 100.0));
        }

        // 3. Envia DeployFinish
        let finish_cmd = ClientCommand::DeployFinish;
        let _ = writeln!(stream, "{}", serde_json::to_string(&finish_cmd).unwrap());
        let _ = stream.flush();

        resp.clear();
        let _ = reader.read_line(&mut resp);

        *status.lock().unwrap() = Some(format!("Deploy de {} concluído com sucesso no R36S!", filename));
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
