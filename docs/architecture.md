# 🏛️ Architecture & Protocols

This document details the software architecture of **R36S DevBridge**, the breakdown between the console daemon (`r36s-agent`) and the PC desktop application (`r36s-studio`), and the exact binary wire protocols used for multi-channel communication over USB.

---

## 🏗️ High-Level Architecture

The system consists of two primary applications communicating over a point-to-point USB Ethernet gadget network (`192.168.7.0/24`):

```
┌──────────────────────────────────────────────┐        USB-C OTG Cable        ┌──────────────────────────────────────────────┐
│           R36S Handheld Console              │  (g_ether / rndis_host)       │               Host Computer (PC)             │
│                 (ArkOS)                      │◄─────────────────────────────►│            (Linux x86_64 / ./app)            │
│                                              │                               │                                              │
│  ┌────────────────────────────────────────┐  │                               │  ┌────────────────────────────────────────┐  │
│  │         r36s-agent (Daemon)            │  │                               │  │        r36s-studio (GUI App)           │  │
│  │                                        │  │                               │  │                                        │  │
│  │  • Video Thread (DRM/KMS + pread)      │──┼─── TCP 7366 [VID0 Stream] ────┼─►│  • Viewport (eframe/egui + wgpu)       │  │
│  │  • Telemetry Thread (/proc, /sys)      │──┼─── TCP 7365 [Status Stream] ──┼─►│  • Realtime Metrics & Sparklines       │  │
│  │  • Control Listener (JSON-RPC)         │◄─┼─── TCP 7367 [Commands/PIDs] ──┼──│  • Remote Process Manager & Terminate  │  │
│  │  • File Deploy Listener (Chunks)       │◄─┼─── TCP 7368 [Binary Transfer] ─┼──│  • Drag & Drop File Deployer           │  │
│  └────────────────────────────────────────┘  │                               │  └────────────────────────────────────────┘  │
│                                              │                               │                                              │
└──────────────────────────────────────────────┘                               └──────────────────────────────────────────────┘
```

---

## 🔌 Networking: USB Ethernet Gadget Architecture

Instead of relying on slow serial USB (CDC-ACM) or Wi-Fi (which requires an external USB Wi-Fi dongle on the R36S), DevBridge leverages the Linux kernel **USB Gadget subsystem**:

- **Driver:** `dwc2` (DesignWare Core USB 2.0 controller) running in **peripheral mode**.
- **Function:** `g_ether` (Ethernet over USB Gadget) implementing CDC Ethernet / RNDIS.
- **R36S IP Address:** Statically assigned to `192.168.7.2/24` via udev rule (`/etc/udev/rules.d/99-usb0-ip.rules`).
- **PC IP Address:** Dynamically configured by `r36s-studio` on startup to `192.168.7.1/24` using `ip addr add` or NetworkManager.
- **Latency:** Typical ping latency over this interface is **0.25 ms – 0.40 ms**, with throughput reaching the full theoretical limit of USB 2.0 High Speed (~30–35 MB/s).

---

## 📡 Port Multiplexing & Channel Separation

To prevent heavy video frames from starving critical commands or telemetry, DevBridge avoids multiplexing all data over a single stream. Instead, it utilizes four dedicated TCP ports:

| Port | Direction | Channel | Protocol Format | Description |
| :---: | :---: | :---: | :---: | :--- |
| **`7365`** | R36S ➔ PC | Telemetry & Health | JSON Stream (Newline Delimited) | CPU load, thermal readings, battery state, RAM usage, Mali GPU load, and top process stats. Broadcast every 500ms. |
| **`7366`** | R36S ➔ PC | Video Mirroring | Custom Binary Protocol (`VID0`) | Sub-millisecond compressed frames, frame-diffing, keyframes, dynamic resolution metadata. Single-client preemptive. |
| **`7367`** | PC ⇄ R36S | Remote Control | UTF-8 JSON Commands & Responses | Process management (`kill_process`, `restart_es`, `reboot`, `stream_control`). Bidirectional synchronous RPC. |
| **`7368`** | PC ➔ R36S | File Transfer | Binary Chunk Streaming (with SHA-256) | Push ROMs, native port executables, and scripts directly to `/roms` with atomic write and checksum verification. |

