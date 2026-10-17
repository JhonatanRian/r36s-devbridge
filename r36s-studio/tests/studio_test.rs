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
