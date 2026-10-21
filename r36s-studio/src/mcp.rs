use crate::bridge::BridgeClient;
use crate::ipc::base64_decode;
use crate::protocol::{ClientCommand, ServerResponse};
use crate::sequence::SequenceStep;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

pub fn run_mcp_server() -> Result<(), String> {
    eprintln!("[r36s-mcp] Iniciando servidor MCP STDIO (v{})...", MCP_PROTOCOL_VERSION);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    let client = BridgeClient::new();

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("[r36s-mcp] EOF recebido em stdin. Encerrando servidor.");
                break;
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(resp) = handle_mcp_message(trimmed, &client) {
                    let mut json_str = serde_json::to_string(&resp)
                        .map_err(|e| format!("Falha ao serializar JSON-RPC: {}", e))?;
                    json_str.push('\n');
                    writer
                        .write_all(json_str.as_bytes())
                        .map_err(|e| format!("Falha ao escrever em stdout: {}", e))?;
                    writer
                        .flush()
                        .map_err(|e| format!("Falha no flush de stdout: {}", e))?;
                }
            }
            Err(e) => {
                eprintln!("[r36s-mcp] Erro na leitura de stdin: {}", e);
                break;
            }
        }
    }
    Ok(())
}

pub fn handle_mcp_message(line: &str, client: &BridgeClient) -> Option<JsonRpcResponse> {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: serde_json::Value::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("Parse error: {}", e),
                    data: None,
                }),
            });
        }
    };

    let id = match req.id {
        Some(val) => val,
        None => {
            // Notificação: processar sem enviar resposta
            handle_notification(&req.method, req.params.as_ref());
            return None;
        }
    };

    match req.method.as_str() {
        "initialize" => {
            let res = serde_json::json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "r36s-studio-mcp",
                    "version": "0.3.0"
                }
            });
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(res),
                error: None,
            })
        }
        "ping" => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(serde_json::json!({})),
            error: None,
        }),
        "tools/list" => {
            let tools = get_tool_definitions();
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({ "tools": tools })),
                error: None,
            })
        }
        "tools/call" => {
            let params = req.params.unwrap_or(serde_json::Value::Null);
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));

            let result = dispatch_tool_call(name, &arguments, client);
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(result),
                error: None,
            })
        }
        other => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", other),
                data: None,
            }),
        }),
    }
}

fn handle_notification(method: &str, _params: Option<&serde_json::Value>) {
    match method {
        "notifications/initialized" => {
            eprintln!("[r36s-mcp] Cliente MCP inicializado com sucesso.");
        }
        "notifications/cancelled" => {
            eprintln!("[r36s-mcp] Operação cancelada pelo cliente.");
        }
        other => {
            eprintln!("[r36s-mcp] Notificação desconhecida recebida: {}", other);
        }
    }
}

