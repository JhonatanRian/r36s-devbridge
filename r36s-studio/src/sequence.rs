use crate::connection::{ConnectionManager, ConnectionStatus, InputMode};
use crate::protocol::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_PULSE_DURATION_MS: u16 = 80;
pub const LEASE_RENEWAL_INTERVAL: Duration = Duration::from_millis(250);
pub const ACK_TIMEOUT: Duration = Duration::from_millis(1000);

pub const ALL_BUTTONS_MASK: u32 = BTN_MASK_A
    | BTN_MASK_B
    | BTN_MASK_X
    | BTN_MASK_Y
    | BTN_MASK_L1
    | BTN_MASK_R1
    | BTN_MASK_L2
    | BTN_MASK_R2
    | BTN_MASK_SELECT
    | BTN_MASK_START
    | BTN_MASK_FN
    | BTN_MASK_L3
    | BTN_MASK_R3
    | BTN_MASK_DPAD_UP
    | BTN_MASK_DPAD_DOWN
    | BTN_MASK_DPAD_LEFT
    | BTN_MASK_DPAD_RIGHT;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SequenceStep {
    Pulse {
        mask: u32,
        duration_ms: Option<u16>,
    },
    Hold {
        mask: u32,
    },
    Release {
        mask: u32,
    },
    Axis {
        left_x: i16,
        left_y: i16,
        right_x: i16,
        right_y: i16,
    },
    WaitMs(u64),
}

impl SequenceStep {
    pub fn pulse(mask: u32) -> Self {
        Self::Pulse {
            mask,
            duration_ms: None,
        }
    }

    pub fn pulse_with_duration(mask: u32, duration_ms: u16) -> Self {
        Self::Pulse {
            mask,
            duration_ms: Some(duration_ms),
        }
    }

    pub fn hold(mask: u32) -> Self {
        Self::Hold { mask }
    }

    pub fn release(mask: u32) -> Self {
        Self::Release { mask }
    }

    pub fn axis(left_x: i16, left_y: i16, right_x: i16, right_y: i16) -> Self {
        Self::Axis {
            left_x,
            left_y,
            right_x,
            right_y,
        }
    }

