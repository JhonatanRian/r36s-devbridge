use crate::connection::ConnectionStatus;
use crate::protocol::SystemMetrics;
use egui::{Color32, ProgressBar, RichText, Ui};
use std::collections::VecDeque;

pub struct TelemetryDashboard {
    cpu_history: VecDeque<f32>,
    max_history: usize,
}

impl TelemetryDashboard {
    pub fn new() -> Self {
        Self {
            cpu_history: VecDeque::with_capacity(60),
            max_history: 60,
        }
    }

    pub fn update_metrics(&mut self, metrics_opt: Option<&SystemMetrics>) {
        if let Some(metrics) = metrics_opt {
            self.cpu_history.push_back(metrics.cpu_usage);
            if self.cpu_history.len() > self.max_history {
                self.cpu_history.pop_front();
            }
        }
    }

    pub fn render(
        &self,
        ui: &mut Ui,
        status: &ConnectionStatus,
        metrics_opt: Option<&SystemMetrics>,
    ) {
        ui.vertical(|ui| {
            // Cartão de Conexão
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    match status {
                        ConnectionStatus::Searching { usb_detected, message } => {
                            let icon = if *usb_detected { "🟡" } else { "🔴" };
                            ui.label(RichText::new(format!("{} Status USB", icon)).strong());
                            ui.label(RichText::new(message).small());
                        }
                        ConnectionStatus::Connecting { target } => {
                            ui.label(RichText::new("🟡 Conectando").strong());
                            ui.label(RichText::new(target).small());
                        }
                        ConnectionStatus::Connected { target, latency_ms } => {
                            ui.label(RichText::new("🟢 R36S Conectado").color(Color32::from_rgb(0, 220, 100)).strong());
                            ui.label(RichText::new(format!("{} ({:.1} ms)", target, latency_ms)).small());
                        }
                    }
                });
            });

            ui.add_space(8.0);

            // Jogo / Emulador Ativo
            if let Some(metrics) = metrics_opt {
                ui.group(|ui| {
                    ui.label(RichText::new("🎮 Aplicação em Execução").strong());
                    let game_text = metrics
                        .active_game
                        .as_deref()
                        .unwrap_or("EmulationStation / Nenhum jogo ativo");
                    ui.colored_label(Color32::from_rgb(100, 200, 255), game_text);
                });

                ui.add_space(8.0);

                // CPU
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⚡ Processador (RK3326 Cortex-A35)").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(format!("{:.1}%", metrics.cpu_usage));
                        });
                    });

                    ui.add(
                        ProgressBar::new(metrics.cpu_usage / 100.0)
                            .animate(true)
                            .fill(if metrics.cpu_usage > 85.0 {
                                Color32::from_rgb(240, 80, 80)
                            } else {
                                Color32::from_rgb(60, 160, 240)
                            }),
                    );

                    // Frequências dos 4 Cores
                    ui.horizontal_wrapped(|ui| {
                        for (i, freq) in metrics.cpu_freq_mhz.iter().enumerate() {
                            ui.label(RichText::new(format!("Core {}: {} MHz", i, freq)).small());
                            if i < metrics.cpu_freq_mhz.len() - 1 {
                                ui.separator();
                            }
                        }
                    });

                    // Temperatura
                    ui.horizontal(|ui| {
                        ui.label("Temperatura:");
                        let temp = metrics.cpu_temp_c;
                        let color = if temp > 75.0 {
                            Color32::from_rgb(255, 60, 60)
                        } else if temp > 60.0 {
                            Color32::from_rgb(255, 180, 0)
                        } else {
                            Color32::from_rgb(80, 220, 100)
                        };
                        ui.colored_label(color, format!("{:.1} °C", temp));
                    });
                });

                ui.add_space(8.0);

                // Memória RAM
                ui.group(|ui| {
                    let ram_pct = (metrics.ram_used_mb as f32 / metrics.ram_total_mb as f32).clamp(0.0, 1.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("💾 Memória RAM").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(format!("{} / {} MB ({:.0}%)", metrics.ram_used_mb, metrics.ram_total_mb, ram_pct * 100.0));
                        });
                    });

                    ui.add(
                        ProgressBar::new(ram_pct)
                            .animate(false)
                            .fill(Color32::from_rgb(180, 100, 240)),
                    );
                });

                ui.add_space(8.0);

                // GPU Mali-G31
                ui.group(|ui| {
                    ui.label(RichText::new("🎨 Gráfico Integrado (Mali-G31 MP2)").strong());
                    ui.horizontal(|ui| {
                        if let Some(f) = metrics.mali_gpu_freq_mhz {
                            ui.label(format!("Freq: {} MHz", f));
                        } else {
                            ui.label("Freq: 520 MHz (ArkOS Default)");
                        }
                        if let Some(l) = metrics.mali_gpu_load {
                            ui.separator();
                            ui.label(format!("Carga: {:.1}%", l));
                        }
                    });
                });
            } else {
                ui.group(|ui| {
                    ui.label("Aguardando dados de telemetria do R36S...");
                    ui.label(RichText::new("Conecte o portátil via cabo USB OTG com o Port ativado.").small());
                });
            }
        });
    }
}
