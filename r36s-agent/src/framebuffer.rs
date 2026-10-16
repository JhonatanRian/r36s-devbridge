use crate::protocol::{
    FORMAT_BGRA8888_LZ4, FORMAT_RGB565_LZ4, FORMAT_RGBA8888_LZ4,
    FRAME_FLAG_FB_UNAVAILABLE, FRAME_FLAG_KEYFRAME, FRAME_FLAG_UNCHANGED,
};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;
use std::thread;
use std::time::{Duration, Instant};

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

// DRM IOCTLs e Constantes para captura direta de jogos acelerados (KMS/DRM / Mali GBM)
const DRM_IOCTL_GEM_CLOSE: libc::c_int = 0x40086409;
const DRM_IOCTL_SET_CLIENT_CAP: libc::c_int = 0x4010640d;
const DRM_IOCTL_DROP_MASTER: libc::c_int = 0x0000641f;
const DRM_IOCTL_MODE_GETPLANERESOURCES: libc::c_int = 0xc01064b5u32 as libc::c_int;
const DRM_IOCTL_MODE_GETPLANE: libc::c_int = 0xc02064b6u32 as libc::c_int;
const DRM_IOCTL_MODE_GETFB: libc::c_int = 0xc01c64adu32 as libc::c_int;
const DRM_IOCTL_PRIME_HANDLE_TO_FD: libc::c_int = 0xc00c642du32 as libc::c_int;
const DRM_CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;

// Constantes e estruturas para DMA-BUF CPU Synchronization (RFC/Linux dma-buf.h)
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 4;
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x40086200;

#[repr(C)]
struct DmaBufSync {
    flags: u64,
}

#[repr(C)]
struct DrmGemClose {
    handle: u32,
    pad: u32,
}

#[repr(C)]
struct DrmSetClientCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
struct DrmModeGetPlaneRes {
    plane_id_ptr: u64,
    count_planes: u32,
}

#[repr(C)]
struct DrmModeGetPlane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    gamma_size: u32,
    count_format_types: u32,
    format_type_ptr: u64,
}

#[repr(C)]
struct DrmModeFbCmd {
    fb_id: u32,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u32,
    depth: u32,
    handle: u32,
}

#[repr(C)]
struct DrmPrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum DrmState {
    ActiveKms,
    FbdevFallback,
}

pub struct DrmReader {
    plane_ids: Vec<u32>,
}

impl DrmReader {
    pub fn new() -> Self {
        Self {
            plane_ids: Vec::with_capacity(16),
        }
    }

    fn gem_close(fd: libc::c_int, handle: u32) {
        if handle > 0 {
            let mut arg = DrmGemClose { handle, pad: 0 };
            unsafe {
                libc::ioctl(fd, DRM_IOCTL_GEM_CLOSE, &mut arg);
            }
        }
    }

