use crate::protocol::*;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Linux Input Event Constants (<linux/input-event-codes.h>)
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;

pub const SYN_REPORT: u16 = 0x00;

// Buttons
pub const BTN_SOUTH: u16 = 0x130; // B no R36S
pub const BTN_EAST: u16 = 0x131;  // A no R36S
pub const BTN_C: u16 = 0x132;
pub const BTN_NORTH: u16 = 0x133; // X no R36S
pub const BTN_WEST: u16 = 0x134;  // Y no R36S
pub const BTN_TL: u16 = 0x136;    // L1
pub const BTN_TR: u16 = 0x137;    // R1
pub const BTN_TL2: u16 = 0x138;   // L2
pub const BTN_TR2: u16 = 0x139;   // R2
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
            time: libc::timeval { tv_sec: 0, tv_usec: 0 },
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

/// Abstração de baixo nível para leitura e injeção de Gamepad (DIP / OCP)
pub trait GamepadBackend: Send {
    fn device_name(&self) -> String;
    fn poll_events(&mut self, state: &mut GamepadState);
    fn inject_action(&mut self, action: &GamepadInputAction, state: &mut GamepadState) -> Result<(), String>;
}

/// Implementação real de Hardware para Linux / ArkOS via evdev
pub struct EvdevGamepadBackend {
    main_fd: Option<RawFd>,
    aux_fds: Vec<RawFd>,
    device_name: String,
    abs_calibration: HashMap<u16, (i32, i32)>, // axis -> (min, max)
}

impl EvdevGamepadBackend {
    pub fn new() -> Self {
        let (main_fd, aux_fds, device_name, abs_calibration) = Self::discover_and_open();
        Self {
            main_fd,
            aux_fds,
            device_name,
            abs_calibration,
        }
    }

    fn discover_and_open() -> (Option<RawFd>, Vec<RawFd>, String, HashMap<u16, (i32, i32)>) {
        let canonical_path = Path::new("/dev/input/by-path/platform-odroidgo2-joypad-event-joystick");
        let mut candidates = Vec::new();

        if canonical_path.exists() {
            candidates.push(canonical_path.to_path_buf());
        }

        // Busca em /dev/input/event0..event15
        if let Ok(entries) = fs::read_dir("/dev/input") {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("event") && !candidates.contains(&p) {
                        candidates.push(p);
                    }
                }
            }
        }

        let mut primary_fd = None;
        let mut primary_name = "Nenhum controle detectado".to_string();
        let mut aux_fds = Vec::new();
        let mut calibration = HashMap::new();

        for path in candidates {
            // Tenta abrir em Leitura/Escrita não-bloqueante para permitir leitura E injeção
            let fd = match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
            {
                Ok(file) => file.as_raw_fd(),
                Err(_) => {
                    // Fallback para somente leitura se permissão de escrita falhar
                    match OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&path)
                    {
                        Ok(file) => file.as_raw_fd(),
                        Err(_) => continue,
                    }
                }
            };

            let mut name_buf = [0u8; 128];
            let name_len = ioctl_eviocgname(fd, &mut name_buf);
            let name_str = if name_len > 0 {
                String::from_utf8_lossy(&name_buf[..name_len as usize])
                    .trim_matches(char::from(0))
                    .trim()
                    .to_string()
            } else {
                path.to_string_lossy().to_string()
            };

            let lower = name_str.to_lowercase();
            let is_joypad = lower.contains("odroidgo")
                || lower.contains("gamepad")
                || lower.contains("joypad")
                || lower.contains("opensimhardware");

            if is_joypad && primary_fd.is_none() {
                // Calibra eixos do analógico
                for &axis in &[ABS_X, ABS_Y, ABS_RX, ABS_RY, ABS_Z, ABS_RZ] {
                    let mut absinfo = InputAbsInfo::default();
                    if ioctl_eviocgabs(fd, axis, &mut absinfo) == 0 {
                        calibration.insert(axis, (absinfo.minimum, absinfo.maximum));
                    }
                }
                primary_fd = Some(fd);
                primary_name = name_str;
            } else if lower.contains("gpio-keys") || lower.contains("pwrkey") || is_joypad {
                aux_fds.push(fd);
            }
        }

        // Garantir calibragem padrão se não detectada
        calibration.entry(ABS_X).or_insert((0, 1023));
        calibration.entry(ABS_Y).or_insert((0, 1023));
        calibration.entry(ABS_RX).or_insert((0, 1023));
        calibration.entry(ABS_RY).or_insert((0, 1023));

        (primary_fd, aux_fds, primary_name, calibration)
    }

    fn normalize_axis(&self, axis: u16, raw_val: i32) -> i16 {
        let (min, max) = self.abs_calibration.get(&axis).copied().unwrap_or((0, 1023));
        if max <= min {
            return 0;
        }

        // Mapeia de [min, max] para [-32767, 32767]
        let mid = (min + max) as f32 / 2.0;
        let half_range = (max - min) as f32 / 2.0;
        let norm = (raw_val as f32 - mid) / half_range;
        let clamped = norm.clamp(-1.0, 1.0);

        // Deadzone de 8% para filtrar ruído analógico do ADC Rockchip
        if clamped.abs() < 0.08 {
            0
        } else {
            (clamped * 32767.0).round() as i16
        }
    }

    fn denormalize_axis(&self, axis: u16, norm_val: i16) -> i32 {
        let (min, max) = self.abs_calibration.get(&axis).copied().unwrap_or((0, 1023));
        let mid = (min + max) as f32 / 2.0;
        let half_range = (max - min) as f32 / 2.0;
        let frac = (norm_val as f32) / 32767.0;
        let raw = mid + (frac * half_range);
        (raw.round() as i32).clamp(min, max)
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
            EV_ABS => {
                match ev.code {
                    ABS_X => state.left_x = self.normalize_axis(ABS_X, ev.value),
                    ABS_Y => state.left_y = self.normalize_axis(ABS_Y, ev.value),
                    ABS_RX | ABS_Z => state.right_x = self.normalize_axis(ABS_RX, ev.value),
                    ABS_RY | ABS_RZ => state.right_y = self.normalize_axis(ABS_RY, ev.value),
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
                }
            }
            _ => {}
        }
    }
}