    pub fn wait_ms(ms: u64) -> Self {
        Self::WaitMs(ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceStatus {
    Pending,
    Running {
        current_step: usize,
        total_steps: usize,
    },
    Completed,
    Cancelled,
    Failed {
        step: usize,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceError {
    NotConnected,
    AlreadyRunning,
    EmptySequence,
    PulseUnsupported,
    InvalidStep { step: usize, reason: String },
    CommandRejected { step: usize, message: String },
    LeaseFailed(String),
    Disconnected,
    Cancelled,
}

impl std::fmt::Display for SequenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SequenceError::NotConnected => write!(f, "R36S não está conectado"),
            SequenceError::AlreadyRunning => {
                write!(f, "Já existe uma sequência em execução nesta sessão")
            }
            SequenceError::EmptySequence => write!(f, "Sequência vazia"),
            SequenceError::PulseUnsupported => write!(
                f,
                "Agente não suporta pulsos atômicos (gamepad-pulse-v1)"
            ),
            SequenceError::InvalidStep { step, reason } => {
                write!(f, "Passo {} inválido: {}", step, reason)
            }
            SequenceError::CommandRejected { step, message } => {
                write!(f, "Passo {} rejeitado pelo agente: {}", step, message)
            }
            SequenceError::LeaseFailed(msg) => {
                write!(f, "Falha na renovação da concessão (lease): {}", msg)
            }
            SequenceError::Disconnected => write!(f, "Conexão perdida durante a execução"),
            SequenceError::Cancelled => write!(f, "Sequência cancelada"),
        }
    }
}

impl std::error::Error for SequenceError {}

pub fn validate_sequence(
    steps: &[SequenceStep],
    pulse_supported: bool,
) -> Result<(), SequenceError> {
    if steps.is_empty() {
        return Err(SequenceError::EmptySequence);
    }

    let mut simulated_held: u32 = 0;

    for (idx, step) in steps.iter().enumerate() {
        match step {
            SequenceStep::Pulse { mask, duration_ms } => {
                if *mask == 0 || (*mask & !ALL_BUTTONS_MASK) != 0 {
                    return Err(SequenceError::InvalidStep {
                        step: idx,
                        reason: format!("Máscara de botão inválida: 0x{:X}", mask),
                    });
                }
                if let Some(dur) = *duration_ms {
                    if dur == 0 || dur > 1000 {
                        return Err(SequenceError::InvalidStep {
                            step: idx,
                            reason: format!(
                                "Duração inválida: {} ms (deve estar entre 1 e 1000 ms)",
                                dur
                            ),
                        });
                    }
                }
                if !pulse_supported {
                    return Err(SequenceError::PulseUnsupported);
                }
                if simulated_held & *mask != 0 {
                    return Err(SequenceError::InvalidStep {
                        step: idx,
                        reason: format!("Botão 0x{:X} já está retido em Hold", mask),
                    });
                }
            }
            SequenceStep::Hold { mask } => {
                if *mask == 0 || (*mask & !ALL_BUTTONS_MASK) != 0 {
                    return Err(SequenceError::InvalidStep {
                        step: idx,
                        reason: format!("Máscara de botão inválida: 0x{:X}", mask),
                    });
                }
                if simulated_held & *mask != 0 {
                    return Err(SequenceError::InvalidStep {
                        step: idx,
                        reason: format!("Botão 0x{:X} já está retido em Hold", mask),
                    });
                }
                simulated_held |= *mask;
            }
            SequenceStep::Release { mask } => {
                if *mask == 0 || (*mask & !ALL_BUTTONS_MASK) != 0 {
                    return Err(SequenceError::InvalidStep {
                        step: idx,
                        reason: format!("Máscara de botão inválida: 0x{:X}", mask),
                    });
                }
                simulated_held &= !*mask;
            }
            SequenceStep::Axis { .. } => {}
            SequenceStep::WaitMs(ms) => {
                if *ms == 0 {
                    return Err(SequenceError::InvalidStep {
                        step: idx,
                        reason: "Tempo de espera deve ser maior que zero".into(),
                    });
                }
            }
        }
    }
    Ok(())
}

pub struct SequenceHandle {
    pub cancel_token: Arc<AtomicBool>,
    pub status: Arc<Mutex<SequenceStatus>>,
    pub thread_handle: Option<thread::JoinHandle<Result<(), SequenceError>>>,
}

impl SequenceHandle {
    pub fn cancel(&self) {
        self.cancel_token.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.status(),
            SequenceStatus::Pending | SequenceStatus::Running { .. }
        )
    }

    pub fn status(&self) -> SequenceStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn wait(mut self) -> Result<(), SequenceError> {
        if let Some(h) = self.thread_handle.take() {
            h.join().unwrap_or(Err(SequenceError::Cancelled))
        } else {
            match self.status() {
                SequenceStatus::Completed => Ok(()),
                SequenceStatus::Cancelled => Err(SequenceError::Cancelled),
                SequenceStatus::Failed { step, error } => {
                    Err(SequenceError::CommandRejected {
                        step,
                        message: error,
                    })
                }
                _ => Ok(()),
            }
        }
    }
}

pub fn wait_for_lease_ack(
    conn: &ConnectionManager,
    rx: &Receiver<(u64, ServerResponse)>,
    session_gen: u64,
    cancel_token: &Arc<AtomicBool>,
) -> Result<(), SequenceError> {
    let start = Instant::now();
    loop {
        if cancel_token.load(Ordering::SeqCst) {
            return Err(SequenceError::Cancelled);
        }
        if conn.session_generation.load(Ordering::SeqCst) != session_gen {
            return Err(SequenceError::Disconnected);
        }
        let timeout = Duration::from_millis(50);
        match rx.recv_timeout(timeout) {
            Ok((gen, resp)) => {
                if gen != session_gen {
                    continue;
                }
                match resp {
                    ServerResponse::GamepadStateResponse(st) => {
                        if st.input_enabled {
                            return Ok(());
                        }
                    }
                    ServerResponse::GamepadAck {
                        success: false,
                        message,
                    } => {
                        return Err(SequenceError::LeaseFailed(message));
                    }
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if start.elapsed() >= ACK_TIMEOUT {
                    return Err(SequenceError::LeaseFailed(
                        "Timeout aguardando confirmação de envio".into(),
                    ));
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(SequenceError::Disconnected);
            }
        }
    }
}

pub fn wait_for_input_ack(
    step_idx: usize,
    conn: &ConnectionManager,
    rx: &Receiver<(u64, ServerResponse)>,
    session_gen: u64,
    cancel_token: &Arc<AtomicBool>,
) -> Result<(), SequenceError> {
    let start = Instant::now();
    loop {
        if cancel_token.load(Ordering::SeqCst) {
            return Err(SequenceError::Cancelled);
        }
        if conn.session_generation.load(Ordering::SeqCst) != session_gen {
            return Err(SequenceError::Disconnected);
        }
        let timeout = Duration::from_millis(50);
        match rx.recv_timeout(timeout) {
            Ok((gen, resp)) => {
                if gen != session_gen {
                    continue;
                }
                match resp {
                    ServerResponse::GamepadAck { success, message } => {
                        if success {
                            return Ok(());
                        } else {
                            return Err(SequenceError::CommandRejected {
                                step: step_idx,
                                message,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if start.elapsed() >= ACK_TIMEOUT {
                    return Err(SequenceError::CommandRejected {
                        step: step_idx,
                        message: "Timeout aguardando confirmação GamepadAck".into(),
                    });
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(SequenceError::Disconnected);
            }
        }
    }
}

pub fn wait_with_lease_renewal(
    duration: Duration,
    conn: &ConnectionManager,
    rx: &Receiver<(u64, ServerResponse)>,
    session_gen: u64,
    cancel_token: &Arc<AtomicBool>,
    last_lease_time: &mut Instant,
) -> Result<(), SequenceError> {
    let start = Instant::now();
    while start.elapsed() < duration {
        if cancel_token.load(Ordering::SeqCst) {
            return Err(SequenceError::Cancelled);
        }
        if conn.session_generation.load(Ordering::SeqCst) != session_gen {
            return Err(SequenceError::Disconnected);
        }

        if last_lease_time.elapsed() >= LEASE_RENEWAL_INTERVAL {
            conn.send_command(ClientCommand::SetGamepadInput { enabled: true });
            wait_for_lease_ack(conn, rx, session_gen, cancel_token)?;
            *last_lease_time = Instant::now();
        }

        let remaining = duration.saturating_sub(start.elapsed());
        let slice = remaining.min(Duration::from_millis(20));
        thread::sleep(slice);
    }
    Ok(())
}

pub fn run_sequence_worker(
    steps: Vec<SequenceStep>,
    conn: ConnectionManager,
    session_gen: u64,
    cancel_token: Arc<AtomicBool>,
    status: Arc<Mutex<SequenceStatus>>,
    prior_mode: InputMode,
) -> Result<(), SequenceError> {
    let rx = conn.subscribe_responses();

    // 1. Enable Send mode and confirm lease before the first step
    conn.set_input_mode(InputMode::Send);
    if let Err(e) = wait_for_lease_ack(&conn, &rx, session_gen, &cancel_token) {
        conn.automation_buttons.store(0, Ordering::Relaxed);
        conn.sequence_running.store(false, Ordering::SeqCst);
        let final_status = match &e {
            SequenceError::Cancelled => SequenceStatus::Cancelled,
            _ => SequenceStatus::Failed {
                step: 0,
                error: e.to_string(),
            },
        };
        *status.lock().unwrap() = final_status;
        conn.request_repaint();
        return Err(e);
    }

    let total_steps = steps.len();
    let mut held_buttons: u32 = 0;
    let mut current_axes = [0i16; 4];
    let mut last_lease_time = Instant::now();

    let mut result = Ok(());

    for (idx, step) in steps.iter().enumerate() {
        if cancel_token.load(Ordering::SeqCst) {
            result = Err(SequenceError::Cancelled);
            break;
        }
        if conn.session_generation.load(Ordering::SeqCst) != session_gen {
            result = Err(SequenceError::Disconnected);
            break;
        }

        *status.lock().unwrap() = SequenceStatus::Running {
            current_step: idx,
            total_steps,
        };
        conn.request_repaint();

        match step {
            SequenceStep::Pulse { mask, duration_ms } => {
                let dur_ms = duration_ms.unwrap_or(DEFAULT_PULSE_DURATION_MS);
                conn.automation_buttons.fetch_or(*mask, Ordering::Relaxed);
                conn.request_repaint();

                conn.send_command(ClientCommand::InjectGamepadInput {
                    action: GamepadInputAction::Pulse {
                        mask: *mask,
                        duration_ms: dur_ms,
                    },
                });

                match wait_for_input_ack(idx, &conn, &rx, session_gen, &cancel_token) {
                    Ok(()) => {
                        if let Err(e) = wait_with_lease_renewal(
                            Duration::from_millis(dur_ms as u64),
                            &conn,
                            &rx,
                            session_gen,
                            &cancel_token,
                            &mut last_lease_time,
                        ) {
                            conn.automation_buttons.fetch_and(!*mask, Ordering::Relaxed);
                            conn.request_repaint();
                            result = Err(e);
                            break;
                        }
                        conn.automation_buttons.fetch_and(!*mask, Ordering::Relaxed);
                        conn.request_repaint();
                    }
                    Err(e) => {
                        conn.automation_buttons.fetch_and(!*mask, Ordering::Relaxed);
                        conn.request_repaint();
                        result = Err(e);
                        break;
                    }
                }
            }
            SequenceStep::Hold { mask } => {
                conn.automation_buttons.fetch_or(*mask, Ordering::Relaxed);
                conn.request_repaint();

                conn.send_command(ClientCommand::InjectGamepadInput {
                    action: GamepadInputAction::Button {
                        mask: *mask,
                        pressed: true,
                    },
                });

                if let Err(e) = wait_for_input_ack(idx, &conn, &rx, session_gen, &cancel_token) {
                    conn.automation_buttons.fetch_and(!*mask, Ordering::Relaxed);
                    conn.request_repaint();
                    result = Err(e);
                    break;
                }
                held_buttons |= *mask;
            }
            SequenceStep::Release { mask } => {
                conn.send_command(ClientCommand::InjectGamepadInput {
                    action: GamepadInputAction::Button {
                        mask: *mask,
                        pressed: false,
                    },
                });

                if let Err(e) = wait_for_input_ack(idx, &conn, &rx, session_gen, &cancel_token) {
                    result = Err(e);
                    break;
                }
                held_buttons &= !*mask;
                conn.automation_buttons.fetch_and(!*mask, Ordering::Relaxed);
                conn.request_repaint();
            }
            SequenceStep::Axis {
                left_x,
                left_y,
                right_x,
                right_y,
            } => {
                conn.send_command(ClientCommand::InjectGamepadInput {
                    action: GamepadInputAction::Axis {
                        left_x: *left_x,
                        left_y: *left_y,
                        right_x: *right_x,
                        right_y: *right_y,
                    },
                });

                if let Err(e) = wait_for_input_ack(idx, &conn, &rx, session_gen, &cancel_token) {
                    result = Err(e);
                    break;
                }
                current_axes = [*left_x, *left_y, *right_x, *right_y];
            }
            SequenceStep::WaitMs(ms) => {
                if let Err(e) = wait_with_lease_renewal(
                    Duration::from_millis(*ms),
                    &conn,
                    &rx,
                    session_gen,
                    &cancel_token,
                    &mut last_lease_time,
                ) {
                    result = Err(e);
                    break;
                }
            }
        }
    }

    // 2. Clear visual feedback immediately
    conn.automation_buttons.store(0, Ordering::Relaxed);
    conn.request_repaint();

    // 3. Cleanup held inputs & restore prior mode if still connected to the same session
    let still_connected = conn.session_generation.load(Ordering::SeqCst) == session_gen
        && matches!(
            *conn.status.lock().unwrap(),
            ConnectionStatus::Connected { .. }
        );

    if still_connected {
        if held_buttons != 0 {
            conn.send_command(ClientCommand::InjectGamepadInput {
                action: GamepadInputAction::Button {
                    mask: held_buttons,
                    pressed: false,
                },
            });
            let _ = wait_for_input_ack(total_steps, &conn, &rx, session_gen, &cancel_token);
        }
        if current_axes != [0, 0, 0, 0] {
            conn.send_command(ClientCommand::InjectGamepadInput {
                action: GamepadInputAction::Axis {
                    left_x: 0,
                    left_y: 0,
                    right_x: 0,
                    right_y: 0,
                },
            });
            let _ = wait_for_input_ack(total_steps, &conn, &rx, session_gen, &cancel_token);
        }
        conn.set_input_mode(prior_mode);
    }

    // 4. Release guard
    conn.sequence_running.store(false, Ordering::SeqCst);

    // 5. Update final status
    let final_status = match &result {
        Ok(()) => SequenceStatus::Completed,
        Err(SequenceError::Cancelled) => SequenceStatus::Cancelled,
        Err(SequenceError::CommandRejected { step, message }) => {
            SequenceStatus::Failed {
                step: *step,
                error: message.clone(),
            }
        }
        Err(e) => SequenceStatus::Failed {
            step: 0,
            error: e.to_string(),
        },
    };
    *status.lock().unwrap() = final_status;
    conn.request_repaint();

    result
}
