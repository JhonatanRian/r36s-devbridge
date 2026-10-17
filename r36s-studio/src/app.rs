use crate::connection::{ConnectionManager, ConnectionStatus};
use crate::deploy_tab::DeployTab;
use crate::gamepad_panel::GamepadPanel;
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
    gamepad_panel: GamepadPanel,
    deploy_tab: DeployTab,
    logs_tab: LogsTab,
    process_tab: ProcessTab,
    current_tab: ActiveTab,
    show_telemetry_panel: bool,
}

impl DevBridgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let connection = ConnectionManager::new();
        connection.set_egui_ctx(cc.egui_ctx.clone());

        Self {
            connection,
            viewport: ViewportRenderer::new(),
            telemetry: TelemetryDashboard::new(),
            gamepad_panel: GamepadPanel::new(),
            deploy_tab: DeployTab::new(),
            logs_tab: LogsTab::new(),
            process_tab: ProcessTab::new(),
            current_tab: ActiveTab::ViewportAndTelemetry,
            show_telemetry_panel: true,
        }
    }
}

impl eframe::App for DevBridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Arquitetura orientada a eventos: o streaming de vídeo solicita repintura imediata
        // via `ctx.request_repaint()` assim que um novo quadro é decodificado no `video_worker`.
        // Mantemos cadência rápida para gamepad interativo e fallback conservador para telemetria/watchdog.
        let is_streaming = self.connection.is_streaming.load(Ordering::Relaxed);
        let is_gamepad_listening = self.connection.gamepad_listening.load(Ordering::Relaxed);
        let is_gamepad_input = self.connection.gamepad_input_enabled.load(Ordering::Relaxed);

        if is_gamepad_listening || is_gamepad_input {
            ctx.request_repaint_after(Duration::from_millis(30)); // ~33 FPS para feedback de botões
        } else if is_streaming {
            // Heartbeat de segurança caso o streaming fique ocioso ou perca quadros
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            // Repintura conservadora para atualização periódica de status e métricas
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
        let throughput = *self.connection.throughput_kbps.lock().unwrap();

        // Clone only the immutable frame reference; GPU upload never holds the network lock.
        let frame_snapshot = self.connection.current_frame.lock().unwrap().clone();
        let received = *self.connection.last_video_frame.lock().unwrap();
        self.viewport.set_received(received);
        let frame_available =
            frame_snapshot.is_some() && received.map_or(false, |t| t.elapsed().as_secs_f32() < 1.0);
        if matches!(status_snapshot, ConnectionStatus::Connected { .. }) && is_streaming {
            self.viewport.update_frame(ctx, frame_snapshot.as_deref());
        } else {
            self.viewport.clear();
        }
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
                        let color = if *usb_detected {
                            Color32::from_rgb(255, 200, 0)
                        } else {
                            Color32::from_rgb(255, 80, 80)
                        };
                        let label = if *usb_detected {
                            "🟡 USB Detectado"
                        } else {
                            "🔴 Buscando R36S..."
                        };
                        ui.colored_label(color, RichText::new(label).strong());
                    }
                    ConnectionStatus::Connecting { stage, .. } => {
                        ui.colored_label(
                            Color32::from_rgb(255, 200, 0),
                            RichText::new(format!("🟡 {}", stage)).strong(),
                        );
                    }
                    ConnectionStatus::Connected {
                        target, latency_ms, ..
                    } => {
                        ui.colored_label(
                            Color32::from_rgb(0, 220, 100),
                            RichText::new(format!(
                                "🟢 Conectado em {} ({:.1}ms)",
                                target, latency_ms
                            ))
                            .strong(),
                        );
                    }
                    ConnectionStatus::Error { message } => {
                        ui.colored_label(
                            Color32::from_rgb(255, 80, 80),
                            RichText::new(format!("Erro: {}", message)).strong(),
                        );
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

                if self.current_tab == ActiveTab::ViewportAndTelemetry {
                    let panel_btn_text = if self.show_telemetry_panel {
                        "◀ Ocultar Painel"
                    } else {
                        "▶ Exibir Telemetria"
                    };
                    if ui.button(panel_btn_text).clicked() {
                        self.show_telemetry_panel = !self.show_telemetry_panel;
                    }
                }

                ui.separator();

                // Botões de Abas limpos e diretos (sem right_to_left overlap)
                ui.selectable_value(
                    &mut self.current_tab,
                    ActiveTab::ViewportAndTelemetry,
                    "🖥 Monitor & Tela",
                );
                ui.selectable_value(&mut self.current_tab, ActiveTab::Deploy, "📦 Deploy");
                ui.selectable_value(&mut self.current_tab, ActiveTab::Logs, "📜 Logs");
                ui.selectable_value(&mut self.current_tab, ActiveTab::Processes, "⚙ Processos");
            });
            ui.add_space(4.0);
        });

        // Barra Inferior (Footer)
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("ArkOS R36S Toolkit • RK3326 Cortex-A35 • Mali-G31").small(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(concat!("DevBridge v", env!("CARGO_PKG_VERSION"))).small(),
                    );
                });
            });
        });

        // Coluna Esquerda: Painel lateral nativo de telemetria e controles (100% de altura, largura ajustável)
        if self.current_tab == ActiveTab::ViewportAndTelemetry && self.show_telemetry_panel {
            egui::SidePanel::left("telemetry_side_panel")
                .resizable(true)
                .default_width(320.0)
                .width_range(260.0..=480.0)
                .show(ctx, |ui| {
                    ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            // Métricas do Sistema
                            self.telemetry.render(
                                ui,
                                &status_snapshot,
                                metrics_snapshot.as_ref(),
                                throughput,
                            );

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(8.0);

                            // Controle Interativo do R36S (Escuta ao vivo e injeção de comandos)
                            self.gamepad_panel.render(ui, &self.connection);
                        });
                });
        }

        self.gamepad_panel.guard(
            &self.connection,
            self.current_tab == ActiveTab::ViewportAndTelemetry
                && ctx.input(|i| i.focused)
                && matches!(status_snapshot, ConnectionStatus::Connected { .. }),
        );

        // Painel Central: ocupa todo o espaço restante da tela do notebook
        egui::CentralPanel::default().show(ctx, |ui| match self.current_tab {
            ActiveTab::ViewportAndTelemetry => {
                let mut take_screenshot = false;
                let previous_mode = self.viewport.mode;
                self.viewport
                    .controls(ui, frame_available, &mut take_screenshot);
                if previous_mode != self.viewport.mode {
                    self.gamepad_panel.release(&self.connection);
                }
                if take_screenshot {
                    if let Some(frame) = frame_snapshot.as_ref() {
                        self.viewport.save_screenshot(std::sync::Arc::clone(frame));
                    }
                }

                match self.viewport.mode {
                    crate::viewport::ViewportMode::Handheld => {
                        self.gamepad_panel
                            .console(ui, &self.connection, &self.viewport);
                    }
                    crate::viewport::ViewportMode::Display4x3 => {
                        let connected =
                            matches!(status_snapshot, ConnectionStatus::Connected { .. });
                        self.viewport.paint_display_4x3(
                            ui,
                            connected,
                            is_streaming,
                            self.connection.video_connected.load(Ordering::Relaxed),
                        );
                        self.gamepad_panel.interact_keyboard(ui, &self.connection);
                    }
                }
            }
            ActiveTab::Deploy => {
                self.deploy_tab.render(ui, &self.connection);
            }
            ActiveTab::Logs => {
                self.logs_tab.render(ui, &self.connection);
            }
            ActiveTab::Processes => {
                self.process_tab
                    .render(ui, &self.connection, metrics_snapshot.as_ref());
            }
        });
    }
}