impl GamepadBackend for EvdevGamepadBackend {
    fn device_name(&self) -> String {
        self.device_name.clone()
    }

    fn poll_events(&mut self, state: &mut GamepadState) {
        state.device_name = self.device_name.clone();

        let mut fds_to_poll = Vec::new();
        if let Some(fd) = self.main_fd {
            fds_to_poll.push(fd);
        }
        fds_to_poll.extend(&self.aux_fds);

        if fds_to_poll.is_empty() {
            // Tenta redescobrir se os controles foram desconectados ou criados depois
            let (main_fd, aux_fds, device_name, abs_calibration) = Self::discover_and_open();
            self.main_fd = main_fd;
            self.aux_fds = aux_fds;
            self.device_name = device_name;
            self.abs_calibration = abs_calibration;
            return;
        }

        let mut poll_structs: Vec<libc::pollfd> = fds_to_poll
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();

        // Poll rápido com timeout de 8ms para baixa latência e repouso de CPU
        let ret = unsafe {
            libc::poll(
                poll_structs.as_mut_ptr(),
                poll_structs.len() as libc::nfds_t,
                8,
            )
        };

        if ret <= 0 {
            return;
        }

        let mut raw_buf = [0u8; std::mem::size_of::<InputEvent>() * 16];
        let event_size = std::mem::size_of::<InputEvent>();

        for p in poll_structs {
            if (p.revents & libc::POLLIN) != 0 {
                let bytes_read = unsafe {
                    libc::read(
                        p.fd,
                        raw_buf.as_mut_ptr() as *mut libc::c_void,
                        raw_buf.len(),
                    )
                };

                if bytes_read > 0 {
                    let count = (bytes_read as usize) / event_size;
                    for i in 0..count {
                        let offset = i * event_size;
                        let ev: InputEvent = unsafe {
                            std::ptr::read_unaligned(raw_buf[offset..].as_ptr() as *const InputEvent)
                        };
                        self.process_single_event(&ev, state);
                    }
                }
            }
        }
    }

