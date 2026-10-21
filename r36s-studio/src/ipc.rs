use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const IPC_VERSION: &str = "1.0";
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024; // 1 MiB
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcErrorCode {
    InvalidRequest = 1001,
    UnsupportedVersion = 1002,
    PayloadTooLarge = 1003,
    UnknownOperation = 1004,
    NotConnected = 2001,
    CaptureTimeout = 2002,
    FramebufferUnavailable = 2003,
    CommandRejected = 3001,
    CommandTimeout = 3002,
    CommandIndeterminate = 3003,
    CapabilityNotSupported = 3004,
    MacroFailed = 4001,
    InternalError = 5000,
}

impl IpcErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            IpcErrorCode::InvalidRequest => "INVALID_REQUEST",
            IpcErrorCode::UnsupportedVersion => "UNSUPPORTED_VERSION",
            IpcErrorCode::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            IpcErrorCode::UnknownOperation => "UNKNOWN_OPERATION",
            IpcErrorCode::NotConnected => "NOT_CONNECTED",
            IpcErrorCode::CaptureTimeout => "CAPTURE_TIMEOUT",
            IpcErrorCode::FramebufferUnavailable => "FRAMEBUFFER_UNAVAILABLE",
            IpcErrorCode::CommandRejected => "COMMAND_REJECTED",
            IpcErrorCode::CommandTimeout => "COMMAND_TIMEOUT",
            IpcErrorCode::CommandIndeterminate => "COMMAND_INDETERMINATE",
            IpcErrorCode::CapabilityNotSupported => "CAPABILITY_NOT_SUPPORTED",
            IpcErrorCode::MacroFailed => "MACRO_FAILED",
            IpcErrorCode::InternalError => "INTERNAL_ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpcError {
    pub code: u32,
    pub name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl IpcError {
    pub fn new(code: IpcErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code as u32,
            name: code.as_str().to_string(),
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(
        code: IpcErrorCode,
        message: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            code: code as u32,
            name: code.as_str().to_string(),
            message: message.into(),
            data: Some(data),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpcRequest {
    pub version: String,
    pub id: String,
    pub operation: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl IpcRequest {
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpcResponse {
    pub version: String,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}

impl IpcResponse {
    pub fn success(id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            version: IPC_VERSION.to_string(),
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: impl Into<String>, error: IpcError) -> Self {
        Self {
            version: IPC_VERSION.to_string(),
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }
}

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenshotPngResult {
    pub width: u32,
    pub height: u32,
    pub frame_index: u64,
    pub captured_at_ms: u64,
    pub png_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiagnosticsResult {
    pub timestamp_ms: u64,
    pub is_connected: bool,
    pub connection_state: String,
    pub fps: f32,
    pub session_generation: u64,
    pub metrics: Option<crate::protocol::SystemMetrics>,
    pub telemetry: Option<crate::protocol::TelemetrySnapshot>,
    pub gamepad: Option<crate::protocol::GamepadState>,
    pub sandbox_supported: bool,
    pub sandbox_running: bool,
    pub sandbox_pid: Option<u32>,
    pub sandbox_exit_code: Option<i32>,
    pub sandbox_uptime_ms: Option<u64>,
    pub sandbox_recent_logs: Option<String>,
    #[serde(default)]
    pub sandbox_term_signal: Option<i32>,
    #[serde(default)]
    pub sandbox_stdout_tail: Option<String>,
    #[serde(default)]
    pub sandbox_stderr_tail: Option<String>,
    #[serde(default)]
    pub sandbox_restore_error: Option<String>,
    #[serde(default)]
    pub sandbox_phase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SendCommandResult {
    pub status: String,
    pub message: Option<String>,
    pub response: Option<crate::protocol::ServerResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunMacroResult {
    pub success: bool,
    pub steps_completed: usize,
    pub error: Option<String>,
}

pub const ALLOWED_OPERATIONS: &[&str] = &[
    "GetScreenshotPng",
    "GetDiagnostics",
    "SendCommand",
    "RunMacro",
    "Ping",
];

pub fn validate_request_raw(raw: &[u8]) -> Result<IpcRequest, IpcError> {
    if raw.len() > MAX_REQUEST_BYTES {
        return Err(IpcError::new(
            IpcErrorCode::PayloadTooLarge,
            format!(
                "Tamanho do payload ({} bytes) excede o limite máximo de {} bytes",
                raw.len(),
                MAX_REQUEST_BYTES
            ),
        ));
    }

    let req: IpcRequest = match serde_json::from_slice(raw) {
        Ok(r) => r,
        Err(e) => {
            return Err(IpcError::new(
                IpcErrorCode::InvalidRequest,
                format!("JSON inválido na requisição: {}", e),
            ));
        }
    };

    if req.version != IPC_VERSION {
        return Err(IpcError::new(
            IpcErrorCode::UnsupportedVersion,
            format!(
                "Versão '{}' não suportada. Versão requerida: '{}'",
                req.version, IPC_VERSION
            ),
        ));
    }

    if req.id.trim().is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidRequest,
            "Campo 'id' não pode ser vazio",
        ));
    }

    if !ALLOWED_OPERATIONS.contains(&req.operation.as_str()) {
        return Err(IpcError::new(
            IpcErrorCode::UnknownOperation,
            format!("Operação desconhecida: '{}'", req.operation),
        ));
    }

    Ok(req)
}

pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let target_len = (data.len() + 2) / 3 * 4;
    let mut out = Vec::with_capacity(target_len);
    let chunks = data.chunks_exact(3);
    let rem = chunks.remainder();

    for chunk in chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(TABLE[((n >> 18) & 0x3F) as usize]);
        out.push(TABLE[((n >> 12) & 0x3F) as usize]);
        out.push(TABLE[((n >> 6) & 0x3F) as usize]);
        out.push(TABLE[(n & 0x3F) as usize]);
    }

    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(TABLE[((n >> 18) & 0x3F) as usize]);
            out.push(TABLE[((n >> 12) & 0x3F) as usize]);
            out.push(b'=');
            out.push(b'=');
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(TABLE[((n >> 18) & 0x3F) as usize]);
            out.push(TABLE[((n >> 12) & 0x3F) as usize]);
            out.push(TABLE[((n >> 6) & 0x3F) as usize]);
            out.push(b'=');
        }
        _ => {}
    }

    // SAFETY: TABLE e b'=' são estritamente caracteres ASCII válidos (< 128)
    unsafe { String::from_utf8_unchecked(out) }
}

pub fn base64_decode(data: &str) -> Option<Vec<u8>> {
    let bytes = data.as_bytes();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if bytes.len() % 4 != 0 {
        return None;
    }

    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let decode_val = |b: u8| -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some((b - b'A') as u32),
            b'a'..=b'z' => Some((b - b'a' + 26) as u32),
            b'0'..=b'9' => Some((b - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    };

    for chunk in bytes.chunks_exact(4) {
        let b0 = decode_val(chunk[0])?;
        let b1 = decode_val(chunk[1])?;
        let b2 = decode_val(chunk[2])?;
        let b3 = decode_val(chunk[3])?;

        let n = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;

        out.push(((n >> 16) & 0xFF) as u8);
        if chunk[2] != b'=' {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if chunk[3] != b'=' {
            out.push((n & 0xFF) as u8);
        }
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_request_response_roundtrip() {
        let req = IpcRequest {
            version: IPC_VERSION.to_string(),
            id: "req-123".to_string(),
            operation: "GetDiagnostics".to_string(),
            payload: serde_json::json!({}),
        };
        let serialized = serde_json::to_string(&req).unwrap();
        let validated = validate_request_raw(serialized.as_bytes()).unwrap();
        assert_eq!(validated, req);

        let resp = IpcResponse::success("req-123", serde_json::json!({"fps": 60.0}));
        let line = resp.to_json_line().unwrap();
        assert!(line.ends_with('\n'));
        let parsed: IpcResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed, resp);
    }

    #[test]
    fn test_ipc_validation_rejections() {
        // Unsupported version
        let raw = serde_json::json!({
            "version": "9.9",
            "id": "1",
            "operation": "GetDiagnostics",
            "payload": {}
        });
        let err = validate_request_raw(&serde_json::to_vec(&raw).unwrap()).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::UnsupportedVersion as u32);

        // Unknown operation
        let raw = serde_json::json!({
            "version": IPC_VERSION,
            "id": "1",
            "operation": "DropDatabase",
            "payload": {}
        });
        let err = validate_request_raw(&serde_json::to_vec(&raw).unwrap()).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::UnknownOperation as u32);

        // Payload too large
        let big_vec = vec![b'a'; MAX_REQUEST_BYTES + 10];
        let err = validate_request_raw(&big_vec).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::PayloadTooLarge as u32);

        // Invalid JSON
        let err = validate_request_raw(b"not json").unwrap_err();
        assert_eq!(err.code, IpcErrorCode::InvalidRequest as u32);
    }

    #[test]
    fn test_base64_roundtrip() {
        let cases: &[&[u8]] = &[
            b"",
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            b"\x00\xFF\xAA\x55\x12\x34\x56\x78\x9A\xBC\xDE\xF0",
        ];
        for case in cases {
            let enc = base64_encode(case);
            let dec = base64_decode(&enc).expect("Decodificação base64");
            assert_eq!(&dec[..], *case);
        }
    }
}
