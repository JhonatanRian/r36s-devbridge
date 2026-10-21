use r36s_agent::server::AgentServer;

fn main() {
    println!("========================================");
    println!("   R36S DevBridge Agent (ArkOS Daemon)  ");
    println!("========================================");

    let server = AgentServer::new();
    server.start();
}
