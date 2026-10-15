use crate::connection::DecodedFrame;
use egui::{ColorImage, Context, TextureHandle, TextureOptions, Ui, Vec2};
use std::fs::File;
use std::time::Instant;

#[derive(PartialEq, Clone, Copy)]
pub enum ViewportScale {
    Scale1x, // 640x480
    Scale1_5x, // 960x720
    Fit,     // Ajusta mantendo 4:3
}

pub struct ViewportRenderer {
    texture: Option<TextureHandle>,
    last_frame_index: u32,
    fps_counter: u32,
    current_fps: u32,
    last_fps_calc: Instant,
    scale_mode: ViewportScale,
    nearest_filter: bool,
    screenshot_notice: Option<(String, Instant)>,
}

impl ViewportRenderer {
    pub fn new() -> Self {
        Self {
            texture: None,
            last_frame_index: 0,
            fps_counter: 0,
            current_fps: 0,
            last_fps_calc: Instant::now(),
            scale_mode: ViewportScale::Fit,
            nearest_filter: true,
            screenshot_notice: None,
        }
    }

    pub fn update_frame(&mut self, ctx: &Context, frame_opt: Option<&DecodedFrame>) {
        if let Some(frame) = frame_opt {
            if frame.frame_index != self.last_frame_index {
                self.last_frame_index = frame.frame_index;
                self.fps_counter += 1;

                if self.last_fps_calc.elapsed().as_millis() >= 1000 {
                    self.current_fps = self.fps_counter;
                    self.fps_counter = 0;
                    self.last_fps_calc = Instant::now();
                }

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

    pub fn render(&mut self, ui: &mut Ui, is_streaming: bool, frame_opt: Option<&DecodedFrame>) {
        ui.vertical(|ui| {
            // Barra de controles do Viewport
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("🖥 Viewport R36S (640x480)").strong());

                ui.separator();

                ui.radio_value(&mut self.scale_mode, ViewportScale::Scale1x, "1x (640x480)");
                ui.radio_value(&mut self.scale_mode, ViewportScale::Scale1_5x, "1.5x (960x720)");
                ui.radio_value(&mut self.scale_mode, ViewportScale::Fit, "Ajustar (4:3)");

                ui.separator();

                ui.checkbox(&mut self.nearest_filter, "Pixel-Art (Nearest)");

                ui.separator();

                if is_streaming {
                    ui.colored_label(
                        egui::Color32::from_rgb(0, 220, 100),
                        format!("● {} FPS", self.current_fps),
                    );
                } else {
                    ui.colored_label(egui::Color32::GRAY, "○ Pausado");
                }

                if let Some(frame) = frame_opt {
                    if ui.button("📷 Salvar Screenshot").clicked() {
                        self.save_screenshot(frame);
                    }
                }
            });

            if let Some((msg, time)) = &self.screenshot_notice {
                if time.elapsed().as_secs() < 4 {
                    ui.colored_label(egui::Color32::from_rgb(100, 255, 100), msg);
                }
            }

            ui.separator();

            // Área de exibição do Framebuffer
            let available = ui.available_size();
            let (target_w, target_h) = match self.scale_mode {
                ViewportScale::Scale1x => (640.0, 480.0),
                ViewportScale::Scale1_5x => (960.0, 720.0),
                ViewportScale::Fit => {
                    let aspect = 640.0 / 480.0;
                    let mut w = available.x.max(320.0);
                    let mut h = w / aspect;
                    if h > available.y {
                        h = available.y.max(240.0);
                        w = h * aspect;
                    }
                    (w, h)
                }
            };

            let (rect, _response) = ui.allocate_exact_size(Vec2::new(target_w, target_h), egui::Sense::hover());

            if let Some(tex) = &self.texture {
                let mut mesh = egui::Mesh::with_texture(tex.id());
                mesh.add_rect_with_uv(
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                ui.painter().add(mesh);
            } else {
                ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(18, 20, 24));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if is_streaming {
                        "Aguardando primeiro frame do R36S..."
                    } else {
                        "Streaming pausado. Clique em 'Iniciar Stream' para exibir a tela."
                    },
                    egui::FontId::proportional(16.0),
                    egui::Color32::GRAY,
                );
            }
        });
    }

    fn save_screenshot(&mut self, frame: &DecodedFrame) {
        let timestamp = chrono_or_fallback_timestamp();
        let filename = format!("r36s_screenshot_{}.ppm", timestamp);
        if let Ok(mut file) = File::create(&filename) {
            use std::io::Write;
            let _ = writeln!(file, "P6\n{} {}\n255", frame.width, frame.height);
            for chunk in frame.rgba_data.chunks_exact(4) {
                let _ = file.write_all(&[chunk[0], chunk[1], chunk[2]]);
            }
            self.screenshot_notice = Some((format!("Screenshot salva: {}", filename), Instant::now()));
        }
    }
}

fn chrono_or_fallback_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
