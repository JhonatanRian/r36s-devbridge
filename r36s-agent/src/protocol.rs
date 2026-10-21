use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "0.2.0";

pub const CONTROL_PORT: u16 = 7365;
pub const VIDEO_PORT: u16 = 7366;

pub const VIDEO_MAGIC: [u8; 4] = *b"VID0";
pub const FORMAT_RGB565_LZ4: u8 = 1;
pub const FORMAT_RGBA8888_LZ4: u8 = 2;
#[allow(dead_code)]
pub const FORMAT_RAW_RGB565: u8 = 3;
pub const FORMAT_BGRA8888_LZ4: u8 = 4;

// Flags de transmissão de quadro de vídeo
pub const FRAME_FLAG_KEYFRAME: u8 = 0;
pub const FRAME_FLAG_UNCHANGED: u8 = 1;
pub const FRAME_FLAG_FB_UNAVAILABLE: u8 = 2;
#[allow(dead_code)]
pub const FRAME_FLAG_MOCK: u8 = 3;
pub const FRAME_FLAG_DELTA: u8 = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
    pub memory_mb: u64,
    #[serde(default)]
    pub cpu_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemMetrics {
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub cpu_usage: f32,
    #[serde(default)]
    pub cpu_freq_mhz: Vec<u32>,
    pub cpu_temp_c: Option<f32>,
    #[serde(default)]
    pub ram_used_mb: u64,
    #[serde(default)]
    pub ram_total_mb: u64,
    pub mali_gpu_load: Option<f32>,
    pub mali_gpu_freq_mhz: Option<u32>,
    pub active_game: Option<String>,
    #[serde(default)]
    pub running_processes: Vec<ProcessInfo>,
    #[serde(default)]
    pub fb_width: u16,
    #[serde(default)]
    pub fb_height: u16,
    #[serde(default)]
    pub fb_bpp: u8,
    #[serde(default)]
    pub fb_format: u8,
    #[serde(default)]
    pub cpu_core_usage: Vec<f32>,
    #[serde(default)]
    pub cma_total_mb: u64,
    #[serde(default)]
    pub cma_free_mb: u64,
    #[serde(default)]
    pub cma_available: bool,
    #[serde(default)]
    pub thermal_throttling: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum ClientCommand {
    Handshake {
        client_version: String,
        client_id: String,
    },
    Ping,
    GetMetrics,
    StartStream {
        target_fps: u32,
    },
    StopStream,
    DeployStart {
        relative_path: String,
        filename: String,
        total_size: u64,
        sha256: String,
        #[serde(default)]
        executable: Option<bool>,
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
    SetGamepadMonitor {
        enabled: bool,
    },
    GetGamepadState,
    SetGamepadInput {
        enabled: bool,
    },
    InjectGamepadInput {
        action: GamepadInputAction,
    },
    RunSandbox {
        path: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        exclusive_drm: bool,
    },
    StopSandbox {
        #[serde(default)]
        timeout_ms: u64,
    },
    GetSandboxStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum ServerResponse {
    HandshakeAck {
        agent_version: String,
        protocol_version: String,
        compatible: bool,
        #[serde(default)]
        capabilities: Vec<String>,
        fb_width: u16,
        fb_height: u16,
        fb_bpp: u8,
        fb_format: u8,
    },
    Pong,
    Metrics(SystemMetrics),
    StreamStatus {
        active: bool,
        fps: u32,
    },
    DeployProgress {
        status: String,
        message: String,
        chunk_index: u32,
    },
    DeploySuccess {
        path: String,
        sha256: String,
    },
    DeployError {
        error: String,
    },
    LogsResponse {
        log_type: String,
        content: String,
    },
    KillResult {
        pid: u32,
        success: bool,
        message: String,
    },
    RestartESResult {
        success: bool,
        message: String,
    },
    GamepadStateResponse(GamepadState),
    GamepadAck {
        success: bool,
        message: String,
    },
    SandboxStartResult {
        success: bool,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        error: Option<String>,
    },
    SandboxStopResult {
        success: bool,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        term_signal: Option<i32>,
        #[serde(default)]
        restore_error: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    SandboxStatus {
        running: bool,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        term_signal: Option<i32>,
        #[serde(default)]
        uptime_ms: u64,
        #[serde(default)]
        stdout_tail: String,
        #[serde(default)]
        stderr_tail: String,
        #[serde(default)]
        restore_error: Option<String>,
    },
    Error {
        message: String,
    },
}

// Constantes de bitmask dos botões do Gamepad do R36S
pub const BTN_MASK_DPAD_UP: u32 = 1 << 0;
pub const BTN_MASK_DPAD_DOWN: u32 = 1 << 1;
pub const BTN_MASK_DPAD_LEFT: u32 = 1 << 2;
pub const BTN_MASK_DPAD_RIGHT: u32 = 1 << 3;
pub const BTN_MASK_A: u32 = 1 << 4;
pub const BTN_MASK_B: u32 = 1 << 5;
pub const BTN_MASK_X: u32 = 1 << 6;
pub const BTN_MASK_Y: u32 = 1 << 7;
pub const BTN_MASK_L1: u32 = 1 << 8;
pub const BTN_MASK_R1: u32 = 1 << 9;
pub const BTN_MASK_L2: u32 = 1 << 10;
pub const BTN_MASK_R2: u32 = 1 << 11;
pub const BTN_MASK_SELECT: u32 = 1 << 12;
pub const BTN_MASK_START: u32 = 1 << 13;
pub const BTN_MASK_FN: u32 = 1 << 14;
pub const BTN_MASK_L3: u32 = 1 << 15;
pub const BTN_MASK_R3: u32 = 1 << 16;
pub const BTN_MASK_VOL_UP: u32 = 1 << 17;
pub const BTN_MASK_VOL_DOWN: u32 = 1 << 18;
pub const BTN_MASK_POWER: u32 = 1 << 19;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GamepadState {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub input_enabled: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub transitions: Vec<ButtonTransition>,
    #[serde(default)]
    pub overflow: bool,
    pub listening: bool,
    pub device_name: String,
    pub buttons: u32,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ButtonTransition {
    pub sequence: u64,
    pub buttons: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum GamepadInputAction {
    Button {
        mask: u32,
        pressed: bool,
    },
    Axis {
        left_x: i16,
        left_y: i16,
        right_x: i16,
        right_y: i16,
    },
    Pulse {
        mask: u32,
        duration_ms: u16,
    },
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
    pub fn new(
        frame_index: u32,
        width: u16,
        height: u16,
        format: u8,
        flags: u8,
        size: u32,
    ) -> Self {
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

    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_metrics_roundtrip_new_fields() {
        let metrics = SystemMetrics {
            timestamp: 123456789,
            cpu_usage: 45.5,
            cpu_freq_mhz: vec![1200, 1200, 1200, 1200],
            cpu_temp_c: Some(55.2),
            ram_used_mb: 250,
            ram_total_mb: 1024,
            mali_gpu_load: Some(12.5),
            mali_gpu_freq_mhz: Some(500),
            active_game: Some("RetroArch".to_string()),
            running_processes: Vec::new(),
            fb_width: 640,
            fb_height: 480,
            fb_bpp: 16,
            fb_format: 1,
            cpu_core_usage: vec![40.0, 50.0, 42.0, 50.0],
            cma_total_mb: 16,
            cma_free_mb: 12,
            cma_available: true,
            thermal_throttling: Some(false),
        };

        let json = serde_json::to_string(&metrics).expect("serialize metrics");
        let deserialized: SystemMetrics = serde_json::from_str(&json).expect("deserialize metrics");

        assert_eq!(deserialized.timestamp, 123456789);
        assert_eq!(deserialized.cpu_core_usage, vec![40.0, 50.0, 42.0, 50.0]);
        assert_eq!(deserialized.cma_total_mb, 16);
        assert_eq!(deserialized.cma_free_mb, 12);
        assert!(deserialized.cma_available);
        assert_eq!(deserialized.thermal_throttling, Some(false));
    }

    #[test]
    fn test_system_metrics_legacy_deserialization_defaults() {
        let legacy_json = r#"{
            "timestamp": 123456789,
            "cpu_usage": 35.0,
            "cpu_freq_mhz": [1000],
            "cpu_temp_c": 50.0,
            "ram_used_mb": 300,
            "ram_total_mb": 1024,
            "mali_gpu_load": null,
            "mali_gpu_freq_mhz": null,
            "active_game": null,
            "running_processes": [],
            "fb_width": 640,
            "fb_height": 480,
            "fb_bpp": 16,
            "fb_format": 1
        }"#;

        let deserialized: SystemMetrics = serde_json::from_str(legacy_json).expect("deserialize legacy metrics");
        assert!(deserialized.cpu_core_usage.is_empty());
        assert_eq!(deserialized.cma_total_mb, 0);
        assert_eq!(deserialized.cma_free_mb, 0);
        assert!(!deserialized.cma_available);
        assert_eq!(deserialized.thermal_throttling, None);
    }

    #[test]
    fn test_gamepad_input_action_pulse_serialization_roundtrip() {
        let action = GamepadInputAction::Pulse {
            mask: BTN_MASK_A | BTN_MASK_START,
            duration_ms: 80,
        };
        let json = serde_json::to_string(&action).expect("serialize pulse");
        assert_eq!(
            json,
            r#"{"type":"Pulse","payload":{"mask":8208,"duration_ms":80}}"#
        );
        let deserialized: GamepadInputAction =
            serde_json::from_str(&json).expect("deserialize pulse");
        assert_eq!(deserialized, action);
    }

    #[test]
    fn test_sandbox_commands_serialization_roundtrip() {
        let run_cmd = ClientCommand::RunSandbox {
            path: "/roms/ports/test.sh".to_string(),
            args: vec!["--windowed".to_string(), "arg2".to_string()],
            env: vec![("ENV_KEY".to_string(), "ENV_VAL".to_string())],
            cwd: Some("/roms/ports".to_string()),
            exclusive_drm: true,
        };
        let run_json = serde_json::to_string(&run_cmd).expect("serialize RunSandbox");
        let run_deserialized: ClientCommand =
            serde_json::from_str(&run_json).expect("deserialize RunSandbox");
        assert_eq!(run_deserialized, run_cmd);

        // Test with defaults (minimal JSON)
        let minimal_run_json = r#"{"type":"RunSandbox","payload":{"path":"/bin/ls"}}"#;
        let minimal_run: ClientCommand =
            serde_json::from_str(minimal_run_json).expect("deserialize minimal RunSandbox");
        match minimal_run {
            ClientCommand::RunSandbox {
                path,
                args,
                env,
                cwd,
                exclusive_drm,
            } => {
                assert_eq!(path, "/bin/ls");
                assert!(args.is_empty());
                assert!(env.is_empty());
                assert_eq!(cwd, None);
                assert!(!exclusive_drm);
            }
            _ => panic!("Expected RunSandbox variant"),
        }

        let stop_cmd = ClientCommand::StopSandbox { timeout_ms: 5000 };
        let stop_json = serde_json::to_string(&stop_cmd).expect("serialize StopSandbox");
        let stop_deserialized: ClientCommand =
            serde_json::from_str(&stop_json).expect("deserialize StopSandbox");
        assert_eq!(stop_deserialized, stop_cmd);

        let status_cmd = ClientCommand::GetSandboxStatus;
        let status_json = serde_json::to_string(&status_cmd).expect("serialize GetSandboxStatus");
        let status_deserialized: ClientCommand =
            serde_json::from_str(&status_json).expect("deserialize GetSandboxStatus");
        assert_eq!(status_deserialized, status_cmd);
    }

    #[test]
    fn test_sandbox_responses_serialization_roundtrip() {
        let start_res = ServerResponse::SandboxStartResult {
            success: true,
            pid: Some(4242),
            error: None,
        };
        let start_json = serde_json::to_string(&start_res).expect("serialize SandboxStartResult");
        let start_deserialized: ServerResponse =
            serde_json::from_str(&start_json).expect("deserialize SandboxStartResult");
        assert_eq!(start_deserialized, start_res);

        let stop_res = ServerResponse::SandboxStopResult {
            success: true,
            exit_code: Some(0),
            term_signal: None,
            restore_error: None,
            error: None,
        };
        let stop_json = serde_json::to_string(&stop_res).expect("serialize SandboxStopResult");
        let stop_deserialized: ServerResponse =
            serde_json::from_str(&stop_json).expect("deserialize SandboxStopResult");
        assert_eq!(stop_deserialized, stop_res);

        let status_res = ServerResponse::SandboxStatus {
            running: false,
            pid: Some(4242),
            exit_code: None,
            term_signal: Some(11), // SIGSEGV
            uptime_ms: 1250,
            stdout_tail: "initialized\nrendering frame 1\n".to_string(),
            stderr_tail: "Segmentation fault\n".to_string(),
            restore_error: None,
        };
        let status_json = serde_json::to_string(&status_res).expect("serialize SandboxStatus");
        let status_deserialized: ServerResponse =
            serde_json::from_str(&status_json).expect("deserialize SandboxStatus");
        assert_eq!(status_deserialized, status_res);
    }
}