    fn inject_action(&mut self, action: &GamepadInputAction, state: &mut GamepadState) -> Result<(), String> {
        let fd = self.main_fd.ok_or_else(|| "Nenhum dispositivo evdev aberto para escrita".to_string())?;

        let mut events = Vec::new();

        match action {
            GamepadInputAction::Button { mask, pressed } => {
                let val = if *pressed { 1 } else { 0 };
                let code = if (*mask & BTN_MASK_A) != 0 {
                    Some(BTN_EAST)
                } else if (*mask & BTN_MASK_B) != 0 {
                    Some(BTN_SOUTH)
                } else if (*mask & BTN_MASK_X) != 0 {
                    Some(BTN_NORTH)
                } else if (*mask & BTN_MASK_Y) != 0 {
                    Some(BTN_WEST)
                } else if (*mask & BTN_MASK_L1) != 0 {
                    Some(BTN_TL)
                } else if (*mask & BTN_MASK_R1) != 0 {
                    Some(BTN_TR)
                } else if (*mask & BTN_MASK_L2) != 0 {
                    Some(BTN_TL2)
                } else if (*mask & BTN_MASK_R2) != 0 {
                    Some(BTN_TR2)
                } else if (*mask & BTN_MASK_DPAD_UP) != 0 {
                    Some(BTN_DPAD_UP)
                } else if (*mask & BTN_MASK_DPAD_DOWN) != 0 {
                    Some(BTN_DPAD_DOWN)
                } else if (*mask & BTN_MASK_DPAD_LEFT) != 0 {
                    Some(BTN_DPAD_LEFT)
                } else if (*mask & BTN_MASK_DPAD_RIGHT) != 0 {
                    Some(BTN_DPAD_RIGHT)
                } else if (*mask & BTN_MASK_SELECT) != 0 {
                    Some(BTN_TRIGGER_HAPPY1)
                } else if (*mask & BTN_MASK_START) != 0 {
                    Some(BTN_TRIGGER_HAPPY2)
                } else if (*mask & BTN_MASK_FN) != 0 {
                    Some(BTN_TRIGGER_HAPPY5)
                } else if (*mask & BTN_MASK_L3) != 0 {
                    Some(BTN_TRIGGER_HAPPY3)
                } else if (*mask & BTN_MASK_R3) != 0 {
                    Some(BTN_TRIGGER_HAPPY4)
                } else {
                    None
                };

                if let Some(c) = code {
                    events.push(InputEvent {
                        time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                        type_: EV_KEY,
                        code: c,
                        value: val,
                    });
                    if *pressed {
                        state.buttons |= *mask;
                    } else {
                        state.buttons &= !*mask;
                    }
                }
            }
            GamepadInputAction::Axis { left_x, left_y, right_x, right_y } => {
                events.push(InputEvent {
                    time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                    type_: EV_ABS,
                    code: ABS_X,
                    value: self.denormalize_axis(ABS_X, *left_x),
                });
                events.push(InputEvent {
                    time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                    type_: EV_ABS,
                    code: ABS_Y,
                    value: self.denormalize_axis(ABS_Y, *left_y),
                });
                events.push(InputEvent {
                    time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                    type_: EV_ABS,
                    code: ABS_RX,
                    value: self.denormalize_axis(ABS_RX, *right_x),
                });
                events.push(InputEvent {
                    time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                    type_: EV_ABS,
                    code: ABS_RY,
                    value: self.denormalize_axis(ABS_RY, *right_y),
                });
                state.left_x = *left_x;
                state.left_y = *left_y;
                state.right_x = *right_x;
                state.right_y = *right_y;
            }
        }

        // Envia SYN_REPORT para o kernel descarregar o lote de eventos
        events.push(InputEvent {
            time: libc::timeval { tv_sec: 0, tv_usec: 0 },
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });

        let event_size = std::mem::size_of::<InputEvent>();
        for ev in &events {
            let res = unsafe {
                libc::write(
                    fd,
                    ev as *const InputEvent as *const libc::c_void,
                    event_size,
                )
            };
            if res < 0 {
                return Err("Falha ao gravar input_event no descritor evdev".to_string());
            }
        }

        Ok(())
    }
}

