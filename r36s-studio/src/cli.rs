use crate::bridge::BridgeClient;
use crate::ipc::base64_decode;
use crate::mcp::parse_input_step;
use crate::protocol::{ClientCommand, ServerResponse};
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub fn run_cli_command(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return print_usage_and_fail("Subcomando CLI não fornecido.");
    }

    let client = BridgeClient::new();
    match args[0].as_str() {
        "status" => cmd_status(&client),
        "screenshot" => cmd_screenshot(&client, &args[1..]),
        "input" => cmd_input(&client, &args[1..]),
        "deploy" => cmd_deploy(&client, &args[1..]),
        "run" => cmd_run(&client, &args[1..]),
        "stop" => cmd_stop(&client),
        "logs" => cmd_logs(&client, &args[1..]),
        other => print_usage_and_fail(&format!("Subcomando desconhecido: '{}'", other)),
    }
}

fn print_usage_and_fail(error_msg: &str) -> Result<(), String> {
    eprintln!("Erro: {}", error_msg);
    eprintln!("\nUso da CLI do DevBridge:");
    eprintln!("  ./app cli status");
    eprintln!("  ./app cli screenshot <caminho.png>");
    eprintln!("  ./app cli input <sequência> (ex: 'START,WAIT:500,A')");
    eprintln!("  ./app cli deploy <origem_pc> <destino_r36s> [--executable]");
    eprintln!("  ./app cli run <comando_r36s>");
    eprintln!("  ./app cli stop");
    eprintln!("  ./app cli logs [sandbox|dmesg|retroarch|es|agent]");
    Err("".to_string())
}

fn cmd_status(client: &BridgeClient) -> Result<(), String> {
    let diag = client.get_diagnostics().map_err(|e| e.to_string())?;
    let json_output = serde_json::to_string_pretty(&diag)
        .map_err(|e| format!("Falha ao serializar status: {}", e))?;
    println!("{}", json_output);
    Ok(())
}

fn cmd_screenshot(client: &BridgeClient, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return print_usage_and_fail("Caminho do arquivo PNG de destino obrigatório. Uso: ./app cli screenshot <caminho.png>");
    }

    let dest_path = Path::new(&args[0]);
    if let Some(parent) = dest_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(format!("Diretório pai não existe: {:?}", parent));
        }
    }

    let shot = client.get_screenshot_png().map_err(|e| e.to_string())?;

    let png_bytes = base64_decode(&shot.png_base64)
        .ok_or_else(|| "Base64 de captura corrompido retornado pelo host".to_string())?;

    // Validação estrita do cabeçalho PNG mágico
    if png_bytes.len() < 8 || &png_bytes[0..8] != &[137, 80, 78, 71, 13, 10, 26, 10] {
        return Err("Dados recebidos não possuem cabeçalho PNG válido".to_string());
    }

    // Escrita em arquivo temporário com sync e rename atômico para garantir integridade
    let pid = std::process::id();
    let file_name = dest_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("screenshot.png");
    let tmp_path = dest_path.with_file_name(format!(".{}.tmp.{}", file_name, pid));

    let write_result = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp_path)?;
        file.write_all(&png_bytes)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&tmp_path, dest_path)?;
        Ok(())
    })();

    if let Err(e) = write_result {
        if tmp_path.exists() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        return Err(format!("Falha ao gravar arquivo PNG atômico {:?}: {}", dest_path, e));
    }

    let result = serde_json::json!({
        "status": "success",
        "operation": "screenshot",
        "path": dest_path.to_string_lossy(),
        "width": shot.width,
        "height": shot.height,
        "frame_index": shot.frame_index
    });
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(())
}

