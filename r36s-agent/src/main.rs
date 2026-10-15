mod deploy;
mod framebuffer;
mod metrics;
mod process;
mod protocol;
mod server;

use server::AgentServer;

fn main() {
    println!("========================================");
    println!("   R36S DevBridge Agent (ArkOS Daemon)  ");
    println!("========================================");

    let server = AgentServer::new();
    server.start();
}
