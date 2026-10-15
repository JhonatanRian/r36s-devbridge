mod app;
mod connection;
mod deploy_tab;
mod logs_tab;
mod process_tab;
mod protocol;
mod telemetry;
mod viewport;

use app::DevBridgeApp;
use eframe::NativeOptions;

fn main() -> eframe::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("warn,r36s_studio=info"));

    let native_options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("R36S DevBridge Studio")
            .with_inner_size([1180.0, 740.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "R36S DevBridge Studio",
        native_options,
        Box::new(|cc| Ok(Box::new(DevBridgeApp::new(cc)))),
    )
}
