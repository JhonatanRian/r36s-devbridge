use r36s_studio::protocol::*;

#[test]
fn test_video_header_parsing() {
    let mut bytes = [0u8; 20];
    bytes[0..4].copy_from_slice(b"VID0");
    bytes[4..8].copy_from_slice(&100u32.to_le_bytes()); // frame 100
    bytes[8..10].copy_from_slice(&640u16.to_le_bytes()); // width 640
    bytes[10..12].copy_from_slice(&480u16.to_le_bytes()); // height 480
    bytes[12] = FORMAT_RGB565_LZ4;
    bytes[13] = 0; // flags
    bytes[14..18].copy_from_slice(&4096u32.to_le_bytes()); // compressed size

    let header = VideoFrameHeader::from_bytes(&bytes).expect("Falha ao analisar cabeçalho");
    let magic = header.magic;
    let idx = header.frame_index;
    let w = header.width;
    let h = header.height;
    let fmt = header.format;
    let sz = header.compressed_size;

    assert_eq!(magic, VIDEO_MAGIC);
    assert_eq!(idx, 100);
    assert_eq!(w, 640);
    assert_eq!(h, 480);
    assert_eq!(fmt, FORMAT_RGB565_LZ4);
    assert_eq!(sz, 4096);
}

#[test]
fn test_rgb565_to_rgba_conversion() {
    // 1 pixel vermelho puro em RGB565: (31 << 11) = 0xF800
    // Low byte: 0x00, High byte: 0xF8
    let red_pixel_565 = [0x00u8, 0xF8u8];
    let rgba = convert_rgb565(&red_pixel_565, 1, 1);

    assert_eq!(rgba.len(), 4);
    assert_eq!(rgba[0], 255); // Red
    assert_eq!(rgba[1], 0);   // Green
    assert_eq!(rgba[2], 0);   // Blue
    assert_eq!(rgba[3], 255); // Alpha

    // 1 pixel verde puro em RGB565: (63 << 5) = 0x07E0
    // Low byte: 0xE0, High byte: 0x07
    let green_pixel_565 = [0xE0u8, 0x07u8];
    let rgba_green = convert_rgb565(&green_pixel_565, 1, 1);
    assert_eq!(rgba_green[0], 0);
    assert_eq!(rgba_green[1], 255);
    assert_eq!(rgba_green[2], 0);
    assert_eq!(rgba_green[3], 255);
}

fn convert_rgb565(rgb565: &[u8], width: usize, height: usize) -> Vec<u8> {
    let total_pixels = width * height;
    let mut rgba = vec![255u8; total_pixels * 4];

    for i in 0..total_pixels {
        let low = rgb565[i * 2] as u16;
        let high = rgb565[i * 2 + 1] as u16;
        let pixel = low | (high << 8);

        let r = ((pixel >> 11) & 0x1F) as u32;
        let g = ((pixel >> 5) & 0x3F) as u32;
        let b = (pixel & 0x1F) as u32;

        let out_idx = i * 4;
        rgba[out_idx] = ((r * 255) / 31) as u8;
        rgba[out_idx + 1] = ((g * 255) / 63) as u8;
        rgba[out_idx + 2] = ((b * 255) / 31) as u8;
        rgba[out_idx + 3] = 255;
    }

    rgba
}

#[test]
fn test_keyframe_and_delta_xor_decoding() {
    use r36s_studio::connection::ConnectionManager;

    let width = 4;
    let height = 4;
    let frame_len = width * height * 2; // RGB565

    // 1. Simula quadro de referência inicial (Keyframe)
    let keyframe_raw: Vec<u8> = (0..frame_len).map(|i| (i * 23 % 256) as u8).collect();
    let mut comp_keyframe_raw = vec![0u8; lz4_flex::block::get_maximum_output_size(keyframe_raw.len())];
    let comp_sz = lz4_flex::block::compress_into(&keyframe_raw, &mut comp_keyframe_raw).unwrap();

    // Studio recebe keyframe
    let decomp_keyframe = lz4_flex::decompress(&comp_keyframe_raw[..comp_sz], frame_len).unwrap();
    let mut raw_reference_buffer = decomp_keyframe.clone();
    assert_eq!(raw_reference_buffer, keyframe_raw);

    // 2. Simula próximo quadro modificado (Frame 2)
    let mut frame2_raw = keyframe_raw.clone();
    // Modifica alguns pixels
    frame2_raw[2] ^= 0xAA;
    frame2_raw[3] ^= 0x55;
    frame2_raw[10] ^= 0xFF;

    // Agente calcula XOR delta: delta = frame2 ^ keyframe
    let mut delta = vec![0u8; frame_len];
    for i in 0..frame_len {
        delta[i] = frame2_raw[i] ^ keyframe_raw[i];
    }

    let mut comp_delta_buf = vec![0u8; lz4_flex::block::get_maximum_output_size(delta.len())];
    let delta_sz = lz4_flex::block::compress_into(&delta, &mut comp_delta_buf).unwrap();

    // Studio recebe delta
    let decomp_delta = lz4_flex::decompress(&comp_delta_buf[..delta_sz], frame_len).unwrap();
    ConnectionManager::apply_xor_delta(&mut raw_reference_buffer, &decomp_delta);

    // O buffer de referência reconstruído deve ser exatamente igual ao frame2_raw
    assert_eq!(raw_reference_buffer, frame2_raw);

    // 3. Converte para RGBA e garante conformidade de cores
    let rgba = ConnectionManager::rgb565_to_rgba(&raw_reference_buffer, width, height);
    assert_eq!(rgba.len(), width * height * 4);
    for chunk in rgba.chunks_exact(4) {
        assert_eq!(chunk[3], 255); // Alpha sempre opaco
    }
}

