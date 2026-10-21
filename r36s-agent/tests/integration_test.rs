use r36s_agent::framebuffer::FramebufferReader;
use r36s_agent::metrics::MetricsCollector;
use r36s_agent::protocol::*;

#[test]
fn test_video_header_roundtrip() {
    let header = VideoFrameHeader::new(42, 640, 480, FORMAT_RGB565_LZ4, 0, 12345);
    let bytes = header.to_bytes();
    let decoded = VideoFrameHeader::from_bytes(&bytes).expect("Falha ao decodificar header");

    let magic = decoded.magic;
    let idx = decoded.frame_index;
    let w = decoded.width;
    let h = decoded.height;
    let fmt = decoded.format;
    let flg = decoded.flags;
    let sz = decoded.compressed_size;

    assert_eq!(magic, VIDEO_MAGIC);
    assert_eq!(idx, 42);
    assert_eq!(w, 640);
    assert_eq!(h, 480);
    assert_eq!(fmt, FORMAT_RGB565_LZ4);
    assert_eq!(flg, 0);
    assert_eq!(sz, 12345);
}

#[test]
fn test_command_json_serialization() {
    let cmd = ClientCommand::StartStream { target_fps: 30 };
    let json = serde_json::to_string(&cmd).expect("Falha ao serializar comando");
    let parsed: ClientCommand = serde_json::from_str(&json).expect("Falha ao desserializar comando");

    match parsed {
        ClientCommand::StartStream { target_fps } => assert_eq!(target_fps, 30),
        _ => panic!("Comando inesperado"),
    }
}

#[test]
fn test_metrics_collector() {
    let collector = MetricsCollector::new();
    let metrics1 = collector.collect();
    assert!(metrics1.ram_total_mb > 0, "RAM total deve ser maior que 0");
    assert!(metrics1.cpu_usage >= 0.0 && metrics1.cpu_usage <= 100.0, "Uso de CPU deve estar entre 0 e 100");

    // Primeira amostra: cpu_core_usage deve iniciar vazio pois ainda não há delta
    assert!(metrics1.cpu_core_usage.is_empty(), "Primeira amostra não deve inventar dados de núcleo");

    // Simula intervalo e executa segunda coleta
    std::thread::sleep(std::time::Duration::from_millis(50));
    let metrics2 = collector.collect();
    assert!(metrics2.ram_total_mb > 0);
    assert!(metrics2.cpu_usage >= 0.0 && metrics2.cpu_usage <= 100.0);

    // Na segunda amostra em sistema Linux com pelo menos 4 núcleos, cpu_core_usage terá 4 itens
    #[cfg(target_os = "linux")]
    {
        if metrics2.cpu_core_usage.len() == 4 {
            for &core in &metrics2.cpu_core_usage {
                assert!(core >= 0.0 && core <= 100.0, "Uso por núcleo deve estar entre 0 e 100%");
            }
        }
    }

    // CMA pode estar ativa ou não dependendo do kernel host/target
    if metrics2.cma_available {
        assert!(metrics2.cma_free_mb <= metrics2.cma_total_mb);
    } else {
        assert_eq!(metrics2.cma_total_mb, 0);
        assert_eq!(metrics2.cma_free_mb, 0);
    }
}

#[test]
fn test_framebuffer_mock_and_lz4() {
    std::env::set_var("DEVBRIDGE_ALLOW_MOCK", "1");
    let mut reader = FramebufferReader::new();
    let mut comp_buf = Vec::new();
    let (w, h, format, _flags, compressed_len) = reader.capture_frame(&mut comp_buf);

    assert_eq!(w, 640);
    assert_eq!(h, 480);
    assert_eq!(format, FORMAT_RGB565_LZ4);
    assert!(compressed_len > 0, "Frame comprimido não deve estar vazio");

    // Testa descompressão LZ4 (dados gravados a partir do offset 20)
    let expected_len = (w as usize) * (h as usize) * 2;
    let decompressed = lz4_flex::decompress(&comp_buf[20..20 + compressed_len], expected_len).expect("Falha ao descompactar LZ4");
    assert_eq!(decompressed.len(), expected_len);
}

#[test]
fn gamepad_rejects_input_without_session() {
    let service = r36s_agent::gamepad::GamepadService::new();
    assert!(!service.get_state().input_enabled);
    assert!(service.inject_action(7, &GamepadInputAction::Button {
        mask: BTN_MASK_A, pressed: true,
    }).is_err());
}

