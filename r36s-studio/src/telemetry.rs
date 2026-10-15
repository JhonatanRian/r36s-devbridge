use crate::connection::ConnectionStatus;
use crate::protocol::SystemMetrics;
use egui::{Color32, Pos2, ProgressBar, RichText, Stroke, Ui, Vec2};
use std::collections::VecDeque;

pub struct TelemetryDashboard {
    cpu_history: VecDeque<f32>,
    max_history: usize,
    last_metric_ts: u64,
}

impl TelemetryDashboard {
    pub fn new() -> Self {
        Self {
            cpu_history: VecDeque::with_capacity(60),
            max_history: 60,
            last_metric_ts: 0,
        }
    }

    pub fn update_metrics(&mut self, metrics_opt: Option<&SystemMetrics>) {
        if let Some(metrics) = metrics_opt {
            if metrics.timestamp != self.last_metric_ts {
                self.last_metric_ts = metrics.timestamp;
                self.cpu_history.push_back(metrics.cpu_usage);
                if self.cpu_history.len() > self.max_history {
                    self.cpu_history.pop_front();
                }
            }
        }
    }

    pub fn render(
        &self,
        ui: &mut Ui,
        status: &ConnectionStatus,
        metrics_opt: Option<&SystemMetrics>,
        throughput_kbps: f32,
    ) {
        ui.vertical(|ui| {
            // Cartão de Conexão & Throughput
            ui.group(|ui| {
                match status {
                    ConnectionStatus::Searching { usb_detected, message } => {
                        let icon = if *usb_detected { "🟡" } else { "🔴" };
                        ui.label(RichText::new(format!("{} Status da Conexão", icon)).strong());
                        ui.label(RichText::new(message).small());
                    }
                    ConnectionStatus::Connecting { target, stage } => {
                        ui.label(RichText::new("🟡 Conectando ao R36S...").strong());
                        ui.label(RichText::new(format!("{}: {}", target, stage)).small());
                    }
                    ConnectionStatus::Connected { target, latency_ms, agent_version } => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("🟢 R36S Conectado").color(Color32::from_rgb(0, 220, 100)).strong());
                            ui.label(RichText::new(format!("v{}", agent_version)).small());
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} ({:.1} ms)", target, latency_ms)).small());
                            ui.separator();
                            ui.label(RichText::new(format!("{:.1} KB/s", throughput_kbps)).small().color(Color32::from_rgb(120, 200, 255)));
                        });
                    }
                    ConnectionStatus::Error { message } => {
                        ui.colored_label(Color32::from_rgb(255, 80, 80), RichText::new(format!("Erro: {}", message)).strong());
                    }
                }
            });

            ui.add_space(6.0);

            if let Some(metrics) = metrics_opt {
                // Jogo / Emulador Ativo
                ui.group(|ui| {
                    ui.label(RichText::new("🎮 Aplicação em Execução").strong());
                    let game_text = metrics
                        .active_game
                        .as_deref()
                        .unwrap_or("EmulationStation / Nenhum jogo ativo");
                    ui.colored_label(Color32::from_rgb(100, 200, 255), game_text);
                });

                ui.add_space(6.0);

                // Processador CPU & Gráfico de Linha
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⚡ CPU (RK3326 Cortex-A35)").strong());
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

                    // Gráfico de Linha ao Vivo (Sparkline 60 segundos)
                    self.render_cpu_graph(ui);

                    // Frequências dos Cores
                    if !metrics.cpu_freq_mhz.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            for (i, freq) in metrics.cpu_freq_mhz.iter().enumerate() {
                                ui.label(RichText::new(format!("C{}: {}MHz", i, freq)).small());
                                if i < metrics.cpu_freq_mhz.len() - 1 {
                                    ui.label("|");
                                }
                            }
                        });
                    } else {
                        ui.label(RichText::new("Frequências: Indisponíveis").small().color(Color32::GRAY));
                    }

                    // Temperatura Real
                    ui.horizontal(|ui| {
                        ui.label("Temperatura:");
                        if let Some(temp) = metrics.cpu_temp_c {
                            let color = if temp > 75.0 {
                                Color32::from_rgb(255, 60, 60)
                            } else if temp > 60.0 {
                                Color32::from_rgb(255, 180, 0)
                            } else {
                                Color32::from_rgb(80, 220, 100)
                            };
                            ui.colored_label(color, format!("{:.1} °C", temp));
                        } else {
                            ui.colored_label(Color32::GRAY, "Indisponível");
                        }
                    });
                });

                ui.add_space(6.0);

                // Memória RAM
                ui.group(|ui| {
                    let ram_pct = if metrics.ram_total_mb > 0 {
                        (metrics.ram_used_mb as f32 / metrics.ram_total_mb as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("💾 Memória RAM").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(format!("{} / {} MB", metrics.ram_used_mb, metrics.ram_total_mb));
                        });
                    });

                    ui.add(
                        ProgressBar::new(ram_pct)
                            .animate(false)
                            .fill(Color32::from_rgb(180, 100, 240)),
                    );
                });

                ui.add_space(6.0);

                // GPU Mali-G31
                ui.group(|ui| {
                    ui.label(RichText::new("🎨 Gráfico (Mali-G31 MP2)").strong());
                    ui.horizontal(|ui| {
                        if let Some(f) = metrics.mali_gpu_freq_mhz {
                            ui.label(format!("Freq: {} MHz", f));
                        } else {
                            ui.label(RichText::new("Freq: Indisponível").color(Color32::GRAY));
                        }
                        if let Some(l) = metrics.mali_gpu_load {
                            ui.separator();
                            ui.label(format!("Carga: {:.1}%", l));
                        }
                    });
                });

                ui.add_space(6.0);

                // Detalhes do Framebuffer detectado
                ui.group(|ui| {
                    ui.label(RichText::new("📺 Display & Framebuffer").strong());
                    ui.label(
                        RichText::new(format!(
                            "Resolução: {}x{} @ {} bpp (fmt {})",
                            metrics.fb_width, metrics.fb_height, metrics.fb_bpp, metrics.fb_format
                        ))
                        .small(),
                    );
                });
            } else {
                ui.group(|ui| {
                    ui.label("Aguardando dados de telemetria do R36S...");
                    ui.label(RichText::new("Plugue o cabo USB OTG com o Port DevBridge ativado.").small());
                });
            }
        });
    }

    fn render_cpu_graph(&self, ui: &mut Ui) {
        let (rect, _response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(15, 18, 22));

        if self.cpu_history.len() < 2 {
            return;
        }

        let step_x = rect.width() / (self.max_history as f32);
        let mut points = Vec::with_capacity(self.cpu_history.len());

        let start_x = rect.right() - (self.cpu_history.len() as f32) * step_x;
        for (i, &usage) in self.cpu_history.iter().enumerate() {
            let x = start_x + (i as f32) * step_x;
            let norm_y = (usage / 100.0).clamp(0.0, 1.0);
            let y = rect.bottom() - norm_y * rect.height();
            points.push(Pos2::new(x, y));
        }

        for window in points.windows(2) {
            ui.painter().line_segment(
                [window[0], window[1]],
                Stroke::new(1.5_f32, Color32::from_rgb(80, 180, 255)),
            );
        }
    }
}
