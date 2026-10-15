use crate::protocol::{
    FORMAT_BGRA8888_LZ4, FORMAT_RGB565_LZ4, FORMAT_RGBA8888_LZ4,
};
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::time::Instant;

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct FbBitfield {
    pub offset: u32,
    pub length: u32,
    pub msb_right: u32,
}

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct FbVarScreeninfo {
    pub xres: u32,
    pub yres: u32,
    pub xres_virtual: u32,
    pub yres_virtual: u32,
    pub xoffset: u32,
    pub yoffset: u32,
    pub bits_per_pixel: u32,
    pub grayscale: u32,
    pub red: FbBitfield,
    pub green: FbBitfield,
    pub blue: FbBitfield,
    pub transp: FbBitfield,
    pub nonstd: u32,
    pub activate: u32,
    pub height: u32,
    pub width: u32,
    pub accel_flags: u32,
    pub pixclock: u32,
    pub left_margin: u32,
    pub right_margin: u32,
    pub upper_margin: u32,
    pub lower_margin: u32,
    pub hsync_len: u32,
    pub vsync_len: u32,
    pub sync: u32,
    pub vmode: u32,
    pub rotate: u32,
    pub colorspace: u32,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct FbFixScreeninfo {
    pub id: [u8; 16],
    pub smem_start: usize,
    pub smem_len: u32,
    pub type_: u32,
    pub type_aux: u32,
    pub visual: u32,
    pub xpanstep: u16,
    pub ypanstep: u16,
    pub ywrapstep: u16,
    pub line_length: u32,
    pub mmio_start: usize,
    pub mmio_len: u32,
    pub accel: u32,
    pub capabilities: u16,
    pub reserved: [u16; 2],
}

pub struct FramebufferReader {
    file: Option<File>,
    mmap: Option<Mmap>,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub line_length: usize,
    pub format: u8,
    prev_frame_hash: u64,
    force_keyframe: bool,
    packed_buffer: Vec<u8>,
    mock_start: Instant,
    mock_frame: u32,
}

impl FramebufferReader {
    pub fn new() -> Self {
        match Self::try_open_fb0() {
            Ok((file, mmap, width, height, bpp, line_length, format)) => {
                let bytes_per_pixel = (bpp as usize) / 8;
                let packed_size = (width as usize) * (height as usize) * bytes_per_pixel;
                Self {
                    file: Some(file),
                    mmap: Some(mmap),
                    width,
                    height,
                    bpp,
                    line_length,
                    format,
                    prev_frame_hash: 0,
                    force_keyframe: true,
                    packed_buffer: vec![0u8; packed_size],
                    mock_start: Instant::now(),
                    mock_frame: 0,
                }
            }
            Err(e) => {
                eprintln!("[r36s-agent] Aviso: /dev/fb0 inacessível (usando mock framebuffer): {}", e);
                Self {
                    file: None,
                    mmap: None,
                    width: 640,
                    height: 480,
                    bpp: 16,
                    line_length: 640 * 2,
                    format: FORMAT_RGB565_LZ4,
                    prev_frame_hash: 0,
                    force_keyframe: true,
                    packed_buffer: vec![0u8; 640 * 480 * 2],
                    mock_start: Instant::now(),
                    mock_frame: 0,
                }
            }
        }
    }

    fn try_open_fb0() -> Result<(File, Mmap, u16, u16, u8, usize, u8), String> {
        let file = File::open("/dev/fb0").map_err(|e| format!("Falha ao abrir /dev/fb0: {}", e))?;
        let fd = file.as_raw_fd();

        let mut vinfo: FbVarScreeninfo = unsafe { std::mem::zeroed() };
        let mut finfo: FbFixScreeninfo = unsafe { std::mem::zeroed() };

        // 0x4600 = FBIOGET_VSCREENINFO, 0x4602 = FBIOGET_FSCREENINFO
        let vinfo_ok = unsafe { libc::ioctl(fd, 0x4600, &mut vinfo) == 0 };
        let finfo_ok = unsafe { libc::ioctl(fd, 0x4602, &mut finfo) == 0 };

        let width = if vinfo_ok && vinfo.xres > 0 { vinfo.xres as u16 } else { 640 };
        let height = if vinfo_ok && vinfo.yres > 0 { vinfo.yres as u16 } else { 480 };
        let bpp = if vinfo_ok && vinfo.bits_per_pixel > 0 { vinfo.bits_per_pixel as u8 } else { 16 };
        let bytes_per_pixel = (bpp as usize) / 8;

        let line_length = if finfo_ok && finfo.line_length > 0 {
            finfo.line_length as usize
        } else {
            (width as usize) * bytes_per_pixel
        };

        let format = if bpp == 32 {
            if vinfo_ok && vinfo.red.offset == 16 && vinfo.blue.offset == 0 {
                FORMAT_BGRA8888_LZ4
            } else {
                FORMAT_RGBA8888_LZ4
            }
        } else {
            FORMAT_RGB565_LZ4
        };

        // Tamanho de mapeamento: usa smem_len se disponível, ou dobro do framebuffer para double buffering
        let map_len = if finfo_ok && finfo.smem_len > 0 {
            finfo.smem_len as usize
        } else {
            line_length * (height as usize) * 2
        };

        let mmap = unsafe {
            MmapOptions::new()
                .len(map_len)
                .map(&file)
                .map_err(|e| format!("Falha no mmap de /dev/fb0 (tam {}): {}", map_len, e))?
        };

        println!(
            "[r36s-agent] Framebuffer /dev/fb0 aberto com sucesso: {}x{}, {} bpp, stride: {} bytes, formato: {}",
            width, height, bpp, line_length, format
        );

        Ok((file, mmap, width, height, bpp, line_length, format))
    }

    pub fn reset_session(&mut self) {
        self.force_keyframe = true;
        self.prev_frame_hash = 0;
    }

    pub fn capture_frame(&mut self) -> (u16, u16, u8, u8, Vec<u8>) {
        if self.file.is_some() && self.mmap.is_some() {
            let file = self.file.as_ref().unwrap();
            let mmap = self.mmap.as_ref().unwrap();

            // Consulta offsets atuais para lidar com page-flipping / double-buffering
            let mut vinfo: FbVarScreeninfo = unsafe { std::mem::zeroed() };
            let mut xoffset = 0usize;
            let mut yoffset = 0usize;
            if unsafe { libc::ioctl(file.as_raw_fd(), 0x4600, &mut vinfo) == 0 } {
                xoffset = vinfo.xoffset as usize;
                yoffset = vinfo.yoffset as usize;
            }

            let bpp_bytes = (self.bpp as usize) / 8;
            let row_bytes = (self.width as usize) * bpp_bytes;
            let start = yoffset * self.line_length + xoffset * bpp_bytes;
            let total_frame_bytes = row_bytes * (self.height as usize);

            if self.packed_buffer.len() != total_frame_bytes {
                self.packed_buffer.resize(total_frame_bytes, 0);
            }

            // Se stride for igual e offset zero, cópia direta; caso contrário, cópia linha por linha
            if self.line_length == row_bytes && start == 0 {
                if mmap.len() >= total_frame_bytes {
                    self.packed_buffer.copy_from_slice(&mmap[0..total_frame_bytes]);
                }
            } else {
                for y in 0..(self.height as usize) {
                    let src_offset = start + y * self.line_length;
                    let dst_offset = y * row_bytes;
                    if src_offset + row_bytes <= mmap.len() && dst_offset + row_bytes <= self.packed_buffer.len() {
                        self.packed_buffer[dst_offset..dst_offset + row_bytes]
                            .copy_from_slice(&mmap[src_offset..src_offset + row_bytes]);
                    }
                }
            }

            // Calcula hash rápido para frame-diffing
            let hash = Self::compute_hash(&self.packed_buffer);
            let is_unchanged = !self.force_keyframe && (hash == self.prev_frame_hash);
            self.prev_frame_hash = hash;
            self.force_keyframe = false;

            if is_unchanged {
                // Quadro inalterado: flag = 1, payload comprimido vazio para economizar banda/CPU
                return (self.width, self.height, self.format, 1, Vec::new());
            }

            let compressed = lz4_flex::compress(&self.packed_buffer);
            return (self.width, self.height, self.format, 0, compressed);
        }

        // Gera frame mock caso /dev/fb0 não esteja disponível (testes ou emulação no PC)
        self.generate_mock_frame()
    }

    fn compute_hash(data: &[u8]) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        let step = 32;
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
        let mut raw = vec![0u8; w * h * 2];

        let t = (self.mock_start.elapsed().as_millis() / 20) as usize;
        let box_x = (t % (w - 60)) as usize;
        let box_y = ((t * 2) % (h - 60)) as usize;

        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 2;
                let in_box = x >= box_x && x < box_x + 60 && y >= box_y && y < box_y + 60;
                let color_565: u16 = if in_box {
                    0x07E0 // Verde brillante
                } else {
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
