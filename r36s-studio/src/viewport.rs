use crate::connection::DecodedFrame;
use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions, Ui, Vec2};
use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum ViewportScale {
    Fit,       // Ajusta dinamicamente preservando 4:3
    Scale1x,   // 640x480
    Scale1_5x, // 960x720
    Scale2x,   // 1280x960
}

pub struct ViewportRenderer {
    texture: Option<TextureHandle>,
    last_frame_index: u32,
    scale_mode: ViewportScale,
    nearest_filter: bool,
    screenshot_notice: Option<(String, Instant)>,
}

impl ViewportRenderer {
    pub fn new() -> Self {
        Self {
            texture: None,
            last_frame_index: 0,
            scale_mode: ViewportScale::Fit,
            nearest_filter: true,
            screenshot_notice: None,
        }
    }

    pub fn update_frame(&mut self, ctx: &Context, frame_opt: Option<&DecodedFrame>) {
        if let Some(frame) = frame_opt {
            if frame.frame_index != self.last_frame_index {
                self.last_frame_index = frame.frame_index;

                let color_image = ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba_data,
                );

                let filter = if self.nearest_filter {
                    TextureOptions::NEAREST
                } else {
                    TextureOptions::LINEAR
                };

                match &mut self.texture {
                    Some(tex) => tex.set(color_image, filter),
                    None => {
                        self.texture = Some(ctx.load_texture("r36s_screen", color_image, filter));
                    }
                }
            }
        }
    }

    pub fn render(
        &mut self,
        ui: &mut Ui,
        is_streaming: bool,
        current_fps: f32,
        frame_opt: Option<&DecodedFrame>,
    ) {
        ui.vertical(|ui| {
            // Barra de controles do Viewport responsiva
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("🖥 Viewport 4:3").strong());

                ui.separator();

                ui.radio_value(&mut self.scale_mode, ViewportScale::Fit, "Ajustar (4:3)");
                ui.radio_value(&mut self.scale_mode, ViewportScale::Scale1x, "1x (640x480)");
                ui.radio_value(&mut self.scale_mode, ViewportScale::Scale1_5x, "1.5x (960x720)");
                ui.radio_value(&mut self.scale_mode, ViewportScale::Scale2x, "2x (1280x960)");

                ui.separator();

                ui.checkbox(&mut self.nearest_filter, "Nearest (Pixel-Art)");

                ui.separator();

                if is_streaming {
                    let fps_color = if current_fps >= 25.0 {
                        Color32::from_rgb(0, 220, 100)
                    } else if current_fps >= 10.0 {
                        Color32::from_rgb(255, 180, 0)
                    } else {
                        Color32::from_rgb(255, 80, 80)
                    };
                    ui.colored_label(fps_color, format!("● {:.0} FPS", current_fps));
                } else {
                    ui.colored_label(Color32::GRAY, "○ Pausado");
                }

                if let Some(frame) = frame_opt {
                    if ui.button("📷 Screenshot").clicked() {
                        self.save_screenshot(frame);
                    }
                }
            });

            if let Some((msg, time)) = &self.screenshot_notice {
                if time.elapsed().as_secs() < 4 {
                    ui.colored_label(Color32::from_rgb(100, 255, 100), msg);
                }
            }

            ui.separator();

            // Área de exibição do Framebuffer com ajuste 4:3 ou scroll em escalas fixas
            let available = ui.available_size();
            let (target_w, target_h) = match self.scale_mode {
                ViewportScale::Scale1x => (640.0, 480.0),
                ViewportScale::Scale1_5x => (960.0, 720.0),
                ViewportScale::Scale2x => (1280.0, 960.0),
                ViewportScale::Fit => {
                    let aspect = 640.0 / 480.0;
                    let avail_w = available.x.max(160.0);
                    let avail_h = available.y.max(120.0);
                    let mut w = avail_w;
                    let mut h = w / aspect;
                    if h > avail_h {
                        h = avail_h;
                        w = h * aspect;
                    }
                    (w, h)
                }
            };

            let draw_viewport = |ui: &mut Ui| {
                let (rect, _response) = ui.allocate_exact_size(Vec2::new(target_w, target_h), egui::Sense::hover());

                if let Some(tex) = &self.texture {
                    let mut mesh = egui::Mesh::with_texture(tex.id());
                    mesh.add_rect_with_uv(
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    ui.painter().add(mesh);
                } else {
                    ui.painter().rect_filled(rect, 4.0, Color32::from_rgb(18, 20, 24));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if is_streaming {
                            "Aguardando primeiro frame do R36S..."
                        } else {
                            "Streaming pausado. Clique em 'Iniciar Stream' para exibir a tela."
                        },
                        egui::FontId::proportional(15.0),
                        Color32::GRAY,
                    );
                }
            };

            if self.scale_mode == ViewportScale::Fit {
                ui.centered_and_justified(|ui| {
                    draw_viewport(ui);
                });
            } else {
                egui::ScrollArea::both()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.centered_and_justified(|ui| {
                            draw_viewport(ui);
                        });
                    });
            }
        });
    }

    fn save_screenshot(&mut self, frame: &DecodedFrame) {
        let timestamp = chrono_or_fallback_timestamp();
        let shots_dir = PathBuf::from("screenshots");
        let _ = std::fs::create_dir_all(&shots_dir);
        let path = shots_dir.join(format!("r36s_screenshot_{}.ppm", timestamp));

        match File::create(&path) {
            Ok(mut file) => {
                use std::io::Write;
                let _ = writeln!(file, "P6\n{} {}\n255", frame.width, frame.height);
                for chunk in frame.rgba_data.chunks_exact(4) {
                    let _ = file.write_all(&[chunk[0], chunk[1], chunk[2]]);
                }
                let _ = file.flush();
                self.screenshot_notice = Some((format!("Screenshot salva em: {}", path.display()), Instant::now()));
            }
            Err(e) => {
                self.screenshot_notice = Some((format!("Erro ao salvar screenshot: {}", e), Instant::now()));
            }
        }
    }
}

fn chrono_or_fallback_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