#[test]
fn test_coalesced_video_frame_transmission_and_reception() {
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};

    std::env::set_var("DEVBRIDGE_ALLOW_MOCK", "1");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let client_handle = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        // Leitura do cabeçalho de 20 bytes
        let mut header_buf = [0u8; 20];
        stream.read_exact(&mut header_buf).expect("Falha ao ler header");
        let header = VideoFrameHeader::from_bytes(&header_buf).expect("Header inválido");

        let magic = header.magic;
        let idx = header.frame_index;
        let w = header.width;
        let h = header.height;
        let fmt = header.format;
        let sz = header.compressed_size;

        assert_eq!(magic, VIDEO_MAGIC);
        assert_eq!(idx, 1);
        assert_eq!(w, 640);
        assert_eq!(h, 480);
        assert_eq!(fmt, FORMAT_RGB565_LZ4);
        assert!(sz > 0);

        // Leitura do payload comprimido
        let mut payload = vec![0u8; sz as usize];
        stream.read_exact(&mut payload).expect("Falha ao ler payload");

        let expected_raw_len = (w as usize) * (h as usize) * 2;
        let decompressed = lz4_flex::decompress(&payload, expected_raw_len).expect("Falha ao descompactar payload");
        assert_eq!(decompressed.len(), expected_raw_len);
    });

    let (mut server_stream, _) = listener.accept().unwrap();
    let mut reader = FramebufferReader::new();
    let mut comp_buffer = Vec::with_capacity(20 + lz4_flex::block::get_maximum_output_size(640 * 480 * 2));

    let (w, h, fmt, flg, comp_len) = reader.capture_frame(&mut comp_buffer);
    assert!(comp_len > 0);

    let header = VideoFrameHeader::new(1, w, h, fmt, flg, comp_len as u32);
    comp_buffer[..20].copy_from_slice(&header.to_bytes());

    // Transmissão coalescida em uma única fatia contígua: comp_buffer[..20 + comp_len]
    let packet = &comp_buffer[..20 + comp_len];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    r36s_agent::server::write_before(&mut server_stream, packet, deadline)
        .expect("Falha no envio contíguo do pacote coalescido");

    client_handle.join().unwrap();
}

#[test]
fn test_gamepad_push_events_and_concurrent_control_stream() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let streaming_flag = Arc::new(AtomicBool::new(false));
    let target_fps = Arc::new(AtomicU32::new(30));
    let active_control_id = Arc::new(AtomicU32::new(0));
    let metrics = Arc::new(MetricsCollector::new());
    let gamepad = Arc::new(r36s_agent::gamepad::GamepadService::new());
    let gamepad_clone = Arc::clone(&gamepad);
    let sandbox = Arc::new(r36s_agent::process::SandboxManager::new());

    let server_handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        r36s_agent::server::AgentServer::handle_control_client(
            stream,
            streaming_flag,
            target_fps,
            active_control_id,
            1,
            metrics,
            gamepad,
            sandbox,
        );
    });

    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_nodelay(true).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    // 1. Handshake
    let hs_cmd = serde_json::to_string(&ClientCommand::Handshake {
        client_version: "0.2.9".into(),
        client_id: "test-studio".into(),
    })
    .unwrap()
        + "\n";
    stream.write_all(hs_cmd.as_bytes()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let ack: ServerResponse = serde_json::from_str(line.trim()).unwrap();
    if let ServerResponse::HandshakeAck { capabilities, .. } = ack {
        assert!(capabilities.contains(&"gamepad-v2".to_string()));
        assert!(capabilities.contains(&"gamepad-pulse-v1".to_string()));
    } else {
        panic!("Esperava HandshakeAck");
    }

    // 2. SetGamepadMonitor { enabled: true }
    let mon_cmd =
        serde_json::to_string(&ClientCommand::SetGamepadMonitor { enabled: true }).unwrap() + "\n";
    stream.write_all(mon_cmd.as_bytes()).unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains("GamepadStateResponse"));

    // 3. Emit push event concurrently with sending Ping commands
    let push_state = GamepadState {
        available: true,
        sequence: 42,
        buttons: BTN_MASK_A,
        ..Default::default()
    };
    gamepad_clone.notify_listener(push_state);

    let ping_cmd = serde_json::to_string(&ClientCommand::Ping).unwrap() + "\n";
    stream.write_all(ping_cmd.as_bytes()).unwrap();

    // 4. Client should receive both messages (order can be push then pong, or pong then push)
    let mut received_pong = false;
    let mut received_gamepad_push = false;

    for _ in 0..2 {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let resp: ServerResponse = serde_json::from_str(line.trim()).unwrap();
        match resp {
            ServerResponse::Pong => received_pong = true,
            ServerResponse::GamepadStateResponse(st) => {
                if st.sequence == 42 && st.buttons == BTN_MASK_A {
                    received_gamepad_push = true;
                }
            }
            other => panic!("Resposta inesperada: {:?}", other),
        }
    }

    assert!(received_pong, "Deveria ter recebido Pong");
    assert!(received_gamepad_push, "Deveria ter recebido evento push de Gamepad");

    // Close stream to disconnect cleanly
    drop(stream);
    drop(reader);
    server_handle.join().unwrap();
}

#[test]
fn test_gamepad_pulse_json_tagged_serialization_compatibility() {
    let action = GamepadInputAction::Pulse {
        mask: BTN_MASK_A | BTN_MASK_START,
        duration_ms: 80,
    };
    let json = serde_json::to_string(&action).expect("serialize Pulse");
    assert_eq!(
        json,
        r#"{"type":"Pulse","payload":{"mask":8208,"duration_ms":80}}"#
    );
    let decoded: GamepadInputAction = serde_json::from_str(&json).expect("deserialize Pulse");
    assert_eq!(decoded, action);

    // Also verify ClientCommand::InjectGamepadInput wrap
    let cmd = ClientCommand::InjectGamepadInput { action: action.clone() };
    let cmd_json = serde_json::to_string(&cmd).expect("serialize ClientCommand");
    assert_eq!(
        cmd_json,
        r#"{"type":"InjectGamepadInput","payload":{"action":{"type":"Pulse","payload":{"mask":8208,"duration_ms":80}}}}"#
    );
    let decoded_cmd: ClientCommand = serde_json::from_str(&cmd_json).expect("deserialize ClientCommand");
    match decoded_cmd {
        ClientCommand::InjectGamepadInput { action: decoded_action } => {
            assert_eq!(decoded_action, action);
        }
        _ => panic!("Expected InjectGamepadInput"),
    }
}

