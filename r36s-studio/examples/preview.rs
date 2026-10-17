//! Render the real application and capture one frame; no screenshot code in production.
use eframe::App;
use std::io::Write;
struct Preview {
    app: r36s_studio::app::DevBridgeApp,
    frames: usize,
}
impl App for Preview {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.update(ctx, frame);
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = event {
                let path = std::env::var("DEVBRIDGE_PREVIEW")
                    .unwrap_or("/tmp/devbridge-preview.ppm".into());
                let mut f = std::fs::File::create(path).unwrap();
                writeln!(f, "P6\n{} {}\n255", image.size[0], image.size[1]).unwrap();
                for pixel in &image.pixels {
                    f.write_all(&pixel.to_array()[..3]).unwrap();
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.frames += 1;
        if self.frames == 10 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
fn main() -> eframe::Result<()> {
    let width = std::env::var("DEVBRIDGE_PREVIEW_WIDTH")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(1180.0);
    eframe::run_native(
        "DevBridge Preview",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([width, 740.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            Ok(Box::new(Preview {
                app: r36s_studio::app::DevBridgeApp::new(cc),
                frames: 0,
            }))
        }),
    )
}
