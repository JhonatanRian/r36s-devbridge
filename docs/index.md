# 📖 R36S DevBridge Technical Documentation

Welcome to the internal engineering and architectural documentation of **R36S DevBridge**, an open-source, ultra-low-latency remote development studio and telemetry suite designed specifically for the **R36S handheld gaming console** running **ArkOS** (Linux 4.4.189 on Rockchip RK3326).

This documentation was written by and for systems engineers, kernel hackers, emulator developers, and autonomous AI agents who need to understand the exact mechanics, quirks, hardware limitations, and engineering solutions implemented in this project.

---

## 🧭 Documentation Map

| Document | Topic & Focus Area |
| :--- | :--- |
| **[1. Architecture & Protocols](architecture.md)** | System breakdown: `r36s-agent` (daemon) vs `r36s-studio` (`app`), multi-port TCP multiplexing architecture (ports 7365–7368), binary header formats (`VID0`), LZ4 frame diffing, and zero-allocation data structures. |
| **[2. Hardware & ArkOS Quirks](hardware-and-arkos.md)** | Deep dive into the R36S hardware (RK3326 Cortex-A35, Mali-G31 MP2), the legacy Linux 4.4 kernel, ArkOS filesystem partitions (`EASYROMS`, `BOOT`, `root`), the `dwc2` USB OTG device-tree patch, and the life-cycle of games (`perfmax` -> `ffplay` loading -> RetroArch/PortMaster -> `perfnorm`). |
| **[3. Video Pipeline & DRM/KMS Mechanics](video-pipeline.md)** | Complete analysis of the video capture subsystem: why naive `mmap` causes catastrophic kernel State D deadlocks, how DRM KMS planes work on Rockchip, CRTC active filtering, alpha channel synthesis (0x00 vs 0xFF), and dynamic buffer sizing. |
| **[4. Trouble Shooting & Lessons Learned](troubleshooting.md)** | Detailed record of real-world bugs encountered, diagnostic methodologies (SSH remote inspections, `gdb`, `ftrace`, socket analysis), why TCP video preemption is required, and future optimization vectors. |

---

## 🎯 Project Goals & Design Principles

1. **Zero SD Card Swapping**: Eliminate the need to shut down the handheld, pop out the MicroSD card, copy binaries, and reinsert it. Hot-deploy ports, ROMs, and scripts over USB in seconds.
2. **Deterministic Handheld Performance**: The daemon must run strictly on spare CPU cycles with zero memory leaks, using `< 5%` total CPU load on a 4-core Cortex-A35 without starving demanding emulators (like PS1, N64, or Dreamcast).
3. **Sub-millisecond Latency**: Use lean binary protocols, raw TCP over Ethernet Gadget (`g_ether`), frame-diffing, and fast LZ4 compression instead of heavy video encoders like H.264/FFmpeg, which introduce buffering and latency overhead.
4. **Self-Healing Connectivity**: The system must survive USB cable disconnects, game resolution switches (e.g. from 640x480 EmulationStation to 320x240 GBA), emulator crashes, and process transitions without requiring a handheld reboot.

---

## 🔗 Quick References & Specifications

- **Rockchip RK3326 Technical Reference Manual (TRM)**: ARM Cortex-A35 Quad-core @ 1.3-1.5 GHz, ARM Mali-G31 MP2 GPU, DDR3/DDR3L/LPDDR3 memory controller.
- **ArkOS Official Wiki**: [https://github.com/christianhaitian/arkos/wiki](https://github.com/christianhaitian/arkos/wiki)
- **Linux DRM/KMS Subsystem**: [Kernel DRM Documentation](https://www.kernel.org/doc/html/latest/gpu/drm-kms.html)
- **Linux USB Gadget Subsystem**: [Linux USB Gadget API](https://www.kernel.org/doc/html/latest/driver-api/usb/gadget.html)
- **LZ4 Streaming Compression**: [https://github.com/lz4/lz4](https://github.com/lz4/lz4)
- **eframe / egui GUI Framework**: [https://github.com/emilk/egui](https://github.com/emilk/egui)
