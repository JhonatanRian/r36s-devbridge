use crate::connection::ConnectionManager;
use crate::protocol::{ClientCommand, SystemMetrics};
use egui::{Color32, RichText, ScrollArea, Ui};

pub struct ProcessTab {
    status_msg: Option<String>,
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
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.heading("⚙ Gerenciador de Processos do R36S");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🔄 Reiniciar EmulationStation").clicked() {
                        conn.send_command(ClientCommand::RestartES);
                        self.status_msg = Some("Comando enviado para reiniciar a interface EmulationStation.".to_string());
                    }
                });
            });

            ui.label("Encerre jogos ou processos que tenham travado no portátil sem precisar desligar o console fisicamente.");

            if let Some(msg) = &self.status_msg {
                ui.colored_label(Color32::from_rgb(100, 200, 255), msg);
            }

            ui.separator();

            if let Some(metrics) = metrics_opt {
                ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    egui::Grid::new("process_grid")
                        .striped(true)
                        .min_col_width(80.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("PID").strong());
                            ui.label(RichText::new("Nome").strong());
                            ui.label(RichText::new("RAM").strong());
                            ui.label(RichText::new("Linha de Comando").strong());
                            ui.label(RichText::new("Ação").strong());
                            ui.end_row();

                            for proc in &metrics.running_processes {
                                ui.label(format!("{}", proc.pid));
                                ui.colored_label(Color32::from_rgb(180, 220, 255), &proc.name);
                                ui.label(format!("{} MB", proc.memory_mb));
                                ui.label(RichText::new(&proc.cmdline).small());

                                ui.horizontal(|ui| {
                                    if ui.button(RichText::new("Encerrar").color(Color32::from_rgb(255, 100, 100))).clicked() {
                                        conn.send_command(ClientCommand::KillProcess {
                                            pid: proc.pid,
                                            signal: 15, // SIGTERM
                                        });
                                        self.status_msg = Some(format!("Enviado SIGTERM para o PID {}", proc.pid));
                                    }
                                    if ui.button(RichText::new("Forçar (Kill)").color(Color32::from_rgb(255, 60, 60))).clicked() {
                                        conn.send_command(ClientCommand::KillProcess {
                                            pid: proc.pid,
                                            signal: 9, // SIGKILL
                                        });
                                        self.status_msg = Some(format!("Enviado SIGKILL para o PID {}", proc.pid));
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
