use crate::connection::ConnectionManager;
use crate::protocol::{ClientCommand, SystemMetrics};
use egui::{Color32, RichText, ScrollArea, Ui};

pub struct ProcessTab {
    status_msg: Option<(bool, String)>,
}

impl ProcessTab {
    pub fn new() -> Self {
        Self { status_msg: None }
    }

    pub fn render(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionManager,
        metrics_opt: Option<&SystemMetrics>,
    ) {
        // Verifica se há resultado recente de comando no connection manager
        if let Some(res) = conn.last_process_result.lock().unwrap().take() {
            self.status_msg = Some(res);
        }

        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("⚙ Gerenciador de Processos do R36S");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🔄 Reiniciar EmulationStation").clicked() {
                        conn.send_command(ClientCommand::RestartES);
                        self.status_msg = Some((true, "Comando de reinício da EmulationStation enviado...".to_string()));
                    }
                });
            });

            ui.label("Monitore consumo de CPU e RAM por processo e encerre jogos ou emuladores travados.");

            if let Some((success, ref msg)) = self.status_msg {
                let color = if success {
                    Color32::from_rgb(100, 255, 100)
                } else {
                    Color32::from_rgb(255, 80, 80)
                };
                ui.colored_label(color, RichText::new(msg).strong());
            }

            ui.separator();

            if let Some(metrics) = metrics_opt {
                ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    egui::Grid::new("process_grid")
                        .striped(true)
                        .min_col_width(70.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("PID").strong());
                            ui.label(RichText::new("Nome").strong());
                            ui.label(RichText::new("CPU").strong());
                            ui.label(RichText::new("RAM").strong());
                            ui.label(RichText::new("Linha de Comando").strong());
                            ui.label(RichText::new("Ação").strong());
                            ui.end_row();

                            for proc in &metrics.running_processes {
                                ui.label(format!("{}", proc.pid));
                                ui.colored_label(Color32::from_rgb(180, 220, 255), &proc.name);

                                let cpu_color = if proc.cpu_percent > 50.0 {
                                    Color32::from_rgb(255, 100, 100)
                                } else if proc.cpu_percent > 10.0 {
                                    Color32::from_rgb(255, 200, 80)
                                } else {
                                    Color32::from_rgb(120, 220, 120)
                                };
                                ui.colored_label(cpu_color, format!("{:.1}%", proc.cpu_percent));

                                ui.label(format!("{} MB", proc.memory_mb));

                                let short_cmd = if proc.cmdline.len() > 50 {
                                    format!("{}...", &proc.cmdline[0..47])
                                } else {
                                    proc.cmdline.clone()
                                };
                                ui.label(RichText::new(short_cmd).small()).on_hover_text(&proc.cmdline);

                                ui.horizontal(|ui| {
                                    if ui.button(RichText::new("Term").color(Color32::from_rgb(255, 120, 120))).clicked() {
                                        conn.send_command(ClientCommand::KillProcess {
                                            pid: proc.pid,
                                            signal: 15,
                                        });
                                        self.status_msg = Some((true, format!("Enviado SIGTERM para PID {}", proc.pid)));
                                    }
                                    if ui.button(RichText::new("Kill").color(Color32::from_rgb(255, 60, 60))).clicked() {
                                        conn.send_command(ClientCommand::KillProcess {
                                            pid: proc.pid,
                                            signal: 9,
                                        });
                                        self.status_msg = Some((true, format!("Enviado SIGKILL para PID {}", proc.pid)));
                                    }
                                });
                                ui.end_row();
                            }
                        });
                });
            } else {
                ui.label("Aguardando lista de processos do R36S...");
            }
        });
    }
}
