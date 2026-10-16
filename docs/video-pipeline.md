# 🎥 Video Pipeline & DRM/KMS Mechanics

This document provides a deep, technical analysis of the video capture and streaming subsystem in DevBridge. It explains how screen capture works on Rockchip Linux, why standard Linux capture approaches failed, and how the current bulletproof implementation operates.

---

## ⚡ The Challenge: Capturing Video on Rockchip RK3326

Unlike desktop Linux PCs running X11 or Wayland, the R36S console runs an embedded headless Linux system without a display server. Applications (EmulationStation, RetroArch, ScummVM, native PortMaster games) render directly to the hardware using:

1. **DRM / KMS (Direct Rendering Manager / Kernel Mode Setting)** via `libdrm` or GLES GBM buffers.
2. **Fbdev legacy emulation** (`/dev/fb0`).

Our design goal was to stream 640x480 video at 30–60 FPS over USB with **< 10ms total end-to-end latency** and **< 5% CPU overhead**.

---

## 💀 The Fatal Trap: `mmap` and Kernel State "D" Deadlocks

In early versions of the capture agent, video was acquired by mapping `/dev/fb0` into user space using `mmap()`:

```rust
// ❌ NAIVE APPROACH THAT CAUSES DEADLOCKS
let fb_ptr = libc::mmap(
    ptr::null_mut(),
    screensize,
    PROT_READ,
    MAP_SHARED,
    fb_fd,
    0
);
```

### What Happened During Game Transitions:
When a user launched a game from EmulationStation, the following chain reaction occurred:
1. EmulationStation stopped rendering and closed its DRM context.
2. ArkOS launched `ffplay` to show the "Now Loading" screen.
3. The Rockchip kernel DRM driver (`rockchipdrm`) tore down the active display plane and reconfigured the hardware display controller (VOP).
4. Meanwhile, `r36s-agent` was holding an active memory mapping (`mmap`) onto the framebuffer backing memory.
5. The kernel driver attempted to acquire the internal DRM memory lock while servicing page-faults or buffer reallocations from `mmap`.
6. **Result:** The capture thread entered **Uninterruptible Sleep (`D` state)** inside the kernel.
7. Once in state `D`, the thread could **never wake up**. Even `kill -9` could not terminate the process. The socket remained open in `CLOSE-WAIT`, the video channel froze permanently, and only a physical power cycle could unfreeze the console.

### The Fix: Replacing `mmap` with Positional `pread`

Instead of holding virtual memory mappings that fight with the kernel VOP driver for memory lock arbitration, we switched to positional synchronous reads:

```rust
// ✅ SAFE ATOMIC READ APPROACH (r36s-agent/src/capture.rs)
let bytes_read = libc::pread(
    fb_fd,
    buffer.as_mut_ptr() as *mut libc::c_void,
    screensize,
    0, // Always read from offset 0
);
```

- `pread` performs a synchronous copy from the kernel buffer without establishing page-table mappings.
- If the framebuffer is temporarily reconfigured by the kernel, `pread` simply returns an `EINVAL`, `EBUSY`, or partial read, which our loop catches gracefully and retries on the next tick.
- **Zero deadlocks, zero state `D` hangs.**

---

## 🖼️ DRM / KMS Planes & Active CRTC Filtering

To achieve the best image fidelity, `r36s-agent` probes the DRM KMS subsystem (`/dev/dri/card0`) directly using ioctl calls to query hardware planes:

```
┌────────────────────────────────────────────────────────┐
│                   DRM / KMS Card 0                     │
│                                                        │
│   Plane 0 (Primary) ───► CRTC 0 (VOP) ───► Panel DSI  │
│   Plane 1 (Overlay) ───► CRTC 0 (VOP) ───► Panel DSI  │
│   Plane 2 (Disabled)───► crtc_id = 0                  │
└────────────────────────────────────────────────────────┘
```

### The Bug with Disabled Planes & False Positives:
Earlier iterations of the agent inspected all registered planes on `/dev/dri/card0`. However:
1. When ArkOS displays menus or loading screens, certain overlay planes exist in memory but have their `crtc_id` set to `0` (or `DISABLED`).
2. An older check had a false positive: if any plane summary reported `DISABLED`, the capture loop assumed DRM was inactive and aborted.
3. Furthermore, attempting to read framebuffers associated with disabled planes returned garbage or all-black screens.