/// Backend de simulação / fallback para testes no host ou sem evdev
pub struct MockGamepadBackend {
    name: String,
}

impl MockGamepadBackend {
    pub fn new() -> Self {
        Self {
            name: "R36S Virtual Gamepad (Simulação)".to_string(),
        }
    }
}

impl GamepadBackend for MockGamepadBackend {
    fn device_name(&self) -> String {
        self.name.clone()
    }

    fn poll_events(&mut self, state: &mut GamepadState) {
        state.device_name = self.name.clone();
    }

    fn inject_action(&mut self, action: &GamepadInputAction, state: &mut GamepadState) -> Result<(), String> {
        match action {
            GamepadInputAction::Button { mask, pressed } => {
                if *pressed {
                    state.buttons |= *mask;
                } else {
                    state.buttons &= !*mask;
                }
            }
            GamepadInputAction::Axis { left_x, left_y, right_x, right_y } => {
                state.left_x = *left_x;
                state.left_y = *left_y;
                state.right_x = *right_x;
                state.right_y = *right_y;
            }
        }
        Ok(())
    }
}

/// Serviço Thread-Safe de Gerenciamento do Gamepad do Agente
pub struct GamepadService {
    backend: Arc<Mutex<Box<dyn GamepadBackend>>>,
    state: Arc<Mutex<GamepadState>>,
    listening: Arc<AtomicBool>,
}

impl GamepadService {
    pub fn new() -> Self {
        let backend: Box<dyn GamepadBackend> = if Path::new("/dev/input").exists() {
            Box::new(EvdevGamepadBackend::new())
        } else {
            Box::new(MockGamepadBackend::new())
        };

        let initial_name = backend.device_name();
        let state = Arc::new(Mutex::new(GamepadState {
            listening: false,
            device_name: initial_name,
            buttons: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        }));

        let backend = Arc::new(Mutex::new(backend));
        let listening = Arc::new(AtomicBool::new(false));

        // Worker thread de escuta que só roda ativamente quando listening == true
        let backend_cl = Arc::clone(&backend);
        let state_cl = Arc::clone(&state);
        let listening_cl = Arc::clone(&listening);

        thread::spawn(move || {
            loop {
                if !listening_cl.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }

                {
                    let mut b = backend_cl.lock().unwrap();
                    let mut s = state_cl.lock().unwrap();
                    s.listening = true;
                    b.poll_events(&mut s);
                }

                // Cadência suave para não consumir CPU do console
                thread::sleep(Duration::from_millis(15));
            }
        });

        Self {
            backend,
            state,
            listening,
        }
    }

    pub fn set_listening(&self, enabled: bool) {
        self.listening.store(enabled, Ordering::Relaxed);
        let mut st = self.state.lock().unwrap();
        st.listening = enabled;
        if !enabled {
            st.buttons = 0;
            st.left_x = 0;
            st.left_y = 0;
            st.right_x = 0;
            st.right_y = 0;
        }
    }

    pub fn is_listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }

    pub fn new_mock() -> Self {
        let backend: Box<dyn GamepadBackend> = Box::new(MockGamepadBackend::new());
        let initial_name = backend.device_name();
        let state = Arc::new(Mutex::new(GamepadState {
            listening: false,
            device_name: initial_name,
            buttons: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        }));
        let backend = Arc::new(Mutex::new(backend));
        let listening = Arc::new(AtomicBool::new(false));

        Self {
            backend,
            state,
            listening,
        }
    }

    pub fn get_state(&self) -> GamepadState {
        self.state.lock().unwrap().clone()
    }

    pub fn inject_action(&self, action: &GamepadInputAction) -> Result<(), String> {
        let mut b = self.backend.lock().unwrap();
        let mut s = self.state.lock().unwrap();
        b.inject_action(action, &mut s)
    }
}