#[test]
fn test_async_gamepad_push_events_concurrent_with_telemetry() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use r36s_studio::connection::{ConnectionManager, ConnectionStatus};
    use r36s_studio::protocol::*;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let (cmd_tx, cmd_rx) = channel::<ClientCommand>();
    let status = Arc::new(Mutex::new(ConnectionStatus::Connected {
        target: addr.to_string(),
        latency_ms: 1.0,
        agent_version: "0.2.9".into(),
    }));
    let metrics = Arc::new(Mutex::new(None));
    let logs = Arc::new(Mutex::new(None));
    let deploy_status = Arc::new(Mutex::new(None));
    let proc_result = Arc::new(Mutex::new(None));
    let diag = Arc::new(Mutex::new(Vec::new()));
    let bytes_tx = Arc::new(AtomicU64::new(0));
    let bytes_rx = Arc::new(AtomicU64::new(0));
    let gamepad_listening = Arc::new(AtomicBool::new(true));
    let gamepad_state = Arc::new(Mutex::new(GamepadState::default()));
    let gamepad_supported = Arc::new(AtomicBool::new(true));

    let gamepad_state_clone = Arc::clone(&gamepad_state);
    let metrics_clone = Arc::clone(&metrics);

    // Mock daemon server thread
    let server_handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_nodelay(true).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());

        // 1. Simula envio assíncrono imediato de evento Push do Gamepad vindo do daemon
        let push_event = ServerResponse::GamepadStateResponse(GamepadState {
            available: true,
            input_enabled: false,
            error: None,
            sequence: 101,
            transitions: vec![ButtonTransition {
                sequence: 101,
                buttons: BTN_MASK_A | BTN_MASK_DPAD_UP,
            }],
            overflow: false,
            listening: true,
            device_name: "odroidgo2-joypad".into(),
            buttons: BTN_MASK_A | BTN_MASK_DPAD_UP,
            left_x: 12000,
            left_y: -8000,
            right_x: 0,
            right_y: 0,
        });
        let mut push_json = serde_json::to_string(&push_event).unwrap();
        push_json.push('\n');
        stream.write_all(push_json.as_bytes()).unwrap();
        stream.flush().unwrap();

        // 2. Lê requisições do cliente (ex: Ping ou GetMetrics)
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 0 {
            let cmd: ClientCommand = serde_json::from_str(line.trim()).unwrap();
            line.clear();
            match cmd {
                ClientCommand::Ping => {
                    let mut resp = serde_json::to_string(&ServerResponse::Pong).unwrap();
                    resp.push('\n');
                    stream.write_all(resp.as_bytes()).unwrap();
                    stream.flush().unwrap();
                }
                ClientCommand::GetMetrics => {
                    let mut resp = serde_json::to_string(&ServerResponse::Metrics(SystemMetrics {
                        timestamp: 12345,
                        cpu_usage: 15.0,
                        cpu_freq_mhz: vec![1200],
                        cpu_temp_c: Some(45.0),
                        ram_used_mb: 256,
                        ram_total_mb: 1024,
                        mali_gpu_load: None,
                        mali_gpu_freq_mhz: None,
                        active_game: None,
                        running_processes: vec![],
                        fb_width: 640,
                        fb_height: 480,
                        fb_bpp: 16,
                        fb_format: 1,
                    }))
                    .unwrap();
                    resp.push('\n');
                    stream.write_all(resp.as_bytes()).unwrap();
                    stream.flush().unwrap();
                    break;
                }
                _ => {}
            }
        }
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nodelay(true).unwrap();
    let client_reader = BufReader::new(client_stream.try_clone().unwrap());

    // Executa a sessão de controle do Studio em thread separada
    let session_handle = std::thread::spawn(move || {
        ConnectionManager::run_control_session(
            client_stream,
            client_reader,
            &cmd_rx,
            &status,
            &metrics,
            &logs,
            &deploy_status,
            &proc_result,
            &diag,
            &bytes_tx,
            &bytes_rx,
            &gamepad_listening,
            &gamepad_state,
            &gamepad_supported,
        );
    });

    // Aguarda atualização reativa do gamepad pelo evento push (< 1ms)
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        let st = gamepad_state_clone.lock().unwrap();
        if st.sequence == 101 && st.buttons == (BTN_MASK_A | BTN_MASK_DPAD_UP) {
            assert_eq!(st.left_x, 12000);
            assert_eq!(st.left_y, -8000);
            break;
        }
        drop(st);
        std::thread::sleep(Duration::from_millis(10));
    }

    let final_st = gamepad_state_clone.lock().unwrap().clone();
    assert_eq!(final_st.sequence, 101);
    assert_eq!(final_st.buttons, BTN_MASK_A | BTN_MASK_DPAD_UP);
    assert_eq!(final_st.left_x, 12000);
    assert_eq!(final_st.left_y, -8000);

    // Envia comando de telemetria GetMetrics através do canal
    cmd_tx.send(ClientCommand::GetMetrics).unwrap();

    let start_metrics = Instant::now();
    while start_metrics.elapsed() < Duration::from_secs(2) {
        let m = metrics_clone.lock().unwrap();
        if m.is_some() {
            assert_eq!(m.as_ref().unwrap().ram_total_mb, 1024);
            break;
        }
        drop(m);
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(metrics_clone.lock().unwrap().is_some());

    // Encerra sessão
    drop(cmd_tx);
    server_handle.join().unwrap();
    session_handle.join().unwrap();
}

