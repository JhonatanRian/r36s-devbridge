use crate::protocol::*;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// Linux Input Event Constants (<linux/input-event-codes.h>)
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;

pub const SYN_REPORT: u16 = 0x00;

// Buttons
pub const BTN_SOUTH: u16 = 0x130; // B no R36S
pub const BTN_EAST: u16 = 0x131; // A no R36S
pub const BTN_C: u16 = 0x132;
pub const BTN_NORTH: u16 = 0x133; // X no R36S
pub const BTN_WEST: u16 = 0x134; // Y no R36S
pub const BTN_TL: u16 = 0x136; // L1
pub const BTN_TR: u16 = 0x137; // R1
pub const BTN_TL2: u16 = 0x138; // L2
pub const BTN_TR2: u16 = 0x139; // R2
pub const BTN_SELECT: u16 = 0x13a;
pub const BTN_START: u16 = 0x13b;
pub const BTN_MODE: u16 = 0x13c;
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;

pub const BTN_DPAD_UP: u16 = 0x220;
pub const BTN_DPAD_DOWN: u16 = 0x221;
pub const BTN_DPAD_LEFT: u16 = 0x222;
pub const BTN_DPAD_RIGHT: u16 = 0x223;

pub const BTN_TRIGGER_HAPPY1: u16 = 0x2c0; // Select no odroidgo2-joypad
pub const BTN_TRIGGER_HAPPY2: u16 = 0x2c1; // Start no odroidgo2-joypad
pub const BTN_TRIGGER_HAPPY3: u16 = 0x2c2; // L3 no odroidgo2-joypad
pub const BTN_TRIGGER_HAPPY4: u16 = 0x2c3; // R3 no odroidgo2-joypad
pub const BTN_TRIGGER_HAPPY5: u16 = 0x2c4; // FN no odroidgo2-joypad
pub const BTN_TRIGGER_HAPPY6: u16 = 0x2c5; // Aux no odroidgo2-joypad

pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_POWER: u16 = 116;

