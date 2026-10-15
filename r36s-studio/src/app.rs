use crate::connection::{ConnectionManager, ConnectionStatus};
use crate::deploy_tab::DeployTab;
use crate::logs_tab::LogsTab;
use crate::process_tab::ProcessTab;
use crate::telemetry::TelemetryDashboard;
use crate::viewport::ViewportRenderer;
use egui::{Color32, RichText, ScrollArea};
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(PartialEq)]
pub enum ActiveTab {
    ViewportAndTelemetry,
    Deploy,
    Logs,
    Processes,
}

pub struct DevBridgeApp {
    connection: ConnectionManager,
    viewport: ViewportRenderer,
    telemetry: TelemetryDashboard,
    deploy_tab: DeployTab,
    logs_tab: LogsTab,
    process_tab: ProcessTab,
    current_tab: ActiveTab,
}

impl DevBridgeApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            connection: ConnectionManager::new(),
            viewport: ViewportRenderer::new(),
            telemetry: TelemetryDashboard::new(),
            deploy_tab: DeployTab::new(),
            logs_tab: LogsTab::new(),
            process_tab: ProcessTab::new(),
            current_tab: ActiveTab::ViewportAndTelemetry,
        }
    }
}

impl eframe::App for DevBridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Taxa de repintura controlada para fluidez e baixo consumo de CPU
        let is_streaming = self.connection.is_streaming.load(Ordering::Relaxed);
        if is_streaming {
            ctx.request_repaint_after(Duration::from_millis(30)); // ~33 FPS
        } else {
            ctx.request_repaint_after(Duration::from_millis(150));
        }

        // Drag and drop global de arquivos para a aba de Deploy
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(first_file) = dropped_files.first() {
            if let Some(path) = &first_file.path {
                self.deploy_tab.set_dropped_file(path.clone());
                self.current_tab = ActiveTab::Deploy;
            }
        }

        // Instantâneos rápidos dos Mutexes para liberar threads de rede imediatamente
        let status_snapshot = self.connection.status.lock().unwrap().clone();
        let metrics_snapshot = self.connection.metrics.lock().unwrap().clone();
        let frame_snapshot = self.connection.current_frame.lock().unwrap().clone();
        let current_fps = *self.connection.current_fps.lock().unwrap();
        let throughput = *self.connection.throughput_kbps.lock().unwrap();

        // Atualiza textura do viewport e histórico da telemetria
        self.viewport.update_frame(ctx, frame_snapshot.as_ref());
        self.telemetry.update_metrics(metrics_snapshot.as_ref());

        // Barra Superior (Header & Navegação Responsiva)
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.heading("🕹 R36S DevBridge");

                ui.separator();

                // Status Badge
                match &status_snapshot {
                    ConnectionStatus::Searching { usb_detected, .. } => {
                        let color = if *usb_detected { Color32::from_rgb(255, 200, 0) } else { Color32::from_rgb(255, 80, 80) };
                        let label = if *usb_detected { "🟡 USB Detectado" } else { "🔴 Buscando R36S..." };
                        ui.colored_label(color, RichText::new(label).strong());
                    }
                    ConnectionStatus::Connecting { stage, .. } => {
                        ui.colored_label(Color32::from_rgb(255, 200, 0), RichText::new(format!("🟡 {}", stage)).strong());
                    }
                    ConnectionStatus::Connected { target, latency_ms, .. } => {
                        ui.colored_label(
                            Color32::from_rgb(0, 220, 100),
                            RichText::new(format!("🟢 Conectado em {} ({:.1}ms)", target, latency_ms)).strong(),
                        );
                    }
                    ConnectionStatus::Error { message } => {
                        ui.colored_label(Color32::from_rgb(255, 80, 80), RichText::new(format!("Erro: {}", message)).strong());
                    }
                }

                ui.separator();

                // Botão de Streaming
                let stream_btn_text = if is_streaming {
                    "⏸ Pausar Stream"
                } else {
                    "▶ Iniciar Stream"
                };
                if ui.button(RichText::new(stream_btn_text).strong()).clicked() {
                    self.connection.toggle_streaming();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.current_tab, ActiveTab::Processes, "⚙ Processos");
                    ui.selectable_value(&mut self.current_tab, ActiveTab::Logs, "📜 Logs");
                    ui.selectable_value(&mut self.current_tab, ActiveTab::Deploy, "📦 Deploy");
                    ui.selectable_value(&mut self.current_tab, ActiveTab::ViewportAndTelemetry, "🖥 Monitor & Tela");
                });
            });
            ui.add_space(4.0);
        });

        // Barra Inferior (Footer)
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ArkOS R36S Toolkit • RK3326 Cortex-A35 • Mali-G31 • 640x480").small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("DevBridge v0.2.0 • Protocolo Conectado").small());
                });
            });
        });

        // Coluna Esquerda: Painel lateral nativo de telemetria (100% de altura, largura ajustável)
        if self.current_tab == ActiveTab::ViewportAndTelemetry {
            egui::SidePanel::left("telemetry_side_panel")
                .resizable(true)
                .default_width(330.0)
                .width_range(280.0..=480.0)
                .show(ctx, |ui| {
                    ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            self.telemetry.render(
                                ui,
                                &status_snapshot,
                                metrics_snapshot.as_ref(),
                                throughput,
                            );
                        });
                });
        }

        // Painel Central: ocupa todo o espaço restante da tela do notebook
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.current_tab {
                ActiveTab::ViewportAndTelemetry => {
                    self.viewport.render(
                        ui,
                        is_streaming,
                        current_fps,
                        frame_snapshot.as_ref(),
                    );
                }
                ActiveTab::Deploy => {
                    self.deploy_tab.render(ui, &self.connection);
                }
                ActiveTab::Logs => {
                    self.logs_tab.render(ui, &self.connection);
                }
                ActiveTab::Processes => {
                    self.process_tab.render(ui, &self.connection, metrics_snapshot.as_ref());
                }
            }
        });
    }
}
