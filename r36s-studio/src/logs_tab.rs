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
            selected_log_type: "retroarch".to_string(),
            filter_query: String::new(),
            auto_refresh: true,
            last_refresh: Instant::now(),
            cached_content: "Clique em 'Atualizar Logs' ou ative o auto-refresh...".to_string(),
        }
    }

    pub fn render(&mut self, ui: &mut Ui, conn: &ConnectionManager) {
        ui.vertical(|ui| {
            // Controles superiores
            ui.horizontal(|ui| {
                ui.label("Tipo de Log:");
                egui::ComboBox::from_label("")
                    .selected_text(&self.selected_log_type)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.selected_log_type, "retroarch".to_string(), "RetroArch (retroarch.log)");
                        ui.selectable_value(&mut self.selected_log_type, "java".to_string(), "Java / FreeJ2ME (java.log)");
                        ui.selectable_value(&mut self.selected_log_type, "dmesg".to_string(), "Kernel (dmesg)");
                        ui.selectable_value(&mut self.selected_log_type, "agent".to_string(), "DevBridge Daemon (journal)");
                    });

                ui.separator();

                if ui.button("🔄 Atualizar Logs").clicked() {
                    self.request_logs(conn);
                }

                ui.checkbox(&mut self.auto_refresh, "Auto-refresh (2s)");

                ui.separator();

                ui.label("Filtrar:");
                ui.text_edit_singleline(&mut self.filter_query);

                if ui.button("📋 Copiar").clicked() {
                    ui.output_mut(|o| o.copied_text = self.cached_content.clone());
                }
            });

            // Atualização periódica se ativo
            if self.auto_refresh && self.last_refresh.elapsed().as_secs() >= 2 {
                self.last_refresh = Instant::now();
                self.request_logs(conn);
            }

            // Verifica se chegou resposta de logs
            if let Some((log_type, content)) = &*conn.last_logs.lock().unwrap() {
                if log_type == &self.selected_log_type {
                    self.cached_content = content.clone();
                }
            }

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