fn cmd_input(client: &BridgeClient, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return print_usage_and_fail("Sequência de entrada obrigatória. Uso: ./app cli input <sequência> (ex: 'START,WAIT:500,A')");
    }

    let full_seq_str = args.join(",");
    let tokens: Vec<&str> = full_seq_str.split(',').collect();

    // 1. Validação integral prévia de todos os passos
    let mut steps = Vec::new();
    for token in tokens {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let step = parse_input_step(trimmed)?;
        steps.push(step);
    }

    if steps.is_empty() {
        return Err("Nenhum passo de entrada válido especificado".to_string());
    }

    // 2. Executa a sequência no host através do Bridge
    let res = client.run_macro(steps).map_err(|e| e.to_string())?;

    let output = serde_json::json!({
        "status": "success",
        "operation": "input",
        "steps_completed": res.steps_completed
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn cmd_deploy(client: &BridgeClient, args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return print_usage_and_fail("Parâmetros insuficientes. Uso: ./app cli deploy <origem_pc> <destino_r36s> [--executable]");
    }

    let local_file_str = &args[0];
    let remote_dest_str = &args[1];

    let mut executable = false;
    for arg in &args[2..] {
        if arg == "--executable" {
            executable = true;
        }
    }

    // Destinos em @sandbox/ são automaticamente executáveis
    if remote_dest_str.starts_with("@sandbox") {
        executable = true;
    }

    // Validação de segurança contra path traversal
    if remote_dest_str.contains("..") {
        return Err("Violação de segurança: '..' não permitido no caminho de destino".to_string());
    }

    let local_path = Path::new(local_file_str);
    if !local_path.exists() {
        return Err(format!("Arquivo local não encontrado: {:?}", local_path));
    }

    let (final_path, sha256_hash) = client
        .deploy_file(local_path, remote_dest_str, executable)
        .map_err(|e| e.to_string())?;

    let file_size = local_path.metadata().map(|m| m.len()).unwrap_or(0);

    let output = serde_json::json!({
        "status": "success",
        "operation": "deploy",
        "path": final_path,
        "sha256": sha256_hash,
        "size": file_size,
        "executable": executable
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn cmd_run(client: &BridgeClient, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return print_usage_and_fail("Comando a executar no R36S obrigatório. Uso: ./app cli run <comando_r36s>");
    }

    let raw_cmd = args.join(" ");

    // Validação estrita: rejeita metacaracteres de shell (sem shell implícito)
    for bad_char in &['|', '&', ';', '<', '>', '$', '`', '\n', '\r', '(', ')'] {
        if raw_cmd.contains(*bad_char) {
            return Err(format!(
                "Comando rejeitado: metacaracteres de shell ('{}') são proibidos no modo sandbox. Use chamadas diretas a binários.",
                bad_char
            ));
        }
    }

    // Tokenização respeitando aspas simples e duplas
    let tokens = tokenize_command(&raw_cmd)?;
    if tokens.is_empty() {
        return Err("Comando vazio fornecido".to_string());
    }

    let bin_path = tokens[0].clone();
    let bin_args = tokens[1..].to_vec();

    // Verifica capacidade de sandbox
    let diag = client.get_diagnostics().map_err(|e| e.to_string())?;
    if !diag.sandbox_supported {
        return Err("Capacidade ausente: o agente conectado não suporta o sandbox 'sandbox-v1'.".to_string());
    }

    let cmd = ClientCommand::RunSandbox {
        path: bin_path,
        args: bin_args,
        env: vec![],
        cwd: None,
        exclusive_drm: true,
    };

    let res = client.send_command(cmd).map_err(|e| e.to_string())?;
    match res.response {
        Some(ServerResponse::SandboxStartResult { success: true, pid, .. }) => {
            let out = serde_json::json!({
                "status": "success",
                "operation": "run",
                "pid": pid,
                "exclusive_drm": true
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
            Ok(())
        }
        Some(ServerResponse::SandboxStartResult { success: false, error, .. }) => {
            Err(format!("Falha ao iniciar sandbox: {}", error.unwrap_or_default()))
        }
        other => Err(format!("Resposta inesperada ao iniciar sandbox: {:?}", other)),
    }
}

fn cmd_stop(client: &BridgeClient) -> Result<(), String> {
    let diag = client.get_diagnostics().map_err(|e| e.to_string())?;
    if !diag.sandbox_supported {
        return Err("Capacidade ausente: o agente conectado não suporta sandbox.".to_string());
    }

    let cmd = ClientCommand::StopSandbox { timeout_ms: 3000 };
    let res = client.send_command(cmd).map_err(|e| e.to_string())?;

    match res.response {
        Some(ServerResponse::SandboxStopResult {
            success,
            exit_code,
            term_signal,
            restore_error,
            error,
        }) => {
            if success {
                let restored_es = restore_error.is_none();
                let out = serde_json::json!({
                    "status": "success",
                    "operation": "stop",
                    "exit_code": exit_code,
                    "term_signal": term_signal,
                    "restored_emulationstation": restored_es,
                    "restore_error": restore_error
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
                Ok(())
            } else {
                Err(format!("Falha ao encerrar sandbox: {}", error.unwrap_or_default()))
            }
        }
        other => Err(format!("Resposta inesperada ao encerrar sandbox: {:?}", other)),
    }
}

fn cmd_logs(client: &BridgeClient, args: &[String]) -> Result<(), String> {
    let source = if args.is_empty() {
        "sandbox"
    } else {
        args[0].as_str()
    };

    if source == "sandbox" {
        let diag = client.get_diagnostics().map_err(|e| e.to_string())?;
        let out = serde_json::json!({
            "status": "success",
            "source": "sandbox",
            "stdout_tail": diag.sandbox_stdout_tail.unwrap_or_default(),
            "stderr_tail": diag.sandbox_stderr_tail.unwrap_or_default()
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        Ok(())
    } else if matches!(source, "dmesg" | "retroarch" | "es" | "agent") {
        let cmd = ClientCommand::FetchLogs {
            log_type: source.to_string(),
            lines: 100,
        };
        let res = client.send_command(cmd).map_err(|e| e.to_string())?;
        match res.response {
            Some(ServerResponse::LogsResponse { log_type, content }) => {
                let out = serde_json::json!({
                    "status": "success",
                    "source": log_type,
                    "content": content
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
                Ok(())
            }
            other => Err(format!("Resposta inesperada ao obter logs: {:?}", other)),
        }
    } else {
        Err(format!(
            "Fonte de logs inválida: '{}'. Fontes suportadas: sandbox, dmesg, retroarch, es, agent",
            source
        ))
    }
}

pub fn tokenize_command(s: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    for ch in s.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if ch == '\\' && !in_single_quote {
            escaped = true;
            continue;
        }

        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }

        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }

        if ch.is_whitespace() && !in_single_quote && !in_double_quote {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }

    if in_single_quote || in_double_quote {
        return Err("Aspas não fechadas no comando".to_string());
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    Ok(tokens)
}
