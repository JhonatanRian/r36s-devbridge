use serde::{Deserialize, Serialize};

pub const CONTROL_PORT: u16 = 7365;
pub const VIDEO_PORT: u16 = 7366;

pub const VIDEO_MAGIC: [u8; 4] = *b"VID0";
pub const FORMAT_RGB565_LZ4: u8 = 1;
pub const FORMAT_RGBA8888_LZ4: u8 = 2;
pub const FORMAT_RAW_RGB565: u8 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
    pub memory_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetrics {
    pub timestamp: u64,
    pub cpu_usage: f32,
    pub cpu_freq_mhz: Vec<u32>,
    pub cpu_temp_c: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub mali_gpu_load: Option<f32>,
    pub mali_gpu_freq_mhz: Option<u32>,
    pub active_game: Option<String>,
    pub running_processes: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum ClientCommand {
    Ping,
    GetMetrics,
    StartStream { target_fps: u32 },
    StopStream,
    DeployStart {
        relative_path: String,
        filename: String,
        total_size: u64,
        sha256: String,
    },
    DeployChunk {
        chunk_index: u32,
        data_base64: String,
    },
    DeployFinish,
    FetchLogs {
        log_type: String,
        lines: usize,
    },
    KillProcess {
        pid: u32,
        signal: i32,
    },
    RestartES,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum ServerResponse {
    Pong,
    Metrics(SystemMetrics),
    StreamStatus { active: bool, fps: u32 },
    DeployProgress { status: String, message: String },
    DeploySuccess { path: String, sha256: String },
    DeployError { error: String },
    LogsResponse { log_type: String, content: String },
    KillResult { pid: u32, success: bool, message: String },
    RestartESResult { success: bool, message: String },
    Error { message: String },
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VideoFrameHeader {
    pub magic: [u8; 4],
    pub frame_index: u32,
    pub width: u16,
    pub height: u16,
    pub format: u8,
    pub flags: u8,
    pub compressed_size: u32,
    pub reserved: [u8; 2],
}

impl VideoFrameHeader {
    pub fn new(frame_index: u32, width: u16, height: u16, format: u8, flags: u8, size: u32) -> Self {
        Self {
            magic: VIDEO_MAGIC,
            frame_index: frame_index.to_le(),
            width: width.to_le(),
            height: height.to_le(),
            format,
            flags,
            compressed_size: size.to_le(),
            reserved: [0, 0],
        }
    }

    pub fn to_bytes(&self) -> [u8; 20] {
        let mut bytes = [0u8; 20];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4..8].copy_from_slice(&self.frame_index.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.width.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.height.to_le_bytes());
        bytes[12] = self.format;
        bytes[13] = self.flags;
        bytes[14..18].copy_from_slice(&self.compressed_size.to_le_bytes());
        bytes[18..20].copy_from_slice(&self.reserved);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 20 {
            return None;
        }
        if &bytes[0..4] != &VIDEO_MAGIC {
            return None;
        }
        let frame_index = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let width = u16::from_le_bytes([bytes[8], bytes[9]]);
        let height = u16::from_le_bytes([bytes[10], bytes[11]]);
        let format = bytes[12];
        let flags = bytes[13];
        let compressed_size = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
        let reserved = [bytes[18], bytes[19]];
        Some(Self {
            magic: VIDEO_MAGIC,
            frame_index,
            width,
            height,
            format,
            flags,
            compressed_size,
            reserved,
        })
    }
}
