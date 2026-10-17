use crate::connection::ConnectionManager;
use crate::protocol::ClientCommand;
use egui::{ScrollArea, Ui};
use std::time::Instant;

pub struct LogsTab {
    selected_log_type: String,
    filter_query: String,
    auto_refresh: bool,
    last_refresh: Instant,
    cached_content: String,
}

impl LogsTab {
    pub fn new() -> Self {
        Self {
            selected_log_type: "emulationstation".to_string(),
            filter_query: String::new(),
            auto_refresh: true,
            last_refresh: Instant::now(),
            cached_content: "Clique em 'Atualizar Logs' ou aguarde a primeira sincronização..."
                .to_string(),
        }
    }

    pub fn render(&mut self, ui: &mut Ui, conn: &ConnectionManager) {
        ui.vertical(|ui| {
            // Controles superiores em linha quebrável
            let prev_source = self.selected_log_type.clone();
            ui.horizontal_wrapped(|ui| {
                ui.label("Fonte:");
                egui::ComboBox::from_label("")
                    .selected_text(&self.selected_log_type)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "active_game".to_string(),
                            "🎮 Jogo / Port Ativo (dinâmico)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "run".to_string(),
                            "🚀 Execução / Runcommand (run.log)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "emulationstation".to_string(),
                            "🕹 EmulationStation (es_log.txt)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "retroarch".to_string(),
                            "👾 RetroArch (retroarch.log)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "agent".to_string(),
                            "📡 DevBridge Daemon (r36s-agent.log)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "dmesg".to_string(),
                            "🐧 Kernel (dmesg)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "syslog".to_string(),
                            "📋 Sistema (syslog / messages)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "ppsspp".to_string(),
                            "PPSSPP (PSP)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "drastic".to_string(),
                            "DraStic (NDS)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "mupen64plus".to_string(),
                            "Mupen64Plus (N64)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "port".to_string(),
                            "Port / Instalador (install.log)",
                        );
                        ui.selectable_value(
                            &mut self.selected_log_type,
                            "java".to_string(),
                            "Java / FreeJ2ME (java.log)",
                        );
                    });

                if self.selected_log_type != prev_source {
                    self.cached_content =
                        format!("Carregando logs de {}...", self.selected_log_type);
                    *conn.last_logs.lock().unwrap() = None;
                    self.request_logs(conn);
                }

                ui.separator();

                if ui.button("🔄 Atualizar").clicked() {
                    self.request_logs(conn);
                }

                if ui.button("🧹 Limpar").clicked() {
                    self.cached_content.clear();
                    *conn.last_logs.lock().unwrap() = None;
                }

                ui.checkbox(&mut self.auto_refresh, "Auto (2s)");

                ui.separator();

                ui.label("Filtrar:");
                ui.text_edit_singleline(&mut self.filter_query);

                if ui.button("📋 Copiar").clicked() {
                    ui.output_mut(|o| o.copied_text = self.cached_content.clone());
                }
            });

            // Atualização periódica se auto-refresh ativo
            if self.auto_refresh && self.last_refresh.elapsed().as_secs() >= 2 {
                self.last_refresh = Instant::now();
                self.request_logs(conn);
            }

            // Consome resposta de logs do agente uma única vez para não reescrever após limpar
            let mut logs_guard = conn.last_logs.lock().unwrap();
            if let Some((log_type, content)) = logs_guard.take() {
                if log_type == self.selected_log_type {
                    self.cached_content = content;
                } else {
                    // Restaura se for de outro tipo
                    *logs_guard = Some((log_type, content));
                }
            }
            drop(logs_guard);

            ui.separator();

            // Área de exibição dos logs com fonte monoespaçada
            ScrollArea::vertical()
                .auto_shrink([false; 2])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if self.filter_query.is_empty() {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.cached_content.as_str())
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    } else {
                        let query = self.filter_query.to_lowercase();
                        let filtered_text: String = self
                            .cached_content
                            .lines()
                            .filter(|l| l.to_lowercase().contains(&query))
                            .collect::<Vec<&str>>()
                            .join("\n");

                        ui.add(
                            egui::TextEdit::multiline(&mut filtered_text.as_str())
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    }
                });
        });
    }

    fn request_logs(&self, conn: &ConnectionManager) {
        conn.send_command(ClientCommand::FetchLogs {
            log_type: self.selected_log_type.clone(),
            lines: 120,
        });
    }
}