### The Solution:
The agent now filters planes strictly by their active association:
```rust
// Strictly inspect planes bound to an active CRTC
if plane.crtc_id > 0 && plane.fb_id > 0 {
    // Valid active hardware plane being displayed right now
    capture_drm_plane(plane.fb_id);
}
```
If DRM planes are transitioning, the capture pipeline falls back automatically to `/dev/fb0` with `pread` until the new DRM plane becomes active.

---

## ⚡ GPU Fence Synchronization & Tearing Prevention (DMA-BUF)

When emulators render through Mali GPU (OpenGL ES) with double buffering, the GPU writes to the back-buffer asynchronously in tiles ($16 \times 16$ pixels). If the CPU reads the mapped DMA buffer while the GPU is clearing or rendering:
1. The top rows might be read while still cleared to zero (`0x00`), resulting in flickering or missing status bars.
2. The CPU data caches (D-cache) might hold stale cache lines from previous frames.

### Three-Fold Solution in `r36s-agent`:
1. **Implicit Fence Polling:** Before reading, the agent polls the DMA-BUF file descriptor with `libc::poll(&mut pfd, 1, 10)` (`POLLIN`). This yields CPU time until any pending GPU write reservation fence on the buffer is resolved.
2. **DMA-BUF Cache Synchronization (`DMA_BUF_IOCTL_SYNC`):**
   - **`DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ`**: Signals the kernel that the CPU is beginning to read from the memory, forcing ARM CPU cache invalidation.
   - **`DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ`**: Completes the read operation.
3. **Post-Capture Consistency Check:** After reading, the agent queries the DRM plane's `fb_id` again. If the game issued a `drmModePageFlip` during the copy, the frame is discarded as torn, and the clean frame is retained without flickering.

---

## 🎨 The 32-bit Alpha Channel Black Screen Dilemma

One of the most elusive bugs during development was a **completely black screen** in `r36s-studio` even though:
- Network traffic was moving (~5 MB/s).
- The handheld screen displayed a colorful game.
- The agent was successfully reading 640x480 32-bit buffers.

### Root Cause Analysis:
The Rockchip framebuffer driver stores pixels in 32-bit `BGRA8888` or `RGBA8888` format. However, because the physical LCD panel ignores the alpha channel, the GPU and VOP drivers leave the alpha byte set to **`0x00` (completely transparent)** for all pixels!

When `r36s-studio` passed these uncompressed bytes into egui / OpenGL / wgpu for rendering:
$$\text{Color} = (\text{Red}, \text{Green}, \text{Blue}, \text{Alpha} = 0)$$
The rendering engine treated every pixel as 100% transparent, rendering the black background of the window!

### The Solution: Alpha Channel Synthesis
In `r36s-studio/src/connection.rs`, during frame decoding, we enforce full opacity on all 32-bit pixel buffers:

```rust
// Force opaque alpha for 32-bit color modes
for chunk in rgba_pixels.chunks_exact_mut(4) {
    chunk[3] = 0xFF; // Force Alpha to 255 (100% Opaque)
}
```
Instant fix: the display immediately showed full, vibrant color mirroring the handheld.

---

## 📐 Dynamic Decompression Buffer Sizing

When low-resolution games (e.g. Game Boy Advance at 240x160 or NES at 256x224) switch to high-resolution games (e.g. ScummVM at 640x480), the uncompressed pixel size changes dramatically:

$$\text{BufferSize} = \text{Width} \times \text{Height} \times \text{BytesPerPixel}$$

- $240 \times 160 \times 4 = 153,600\text{ bytes}$
- $640 \times 480 \times 4 = 1,228,800\text{ bytes}$

If the studio's LZ4 decompression buffer was statically allocated for a smaller resolution, expanding a 640x480 frame would cause a buffer overflow error in the LZ4 library, dropping all subsequent frames.

In `r36s-studio`, the decompression buffer is calculated dynamically from the packet header:
```rust
let expected_uncompressed_size = (header.width as usize) * (header.height as usize) * 4;
let max_allowed = expected_uncompressed_size + 65536; // Guard band
decompress_buffer.resize(max_allowed, 0);
```
This allows real-time, on-the-fly resolution switching without dropping connection or restarting the application.