pub fn get_tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "r36s_status_and_metrics",
            "description": "Retorna status da conexão USB, telemetria detalhada de hardware (CPU global e CPU0..3, RAM, CMA total e livre, temperatura, thermal throttling, GPU Mali), jogo/processo ativo e snapshot completo do sandbox no R36S.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        serde_json::json!({
            "name": "r36s_screenshot",
            "description": "Captura a tela atual do R36S como imagem PNG nativa (normalmente 640x480) com canal alpha opaco (255) gerada na memória do PC.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        serde_json::json!({
            "name": "r36s_input",
            "description": "Injeta entradas de controle no R36S: botão individual (press/release/pulse), eixos analógicos ou sequência ordenada de ações com espera WAIT:<ms> (ex: ['START', 'WAIT:500', 'A']).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sequence": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Lista de passos como nomes de botões (START, A, B, UP, DOWN, etc.) e esperas WAIT:<ms> (ex: ['START', 'WAIT:1000', 'A'])"
                    },
                    "button": {
                        "type": "string",
                        "description": "Nome de botão individual (ex: A, B, X, Y, START, SELECT, UP, DOWN, LEFT, RIGHT, L1, R1, L2, R2)"
                    },
                    "axis": {
                        "type": "string",
                        "enum": ["left_x", "left_y", "right_x", "right_y"],
                        "description": "Identificador do eixo analógico a acionar (left_x, left_y, right_x, right_y)"
                    },
                    "value": {
                        "type": "integer",
                        "minimum": -1800,
                        "maximum": 1800,
                        "description": "Valor analógico de -1800 a 1800 (0 para centralizar/repouso)"
                    },
                    "action": {
                        "type": "string",
                        "enum": ["press", "release", "pulse"],
                        "description": "Tipo de ação (padrão: pulse)"
                    },
                    "duration_ms": {
                        "type": "integer",
                        "minimum": 10,
                        "maximum": 5000,
                        "description": "Duração do pulso em milissegundos (padrão: 80ms)"
                    }
                },
                "additionalProperties": false
            }
        }),
        serde_json::json!({
            "name": "r36s_deploy",
            "description": "Transfere um arquivo do PC para o R36S com validação transacional SHA-256 e permissão executável. Suporta diretório de ROMs e prefixo @sandbox/ em tmpfs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "local_path": {
                        "type": "string",
                        "description": "Caminho absoluto ou relativo do arquivo no PC local"
                    },
                    "remote_path": {
                        "type": "string",
                        "description": "Caminho de destino no R36S (ex: @sandbox/meu_teste ou ports/meu_jogo.sh)"
                    },
                    "executable": {
                        "type": "boolean",
                        "description": "Se verdadeiro, aplica permissão de execução (chmod 0755)"
                    }
                },
                "required": ["local_path", "remote_path"],
                "additionalProperties": false
            }
        }),
        serde_json::json!({
            "name": "r36s_sandbox",
            "description": "Controla a execução isolada de testes no R36S com handoff DRM Master automático e restauração do EmulationStation: start, status ou stop.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "status", "stop"],
                        "description": "Ação a executar no sandbox"
                    },
                    "command": {
                        "type": "string",
                        "description": "Caminho do binário para 'start' (ex: /tmp/r36s-sandbox/meu_teste)"
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Argumentos para o comando"
                    },
                    "exclusive_drm": {
                        "type": "boolean",
                        "description": "Se verdadeiro, suspende o EmulationStation para ceder DRM Master exclusivo (padrão: true)"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 500,
                        "maximum": 30000,
                        "description": "Timeout para stop em ms (padrão: 3000)"
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }
        }),
        serde_json::json!({
            "name": "r36s_logs",
            "description": "Recupera logs em RAM do R36S: saídas stdout/stderr de processos em teste no sandbox, ou logs de emuladores e sistema (active_game, run, emulationstation, retroarch, agent, dmesg, syslog, port, java, ppsspp, drastic, mupen64plus). Suporta filtro por grep. Nunca consulta journalctl.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "enum": [
                            "sandbox",
                            "active_game",
                            "run",
                            "emulationstation",
                            "es",
                            "retroarch",
                            "agent",
                            "dmesg",
                            "syslog",
                            "port",
                            "java",
                            "ppsspp",
                            "drastic",
                            "mupen64plus"
                        ],
                        "description": "Fonte dos logs (padrão: sandbox)"
                    },
                    "lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "description": "Quantidade máxima de linhas (padrão: 50)"
                    },
                    "grep": {
                        "type": "string",
                        "description": "Padrão textual para filtrar linhas do log (insensível a maiúsculas/minúsculas)"
                    }
                },
                "additionalProperties": false
            }
        }),
    ]
}

