use crate::protocol::{FORMAT_RGB565_LZ4, FORMAT_RGBA8888_LZ4};
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::time::Instant;

pub struct FramebufferReader {
    file: Option<File>,
    mmap: Option<Mmap>,
    width: u16,
    height: u16,
    bpp: u8,
    line_length: usize,
    format: u8,
    prev_frame_hash: u64,
    mock_start: Instant,
    mock_frame: u32,
}

impl FramebufferReader {
    pub fn new() -> Self {
        let (file, mmap, width, height, bpp, line_length, format) = Self::try_open_fb0();
        Self {
            file,
            mmap,
            width,
            height,
            bpp,
            line_length,
            format,
            prev_frame_hash: 0,
            mock_start: Instant::now(),
            mock_frame: 0,
        }
    }

    fn try_open_fb0() -> (Option<File>, Option<Mmap>, u16, u16, u8, usize, u8) {
        if let Ok(file) = File::open("/dev/fb0") {
            // Tenta obter dimensões através de ioctl FBIOGET_VSCREENINFO se possível
            // Padrão R36S: 640x480, 16bpp RGB565 ou 32bpp
            let mut width = 640;
            let mut height = 480;
            let mut bpp = 16;
            let mut format = FORMAT_RGB565_LZ4;

            #[repr(C)]
            struct FbVarScreeninfo {
                xres: u32,
                yres: u32,
                xres_virtual: u32,
                yres_virtual: u32,
                xoffset: u32,
                yoffset: u32,
                bits_per_pixel: u32,
                grayscale: u32,
                // outros campos omitidos
                _padding: [u8; 128],
            }

            unsafe {
                use std::os::unix::io::AsRawFd;
                let mut vinfo: FbVarScreeninfo = std::mem::zeroed();
                // 0x4600 = FBIOGET_VSCREENINFO
                if libc::ioctl(file.as_raw_fd(), 0x4600, &mut vinfo) == 0 {
                    if vinfo.xres > 0 && vinfo.yres > 0 {
                        width = vinfo.xres as u16;
                        height = vinfo.yres as u16;
                        bpp = vinfo.bits_per_pixel as u8;
                        if bpp == 32 {
                            format = FORMAT_RGBA8888_LZ4;
                        }
                    }
                }
            }

            let expected_bytes = (width as usize) * (height as usize) * ((bpp as usize) / 8);
            if let Ok(mmap) = unsafe { MmapOptions::new().len(expected_bytes).map(&file) } {
                let line_length = (width as usize) * ((bpp as usize) / 8);
                return (Some(file), Some(mmap), width, height, bpp, line_length, format);
            }
        }

        // Mock / Fallback para quando rodar fora do console real
        (None, None, 640, 480, 16, 640 * 2, FORMAT_RGB565_LZ4)
    }

    pub fn capture_frame(&mut self) -> (u16, u16, u8, u8, Vec<u8>) {
        if let Some(ref mmap) = self.mmap {
            let total_bytes = (self.width as usize) * (self.height as usize) * ((self.bpp as usize) / 8);
            if mmap.len() >= total_bytes {
                let frame_slice = &mmap[0..total_bytes];
                
                // Calcula hash rápido para frame-diffing
                let hash = Self::simple_hash(frame_slice);
                let is_unchanged = hash == self.prev_frame_hash;
                self.prev_frame_hash = hash;

                let flags = if is_unchanged { 1 } else { 0 };
                let compressed = lz4_flex::compress(frame_slice);
                return (self.width, self.height, self.format, flags, compressed);
            }
        }

        // Gera frame mock caso /dev/fb0 não esteja disponível (ótimo para testes e emulação no PC)
        self.generate_mock_frame()
    }

    fn simple_hash(data: &[u8]) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        let step = 64; // Amostragem rápida a cada 64 bytes
        let mut i = 0;
        while i < data.len() {
            hash ^= data[i] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            i += step;
        }
        hash
    }

    fn generate_mock_frame(&mut self) -> (u16, u16, u8, u8, Vec<u8>) {
        self.mock_frame += 1;
        let w = self.width as usize;
        let h = self.height as usize;
        let mut raw = vec![0u8; w * h * 2]; // 16-bit RGB565

        let t = (self.mock_start.elapsed().as_millis() / 20) as usize;
        let box_x = (t % (w - 60)) as usize;
        let box_y = ((t * 2) % (h - 60)) as usize;

        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 2;
                let in_box = x >= box_x && x < box_x + 60 && y >= box_y && y < box_y + 60;
                let color_565: u16 = if in_box {
                    // Ciano/Verde brilhante para caixa em movimento
                    0x07E0
                } else {
                    // Gradiente de fundo azul/roxo
                    let r = ((x * 31) / w) as u16;
                    let g = ((y * 63) / h) as u16;
                    let b = 15u16;
                    (r << 11) | (g << 5) | b
                };

                raw[idx] = (color_565 & 0xFF) as u8;
                raw[idx + 1] = ((color_565 >> 8) & 0xFF) as u8;
            }
        }

        let compressed = lz4_flex::compress(&raw);
        (self.width, self.height, FORMAT_RGB565_LZ4, 0, compressed)
    }
}