    /// Inspeciona se há processos KMS ativos no DRM (EmulationStation, RetroArch, ffplay, etc.)
    /// Apenas consulta debugfs em RAM sem abrir /dev/dri/card0.
    pub fn inspect_state() -> DrmState {
        let clients_txt = match std::fs::read_to_string("/sys/kernel/debug/dri/0/clients")
            .or_else(|_| std::fs::read_to_string("/sys/kernel/debug/dri/1/clients"))
        {
            Ok(txt) => txt,
            Err(_) => return DrmState::FbdevFallback,
        };

        let mut external_clients = 0usize;

        for line in clients_txt.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let cmd = parts[0];
                if cmd == "r36s-agent" {
                    continue;
                }
                external_clients += 1;
            }
        }

        if external_clients > 0 {
            DrmState::ActiveKms
        } else {
            DrmState::FbdevFallback
        }
    }

    pub fn capture(&mut self, packed_buffer: &mut Vec<u8>) -> Option<(u16, u16, u8)> {
        let card = File::open("/dev/dri/card0")
            .or_else(|_| File::open("/dev/dri/card1"))
            .ok()?;
        let fd = card.as_raw_fd();

        let cap = DrmSetClientCap {
            capability: DRM_CLIENT_CAP_UNIVERSAL_PLANES,
            value: 1,
        };
        unsafe {
            libc::ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &cap);
            libc::ioctl(fd, DRM_IOCTL_DROP_MASTER, 0);
        }

        let mut pres: DrmModeGetPlaneRes = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &mut pres) } < 0 || pres.count_planes == 0 {
            return None;
        }

        self.plane_ids.resize(pres.count_planes as usize, 0);
        pres.plane_id_ptr = self.plane_ids.as_mut_ptr() as u64;
        if unsafe { libc::ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &mut pres) } < 0 {
            return None;
        }

        let mut best_fb: Option<DrmModeFbCmd> = None;
        for &pid in &self.plane_ids {
            let mut plane: DrmModeGetPlane = unsafe { std::mem::zeroed() };
            plane.plane_id = pid;
            if unsafe { libc::ioctl(fd, DRM_IOCTL_MODE_GETPLANE, &mut plane) } == 0 {
                // VINCULAÇÃO CRÍTICA: O plano DEVE estar ativamente conectado a um CRTC ativo (em exibição na tela)
                if plane.crtc_id == 0 || plane.fb_id == 0 {
                    continue;
                }

                let mut fb: DrmModeFbCmd = unsafe { std::mem::zeroed() };
                fb.fb_id = plane.fb_id;
                if unsafe { libc::ioctl(fd, DRM_IOCTL_MODE_GETFB, &mut fb) } == 0 && fb.handle > 0 {
                    if fb.width >= 320 && fb.height >= 240 {
                        let cur_area = (fb.width as u64) * (fb.height as u64);
                        let best_area = best_fb
                            .as_ref()
                            .map(|b| (b.width as u64) * (b.height as u64))
                            .unwrap_or(0);
                        if cur_area >= best_area {
                            if let Some(old_fb) = best_fb.replace(fb) {
                                Self::gem_close(fd, old_fb.handle);
                            }
                        } else {
                            Self::gem_close(fd, fb.handle);
                        }
                    } else {
                        Self::gem_close(fd, fb.handle);
                    }
                }
            }
        }

        let fb = best_fb?;
        let width = fb.width as u16;
        let height = fb.height as u16;
        let pitch = fb.pitch as usize;
        let bpp = if fb.bpp > 0 { fb.bpp as u8 } else { 32 };
        let bytes_per_pixel = (bpp as usize) / 8;
        let row_bytes = (width as usize) * bytes_per_pixel;

        let mut prime: DrmPrimeHandle = unsafe { std::mem::zeroed() };
        prime.handle = fb.handle;
        prime.flags = 0;
        prime.fd = -1;

        let prime_res = unsafe { libc::ioctl(fd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &mut prime) };
        Self::gem_close(fd, fb.handle);

        if prime_res < 0 || prime.fd < 0 {
            return None;
        }

        let dma_fd = prime.fd;

        // 1. Sincronização explícita: espera até 10ms se a GPU estiver finalizando a renderização do buffer
        let mut pfd = libc::pollfd {
            fd: dma_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let _ = unsafe { libc::poll(&mut pfd, 1, 10) };

        // 2. Coerência de cache: sinaliza início de leitura da CPU no DMA-BUF (invalida D-cache ARM)
        let mut sync_start = DmaBufSync {
            flags: DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ,
        };
        let _ = unsafe { libc::ioctl(dma_fd, DMA_BUF_IOCTL_SYNC as _, &mut sync_start) };

        let mmap_len = (pitch * (height as usize) + 4095) & !4095;
        let map_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mmap_len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                dma_fd,
                0,
            )
        };

        if map_ptr == libc::MAP_FAILED {
            let mut sync_end = DmaBufSync {
                flags: DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ,
            };
            let _ = unsafe { libc::ioctl(dma_fd, DMA_BUF_IOCTL_SYNC as _, &mut sync_end) };
            unsafe {
                libc::close(dma_fd);
            }
            return None;
        }

        let total_frame_bytes = row_bytes * (height as usize);
        if packed_buffer.len() != total_frame_bytes {
            packed_buffer.resize(total_frame_bytes, 0);
        }

        let src_slice = unsafe { std::slice::from_raw_parts(map_ptr as *const u8, mmap_len) };
        if pitch == row_bytes {
            if total_frame_bytes <= src_slice.len() {
                packed_buffer.copy_from_slice(&src_slice[..total_frame_bytes]);
            }
        } else {
            for y in 0..(height as usize) {
                let src_off = y * pitch;
                let dst_off = y * row_bytes;
                if src_off + row_bytes <= src_slice.len() && dst_off + row_bytes <= packed_buffer.len() {
                    packed_buffer[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&src_slice[src_off..src_off + row_bytes]);
                }
            }
        }

        // 3. Finaliza leitura da CPU no DMA-BUF
        let mut sync_end = DmaBufSync {
            flags: DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ,
        };
        let _ = unsafe { libc::ioctl(dma_fd, DMA_BUF_IOCTL_SYNC as _, &mut sync_end) };

        unsafe {
            libc::munmap(map_ptr, mmap_len);
            libc::close(dma_fd);
        }

        // Fecha /dev/dri/card0 imediatamente
        drop(card);

        let format = if bpp == 32 {
            FORMAT_BGRA8888_LZ4
        } else {
            FORMAT_RGB565_LZ4
        };

        Some((width, height, format))
    }
}