#[test]
fn test_bgra_to_rgba_inplace_matches_legacy_conversion() {
    use r36s_studio::connection::ConnectionManager;

    // Função de referência idêntica à implementação anterior
    fn legacy_bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
        let mut rgba = bgra.to_vec();
        for chunk in rgba.chunks_exact_mut(4) {
            let b = chunk[0];
            let r = chunk[2];
            chunk[0] = r;
            chunk[2] = b;
            chunk[3] = 255;
        }
        rgba
    }

    // 1. Teste de pixels específicos (Blue, Red, Green, White, Black com alpha variado)
    let sample_bgra = vec![
        255, 0, 0, 100, // Azul puro -> deve virar Vermelho=0, Azul=255, Alpha=255
        0, 0, 255, 50,  // Vermelho puro -> deve virar Vermelho=255, Azul=0, Alpha=255
        0, 255, 0, 0,   // Verde puro -> deve virar Verde=255, Alpha=255
        10, 20, 30, 40, // Misto
    ];

    let expected = legacy_bgra_to_rgba(&sample_bgra);
    let mut actual = sample_bgra.clone();
    ConnectionManager::bgra_to_rgba_inplace(&mut actual);

    assert_eq!(actual, expected);
    assert_eq!(&actual[0..4], &[0, 0, 255, 255]);
    assert_eq!(&actual[4..8], &[255, 0, 0, 255]);
    assert_eq!(&actual[8..12], &[0, 255, 0, 255]);
    assert_eq!(&actual[12..16], &[30, 20, 10, 255]);

    // 2. Teste em buffer de tamanho realista (640x480 BGRA8888 = 1.228.800 bytes)
    let total_bytes = 640 * 480 * 4;
    let mut large_bgra = vec![0u8; total_bytes];
    for (i, byte) in large_bgra.iter_mut().enumerate() {
        *byte = (i * 37 % 256) as u8;
    }

    let expected_large = legacy_bgra_to_rgba(&large_bgra);
    let mut actual_large = large_bgra.clone();
    ConnectionManager::bgra_to_rgba_inplace(&mut actual_large);

    assert_eq!(actual_large, expected_large);
    // Também valida que ConnectionManager::bgra_to_rgba é consistente
    let helper_large = ConnectionManager::bgra_to_rgba(&large_bgra);
    assert_eq!(helper_large, expected_large);
}

#[test]
fn test_zero_allocation_lz4_decompress_and_in_place_transform() {
    use r36s_studio::connection::ConnectionManager;

    let width = 64;
    let height = 64;
    let raw_len = width * height * 4;

    // Cria frame BGRA sintético
    let mut raw_frame = vec![0u8; raw_len];
    for (i, byte) in raw_frame.iter_mut().enumerate() {
        *byte = ((i * 17) & 0xFF) as u8;
    }

    // Comprime com LZ4
    let mut comp_buf = vec![0u8; lz4_flex::block::get_maximum_output_size(raw_len)];
    let comp_sz = lz4_flex::block::compress_into(&raw_frame, &mut comp_buf).unwrap();

    // Buffer de descompressão pré-alocado (reutilizável)
    let mut reusable_decomp_buf = vec![0u8; raw_len];
    let decomp_sz = lz4_flex::block::decompress_into(
        &comp_buf[..comp_sz],
        &mut reusable_decomp_buf[..raw_len],
    )
    .unwrap();
    assert_eq!(decomp_sz, raw_len);
    assert_eq!(&reusable_decomp_buf[..], &raw_frame[..]);

    // Converte in-place
    ConnectionManager::bgra_to_rgba_inplace(&mut reusable_decomp_buf);

    // Valida propriedades RGBA e Alpha fixado em 255
    for chunk in reusable_decomp_buf.chunks_exact(4) {
        assert_eq!(chunk[3], 255);
    }
}