fn dispatch_tool_call(name: &str, args: &serde_json::Value, client: &BridgeClient) -> serde_json::Value {
    match name {
        "r36s_status_and_metrics" => match client.get_diagnostics() {
            Ok(diag) => {
                let text = serde_json::to_string_pretty(&diag).unwrap_or_default();
                serde_json::json!({
                    "content": [
                        { "type": "text", "text": text }
                    ],
                    "isError": false
                })
            }
            Err(e) => make_tool_error(format!("Falha ao consultar diagnósticos do console: {}", e)),
        },
        "r36s_screenshot" => match client.get_screenshot_png() {
            Ok(shot) => {
                // Valida assinatura PNG
                if let Some(bytes) = base64_decode(&shot.png_base64) {
                    if bytes.len() >= 8 && &bytes[0..8] == &[137, 80, 78, 71, 13, 10, 26, 10] {
                        return serde_json::json!({
                            "content": [
                                {
                                    "type": "image",
                                    "data": shot.png_base64,
                                    "mimeType": "image/png"
                                }
                            ],
                            "isError": false
                        });
                    }
                }
                make_tool_error("PNG decodificado inválido ou corrompido")
            }
            Err(e) => make_tool_error(format!("Falha na captura de tela: {}", e)),
        },
        "r36s_input" => {
            let mut steps = Vec::new();
            if let Some(seq) = args.get("sequence").and_then(|v| v.as_array()) {
                for item in seq {
                    if let Some(s) = item.as_str() {
                        match parse_input_step(s) {
                            Ok(step) => steps.push(step),
                            Err(err) => return make_tool_error(err),
                        }
                    }
                }
            } else if let Some(axis_str) = args.get("axis").and_then(|v| v.as_str()) {
                let axis_name = axis_str.to_lowercase();
                if !matches!(axis_name.as_str(), "left_x" | "left_y" | "right_x" | "right_y") {
                    return make_tool_error(format!(
                        "Eixo analógico desconhecido: '{}'. Use left_x, left_y, right_x ou right_y",
                        axis_str
                    ));
                }
                let value = match args.get("value").and_then(|v| v.as_i64()) {
                    Some(val) if (-1800..=1800).contains(&val) => val as i16,
                    Some(val) => {
                        return make_tool_error(format!(
                            "Valor do eixo analógico fora dos limites (-1800..1800): {}",
                            val
                        ))
                    }
                    None => {
                        return make_tool_error(
                            "Argumento obrigatório 'value' ausente para o eixo analógico",
                        )
                    }
                };

                let left_x = if axis_name == "left_x" { value } else { 0 };
                let left_y = if axis_name == "left_y" { value } else { 0 };
                let right_x = if axis_name == "right_x" { value } else { 0 };
                let right_y = if axis_name == "right_y" { value } else { 0 };

                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("pulse");
                let duration_ms = args.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(80);

                match action {
                    "press" | "hold" => {
                        steps.push(SequenceStep::Axis {
                            left_x,
                            left_y,
                            right_x,
                            right_y,
                        });
                    }
                    "release" => {
                        steps.push(SequenceStep::Axis {
                            left_x: 0,
                            left_y: 0,
                            right_x: 0,
                            right_y: 0,
                        });
                    }
                    _ => {
                        // pulse: aplica o valor, aguarda e retorna ao centro
                        steps.push(SequenceStep::Axis {
                            left_x,
                            left_y,
                            right_x,
                            right_y,
                        });
                        steps.push(SequenceStep::WaitMs(duration_ms));
                        steps.push(SequenceStep::Axis {
                            left_x: 0,
                            left_y: 0,
                            right_x: 0,
                            right_y: 0,
                        });
                    }
                }
            } else if let Some(button_str) = args.get("button").and_then(|v| v.as_str()) {
                let mask = match parse_button_mask(button_str) {
                    Some(m) => m,
                    None => return make_tool_error(format!("Botão desconhecido: '{}'", button_str)),
                };
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("pulse");
                let duration_ms = args.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(80) as u16;

                match action {
                    "press" | "hold" => steps.push(SequenceStep::Hold { mask }),
                    "release" => steps.push(SequenceStep::Release { mask }),
                    _ => steps.push(SequenceStep::Pulse {
                        mask,
                        duration_ms: Some(duration_ms),
                    }),
                }
            } else {
                return make_tool_error("Argumentos de entrada ausentes (especifique 'sequence', 'button' ou 'axis')");
            }

            if steps.is_empty() {
                return make_tool_error("Nenhum passo de entrada válido especificado");
            }

            match client.run_macro(steps) {
                Ok(res) => serde_json::json!({
                    "content": [
                        { "type": "text", "text": format!("Entrada executada com sucesso: {} passos concluídos", res.steps_completed) }
                    ],
                    "isError": false
                }),
                Err(e) => make_tool_error(format!("Falha na execução de entrada: {}", e)),
            }
        }
        "r36s_deploy" => {
            let local_path_str = match args.get("local_path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return make_tool_error("Argumento obrigatório 'local_path' não fornecido"),
            };
            let remote_path_str = match args.get("remote_path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return make_tool_error("Argumento obrigatório 'remote_path' não fornecido"),
            };
            let executable = args.get("executable").and_then(|v| v.as_bool()).unwrap_or(false);

            let local_path = Path::new(local_path_str);
            if !local_path.exists() {
                return make_tool_error(format!("Arquivo local não encontrado: {:?}", local_path));
            }

            match client.deploy_file(local_path, remote_path_str, executable) {
                Ok((path, hash)) => serde_json::json!({
                    "content": [
                        {
                            "type": "text",
                            "text": format!("Deploy concluído com sucesso:\n- Destino R36S: {}\n- SHA-256 verificado: {}", path, hash)
                        }
                    ],
                    "isError": false
                }),
                Err(e) => make_tool_error(format!("Falha no deploy transacional: {}", e)),
            }
        }
        "r36s_sandbox" => {
            let action = match args.get("action").and_then(|v| v.as_str()) {
                Some(a) => a,
                None => return make_tool_error("Argumento obrigatório 'action' não fornecido"),
            };

            match action {
                "start" => {
                    let cmd_path = match args.get("command").and_then(|v| v.as_str()) {
                        Some(c) => c.to_string(),
                        None => return make_tool_error("Argumento 'command' obrigatório para ação 'start'"),
                    };
                    let cmd_args = args
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let exclusive_drm = args.get("exclusive_drm").and_then(|v| v.as_bool()).unwrap_or(true);

                    let cmd = ClientCommand::RunSandbox {
                        path: cmd_path,
                        args: cmd_args,
                        env: vec![],
                        cwd: None,
                        exclusive_drm,
                    };
                    match client.send_command(cmd) {
                        Ok(res) => match res.response {
                            Some(ServerResponse::SandboxStartResult { success: true, pid, .. }) => {
                                serde_json::json!({
                                    "content": [
                                        { "type": "text", "text": format!("Sandbox iniciado com sucesso. PID: {:?}", pid) }
                                    ],
                                    "isError": false
                                })
                            }
                            Some(ServerResponse::SandboxStartResult { success: false, error, .. }) => {
                                make_tool_error(format!("Falha ao iniciar sandbox: {}", error.unwrap_or_default()))
                            }
                            other => make_tool_error(format!("Resposta inesperada do sandbox start: {:?}", other)),
                        },
                        Err(e) => make_tool_error(format!("Erro ao despachar comando sandbox start: {}", e)),
                    }
                }
                "status" => match client.get_diagnostics() {
                    Ok(diag) => {
                        let status_json = serde_json::json!({
                            "supported": diag.sandbox_supported,
                            "running": diag.sandbox_running,
                            "phase": diag.sandbox_phase,
                            "pid": diag.sandbox_pid,
                            "exit_code": diag.sandbox_exit_code,
                            "term_signal": diag.sandbox_term_signal,
                            "uptime_ms": diag.sandbox_uptime_ms,
                            "stdout_tail": diag.sandbox_stdout_tail,
                            "stderr_tail": diag.sandbox_stderr_tail,
                            "restore_error": diag.sandbox_restore_error
                        });
                        serde_json::json!({
                            "content": [
                                { "type": "text", "text": serde_json::to_string_pretty(&status_json).unwrap() }
                            ],
                            "isError": false
                        })
                    }
                    Err(e) => make_tool_error(format!("Falha ao consultar status do sandbox: {}", e)),
                },
                "stop" => {
                    let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(3000);
                    let cmd = ClientCommand::StopSandbox { timeout_ms };
                    match client.send_command(cmd) {
                        Ok(res) => match res.response {
                            Some(ServerResponse::SandboxStopResult {
                                success,
                                exit_code,
                                term_signal,
                                restore_error,
                                error,
                            }) => {
                                if success {
                                    let restored_es = restore_error.is_none();
                                    serde_json::json!({
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": format!(
                                                    "Sandbox encerrado. Código de saída: {:?}, Sinal: {:?}, EmulationStation restaurado: {}",
                                                    exit_code, term_signal, restored_es
                                                )
                                            }
                                        ],
                                        "isError": false
                                    })
                                } else {
                                    make_tool_error(format!("Erro ao encerrar sandbox: {}", error.unwrap_or_default()))
                                }
                            }
                            other => make_tool_error(format!("Resposta inesperada no sandbox stop: {:?}", other)),
                        },
                        Err(e) => make_tool_error(format!("Erro ao despachar comando sandbox stop: {}", e)),
                    }
                }
                other => make_tool_error(format!("Ação de sandbox desconhecida: '{}'. Use start, status ou stop", other)),
            }
        }
        "r36s_logs" => {
            let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("sandbox");
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let grep = args.get("grep").and_then(|v| v.as_str());

            if source == "sandbox" {
                match client.get_diagnostics() {
                    Ok(diag) => {
                        let mut stdout = diag.sandbox_stdout_tail.unwrap_or_default();
                        let mut stderr = diag.sandbox_stderr_tail.unwrap_or_default();
                        if let Some(pattern) = grep {
                            let p = pattern.to_lowercase();
                            stdout = stdout.lines().filter(|l| l.to_lowercase().contains(&p)).collect::<Vec<_>>().join("\n");
                            stderr = stderr.lines().filter(|l| l.to_lowercase().contains(&p)).collect::<Vec<_>>().join("\n");
                        }
                        let out = serde_json::json!({
                            "source": "sandbox",
                            "stdout_tail": stdout,
                            "stderr_tail": stderr
                        });
                        serde_json::json!({
                            "content": [
                                { "type": "text", "text": serde_json::to_string_pretty(&out).unwrap() }
                            ],
                            "isError": false
                        })
                    }
                    Err(e) => make_tool_error(format!("Falha ao obter logs do sandbox: {}", e)),
                }
            } else if matches!(
                source,
                "active_game" | "active" | "game"
                | "run" | "runcommand"
                | "emulationstation" | "es"
                | "retroarch"
                | "agent" | "daemon" | "journalctl"
                | "dmesg"
                | "syslog" | "system"
                | "port" | "installer"
                | "java" | "freej2me"
                | "ppsspp" | "psp"
                | "drastic" | "nds"
                | "mupen64plus" | "n64"
            ) {
                let cmd = ClientCommand::FetchLogs {
                    log_type: source.to_string(),
                    lines,
                };
                match client.send_command(cmd) {
                    Ok(res) => match res.response {
                        Some(ServerResponse::LogsResponse { log_type, content }) => {
                            let filtered = if let Some(pattern) = grep {
                                let p = pattern.to_lowercase();
                                content.lines().filter(|l| l.to_lowercase().contains(&p)).collect::<Vec<_>>().join("\n")
                            } else {
                                content
                            };
                            serde_json::json!({
                                "content": [
                                    { "type": "text", "text": format!("=== Logs [{}] ===\n{}", log_type, filtered) }
                                ],
                                "isError": false
                            })
                        }
                        other => make_tool_error(format!("Resposta inesperada de logs: {:?}", other)),
                    },
                    Err(e) => make_tool_error(format!("Falha ao buscar logs: {}", e)),
                }
            } else {
                make_tool_error(format!(
                    "Fonte de logs inválida: '{}'. Fontes válidas: sandbox, active_game, run, emulationstation, retroarch, agent, dmesg, syslog, port, java, ppsspp, drastic, mupen64plus",
                    source
                ))
            }
        }
        other => make_tool_error(format!("Ferramenta MCP desconhecida: '{}'", other)),
    }
}

