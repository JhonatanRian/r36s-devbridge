# 🕹️ R36S DevBridge (ArkOS Developer Toolkit)

[![Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-ArkOS%20%7C%20Linux%20x86__64-brightgreen.svg)]()
[![Hardware](https://img.shields.io/badge/Device-R36S%20%28RK3326%29-purple.svg)]()

A low-latency, real-time developer toolkit and remote studio for the **R36S retro handheld running ArkOS** (Rockchip RK3326 Cortex-A35, Mali-G31 MP2, 640x480 4:3 IPS, Linux 4.4 kernel).

With **DevBridge**, you plug a standard USB data cable between your computer and the R36S OTG port to achieve:
- **Real-Time Handheld Screen Mirroring**: 4:3 live viewport capturing the real hardware framebuffer (`/dev/fb0` at 32 bpp with sub-millisecond LZ4 compression and frame-diffing) at 30+ FPS.
- **Hardware Telemetry Dashboard**: Live monitoring of per-core CPU frequencies (RK3326 Cortex-A35), live CPU usage sparkline history, thermal sensor readings (°C), RAM usage, Mali GPU load/clock, and active running processes.
- **Direct Drag & Drop Wireless/USB Deploy**: Push ROMs, JARs, Ports, and shell scripts directly to `/roms` partition without removing the MicroSD card. Enforces chunk ordering and SHA-256 integrity verification.
- **Remote Process Manager**: Inspect running processes with per-PID CPU% and RAM usage, send `SIGTERM`/`SIGKILL` to hung games, and safely restart EmulationStation remotely.
- **Dynamic Log Streamer**: Real-time log capture for `EmulationStation`, `RetroArch`, `FreeJ2ME/Java`, `Ports`, active game stdout descriptors (`/proc/<pid>/fd/1`), and kernel `dmesg`.

---

## 📸 Overview

```
 ┌─────────────────────────────────────────────────────────┐
 │               R36S DevBridge Studio (PC)                │
 ├─────────────────┬───────────────────────────────────────┤
 │  Telemetry      │  Viewport 4:3 (Centered)              │
 │  • CPU & Spark  │  ┌─────────────────────────────────┐  │
 │  • RAM / GPU    │  │                                 │  │
 │  • Active Game  │  │    Live R36S Handheld Screen    │  │
 │  • Top PIDs     │  │          (640 x 480)            │  │
 │  • Throughput   │  │                                 │  │
 │                 │  └─────────────────────────────────┘  │
 ├─────────────────┴───────────────────────────────────────┤
 │  Status: 🟢 Connected to 192.168.7.2 (0.36ms latency)   │
 └─────────────────────────────────────────────────────────┘
```

---

## 📋 Requirements

1. **R36S Handheld Console** running **ArkOS** (Linux 4.4.189 aarch64).
2. **MicroSD Card** (TF1 / OS card or TF2 / ROMs card).
3. **USB Type-C Data Cable** (connected to the **OTG port** on the left of the R36S).
4. **PC / Laptop** running Linux (Debian, Ubuntu, Arch, Fedora, etc.).

---

## 🚀 Step-by-Step Installation Guide

Follow these steps carefully for first-time setup:

### Step 1: Copy Port Files to MicroSD Card

1. Power off your R36S handheld completely.
2. Eject the MicroSD card containing the **`EASYROMS`** partition (typically TF1 or TF2) and plug it into your computer.
3. Locate the `ports/` directory inside your ROMs partition (e.g., `/media/<user>/EASYROMS/ports/`).
4. Copy the contents of the `dist/ports/` folder from this repository into `ports/`:
   ```text
   EASYROMS/
   └── ports/
       ├── R36S DevBridge.sh
       └── r36s-devbridge/
           ├── patch_dtb_usb.py
           └── payload.zip
   ```
   *(Note: If you are running Linux with the SD card mounted, you can simply run `./install_to_sd.sh` as sudo to automate this).*
5. Safely eject the MicroSD card from your computer and insert it back into your R36S.

---

### Step 2: First-Time Setup on the R36S Console

1. Turn on the R36S console and boot into ArkOS (EmulationStation).
2. Navigate to the **Ports** menu and launch **`R36S DevBridge`**.
3. The screen will open a terminal interface and execute the setup:
   - **Remounts Partitions**: Ensures `/boot`, `/roms`, and `/` are writable (RW).
   - **Device Tree Patch (OTG Peripheral)**: Configures the USB OTG controller from host-only to peripheral gadget mode (creates a verified backup of `original.dtb` with SHA-256 metadata).
   - **USB Gadget Modules**: Registers `dwc2` and `g_ether` in `/etc/modules` and sets up persistent udev rules for `usb0` at `192.168.7.2/24`.
   - **Installs Daemon**: Extracts the static `r36s-agent` binary to `/usr/local/bin/` and registers `r36s-agent.service` in systemd.
4. **Important**: Since the Device Tree was patched, Linux requires a reboot.
   - When the installer notifies **`[AVISO] REINÍCIO DO CONSOLE NECESSÁRIO`**, press any key or wait for the prompt to exit.
5. **Reboot your R36S handheld** via the EmulationStation menu (`Start -> Quit -> Restart System`).

---

### Step 3: Connect via USB Cable and Launch Studio on PC

1. Take a USB-C data cable:
   - Plug one end into the **OTG port (the LEFT USB-C port on the R36S)**.
   - Plug the other end into your PC or laptop.
2. On your PC, navigate to this project folder and run the desktop app:
   ```bash
   ./app
   ```
3. The desktop studio will automatically detect the R36S USB interface (`rndis_host` / `g_ether`), apply the host network profile (`192.168.7.1`), and perform a protocol v0.2.0 handshake with the console.
4. The top header will turn green:
   ```text
   🟢 Conectado em 192.168.7.2 (< 1ms)
   ```
5. Click **`▶ Iniciar Stream`** on the top bar:
   - The R36S live screen will appear in crisp 4:3 pixel-art precision!
   - You can launch any game (RetroArch, Pico-8, PortMaster, FreeJ2ME, etc.) and it will stream in real time.

---

## 🧭 Handheld Port Manager (Rerun Menu)

Whenever you open **`R36S DevBridge`** from the Ports menu on the console in the future, it offers an interactive management menu:

```text
==========================================================
            R 3 6 S   D E V B R I D G E  (MENU)
==========================================================
Status: Service active and running
IP:     192.168.7.2 (usb0)

[1] Re-apply USB network and restart agent (Default)
[2] Reinstall / Update DevBridge
[3] Uninstall and restore original factory state
[4] Exit
==========================================================
Select an option (1-4) [auto 1 in 15s]:
```

- **Option 1**: Re-applies IP and restarts the background service without altering system files.
- **Option 2**: Re-extracts payload and updates the agent.
- **Option 3 (Full Rollback)**: Stops and uninstalls the service, removes `/etc/modules` entries, restores the original verified DTB from backup, and returns the OTG port to normal host mode.

---

## 🛠️ Building from Source

The project is built as an ultra-fast, dependency-free Rust workspace.

### Prerequisites

- Rust toolchain (`stable`):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- Target for AArch64 Musl (static Linux binary for the handheld):
  ```bash
  rustup target add aarch64-unknown-linux-musl
  ```
- Cross-compilation linker (Ubuntu/Debian):
  ```bash
  sudo apt update && sudo apt install -y gcc-aarch64-linux-gnu zip
  ```

### Master Build Script

Simply run the master build script:
```bash
./build.sh
```

This compiles:
1. `r36s-agent` statically for `aarch64-unknown-linux-musl`.
2. Packages `payload.zip` with manifest and SHA-256 hashes.
3. Compiles `r36s-studio` in release mode and outputs `./app`.
4. Updates `dist/ports/` ready for SD card transfer.

---

## 🧪 Automated Testing

Both crates include integration and unit tests:

```bash
# Handheld Agent tests
cd r36s-agent && cargo test

# Desktop Studio tests
cd ../r36s-studio && cargo test
```

---

## 📡 Protocol Specification (v0.2.0)

DevBridge operates over two dedicated TCP ports over the USB Ethernet gadget link (`192.168.7.2`):

| Port | Purpose | Transport |
|---|---|---|
| **7365** | Bidirectional Control & Telemetry | JSON Lines (`\n` delimited) |
| **7366** | Framebuffer Video Stream | Binary 20-byte `VID0` Header + LZ4 payload |

### Video Packet Header (`VID0`)
```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       Magic: 'V' 'I' 'D' '0'          |      Frame Index      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          Width (u16)          |          Height (u16)         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
| Format | Flags |          Compressed Size (u32)               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          Reserved (u16)       | ... Payload (LZ4 block) ...   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```
- **Format 4**: `BGRA8888_LZ4` (Native RK3326 32 bpp).
- **Format 1**: `RGB565_LZ4` (16 bpp mode).
- **Flags 1**: Unchanged frame diff (transmits `0` payload bytes to save USB bandwidth).

---

## 📄 License

Licensed under the MIT License. See [LICENSE](LICENSE) for details.
