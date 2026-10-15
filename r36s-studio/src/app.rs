use crate::connection::{ConnectionManager, ConnectionStatus};
use crate::deploy_tab::DeployTab;
use crate::logs_tab::LogsTab;
use crate::process_tab::ProcessTab;
use crate::telemetry::TelemetryDashboard;
use crate::viewport::ViewportRenderer;
use egui::{Color32, RichText};
use std::sync::atomic::Ordering;

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
        // Solicita repintura contínua se o stream estiver ativo para 30/60 fps fluidos
        let is_streaming = self.connection.is_streaming.load(Ordering::Relaxed);
        if is_streaming {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Atualiza frames e telemetria
        let frame_lock = self.connection.current_frame.lock().unwrap();
        self.viewport.update_frame(ctx, frame_lock.as_ref());

        let metrics_lock = self.connection.metrics.lock().unwrap();
        self.telemetry.update_metrics(metrics_lock.as_ref());

        let status_lock = self.connection.status.lock().unwrap();

        // Barra Superior (Header & Navegação)
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("🕹 R36S DevBridge Studio");

                ui.separator();

                // Status Pill
                match &*status_lock {
                    ConnectionStatus::Searching { usb_detected, .. } => {
                        let color = if *usb_detected { Color32::from_rgb(255, 200, 0) } else { Color32::from_rgb(255, 80, 80) };
                        let label = if *usb_detected { "🟡 USB Detectado (Conectando...)" } else { "🔴 Buscando R36S..." };
                        ui.colored_label(color, RichText::new(label).strong());
                    }
                    ConnectionStatus::Connecting { .. } => {
                        ui.colored_label(Color32::from_rgb(255, 200, 0), RichText::new("🟡 Conectando...").strong());
                    }
                    ConnectionStatus::Connected { target, latency_ms } => {
                        ui.colored_label(
                            Color32::from_rgb(0, 220, 100),
                            RichText::new(format!("🟢 Conectado em {} ({:.1}ms)", target, latency_ms)).strong(),
                        );
                    }
                }

                ui.separator();

                // Botão de Streaming
                let stream_btn_text = if is_streaming {
                    "⏸ Pausar Stream"
                } else {
                    "▶ Iniciar Stream (640x480)"
                };
                if ui.button(RichText::new(stream_btn_text).strong()).clicked() {
                    self.connection.toggle_streaming();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.current_tab, ActiveTab::Processes, "⚙ Processos");
                    ui.selectable_value(&mut self.current_tab, ActiveTab::Logs, "📜 Logs");
                    ui.selectable_value(&mut self.current_tab, ActiveTab::Deploy, "📦 Deploy de Jogos");
                    ui.selectable_value(&mut self.current_tab, ActiveTab::ViewportAndTelemetry, "🖥 Monitor & Viewport");
                });
            });
            ui.add_space(4.0);
        });

        // Barra Inferior (Footer)
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ArkOS R36S Toolkit • RK3326 Cortex-A35 • Mali-G31").small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("DevBridge v0.1.0 • Pronto para desenvolvimento").small());
                });
            });
        });

        // Painel Central Principal
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.current_tab {
                ActiveTab::ViewportAndTelemetry => {
                    ui.columns(2, |columns| {
                        // Coluna Esquerda: Telemetria e Hardware
                        columns[0].set_max_width(380.0);
                        self.telemetry.render(&mut columns[0], &status_lock, metrics_lock.as_ref());

                        // Coluna Direita: Viewport 640x480 da tela
                        self.viewport.render(&mut columns[1], is_streaming, frame_lock.as_ref());
                    });
                }
                ActiveTab::Deploy => {
                    self.deploy_tab.render(ui, &self.connection);
                }
                ActiveTab::Logs => {
                    self.logs_tab.render(ui, &self.connection);
                }
                ActiveTab::Processes => {
                    self.process_tab.render(ui, &self.connection, metrics_lock.as_ref());
                }
            }
        });
    }
}