// Absolute Axes
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InputAbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputEvent {
    pub time: libc::timeval,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

impl Default for InputEvent {
    fn default() -> Self {
        Self {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_: 0,
            code: 0,
            value: 0,
        }
    }
}

#[cfg(target_env = "musl")]
type IoctlReq = libc::c_int;
#[cfg(not(target_env = "musl"))]
type IoctlReq = libc::c_ulong;

// IOCTL Helpers
fn ioctl_eviocgname(fd: RawFd, buf: &mut [u8]) -> libc::c_int {
    let req = (2u32 << 30) | ((b'E' as u32) << 8) | 0x06 | ((buf.len() as u32) << 16);
    unsafe { libc::ioctl(fd, req as IoctlReq, buf.as_mut_ptr()) }
}

fn ioctl_eviocgabs(fd: RawFd, axis: u16, info: &mut InputAbsInfo) -> libc::c_int {
    let size = std::mem::size_of::<InputAbsInfo>() as u32;
    let req = (2u32 << 30) | ((b'E' as u32) << 8) | (0x40 + axis as u32) | (size << 16);
    unsafe { libc::ioctl(fd, req as IoctlReq, info as *mut InputAbsInfo) }
}

fn ioctl_bits(fd: RawFd, nr: u32, buf: &mut [u8]) -> bool {
    let req = (2u32 << 30) | ((b'E' as u32) << 8) | nr | ((buf.len() as u32) << 16);
    unsafe { libc::ioctl(fd, req as IoctlReq, buf.as_mut_ptr()) >= 0 }
}
fn has(bits: &[u8], code: u16) -> bool {
    bits.get(code as usize / 8)
        .map_or(false, |b| b & (1 << (code % 8)) != 0)
}

struct Device {
    file: File,
    path: PathBuf,
    name: String,
    keys: [u8; 96],
    axes: HashMap<u16, InputAbsInfo>,
    pending: GamepadState,
    committed: GamepadState,
    dropped: bool,
}
impl Device {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        let fd = file.as_raw_fd();
        let mut name = [0u8; 128];
        let n = ioctl_eviocgname(fd, &mut name);
        if n <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        let name = String::from_utf8_lossy(&name[..n as usize])
            .trim_end_matches('\0')
            .to_owned();
        let mut keys = [0u8; 96];
        ioctl_bits(fd, 0x20 + EV_KEY as u32, &mut keys);
        let mut abs_bits = [0u8; 8];
        ioctl_bits(fd, 0x20 + EV_ABS as u32, &mut abs_bits);
        let mut axes = HashMap::new();
        for axis in 0..64 {
            if has(&abs_bits, axis) {
                let mut info = InputAbsInfo::default();
                if ioctl_eviocgabs(fd, axis, &mut info) == 0 {
                    axes.insert(axis, info);
                }
            }
        }
        let mut d = Self {
            file,
            path,
            name,
            keys,
            axes,
            pending: GamepadState::default(),
            committed: GamepadState::default(),
            dropped: false,
        };
        d.resync();
        Ok(d)
    }
    fn is_gamepad(&self) -> bool {
        self.axes.contains_key(&ABS_X)
            && self.axes.contains_key(&ABS_Y)
            && has(&self.keys, BTN_SOUTH)
    }
    fn normalize_axis(&self, axis: u16, raw: i32) -> i16 {
        self.axes.get(&axis).map_or(0, |info| normalize(info, raw))
    }
    fn resync(&mut self) {
        self.pending = GamepadState::default();
        let mut keys = [0u8; 96];
        ioctl_bits(self.file.as_raw_fd(), 0x18, &mut keys);
        for code in 0..768 {
            if has(&keys, code) {
                let mut next = self.pending.clone();
                self.process_single_event(
                    &InputEvent {
                        type_: EV_KEY,
                        code,
                        value: 1,
                        ..Default::default()
                    },
                    &mut next,
                );
                self.pending = next;
            }
        }
        for axis in self.axes.keys().copied().collect::<Vec<_>>() {
            let mut info = InputAbsInfo::default();
            if ioctl_eviocgabs(self.file.as_raw_fd(), axis, &mut info) == 0 {
                let mut next = self.pending.clone();
                self.process_single_event(
                    &InputEvent {
                        type_: EV_ABS,
                        code: axis,
                        value: info.value,
                        ..Default::default()
                    },
                    &mut next,
                );
                self.pending = next;
            }
        }
        self.committed = self.pending.clone();
    }
    fn process_single_event(&self, ev: &InputEvent, state: &mut GamepadState) {
        match ev.type_ {
            EV_KEY => {
                let pressed = ev.value > 0;
                let mask = match ev.code {
                    BTN_EAST => BTN_MASK_A,
                    BTN_SOUTH => BTN_MASK_B,
                    BTN_NORTH | BTN_C => BTN_MASK_X,
                    BTN_WEST => BTN_MASK_Y,
                    BTN_TL => BTN_MASK_L1,
                    BTN_TR => BTN_MASK_R1,
                    BTN_TL2 => BTN_MASK_L2,
                    BTN_TR2 => BTN_MASK_R2,
                    BTN_DPAD_UP => BTN_MASK_DPAD_UP,
                    BTN_DPAD_DOWN => BTN_MASK_DPAD_DOWN,
                    BTN_DPAD_LEFT => BTN_MASK_DPAD_LEFT,
                    BTN_DPAD_RIGHT => BTN_MASK_DPAD_RIGHT,
                    BTN_TRIGGER_HAPPY1 | BTN_SELECT => BTN_MASK_SELECT,
                    BTN_TRIGGER_HAPPY2 | BTN_START => BTN_MASK_START,
                    BTN_TRIGGER_HAPPY3 | BTN_THUMBL => BTN_MASK_L3,
                    BTN_TRIGGER_HAPPY4 | BTN_THUMBR => BTN_MASK_R3,
                    BTN_TRIGGER_HAPPY5 | BTN_TRIGGER_HAPPY6 | BTN_MODE => BTN_MASK_FN,
                    KEY_VOLUMEUP => BTN_MASK_VOL_UP,
                    KEY_VOLUMEDOWN => BTN_MASK_VOL_DOWN,
                    KEY_POWER => BTN_MASK_POWER,
                    _ => 0,
                };

                if mask > 0 {
                    if pressed {
                        state.buttons |= mask;
                    } else {
                        state.buttons &= !mask;
                    }
                }
            }
            EV_ABS => match ev.code {
                ABS_X => state.left_x = self.normalize_axis(ABS_X, ev.value),
                ABS_Y => state.left_y = self.normalize_axis(ABS_Y, ev.value),
                ABS_RX | ABS_Z => state.right_x = self.normalize_axis(ev.code, ev.value),
                ABS_RY | ABS_RZ => state.right_y = self.normalize_axis(ev.code, ev.value),
                ABS_HAT0X => {
                    if ev.value < 0 {
                        state.buttons |= BTN_MASK_DPAD_LEFT;
                        state.buttons &= !BTN_MASK_DPAD_RIGHT;
                    } else if ev.value > 0 {
                        state.buttons |= BTN_MASK_DPAD_RIGHT;
                        state.buttons &= !BTN_MASK_DPAD_LEFT;
                    } else {
                        state.buttons &= !(BTN_MASK_DPAD_LEFT | BTN_MASK_DPAD_RIGHT);
                    }
                }
                ABS_HAT0Y => {
                    if ev.value < 0 {
                        state.buttons |= BTN_MASK_DPAD_UP;
                        state.buttons &= !BTN_MASK_DPAD_DOWN;
                    } else if ev.value > 0 {
                        state.buttons |= BTN_MASK_DPAD_DOWN;
                        state.buttons &= !BTN_MASK_DPAD_UP;
                    } else {
                        state.buttons &= !(BTN_MASK_DPAD_UP | BTN_MASK_DPAD_DOWN);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}
fn normalize(info: &InputAbsInfo, raw: i32) -> i16 {
    if info.maximum <= info.minimum {
        return 0;
    }
    let mid = (info.minimum as f64 + info.maximum as f64) / 2.0;
    let half = (info.maximum as f64 - info.minimum as f64) / 2.0;
    let offset = raw as f64 - mid;
    let flat = (info.flat.max(0) as f64).min(half);
    if offset.abs() <= flat {
        return 0;
    }
    (offset.signum() * ((offset.abs() - flat) / (half - flat)).min(1.0) * 32767.0).round() as i16
}

pub struct EvdevGamepadBackend {
    devices: Vec<Device>,
    next_discovery: Instant,
    writer: Option<File>,
    injected_buttons: HashMap<u16, bool>,
    injected_axes: bool,
}
impl EvdevGamepadBackend {
    pub fn new() -> Self {
        Self {
            devices: vec![],
            next_discovery: Instant::now(),
            writer: None,
            injected_buttons: HashMap::new(),
            injected_axes: false,
        }
    }
    fn discover(&mut self) {
        if Instant::now() < self.next_discovery {
            return;
        }
        self.next_discovery = Instant::now() + Duration::from_secs(1);
        let mut devices = vec![];
        if let Ok(entries) = fs::read_dir("/dev/input") {
            let mut paths: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .starts_with("event")
                })
                .collect();
            paths.sort();
            for path in paths {
                if let Ok(d) = Device::open(path) {
                    if d.is_gamepad()
                        || [KEY_VOLUMEUP, KEY_VOLUMEDOWN, KEY_POWER]
                            .iter()
                            .any(|c| has(&d.keys, *c))
                    {
                        devices.push(d);
                    }
                }
            }
        }
        self.devices = devices;
    }
    fn aggregate(&self, state: &mut GamepadState) {
        let previous_buttons = state.buttons;
        let previous_axes = (state.left_x, state.left_y, state.right_x, state.right_y);
        state.buttons = self.devices.iter().fold(0, |a, d| a | d.committed.buttons);
        if let Some(d) = self.devices.iter().find(|d| d.is_gamepad()) {
            state.available = true;
            state.device_name = d.name.clone();
            state.left_x = d.committed.left_x;
            state.left_y = d.committed.left_y;
            state.right_x = d.committed.right_x;
            state.right_y = d.committed.right_y;
        } else {
            state.available = false;
            state.device_name.clear();
            state.left_x = 0;
            state.left_y = 0;
            state.right_x = 0;
            state.right_y = 0;
        }
        let axes_changed =
            previous_axes != (state.left_x, state.left_y, state.right_x, state.right_y);
        let buttons_changed = previous_buttons != state.buttons;
        if buttons_changed || axes_changed {
            state.sequence = state.sequence.wrapping_add(1);
        }
        if buttons_changed {
            if state.transitions.len() == 256 {
                state.transitions.remove(0);
                state.overflow = true;
            }
            state.transitions.push(ButtonTransition {
                sequence: state.sequence,
                buttons: state.buttons,
            });
        }
    }
    fn prepare_poll(&mut self, state: &mut GamepadState) -> Vec<libc::pollfd> {
        if !self.devices.iter().any(|d| d.is_gamepad()) {
            self.discover();
        }
        self.aggregate(state);
        self.devices
            .iter()
            .map(|d| libc::pollfd {
                fd: d.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect()
    }
    fn process_poll_results(&mut self, polls: &[libc::pollfd], state: &mut GamepadState) {
        if polls.len() != self.devices.len() {
            self.aggregate(state);
            return;
        }
        for (index, p) in polls.iter().enumerate() {
            if p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                self.writer = None;
                self.devices.clear();
                self.aggregate(state);
                return;
            }
            if p.revents & libc::POLLIN == 0 {
                continue;
            }
            // Bound work per iteration; commands and lease expiry must not starve.
            for _ in 0..8 {
                let mut events = [InputEvent::default(); 32];
                let bytes = unsafe {
                    libc::read(
                        p.fd,
                        events.as_mut_ptr().cast(),
                        std::mem::size_of_val(&events),
                    )
                };
                if bytes <= 0 {
                    break;
                }
                for ev in &events[..bytes as usize / std::mem::size_of::<InputEvent>()] {
                    let d = &mut self.devices[index];
                    if ev.type_ == EV_SYN && ev.code == 3 {
                        d.dropped = true;
                        state.overflow = true;
                        continue;
                    }
                    if ev.type_ == EV_SYN && ev.code == SYN_REPORT {
                        if d.dropped {
                            d.resync();
                            d.dropped = false;
                        } else {
                            d.committed = d.pending.clone();
                        }
                        self.aggregate(state);
                    } else if !d.dropped {
                        let mut next = d.pending.clone();
                        d.process_single_event(ev, &mut next);
                        d.pending = next;
                    }
                }
            }
        }
    }
    #[cfg(test)]
    fn poll_events(&mut self, state: &mut GamepadState) {
        let mut polls = self.prepare_poll(state);
        unsafe {
            libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 20);
        }
        self.process_poll_results(&polls, state);
    }
    fn enable_writer(&mut self) -> Result<(), String> {
        if self.writer.is_some() {
            return Ok(());
        }
        if !self.devices.iter().any(|d| d.is_gamepad()) {
            self.discover();
        }
        let d = self
            .devices
            .iter()
            .find(|d| d.is_gamepad())
            .ok_or("Controle físico indisponível")?;
        self.writer = Some(
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&d.path)
                .map_err(|e| e.to_string())?,
        );
        Ok(())
    }
    fn write_events(&self, events: &[InputEvent]) -> Result<(), String> {
        let fd = self
            .writer
            .as_ref()
            .ok_or("Envio de comandos desativado")?
            .as_raw_fd();
        let mut offset = 0;
        let len = std::mem::size_of_val(events);
        while offset < len {
            let n = unsafe {
                libc::write(
                    fd,
                    (events.as_ptr() as *const u8).add(offset).cast(),
                    len - offset,
                )
            };
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if n <= 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            offset += n as usize;
        }
        Ok(())
    }
    fn inject_action(&mut self, action: &GamepadInputAction) -> Result<(), String> {
        let d = self
            .devices
            .iter()
            .find(|d| d.is_gamepad())
            .ok_or("Controle indisponível")?;
        let mut events = vec![];
        match action {
            GamepadInputAction::Button { mask, pressed } => {
                if mask.count_ones() != 1 {
                    return Err("Envie exatamente um botão".into());
                }
                let code = (0..768)
                    .find(|c| {
                        if !has(&d.keys, *c) {
                            return false;
                        }
                        let mut st = GamepadState::default();
                        d.process_single_event(
                            &InputEvent {
                                type_: EV_KEY,
                                code: *c,
                                value: 1,
                                ..Default::default()
                            },
                            &mut st,
                        );
                        st.buttons == *mask
                    })
                    .ok_or("Botão não suportado pelo controle")?;
                events.push(InputEvent {
                    type_: EV_KEY,
                    code,
                    value: i32::from(*pressed),
                    ..Default::default()
                });
            }
            GamepadInputAction::Axis {
                left_x,
                left_y,
                right_x,
                right_y,
            } => {
                let rx = if d.axes.contains_key(&ABS_RX) {
                    ABS_RX
                } else {
                    ABS_Z
                };
                let ry = if d.axes.contains_key(&ABS_RY) {
                    ABS_RY
                } else {
                    ABS_RZ
                };
                for (code, norm) in [
                    (ABS_X, *left_x),
                    (ABS_Y, *left_y),
                    (rx, *right_x),
                    (ry, *right_y),
                ] {
                    let info = d.axes.get(&code).ok_or("Eixo não suportado")?;
                    let mid = (info.minimum as f64 + info.maximum as f64) / 2.0;
                    let half = (info.maximum as f64 - info.minimum as f64) / 2.0;
                    let value = (mid + norm as f64 / 32767.0 * half)
                        .round()
                        .clamp(info.minimum as f64, info.maximum as f64)
                        as i32;
                    events.push(InputEvent {
                        type_: EV_ABS,
                        code,
                        value,
                        ..Default::default()
                    });
                }
            }
        }
        events.push(InputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            ..Default::default()
        });
        // Record intent first so a partial write is also released by the watchdog.
        for ev in &events {
            if ev.type_ == EV_KEY {
                self.injected_buttons.insert(ev.code, ev.value != 0);
            }
            if ev.type_ == EV_ABS {
                self.injected_axes = true;
            }
        }
        self.write_events(&events)
    }
    fn release(&mut self) {
        let mut events: Vec<_> = self
            .injected_buttons
            .iter()
            .filter(|(_, v)| **v)
            .map(|(code, _)| InputEvent {
                type_: EV_KEY,
                code: *code,
                value: 0,
                ..Default::default()
            })
            .collect();
        if self.injected_axes {
            if let Some(d) = self.devices.iter().find(|d| d.is_gamepad()) {
                for (code, info) in &d.axes {
                    if *code <= ABS_RZ {
                        events.push(InputEvent {
                            type_: EV_ABS,
                            code: *code,
                            value: ((info.minimum as i64 + info.maximum as i64) / 2) as i32,
                            ..Default::default()
                        });
                    }
                }
            }
        }
        events.push(InputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            ..Default::default()
        });
        if self.writer.is_some() {
            let _ = self.write_events(&events);
        }
        self.writer = None;
        self.injected_buttons.clear();
        self.injected_axes = false;
    }
}