pub struct FramebufferReader {
    drm_reader: DrmReader,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub line_length: usize,
    pub format: u8,
    pub is_real_fb: bool,
    pub fb_error: Option<String>,
    force_keyframe: bool,
    frame_counter: u64,
    packed_buffer: Vec<u8>,
    prev_frame_buffer: Vec<u8>,
    transition_start: Option<Instant>,
    mock_start: Instant,
    mock_frame: u32,
}

impl FramebufferReader {
    pub fn new() -> Self {
        Self {
            drm_reader: DrmReader::new(),
            width: 640,
            height: 480,
            bpp: 16,
            line_length: 640 * 2,
            format: FORMAT_RGB565_LZ4,
            is_real_fb: true,
            fb_error: None,
            force_keyframe: true,
            frame_counter: 0,
            packed_buffer: vec![0u8; 640 * 480 * 2],
            prev_frame_buffer: Vec::new(),
            transition_start: None,
            mock_start: Instant::now(),
            mock_frame: 0,
        }
    }

    pub fn reset_session(&mut self) {
        self.force_keyframe = true;
        self.prev_frame_buffer.clear();
        self.transition_start = None;
    }

    pub fn capture_frame(&mut self, comp_out: &mut Vec<u8>) -> (u16, u16, u8, u8, usize) {
        // 1. Prioridade absoluta: Captura direta do plane DRM KMS ativo com até 2 tentativas rápidas (1ms)
        for _ in 0..2 {
            if let Some((drm_w, drm_h, drm_fmt)) = self.drm_reader.capture(&mut self.packed_buffer) {
                if drm_w != self.width || drm_h != self.height || drm_fmt != self.format {
                    self.width = drm_w;
                    self.height = drm_h;
                    self.format = drm_fmt;
                    self.bpp = if drm_fmt == FORMAT_BGRA8888_LZ4 || drm_fmt == FORMAT_RGBA8888_LZ4 { 32 } else { 16 };
                    self.force_keyframe = true;
                    self.prev_frame_buffer.clear();
                }
                self.transition_start = None;
                self.is_real_fb = true;
                self.fb_error = None;
                return self.compress_and_return_frame(comp_out);
            }
            thread::sleep(Duration::from_millis(1));
        }

        // 2. Se a captura DRM não retornou após tentativa rápida:
        let state = DrmReader::inspect_state();
        if state == DrmState::ActiveKms {
            // Cliente KMS ativo em transição (ex.: troca rápida de modo ou app iniciando).
            // Reporta indisponibilidade transitória imediatamente sem falsificar UNCHANGED.
            return (self.width, self.height, self.format, FRAME_FLAG_FB_UNAVAILABLE, 0);
        }

        // 3. Fallback seguro para /dev/fb0 via pread() (menus de Ports com dialog, console fbcon)
        if let Some(frame) = self.capture_fb0_direct(comp_out) {
            self.transition_start = None;
            return frame;
        }

        if std::env::var("DEVBRIDGE_ALLOW_MOCK").is_ok() {
            self.generate_mock_frame(comp_out)
        } else {
            (self.width, self.height, self.format, FRAME_FLAG_FB_UNAVAILABLE, 0)
        }
    }