fn make_tool_error(msg: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "content": [
            { "type": "text", "text": msg.into() }
        ],
        "isError": true
    })
}

pub fn parse_button_mask(name: &str) -> Option<u32> {
    use crate::protocol::*;
    match name.to_uppercase().as_str() {
        "A" => Some(BTN_MASK_A),
        "B" => Some(BTN_MASK_B),
        "X" => Some(BTN_MASK_X),
        "Y" => Some(BTN_MASK_Y),
        "START" => Some(BTN_MASK_START),
        "SELECT" => Some(BTN_MASK_SELECT),
        "L1" | "LB" => Some(BTN_MASK_L1),
        "R1" | "RB" => Some(BTN_MASK_R1),
        "L2" | "LT" => Some(BTN_MASK_L2),
        "R2" | "RT" => Some(BTN_MASK_R2),
        "L3" => Some(BTN_MASK_L3),
        "R3" => Some(BTN_MASK_R3),
        "FN" | "HOTKEY" => Some(BTN_MASK_FN),
        "UP" | "DPAD_UP" => Some(BTN_MASK_DPAD_UP),
        "DOWN" | "DPAD_DOWN" => Some(BTN_MASK_DPAD_DOWN),
        "LEFT" | "DPAD_LEFT" => Some(BTN_MASK_DPAD_LEFT),
        "RIGHT" | "DPAD_RIGHT" => Some(BTN_MASK_DPAD_RIGHT),
        _ => None,
    }
}