struct BackendSession {
    backend: EvdevGamepadBackend,
    lease: Option<(u32, Instant)>,
}
pub type GamepadListener = Arc<dyn Fn(GamepadState) + Send + Sync>;

pub struct GamepadService {
    backend: Arc<Mutex<BackendSession>>,
    state: Arc<Mutex<GamepadState>>,
    listening: Arc<AtomicBool>,
    listener: Arc<Mutex<Option<GamepadListener>>>,
}
impl GamepadService {
    pub fn new() -> Self {
        let backend = Arc::new(Mutex::new(BackendSession {
            backend: EvdevGamepadBackend::new(),
            lease: None,
        }));
        let state = Arc::new(Mutex::new(GamepadState::default()));
        let listening = Arc::new(AtomicBool::new(false));
        let listener: Arc<Mutex<Option<GamepadListener>>> = Arc::new(Mutex::new(None));
        let (b, s, l, list) = (
            backend.clone(),
            state.clone(),
            listening.clone(),
            listener.clone(),
        );
        thread::spawn(move || loop {
            let poll_work = {
                let mut session = b.lock().unwrap();
                if session
                    .lease
                    .map_or(false, |(_, t)| t.elapsed() > Duration::from_secs(1))
                {
                    session.backend.release();
                    session.lease = None;
                }
                if l.load(Ordering::Relaxed) || session.lease.is_some() {
                    let mut next = s.lock().unwrap().clone();
                    let polls = session.backend.prepare_poll(&mut next);
                    Some((polls, next))
                } else {
                    s.lock().unwrap().input_enabled = false;
                    None
                }
            };
            if let Some((mut polls, mut next)) = poll_work {
                // Blocking poll runs without holding either backend or published-state mutex.
                if polls.is_empty() {
                    thread::sleep(Duration::from_millis(20));
                } else {
                    unsafe {
                        libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 20);
                    }
                }
                let mut session = b.lock().unwrap();
                session.backend.process_poll_results(&polls, &mut next);
                next.listening = l.load(Ordering::Relaxed) && next.available;
                next.input_enabled = session.lease.is_some() && session.backend.writer.is_some();
                if !next.available {
                    next.error = Some("Controle físico indisponível".into());
                } else {
                    next.error = None;
                }
                // get_state can drain transitions during poll: merge only newly produced transitions.
                let mut published = s.lock().unwrap();
                let old_seq = published.sequence;
                let old_avail = published.available;
                let new_events: Vec<_> = next
                    .transitions
                    .iter()
                    .filter(|e| e.sequence > old_seq)
                    .cloned()
                    .collect();
                next.transitions = std::mem::take(&mut published.transitions);
                next.transitions.extend(new_events);
                if next.transitions.len() > 256 {
                    next.transitions.drain(..next.transitions.len() - 256);
                    next.overflow = true;
                }
                let state_changed = next.sequence != old_seq || next.available != old_avail;
                let state_to_push = if state_changed {
                    let to_send = next.clone();
                    next.transitions.clear();
                    next.overflow = false;
                    Some(to_send)
                } else {
                    None
                };
                *published = next;
                drop(published);

                if let Some(st) = state_to_push {
                    let maybe_cb = list.lock().unwrap().clone();
                    if let Some(cb) = maybe_cb {
                        cb(st);
                    }
                }
            } else {
                thread::sleep(Duration::from_millis(100));
            }
        });
        Self {
            backend,
            state,
            listening,
            listener,
        }
    }
    pub fn set_listening(&self, enabled: bool) {
        let mut session = self.backend.lock().unwrap();
        if enabled {
            session.backend.release();
            session.lease = None;
            if !session.backend.devices.iter().any(|d| d.is_gamepad()) {
                session.backend.discover();
            }
            for d in &mut session.backend.devices {
                let mut buffer = [InputEvent::default(); 64];
                // Discard old queued events before taking a fresh kernel snapshot.
                loop {
                    let n = unsafe {
                        libc::read(
                            d.file.as_raw_fd(),
                            buffer.as_mut_ptr().cast(),
                            std::mem::size_of_val(&buffer),
                        )
                    };
                    if n <= 0 {
                        break;
                    }
                }
                d.resync();
            }
        }
        self.listening.store(enabled, Ordering::Relaxed);
        let mut st = self.state.lock().unwrap();
        session.backend.aggregate(&mut st);
        st.input_enabled = session.lease.is_some();
        st.listening = enabled && st.available;
        if !enabled {
            *st = GamepadState {
                available: st.available,
                input_enabled: session.lease.is_some(),
                device_name: st.device_name.clone(),
                ..Default::default()
            };
        }
    }
    pub fn set_input(&self, session_id: u32, enabled: bool) -> Result<(), String> {
        let mut session = self.backend.lock().unwrap();
        if !enabled {
            if session.lease.map_or(false, |(id, _)| id == session_id) {
                session.backend.release();
                session.lease = None;
            }
        } else {
            if session.lease.map_or(false, |(id, _)| id != session_id) {
                session.backend.release();
            }
            session.backend.enable_writer()?;
            self.listening.store(false, Ordering::Relaxed);
            session.lease = Some((session_id, Instant::now()));
        }
        let mut state = self.state.lock().unwrap();
        state.input_enabled = session.lease.is_some();
        if enabled {
            state.listening = false;
            state.buttons = 0;
            state.transitions.clear();
        }
        Ok(())
    }
    pub fn disconnect(&self, id: u32) {
        let _ = self.set_input(id, false);
    }
    pub fn get_state(&self) -> GamepadState {
        let mut st = self.state.lock().unwrap();
        let result = st.clone();
        st.transitions.clear();
        st.overflow = false;
        result
    }
    pub fn inject_action(&self, id: u32, action: &GamepadInputAction) -> Result<(), String> {
        let mut session = self.backend.lock().unwrap();
        if !session.lease.map_or(false, |(owner, t)| {
            owner == id && t.elapsed() < Duration::from_secs(1)
        }) {
            return Err("Sessão de envio desativada ou expirada".into());
        }
        session.backend.inject_action(action)
    }
    pub fn set_listener(&self, listener: Option<GamepadListener>) {
        *self.listener.lock().unwrap() = listener;
    }
    pub fn notify_listener(&self, state: GamepadState) {
        let maybe_cb = self.listener.lock().unwrap().clone();
        if let Some(cb) = maybe_cb {
            cb(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_axis_calibration() {
        let info = InputAbsInfo {
            minimum: -1800,
            maximum: 1800,
            flat: 32,
            ..Default::default()
        };
        assert_eq!(normalize(&info, 0), 0);
        assert_eq!(normalize(&info, 32), 0);
        assert_eq!(normalize(&info, -1800), -32767);
        assert_eq!(normalize(&info, 1800), 32767);
        assert_eq!(normalize(&info, 9999), 32767);
    }
    #[test]
    fn unsupported_axis_is_neutral() {
        assert_eq!(normalize(&InputAbsInfo::default(), 100), 0);
    }
    fn fixture() -> (EvdevGamepadBackend, std::os::unix::net::UnixStream) {
        let (read, write) = std::os::unix::net::UnixStream::pair().unwrap();
        read.set_nonblocking(true).unwrap();
        let fd: std::os::fd::OwnedFd = read.into();
        let mut keys = [0u8; 96];
        for code in [BTN_SOUTH, BTN_EAST] {
            keys[code as usize / 8] |= 1 << (code % 8);
        }
        let info = InputAbsInfo {
            minimum: -1800,
            maximum: 1800,
            flat: 32,
            ..Default::default()
        };
        let d = Device {
            file: File::from(fd),
            path: PathBuf::new(),
            name: "test".into(),
            keys,
            axes: [(ABS_X, info), (ABS_Y, info)].into(),
            pending: GamepadState::default(),
            committed: GamepadState::default(),
            dropped: false,
        };
        let mut b = EvdevGamepadBackend::new();
        b.devices.push(d);
        (b, write)
    }
    fn send(writer: &mut impl std::io::Write, kind: u16, code: u16, value: i32) {
        let e = InputEvent {
            type_: kind,
            code,
            value,
            ..Default::default()
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&e as *const InputEvent).cast(),
                std::mem::size_of::<InputEvent>(),
            )
        };
        writer.write_all(bytes).unwrap();
    }
    #[test]
    fn descriptor_owned_and_reports_committed_atomically() {
        let (mut b, mut tx) = fixture();
        let mut s = GamepadState::default();
        send(&mut tx, EV_KEY, BTN_EAST, 1);
        b.poll_events(&mut s);
        assert_eq!(s.buttons, 0, "incomplete report must not be published");
        send(&mut tx, EV_SYN, SYN_REPORT, 0);
        b.poll_events(&mut s);
        assert_eq!(s.buttons, BTN_MASK_A);
        assert!(s.available);
    }
    #[test]
    fn quick_press_release_survives_one_poll() {
        let (mut b, mut tx) = fixture();
        let mut s = GamepadState::default();
        for value in [1, 0] {
            send(&mut tx, EV_KEY, BTN_EAST, value);
            send(&mut tx, EV_SYN, SYN_REPORT, 0);
        }
        b.poll_events(&mut s);
        assert_eq!(s.buttons, 0);
        assert_eq!(
            s.transitions.iter().map(|t| t.buttons).collect::<Vec<_>>(),
            vec![BTN_MASK_A, 0]
        );
    }
    #[test]
    fn dropped_report_does_not_publish_partial_keys() {
        let (mut b, mut tx) = fixture();
        let mut s = GamepadState::default();
        send(&mut tx, EV_SYN, 3, 0);
        send(&mut tx, EV_KEY, BTN_EAST, 1);
        send(&mut tx, EV_SYN, SYN_REPORT, 0);
        b.poll_events(&mut s);
        assert_eq!(s.buttons, 0);
        assert!(s.overflow);
    }
    #[test]
    fn queue_is_bounded_and_resynchronization_signaled() {
        let (b, _) = fixture();
        let mut s = GamepadState::default();
        for _ in 0..300 {
            s.buttons = BTN_MASK_A;
            b.aggregate(&mut s);
        }
        assert_eq!(s.transitions.len(), 256);
        assert!(s.overflow);
    }
    #[test]
    fn injection_rejects_unknown_and_combined_buttons() {
        let (mut b, _) = fixture();
        assert!(b
            .inject_action(&GamepadInputAction::Button {
                mask: BTN_MASK_A | BTN_MASK_B,
                pressed: true
            })
            .is_err());
        assert!(b
            .inject_action(&GamepadInputAction::Button {
                mask: BTN_MASK_L1,
                pressed: true
            })
            .is_err());
        assert!(
            b.inject_action(&GamepadInputAction::Button {
                mask: BTN_MASK_A,
                pressed: true
            })
            .is_err(),
            "no writer must not report success"
        );
    }
    #[test]
    fn release_writes_key_up_and_syn_report() {
        use std::io::Read;
        let (mut b, _) = fixture();
        let (writer, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        let owned: std::os::fd::OwnedFd = writer.into();
        b.writer = Some(File::from(owned));
        b.inject_action(&GamepadInputAction::Button {
            mask: BTN_MASK_A,
            pressed: true,
        })
        .unwrap();
        b.release();
        let mut bytes = vec![0; std::mem::size_of::<InputEvent>() * 4];
        receiver.read_exact(&mut bytes).unwrap();
        let events: Vec<_> = bytes
            .chunks_exact(std::mem::size_of::<InputEvent>())
            .map(|c| unsafe { std::ptr::read_unaligned(c.as_ptr().cast::<InputEvent>()) })
            .collect();
        assert_eq!((events[0].code, events[0].value), (BTN_EAST, 1));
        assert_eq!((events[2].code, events[2].value), (BTN_EAST, 0));
        assert_eq!(events[3].type_, EV_SYN);
        assert!(b.writer.is_none());
    }
    #[test]
    fn absent_hardware_is_not_mock() {
        let b = EvdevGamepadBackend::new();
        let mut s = GamepadState::default();
        b.aggregate(&mut s);
        assert!(!s.available);
    }
}

#[cfg(test)]
mod mode_regression_tests {
    use super::*;
    #[test]
    fn monitor_ack_revokes_previous_injection_lease_immediately() {
        let service = GamepadService {
            backend: Arc::new(Mutex::new(BackendSession {
                backend: EvdevGamepadBackend::new(),
                lease: Some((42, Instant::now())),
            })),
            state: Arc::new(Mutex::new(GamepadState {
                input_enabled: true,
                ..Default::default()
            })),
            listening: Arc::new(AtomicBool::new(false)),
            listener: Arc::new(Mutex::new(None)),
        };
        service.set_listening(true);
        assert!(!service.get_state().input_enabled);
        assert!(service.backend.lock().unwrap().lease.is_none());
    }
}
