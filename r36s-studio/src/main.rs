use eframe::NativeOptions;
use r36s_studio::app::DevBridgeApp;
use r36s_studio::{cli, mcp};

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "--mcp" => {
                if let Err(e) = mcp::run_mcp_server() {
                    eprintln!("Erro MCP: {}", e);
                    std::process::exit(1);
                }
                return Ok(());
            }
            "cli" => {
                if let Err(e) = cli::run_cli_command(&args[2..]) {
                    if !e.is_empty() {
                        eprintln!("Erro: {}", e);
                    }
                    std::process::exit(1);
                }
                return Ok(());
            }
            other => {
                eprintln!(
                    "Argumento inválido: '{}'. Uso: ./app [cli <subcomando> | --mcp]",
                    other
                );
                std::process::exit(1);
            }
        }
    }

    env_logger::init_from_env(
        env_logger::Env::default().default_filter_or("warn,r36s_studio=info"),
    );

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