---

## 🎞️ The `VID0` Video Protocol Specification

The video protocol is a compact, 18-byte packed binary header followed immediately by compressed payload bytes.

### 1. Packet Layout (Little-Endian)

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       Magic: "VID0"                           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        Frame Number                           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|            Width              |            Height             |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|  Pixel Format |  Flags (Type) |        Compressed Size        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       Compressed Size (cont)  |           Reserved            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        Payload Bytes                          |
|                             ...                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### 2. Header Fields Breakdown

| Offset | Type | Field Name | Description |
| :---: | :---: | :--- | :--- |
| `0..4` | `[u8; 4]` | `magic` | Always ASCII `"VID0"` (`[0x56, 0x49, 0x44, 0x30]`). Enforces stream alignment. |
| `4..8` | `u32` | `frame_num` | Incrementing frame index. Used to detect dropped packets. |
| `8..10` | `u16` | `width` | Screen/Frame width in pixels (typically `640`, or `320` for low-res emulators). |
| `10..12`| `u16` | `height` | Screen/Frame height in pixels (typically `480`, or `240`). |
| `12` | `u8` | `pixel_format` | `1` = Raw RGB888, `2` = Raw RGBA8888, `3` = Raw RGB565. |
| `13` | `u8` | `flags` | `0` = Delta Frame (LZ4 compressed diff against previous frame).<br>`1` = Keyframe (LZ4 compressed full screen).<br>`2` = Raw uncompressed.<br>`3` = Heartbeat / Mock. |
| `14..18`| `u32` | `compressed_size` | Length in bytes of the incoming compressed payload. |
| `18..20`| `[u8; 2]`| `reserved` | 2 bytes reserved for future 60Hz timestamps or color space flags. |

> **⚠️ Critical Protocol Invariant:**
> The header struct in Rust is annotated with `#[repr(C, packed)]`. The size is **exactly 20 bytes**.
> Any diagnostic script or custom client reading from port `7366` must unpack with the struct format:
> `struct.unpack("<4sIHHBBI2s", raw_header)`
> Reversing the position of `compressed_size` and `reserved` will corrupt protocol alignment and cause stream desynchronization.

---

## ⚡ Compression & Delta Differencing Strategy

To keep bandwidth low and CPU overhead on the handheld minimal:

1. **Periodic Keyframes:** The agent transmits a full keyframe (`flags = 1`) every 60 frames (~2 seconds at 30 FPS). This guarantees that whenever a new studio client connects or drops a frame, the display synchronizes within 2 seconds without requiring client-to-server feedback.
2. **LZ4 Acceleration:** The RK3326 Cortex-A35 has limited vector performance. Traditional algorithms like zlib or PNG compression are far too slow, consuming 80%+ of a core. LZ4 compresses a 640x480 RGBA frame (~1.22 MB) down to 20 KB – 120 KB in less than **1.8 milliseconds**.
3. **Sub-millisecond Frame Skip:** If the framebuffer contents are identical to the previously captured frame, the agent transmits nothing, keeping network activity near zero during static screens.

---

## 🔄 Client Preemption & Socket Lifecycle (Port 7366)

The video stream on port 7366 is high throughput (~5–15 Mbps). Having multiple clients connected simultaneously would overwhelm the memory and CPU of the handheld. 

To prevent zombie or abandoned sockets (e.g. from an app crash or quick restart) from locking the video port:

1. The server maintains an atomic session identifier: `active_video_session: Arc<AtomicU32>`.
2. Whenever a new TCP client connects to port `7366`, `active_video_session` is incremented.
3. The active video streaming loop checks `current_session == active_video_session.load(Ordering::Relaxed)`.
4. If a newer connection arrives, the old streaming thread **immediately breaks and terminates its socket**, relinquishing the video capture pipe to the new client without requiring an agent restart.