    fn capture_fb0_direct(&mut self, comp_out: &mut Vec<u8>) -> Option<(u16, u16, u8, u8, usize)> {
        let file = File::open("/dev/fb0").ok()?;
        let fd = file.as_raw_fd();

        let mut vinfo: FbVarScreeninfo = unsafe { std::mem::zeroed() };
        let mut finfo: FbFixScreeninfo = unsafe { std::mem::zeroed() };

        if unsafe { libc::ioctl(fd, 0x4600, &mut vinfo) } != 0 {
            return None;
        }
        let finfo_ok = unsafe { libc::ioctl(fd, 0x4602, &mut finfo) == 0 };

        let width = if vinfo.xres > 0 { vinfo.xres as u16 } else { 640 };
        let height = if vinfo.yres > 0 { vinfo.yres as u16 } else { 480 };
        let bpp = if vinfo.bits_per_pixel > 0 { vinfo.bits_per_pixel as u8 } else { 16 };
        let bytes_per_pixel = (bpp as usize) / 8;

        let line_length = if finfo_ok && finfo.line_length > 0 {
            finfo.line_length as usize
        } else {
            (width as usize) * bytes_per_pixel
        };

        let format = if bpp == 32 {
            if vinfo.red.offset == 16 && vinfo.blue.offset == 0 {
                FORMAT_BGRA8888_LZ4
            } else {
                FORMAT_RGBA8888_LZ4
            }
        } else {
            FORMAT_RGB565_LZ4
        };

        if width != self.width || height != self.height || format != self.format {
            self.width = width;
            self.height = height;
            self.bpp = bpp;
            self.format = format;
            self.line_length = line_length;
            self.force_keyframe = true;
            self.prev_frame_buffer.clear();
        }

        let row_bytes = (width as usize) * bytes_per_pixel;
        let total_frame_bytes = row_bytes * (height as usize);
        if self.packed_buffer.len() != total_frame_bytes {
            self.packed_buffer.resize(total_frame_bytes, 0);
            self.force_keyframe = true;
        }

        let yoffset = vinfo.yoffset as usize;
        let xoffset = vinfo.xoffset as usize;
        let start_offset = (yoffset * line_length + xoffset * bytes_per_pixel) as u64;

        if line_length == row_bytes && xoffset == 0 {
            file.read_exact_at(&mut self.packed_buffer[..total_frame_bytes], start_offset).ok()?;
        } else {
            for y in 0..(height as usize) {
                let src_off = start_offset + (y * line_length) as u64;
                let dst_off = y * row_bytes;
                file.read_exact_at(&mut self.packed_buffer[dst_off..dst_off + row_bytes], src_off).ok()?;
            }
        }

        self.is_real_fb = true;
        self.fb_error = None;
        Some(self.compress_and_return_frame(comp_out))
    }

    fn compress_and_return_frame(&mut self, comp_out: &mut Vec<u8>) -> (u16, u16, u8, u8, usize) {
        self.frame_counter = self.frame_counter.wrapping_add(1);

        // Emite keyframe periódico a cada ~60 quadros para garantir sincronia se um cliente reconectar
        let periodic_keyframe = (self.frame_counter % 60) == 0;

        // Comparação exata de 100% dos bytes (memcmp executado em < 25µs no AArch64 com saída antecipada imediata)
        let is_unchanged = !self.force_keyframe
            && !periodic_keyframe
            && (self.prev_frame_buffer.len() == self.packed_buffer.len())
            && (self.prev_frame_buffer == self.packed_buffer);

        self.force_keyframe = false;

        if is_unchanged {
            // Quadro 100% idêntico: flag = 1, payload comprimido vazio (0 bytes)
            return (self.width, self.height, self.format, FRAME_FLAG_UNCHANGED, 0);
        }

        // Garante que o buffer de saída tenha capacidade suficiente para o pior caso do LZ4
        let max_compressed_len = lz4_flex::block::get_maximum_output_size(self.packed_buffer.len());
        if comp_out.len() < max_compressed_len {
            comp_out.resize(max_compressed_len, 0);
        }

        let compressed_size = match lz4_flex::block::compress_into(&self.packed_buffer, comp_out) {
            Ok(size) => size,
            Err(_) => return (self.width, self.height, self.format, FRAME_FLAG_FB_UNAVAILABLE, 0),
        };

        // Troca os ponteiros de buffer em O(1) (< 5 ns) sem copiar 1.2 MB de RAM!
        // Na próxima captura, self.packed_buffer apontará para o buffer antigo e será sobrescrito diretamente.
        std::mem::swap(&mut self.packed_buffer, &mut self.prev_frame_buffer);

        (self.width, self.height, self.format, FRAME_FLAG_KEYFRAME, compressed_size)
    }

    fn generate_mock_frame(&mut self, comp_out: &mut Vec<u8>) -> (u16, u16, u8, u8, usize) {
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
                    0x07E0 // Verde brilhante
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

        let max_len = lz4_flex::block::get_maximum_output_size(raw.len());
        if comp_out.len() < max_len {
            comp_out.resize(max_len, 0);
        }
        let written = lz4_flex::block::compress_into(&raw, comp_out).unwrap_or(0);
        (self.width, self.height, FORMAT_RGB565_LZ4, 0, written)
    }
}