pub fn parse_input_step(token: &str) -> Result<SequenceStep, String> {
    let t = token.trim();
    if t.is_empty() {
        return Err("Passo de entrada vazio".to_string());
    }

    if let Some(stripped) = t.strip_prefix("WAIT:") {
        let ms: u64 = stripped
            .parse()
            .map_err(|_| format!("Valor de espera inválido: '{}'", stripped))?;
        if ms < 10 || ms > 10000 {
            return Err(format!("Tempo de espera fora dos limites (10..10000 ms): {}", ms));
        }
        return Ok(SequenceStep::WaitMs(ms));
    }

    if let Some(stripped) = t.strip_prefix("AXIS:") {
        let parts: Vec<&str> = stripped.split(':').collect();
        if parts.len() != 2 {
            return Err(format!(
                "Formato de eixo inválido: '{}'. Use AXIS:<eixo>:<valor> (ex: AXIS:left_x:1800)",
                t
            ));
        }
        let axis_name = parts[0].trim().to_lowercase();
        let val: i16 = parts[1]
            .trim()
            .parse()
            .map_err(|_| format!("Valor numérico de eixo inválido: '{}'", parts[1]))?;

        if !(-1800..=1800).contains(&val) {
            return Err(format!(
                "Valor do eixo analógico fora dos limites (-1800..1800): {}",
                val
            ));
        }

        return match axis_name.as_str() {
            "left_x" | "lx" => Ok(SequenceStep::Axis {
                left_x: val,
                left_y: 0,
                right_x: 0,
                right_y: 0,
            }),
            "left_y" | "ly" => Ok(SequenceStep::Axis {
                left_x: 0,
                left_y: val,
                right_x: 0,
                right_y: 0,
            }),
            "right_x" | "rx" => Ok(SequenceStep::Axis {
                left_x: 0,
                left_y: 0,
                right_x: val,
                right_y: 0,
            }),
            "right_y" | "ry" => Ok(SequenceStep::Axis {
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: val,
            }),
            _ => Err(format!(
                "Eixo analógico desconhecido: '{}'. Use left_x, left_y, right_x ou right_y",
                parts[0]
            )),
        };
    }

    if let Some(mask) = parse_button_mask(t) {
        return Ok(SequenceStep::Pulse {
            mask,
            duration_ms: Some(80),
        });
    }

    Err(format!(
        "Passo de entrada não reconhecido: '{}'. Use nomes de botão (A, B, START, ...), WAIT:<ms> ou AXIS:<eixo>:<valor>",
        t
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_input_step_axis() {
        let step = parse_input_step("AXIS:left_x:1800").expect("parse left_x");
        assert_eq!(
            step,
            SequenceStep::Axis {
                left_x: 1800,
                left_y: 0,
                right_x: 0,
                right_y: 0
            }
        );

        let step = parse_input_step("AXIS:ly:-1500").expect("parse ly alias");
        assert_eq!(
            step,
            SequenceStep::Axis {
                left_x: 0,
                left_y: -1500,
                right_x: 0,
                right_y: 0
            }
        );

        let step = parse_input_step("AXIS:right_y:0").expect("parse right_y center");
        assert_eq!(
            step,
            SequenceStep::Axis {
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: 0
            }
        );

        // Out of bounds
        assert!(parse_input_step("AXIS:left_x:1801").is_err());
        assert!(parse_input_step("AXIS:left_x:-1801").is_err());
        // Invalid syntax
        assert!(parse_input_step("AXIS:left_x").is_err());
        assert!(parse_input_step("AXIS:unknown_axis:100").is_err());
    }

    #[test]
    fn test_mcp_tools_list_schema_includes_axis() {
        let tools = get_tool_definitions();
        let input_tool = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("r36s_input"))
            .expect("r36s_input tool found");

        let schema = input_tool.get("inputSchema").expect("inputSchema exists");
        let props = schema.get("properties").expect("properties exist");
        assert!(props.get("axis").is_some(), "axis property must exist in r36s_input schema");
        assert!(props.get("value").is_some(), "value property must exist in r36s_input schema");
        assert!(props.get("button").is_some(), "button property must exist");
        assert!(props.get("sequence").is_some(), "sequence property must exist");
    }

    #[test]
    fn test_mcp_tools_list_schema_r36s_logs() {
        let tools = get_tool_definitions();
        let logs_tool = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("r36s_logs"))
            .expect("r36s_logs tool found");

        let schema = logs_tool.get("inputSchema").expect("inputSchema exists");
        let props = schema.get("properties").expect("properties exist");
        assert!(props.get("source").is_some(), "source property must exist");
        assert!(props.get("lines").is_some(), "lines property must exist");
        assert!(props.get("grep").is_some(), "grep property must exist");

        let source_enum = props
            .get("source")
            .and_then(|s| s.get("enum"))
            .and_then(|e| e.as_array())
            .expect("source enum exists");
        let enum_vals: Vec<&str> = source_enum.iter().filter_map(|v| v.as_str()).collect();

        for expected in &[
            "sandbox",
            "active_game",
            "run",
            "emulationstation",
            "retroarch",
            "agent",
            "dmesg",
            "syslog",
            "port",
            "java",
            "ppsspp",
            "drastic",
            "mupen64plus",
        ] {
            assert!(
                enum_vals.contains(expected),
                "Source enum deve conter {}",
                expected
            );
        }

        // Teste de despacho com fonte inválida
        let client = BridgeClient::new();
        let bad_args = serde_json::json!({ "source": "invalid_source" });
        let res = dispatch_tool_call("r36s_logs", &bad_args, &client);
        assert_eq!(res.get("isError").and_then(|v| v.as_bool()), Some(true));
    }
}
