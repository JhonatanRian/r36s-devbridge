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
    let metrics = collector.collect();
    assert!(metrics.ram_total_mb > 0, "RAM total deve ser maior que 0");
    assert!(metrics.cpu_usage >= 0.0 && metrics.cpu_usage <= 100.0, "Uso de CPU deve estar entre 0 e 100");
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

    // Testa descompressão LZ4
    let expected_len = (w as usize) * (h as usize) * 2;
    let decompressed = lz4_flex::decompress(&comp_buf[..compressed_len], expected_len).expect("Falha ao descompactar LZ4");
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
