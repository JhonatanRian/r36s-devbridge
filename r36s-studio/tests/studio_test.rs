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
