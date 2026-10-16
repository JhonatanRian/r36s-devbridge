# 🔧 Troubleshooting, Diagnostics & Lessons Learned

This document serves as an engineering post-mortem and diagnostic reference. It catalogs the exact failure modes, real-world bug hunts, diagnostic commands, and preventive rules learned during the development of DevBridge.

---

## 🔬 Diagnostic Toolkit for the R36S

When diagnosing issues on the R36S over USB, standard SSH access is available at `192.168.7.2`:

```bash
ssh ark@192.168.7.2
# Default password on ArkOS: ark
```

### Essential Diagnostic Commands

1. **Check Process State and Thread Hangs:**
   ```bash
   ps -eo pid,tid,class,rtprio,stat,comm,wchan:20 | grep -E "r36s-agent|retroarch|emulationstat"
   ```
   *Look for `STAT = D` (Uninterruptible Sleep). If a thread is in state `D`, it is blocked in kernel I/O or a kernel deadlock.*

2. **Inspect Open Sockets and TCP States:**
   ```bash
   ss -tanp '( sport = :7365 or sport = :7366 or sport = :7367 or sport = :7368 )'
   ```
   *Look for sockets stuck in `CLOSE-WAIT` or multiple connections fighting for port 7366.*

3. **Check Active DRM KMS Modes & Planes:**
   ```bash
   cat /sys/kernel/debug/dri/0/summary
   # Or using modetest if installed:
   modetest -p -c
   ```

4. **Verify Framebuffer Resolution and Format:**
   ```bash
   fbset -fb /dev/fb0
   ```

5. **Live Process FD Inspection:**
   ```bash
   # Check what files/devices the agent currently has open
   ls -la /proc/$(pgrep r36s-agent)/fd/
   ```

---

## 📜 Case Studies: Bugs Solved

### Case Study 1: The "Frozen Battery & Time Pause" Phenomenon

#### Symptoms:
The user reported: *"The loading screen never appeared in the app, only the menu was streamed. Once the game started, the stream showed a black screen with 100% battery, and never returned to normal until a physical console reboot. The battery was actually at 92%, it was as if time paused."*

#### Investigation:
1. Connected via SSH to `192.168.7.2`.
2. Verified the actual battery: `/sys/class/power_supply/battery/capacity` reported `92%`.
3. Checked process list: `r36s-agent` was running, but thread `487` was in state `D`:
   ```text
   PID   TID STAT COMM            WCHAN
   483   487 D    r36s-agent      [uninterruptible sleep inside mmap]
   ```
4. Examined socket state: port `7366` had accumulated connections in `CLOSE-WAIT`.

#### Root Cause:
The agent had called `libc::mmap()` on `/dev/fb0`. When ArkOS transitioned from EmulationStation to `ffplay` and then to RetroArch, the Rockchip DRM kernel driver locked the memory subsystem to reconfigure CRTC timings. The agent's thread hung in kernel state `D`. The studio app lost connection, and upon reconnecting, was stuck because the daemon thread was permanently asleep. The image on screen remained the last cached frame (with 100% battery).

#### Resolution:
Replaced all occurrences of `mmap` with positional synchronous `libc::pread()` at offset 0. Since `pread` does not hold kernel page-table locks, it never blocks indefinitely.

---

### Case Study 2: The "Ghost Diagnostic Client" Locking Video

#### Symptoms:
After implementing fixes, the studio app (`./app`) was started, but the viewport displayed "Connecting to video..." indefinitely. The CPU of the agent on the handheld jumped to 25–40%, but `./app` received zero frames.

#### Investigation:
1. Checked active connections on the handheld:
   ```bash
   ss -tanp sport = :7366
   ```
2. Found a single active ESTABLISHED connection originating from an external PID on the PC (`PID 1392727`), transferring gigabytes of data at high speed.
3. Traced the PC process: an automated background diagnostic script from an earlier AI tool invocation was still running in a loop, consuming the single-client video stream!
4. Because the server only serviced one video client at a time, incoming connection requests from `./app` were left queued or immediately dropped.

#### Root Cause & Twofold Flaw:
1. The diagnostic script had an incorrect header struct unpack format:
   ```python
   # INCORRECT:
   struct.unpack("<4sIHHBBHI", data)
   # CORRECT:
   struct.unpack("<4sIHHBBI2s", data)
   ```
   This swapped `compressed_size` and `reserved`, reading huge amounts of pixel data into memory and preventing the script from terminating cleanly on timeout.
2. The server had no mechanism to kick out stale clients when a new connection arrived.

#### Resolution:
1. Implemented **Atomic Video Preemption** in `r36s-agent/src/server.rs`:
   Whenever a new client connects to port 7366, an atomic counter is incremented, immediately causing any existing video sender loop to terminate and release the stream to the new caller.
2. Corrected protocol struct unpacking in all test tools and scripts.

---

## 🤖 Guidelines for Future Developers & AI Agents

1. **Never use `mmap` on embedded Rockchip `/dev/fb*` devices:** Always use `pread`.
2. **Never poll background loops without timeout:** Diagnostic scripts must strictly enforce both per-read timeouts and absolute overall execution timeouts.
3. **Always pack protocol structs:** Keep `#[repr(C, packed)]` and ensure endianness is explicitly defined (`little-endian`).
4. **Assume display contexts will be destroyed:** EmulationStation, `ffplay`, RetroArch, and PortMaster will constantly reconfigure DRM CRTCs and display planes. Always re-evaluate active planes dynamically every frame.
5. **Always enforce opaque alpha for 32-bit framebuffer feeds:** LCD controllers do not care about alpha, so framebuffer drivers frequently output zero alpha (`0x00`). Always override `alpha = 0xFF` on the client viewer.
