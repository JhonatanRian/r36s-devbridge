use crate::connection::DecodedFrame;
use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions, Ui, Vec2};
use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum ViewportMode {
    Handheld,   // Modo Portátil: Console R36S virtual com controles
    Display4x3, // Modo 4:3: Display clássico adaptável ocupando toda a janela
}

pub struct ViewportRenderer {
    texture: Option<TextureHandle>,
    last_frame_index: u32,
    last_frame_time: Option<Instant>,
    received: Option<Instant>,
    screenshot_result: Option<std::sync::mpsc::Receiver<String>>,
    pub mode: ViewportMode,
    nearest_filter: bool,
    screenshot_notice: Option<(String, Instant)>,
}

impl ViewportRenderer {
    pub fn new() -> Self {
        Self {
            texture: None,
            last_frame_index: 0,
            last_frame_time: None,
            received: None,
            screenshot_result: None,
            mode: ViewportMode::Handheld,
            nearest_filter: true,
            screenshot_notice: None,
        }
    }

    pub fn update_frame(&mut self, ctx: &Context, frame_opt: Option<&DecodedFrame>) {
        if let Some(frame) = frame_opt {
            let expected_len = (frame.width as usize) * (frame.height as usize) * 4;
            if frame.rgba_data.len() != expected_len {
                return;
            }

            if self.texture.is_none() || Some(frame.timestamp) != self.last_frame_time {
                self.last_frame_time = Some(frame.timestamp);
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

    pub fn set_received(&mut self, received: Option<Instant>) {
        self.received = received;
    }

    pub fn controls(&mut self, ui: &mut Ui, frame_available: bool, take_screenshot: &mut bool) {
        if let Some(result) = self
            .screenshot_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.screenshot_notice = Some((result, Instant::now()));
            self.screenshot_result = None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.mode, ViewportMode::Handheld, "🎮 Modo Portátil");
            ui.selectable_value(
                &mut self.mode,
                ViewportMode::Display4x3,
                "📺 Modo 4:3 (Tela Cheia)",
            );
            ui.separator();
            ui.weak("Shaders podem reduzir a fluidez do stream.");
            if ui
                .checkbox(&mut self.nearest_filter, "Nearest (Pixel-Art)")
                .changed()
            {
                self.texture = None; // Re-upload once with the selected sampler.
            }
            if frame_available && self.screenshot_result.is_none() {
                if ui.button("📸 Screenshot").clicked() {
                    *take_screenshot = true;
                }
            }
        });
        if let Some((msg, t)) = &self.screenshot_notice {
            if t.elapsed().as_secs() < 4 {
                ui.label(msg);
            }
        }
    }

    pub fn console_width(&self, available: Vec2) -> f32 {
        available.x.min(available.y / 1.58).max(280.0)
    }

    pub fn paint_display_4x3(&self, ui: &mut Ui, connected: bool, streaming: bool, video: bool) {
        let available = ui.available_size();
        let target_ratio = 4.0 / 3.0;
        let (w, h) = if available.x / available.y > target_ratio {
            (available.y * target_ratio, available.y)
        } else {
            (available.x, available.x / target_ratio)
        };
        ui.centered_and_justified(|ui| {
            let (rect, _resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            self.paint_screen(&painter, rect, connected, streaming, video);
        });
    }

    pub fn paint_screen(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        connected: bool,
        streaming: bool,
        video: bool,
    ) {
        painter.rect_filled(rect, 0.0, Color32::from_rgb(12, 15, 19));
        // Se já tivermos uma textura válida decodificada, mantemos ela visível para evitar piscadas
        if connected
            && streaming
            && self
                .received
                .map_or(false, |t| t.elapsed().as_secs_f32() < 1.0)
        {
            if let Some(tex) = &self.texture {
                let ratio = tex.size()[0] as f32 / tex.size()[1] as f32;
                let w = rect.width().min(rect.height() * ratio);
                let target = egui::Rect::from_center_size(rect.center(), egui::vec2(w, w / ratio));
                painter.image(
                    tex.id(),
                    target,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                if !video {
                    painter.text(
                        rect.left_top() + egui::vec2(8.0, 8.0),
                        egui::Align2::LEFT_TOP,
                        "Reconectando — última imagem",
                        egui::FontId::proportional(12.0),
                        Color32::YELLOW,
                    );
                }
                return;
            }
        }
        let message = if !connected {
            "Conecte o R36S pela porta OTG"
        } else if !streaming {
            "Stream pausado\nClique em Iniciar Stream"
        } else if !video {
            "Conectando vídeo…"
        } else if self.texture.is_some() {
            "Vídeo temporariamente indisponível\nAguardando novo quadro…"
        } else {
            "Aguardando primeiro quadro…"
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional((rect.width() / 25.0).clamp(10.0, 18.0)),
            Color32::GRAY,
        );
    }

    pub fn clear(&mut self) {
        self.texture = None;
        self.last_frame_index = 0;
        self.last_frame_time = None;
    }

    pub fn save_screenshot(&mut self, frame: std::sync::Arc<DecodedFrame>) {
        if self.screenshot_result.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.screenshot_result = Some(rx);
        std::thread::spawn(move || {
            let result = write_screenshot(&frame);
            let message = match result {
                Ok(path) => format!("Screenshot salva em: {}", path.display()),
                Err(error) => format!("Erro ao salvar screenshot: {}", error),
            };
            let _ = tx.send(message);
        });
    }
}

fn write_screenshot(frame: &DecodedFrame) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let dir = PathBuf::from("screenshots");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = dir.join(format!("r36s_screenshot_{}.ppm", stamp));
    let temp = dir.join(format!(".r36s_screenshot_{}.tmp", stamp));
    let result = (|| {
        let mut file = File::create(&temp)?;
        let mut buffer = Vec::with_capacity(32 + frame.rgba_data.len() / 4 * 3);
        write!(&mut buffer, "P6\n{} {}\n255\n", frame.width, frame.height)?;
        for pixel in frame.rgba_data.chunks_exact(4) {
            buffer.extend_from_slice(&pixel[..3]);
        }
        file.write_all(&buffer)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        Ok(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
