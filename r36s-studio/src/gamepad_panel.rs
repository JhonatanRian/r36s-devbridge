use crate::connection::{ConnectionManager, ConnectionStatus, InputMode};
use crate::protocol::*;
use crate::viewport::ViewportRenderer;
use egui::{pos2, vec2, Color32, FontFamily, FontId, Key, Pos2, Rect, Rounding, Sense, Stroke, Ui};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub struct GamepadPanel {
    sent_buttons: u32,
    mouse_button: u32,
    stick: Option<usize>,
    axes: [i16; 4],
    last_lease: Instant,
    last_sequence: u64,
    glow: [Option<Instant>; 20],
}
impl GamepadPanel {
    pub fn new() -> Self {
        Self {
            sent_buttons: 0,
            mouse_button: 0,
            stick: None,
            axes: [0; 4],
            last_lease: Instant::now(),
            last_sequence: 0,
            glow: [None; 20],
        }
    }
    pub fn render(&mut self, ui: &mut Ui, c: &ConnectionManager) {
        ui.heading("Controles");
        let connected = matches!(
            *c.status.lock().unwrap(),
            ConnectionStatus::Connected { .. }
        );
        let capable = c.gamepad_supported.load(Ordering::Relaxed);
        let state = c.gamepad_state.lock().unwrap().clone();
        let sequence_active = c.sequence_running.load(Ordering::Relaxed);
        ui.add_enabled_ui(connected && capable && !sequence_active, |ui| {
            let mut listen = c.gamepad_listening.load(Ordering::Relaxed);
            let mut send = c.gamepad_input_enabled.load(Ordering::Relaxed);

            if ui.checkbox(&mut listen, "Escutar controles").changed() {
                if listen {
                    if send {
                        self.release(c);
                    }
                    c.set_gamepad_listening(true);
                } else {
                    c.set_gamepad_listening(false);
                }
            }
            send = c.gamepad_input_enabled.load(Ordering::Relaxed);
            if ui.checkbox(&mut send, "Enviar comandos").changed() {
                if send {
                    if listen {
                        c.set_gamepad_listening(false);
                    }
                    c.set_input_mode(InputMode::Send);
                    self.last_lease = Instant::now();
                } else {
                    self.release(c);
                }
            }
        });
        if sequence_active {
            ui.colored_label(
                Color32::from_rgb(100, 200, 255),
                "Executando sequência de automação…",
            );
        }
        ui.label(if !connected {
            "Desconectado"
        } else if !capable {
            "Atualize o agente para habilitar controles"
        } else if state.listening {
            "Escutando controle físico"
        } else if c.gamepad_listening.load(Ordering::Relaxed) {
            "Aguardando controle físico…"
        } else {
            "Monitoração desativada"
        });
        if !state.device_name.is_empty() {
            ui.small(&state.device_name);
        }
        if let Some(error) = &state.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        if c.gamepad_input_enabled.load(Ordering::Relaxed) {
            ui.small(if state.input_enabled {
                "Envio habilitado"
            } else {
                "Aguardando confirmação de envio…"
            });
            ui.collapsing("Teclado e mouse", |ui| {
                ui.label("Direcional: Setas • ABXY: Teclas A/B/X/Y (ou K/J/I/U)\nL1/R1: Q/E • L2/R2: 1/3\nSELECT/START: Espaço/Enter • FN: F\nL3/R3: Z/C\nClique/arraste no portátil ou use o teclado.");
            });
        }
    }
    pub fn release(&mut self, c: &ConnectionManager) {
        if c.gamepad_input_enabled.swap(false, Ordering::Relaxed)
            || self.sent_buttons != 0
            || self.axes != [0; 4]
        {
            c.send_command(ClientCommand::SetGamepadInput { enabled: false });
        }
        self.sent_buttons = 0;
        self.mouse_button = 0;
        self.stick = None;
        self.axes = [0; 4];
    }
    pub fn guard(&mut self, c: &ConnectionManager, active: bool) {
        if !active {
            self.release(c);
        }
    }
    pub fn console(&mut self, ui: &mut Ui, c: &ConnectionManager, viewport: &ViewportRenderer) {
        let connected = matches!(
            *c.status.lock().unwrap(),
            ConnectionStatus::Connected { .. }
        );
        let mut state = {
            let mut s = c.gamepad_state.lock().unwrap();
            let copy = s.clone();
            s.transitions.clear();
            copy
        };
        let now = Instant::now();
        if connected && state.listening {
            if state.sequence < self.last_sequence {
                self.last_sequence = 0;
            }
            for e in &state.transitions {
                if e.sequence > self.last_sequence {
                    for bit in 0..20 {
                        if e.buttons & (1 << bit) != 0 {
                            self.glow[bit] = Some(now);
                        }
                    }
                    self.last_sequence = e.sequence;
                }
            }
            for bit in 0..20 {
                if self.glow[bit].map_or(false, |t| t.elapsed() < Duration::from_millis(80)) {
                    state.buttons |= 1 << bit;
                }
            }
        } else {
            state = GamepadState::default();
            self.glow = [None; 20];
            self.last_sequence = 0;
        }
        state.buttons |= c.automation_buttons.load(Ordering::Relaxed);
        let w = viewport.console_width(ui.available_size() - vec2(16.0, 16.0));
        egui::ScrollArea::both()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(w, w * 1.58), Sense::click_and_drag());
                    let p = ui.painter_at(rect);
                    let point = |x: f32, y: f32| pos2(rect.left() + x * w, rect.top() + y * w);
                    let r = |x: f32, y: f32, ww: f32, hh: f32| {
                        Rect::from_min_size(point(x, y), vec2(ww * w, hh * w))
                    };
                    p.rect_filled(rect, 0.085 * w, Color32::from_rgb(53, 56, 62));
                    p.rect_filled(rect.shrink(3.0), 0.08 * w, Color32::from_rgb(26, 28, 32));
                    p.rect_stroke(
                        rect.shrink(7.0),
                        0.07 * w,
                        Stroke::new(1.0_f32, Color32::from_rgb(43, 46, 51)),
                    );
                    // Screen and controls share one physical coordinate system (units: body width).
                    let screen = r(0.06, 0.075, 0.88, 0.66);
                    p.rect_filled(
                        screen.expand(0.025 * w),
                        0.02 * w,
                        Color32::from_rgb(10, 11, 14),
                    );
                    viewport.paint_screen(
                        &p,
                        screen,
                        connected,
                        c.is_streaming.load(Ordering::Relaxed),
                        c.video_connected.load(Ordering::Relaxed),
                    );
                    p.line_segment(
                        [point(0.04, 0.785), point(0.96, 0.785)],
                        Stroke::new(2.0_f32, Color32::from_rgb(14, 16, 19)),
                    );
                    for i in 0..3 {
                        p.rect_filled(
                            r(0.425 + i as f32 * 0.055, 0.82, 0.035, 0.012),
                            3.0,
                            Color32::from_rgb(7, 9, 12),
                        );
                    }
                    let mut buttons = vec![];
                    for (x, label, mask) in [
                        (0.16, "L2", BTN_MASK_L2),
                        (0.30, "L1", BTN_MASK_L1),
                        (0.59, "R1", BTN_MASK_R1),
                        (0.73, "R2", BTN_MASK_R2),
                    ] {
                        let rr = r(x, 0.008, 0.11, 0.035);
                        Self::draw_pill_button(&p, rr, label, state.buttons & mask != 0);
                        buttons.push((rr, mask));
                    }
                    let (up, down, left, right) =
                        Self::draw_dpad(&p, point(0.23, 0.98), 0.16 * w, state.buttons);
                    buttons.extend([
                        (up, BTN_MASK_DPAD_UP),
                        (down, BTN_MASK_DPAD_DOWN),
                        (left, BTN_MASK_DPAD_LEFT),
                        (right, BTN_MASK_DPAD_RIGHT),
                    ]);
                    for (x, y, label, mask, col) in [
                        (0.77, 0.88, "X", BTN_MASK_X, Color32::from_rgb(60, 130, 230)),
                        (0.67, 0.98, "Y", BTN_MASK_Y, Color32::from_rgb(60, 190, 95)),
                        (0.87, 0.98, "A", BTN_MASK_A, Color32::from_rgb(220, 65, 65)),
                        (0.77, 1.08, "B", BTN_MASK_B, Color32::from_rgb(235, 190, 45)),
                    ] {
                        let center = point(x, y);
                        Self::draw_face_button(
                            &p,
                            center,
                            0.062 * w,
                            label,
                            col,
                            state.buttons & mask != 0,
                        );
                        buttons.push((
                            Rect::from_center_size(center, vec2(0.124 * w, 0.124 * w)),
                            mask,
                        ));
                    }
                    for (x, y, label, mask) in [
                        (0.5, 1.045, "FN", BTN_MASK_FN),
                        (0.405, 1.20, "SELECT", BTN_MASK_SELECT),
                        (0.595, 1.20, "START", BTN_MASK_START),
                    ] {
                        let center = point(x, y);
                        let rr = Rect::from_center_size(center, vec2(0.085 * w, 0.065 * w));
                        Self::draw_face_button(
                            &p,
                            center,
                            0.037 * w,
                            "",
                            Color32::from_gray(65),
                            state.buttons & mask != 0,
                        );
                        p.text(
                            center + vec2(0.0, 0.053 * w),
                            egui::Align2::CENTER_CENTER,
                            label,
                            FontId::proportional(0.024 * w),
                            Color32::from_gray(130),
                        );
                        buttons.push((rr, mask));
                    }
                    let led = if connected {
                        Color32::from_rgb(30, 235, 100)
                    } else {
                        Color32::from_rgb(240, 55, 55)
                    };
                    p.circle_filled(point(0.5, 1.20), 0.017 * w, led.gamma_multiply(0.2));
                    p.circle_filled(point(0.5, 1.20), 0.007 * w, led);
                    let sticks = [point(0.23, 1.36), point(0.77, 1.36)];
                    for (i, (x, y, mask)) in [
                        (state.left_x, state.left_y, BTN_MASK_L3),
                        (state.right_x, state.right_y, BTN_MASK_R3),
                    ]
                    .iter()
                    .enumerate()
                    {
                        Self::draw_analog_stick(
                            &p,
                            sticks[i],
                            0.13 * w,
                            0.093 * w,
                            *x as f32 / 32767.0,
                            *y as f32 / 32767.0,
                            state.buttons & mask != 0,
                            "",
                        );
                        if *x != 0 || *y != 0 {
                            p.circle_stroke(
                                sticks[i],
                                0.13 * w,
                                Stroke::new(1.4_f32, Color32::from_rgb(60, 140, 150)),
                            );
                        }
                    }
                    self.interact(ui, c, &response, &buttons, sticks, w);
                });
            });
    }
    fn keyboard_buttons(ui: &Ui) -> u32 {
        let wants_keyboard = ui.ctx().wants_keyboard_input();
        let mut keyboard = 0;
        ui.input(|i| {
            if !wants_keyboard {
                for (key, mask) in [
                    (Key::ArrowUp, BTN_MASK_DPAD_UP),
                    (Key::ArrowDown, BTN_MASK_DPAD_DOWN),
                    (Key::ArrowLeft, BTN_MASK_DPAD_LEFT),
                    (Key::ArrowRight, BTN_MASK_DPAD_RIGHT),
                    (Key::A, BTN_MASK_A),
                    (Key::B, BTN_MASK_B),
                    (Key::X, BTN_MASK_X),
                    (Key::Y, BTN_MASK_Y),
                    (Key::K, BTN_MASK_A),
                    (Key::J, BTN_MASK_B),
                    (Key::I, BTN_MASK_X),
                    (Key::U, BTN_MASK_Y),
                    (Key::Q, BTN_MASK_L1),
                    (Key::E, BTN_MASK_R1),
                    (Key::Num1, BTN_MASK_L2),
                    (Key::Num3, BTN_MASK_R2),
                    (Key::Space, BTN_MASK_SELECT),
                    (Key::Backspace, BTN_MASK_SELECT),
                    (Key::Enter, BTN_MASK_START),
                    (Key::F, BTN_MASK_FN),
                    (Key::Z, BTN_MASK_L3),
                    (Key::C, BTN_MASK_R3),
                ] {
                    if i.key_down(key) {
                        keyboard |= mask;
                    }
                }
            }
        });
        keyboard
    }

    pub fn interact_keyboard(&mut self, ui: &Ui, c: &ConnectionManager) {
        if c.sequence_running.load(Ordering::Relaxed) {
            return;
        }
        if !c.gamepad_input_enabled.load(Ordering::Relaxed) {
            return;
        }
        if self.last_lease.elapsed() > Duration::from_millis(250) {
            c.send_command(ClientCommand::SetGamepadInput { enabled: true });
            self.last_lease = Instant::now();
        }
        if !c.gamepad_state.lock().unwrap().input_enabled {
            return;
        }
        let keyboard = Self::keyboard_buttons(ui);
        let desired = keyboard | self.mouse_button;
        for bit in 0..17 {
            let mask = 1 << bit;
            if (desired ^ self.sent_buttons) & mask != 0 {
                c.send_gamepad_action(GamepadInputAction::Button {
                    mask,
                    pressed: desired & mask != 0,
                });
            }
        }
        self.sent_buttons = desired;
    }

    fn interact(
        &mut self,
        ui: &Ui,
        c: &ConnectionManager,
        response: &egui::Response,
        buttons: &[(Rect, u32)],
        sticks: [Pos2; 2],
        w: f32,
    ) {
        if c.sequence_running.load(Ordering::Relaxed) {
            return;
        }
        if !c.gamepad_input_enabled.load(Ordering::Relaxed) {
            return;
        }
        if self.last_lease.elapsed() > Duration::from_millis(250) {
            c.send_command(ClientCommand::SetGamepadInput { enabled: true });
            self.last_lease = Instant::now();
        }
        if !c.gamepad_state.lock().unwrap().input_enabled {
            return;
        }
        let keyboard = Self::keyboard_buttons(ui);
        ui.input(|i| {
            let old_axes = self.axes;
            if i.pointer.primary_pressed() && response.hovered() {
                if let Some(pos) = i.pointer.interact_pos() {
                    self.mouse_button = buttons
                        .iter()
                        .find(|(r, _)| r.contains(pos))
                        .map_or(0, |(_, m)| *m);
                    self.stick = sticks.iter().position(|s| s.distance(pos) < w * 0.13);
                }
            }
            if i.pointer.primary_down() {
                if let (Some(stick), Some(pos)) = (self.stick, i.pointer.interact_pos()) {
                    let d = (pos - sticks[stick]) / (w * 0.10);
                    self.axes[stick * 2] = (d.x.clamp(-1.0, 1.0) * 32767.0) as i16;
                    self.axes[stick * 2 + 1] = (d.y.clamp(-1.0, 1.0) * 32767.0) as i16;
                }
            } else {
                self.mouse_button = 0;
                self.stick = None;
                self.axes = [0; 4];
            }
            if self.axes != old_axes {
                c.send_gamepad_action(GamepadInputAction::Axis {
                    left_x: self.axes[0],
                    left_y: self.axes[1],
                    right_x: self.axes[2],
                    right_y: self.axes[3],
                });
            }
        });
        let desired = keyboard | self.mouse_button;
        for bit in 0..17 {
            let mask = 1 << bit;
            if (desired ^ self.sent_buttons) & mask != 0 {
                c.send_gamepad_action(GamepadInputAction::Button {
                    mask,
                    pressed: desired & mask != 0,
                });
            }
        }
        self.sent_buttons = desired;
    }
    /// Desenha o D-Pad em formato de cruz com feedback de ativação
    fn draw_dpad(
        painter: &egui::Painter,
        center: Pos2,
        radius: f32,
        buttons: u32,
    ) -> (Rect, Rect, Rect, Rect) {
        let arm_w = radius * 0.70;
        let arm_len = radius;

        // Fundo circular do rebaixo do D-Pad
        painter.circle_filled(center, radius * 1.05, Color32::from_rgb(20, 22, 25));
        painter.circle_stroke(
            center,
            radius * 1.05,
            Stroke::new(1.0_f32, Color32::from_rgb(45, 48, 54)),
        );

        let up_rect = Rect::from_center_size(
            pos2(center.x, center.y - arm_len * 0.5),
            vec2(arm_w, arm_len),
        );
        let down_rect = Rect::from_center_size(
            pos2(center.x, center.y + arm_len * 0.5),
            vec2(arm_w, arm_len),
        );
        let left_rect = Rect::from_center_size(
            pos2(center.x - arm_len * 0.5, center.y),
            vec2(arm_len, arm_w),
        );
        let right_rect = Rect::from_center_size(
            pos2(center.x + arm_len * 0.5, center.y),
            vec2(arm_len, arm_w),
        );

        let base_dpad_color = Color32::from_rgb(38, 41, 46);
        let active_color = Color32::from_rgb(55, 110, 105);

        // Braços da cruz
        let is_up = (buttons & BTN_MASK_DPAD_UP) != 0;
        let is_down = (buttons & BTN_MASK_DPAD_DOWN) != 0;
        let is_left = (buttons & BTN_MASK_DPAD_LEFT) != 0;
        let is_right = (buttons & BTN_MASK_DPAD_RIGHT) != 0;

        painter.rect_filled(
            left_rect,
            Rounding::same(3.0),
            if is_left {
                active_color
            } else {
                base_dpad_color
            },
        );
        painter.rect_filled(
            right_rect,
            Rounding::same(3.0),
            if is_right {
                active_color
            } else {
                base_dpad_color
            },
        );
        painter.rect_filled(
            up_rect,
            Rounding::same(3.0),
            if is_up { active_color } else { base_dpad_color },
        );
        painter.rect_filled(
            down_rect,
            Rounding::same(3.0),
            if is_down {
                active_color
            } else {
                base_dpad_color
            },
        );

        // Centro do D-Pad
        painter.rect_filled(
            Rect::from_center_size(center, vec2(arm_w, arm_w)),
            Rounding::ZERO,
            base_dpad_color,
        );
        painter.circle_filled(center, arm_w * 0.35, Color32::from_rgb(28, 30, 34));

        // Vector arrows render correctly without depending on font glyph coverage.
        for (direction, offset) in [
            (0, vec2(0.0, -0.7)),
            (1, vec2(0.0, 0.7)),
            (2, vec2(-0.7, 0.0)),
            (3, vec2(0.7, 0.0)),
        ] {
            let c = center + offset * radius;
            let t = radius * 0.11;
            let points = match direction {
                0 => vec![c + vec2(0.0, -t), c + vec2(-t, t), c + vec2(t, t)],
                1 => vec![c + vec2(0.0, t), c + vec2(-t, -t), c + vec2(t, -t)],
                2 => vec![c + vec2(-t, 0.0), c + vec2(t, -t), c + vec2(t, t)],
                _ => vec![c + vec2(t, 0.0), c + vec2(-t, -t), c + vec2(-t, t)],
            };
            painter.add(egui::Shape::convex_polygon(
                points,
                Color32::from_gray(120),
                Stroke::NONE,
            ));
        }

        (up_rect, down_rect, left_rect, right_rect)
    }

    /// Desenha botão de ação circular (A, B, X, Y) com anel colorido e iluminação ao pressionar
    fn draw_face_button(
        painter: &egui::Painter,
        center: Pos2,
        radius: f32,
        label: &str,
        theme_color: Color32,
        is_pressed: bool,
    ) {
        // Poço / Chanfro do botão
        painter.circle_filled(center, radius + 2.0, Color32::from_rgb(18, 20, 24));

        let button_bg = if is_pressed {
            theme_color.gamma_multiply(0.35)
        } else {
            Color32::from_rgb(42, 45, 52)
        };

        let text_color = if is_pressed {
            Color32::WHITE
        } else {
            theme_color
        };

        let button_center = if is_pressed {
            pos2(center.x, center.y + 1.5)
        } else {
            center
        };

        painter.circle_filled(button_center, radius, button_bg);
        painter.circle_stroke(
            button_center,
            radius,
            Stroke::new(
                1.2_f32,
                if is_pressed {
                    Color32::WHITE
                } else {
                    theme_color
                },
            ),
        );

        painter.text(
            button_center,
            egui::Align2::CENTER_CENTER,
            label,
            FontId::new(radius * 1.05, FontFamily::Proportional),
            text_color,
        );
    }

    /// Desenha botões em formato de pílula (L1/L2/R1/R2, SELECT, START, FN)
    fn draw_pill_button(painter: &egui::Painter, rect: Rect, label: &str, is_pressed: bool) {
        let rounding = Rounding::same(rect.height() * 0.45);
        let bg_color = if is_pressed {
            Color32::from_rgb(55, 110, 105)
        } else {
            Color32::from_rgb(44, 48, 55)
        };

        let stroke_color = if is_pressed {
            Color32::WHITE
        } else {
            Color32::from_rgb(70, 75, 85)
        };

        painter.rect_filled(rect, rounding, bg_color);
        painter.rect_stroke(rect, rounding, Stroke::new(1.0_f32, stroke_color));

        let font_size = (rect.height() * 0.55).clamp(8.0, 12.0);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            FontId::new(font_size, FontFamily::Proportional),
            if is_pressed {
                Color32::BLACK
            } else {
                Color32::from_rgb(200, 205, 215)
            },
        );
    }

    /// Desenha alavanca analógica com poço, tampa texturizada e indicador 2D de deflexão
    fn draw_analog_stick(
        painter: &egui::Painter,
        well_center: Pos2,
        well_radius: f32,
        cap_radius: f32,
        axis_x: f32,
        axis_y: f32,
        is_clicked: bool,
        label: &str,
    ) {
        // Poço / Base embutida
        painter.circle_filled(well_center, well_radius, Color32::from_rgb(16, 17, 20));
        painter.circle_stroke(
            well_center,
            well_radius,
            Stroke::new(1.5_f32, Color32::from_rgb(38, 41, 46)),
        );

        // Marcadores de quadrante do analógico
        painter.line_segment(
            [
                pos2(well_center.x - well_radius, well_center.y),
                pos2(well_center.x + well_radius, well_center.y),
            ],
            Stroke::new(0.5_f32, Color32::from_rgb(28, 30, 35)),
        );
        painter.line_segment(
            [
                pos2(well_center.x, well_center.y - well_radius),
                pos2(well_center.x, well_center.y + well_radius),
            ],
            Stroke::new(0.5_f32, Color32::from_rgb(28, 30, 35)),
        );

        // Deslocamento máximo da tampa em pixels
        let max_travel = well_radius - cap_radius * 0.85;
        let offset_x = (axis_x.clamp(-1.0, 1.0)) * max_travel;
        let offset_y = (axis_y.clamp(-1.0, 1.0)) * max_travel;
        let cap_center = pos2(well_center.x + offset_x, well_center.y + offset_y);

        // Sombra sob a tampa
        painter.circle_filled(
            pos2(cap_center.x + 1.0, cap_center.y + 1.5),
            cap_radius,
            Color32::from_black_alpha(120),
        );

        // Tampa de borracha do analógico
        let cap_color = if is_clicked {
            Color32::from_rgb(50, 130, 155)
        } else {
            Color32::from_rgb(50, 54, 62)
        };
        painter.circle_filled(cap_center, cap_radius, cap_color);

        // Anel texturizado concêntrico (Grip)
        let ring_stroke = if is_clicked {
            Stroke::new(1.5_f32, Color32::WHITE)
        } else {
            Stroke::new(1.2_f32, Color32::from_rgb(75, 80, 92))
        };
        painter.circle_stroke(cap_center, cap_radius * 0.72, ring_stroke);
        painter.circle_filled(cap_center, cap_radius * 0.35, Color32::from_rgb(34, 37, 42));

        // Rótulo L3 / R3
        painter.text(
            cap_center,
            egui::Align2::CENTER_CENTER,
            label,
            FontId::new(cap_radius * 0.55, FontFamily::Proportional),
            if is_clicked {
                Color32::BLACK
            } else {
                Color32::from_rgb(180, 185, 195)
            },
        );
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    #[test]
    fn input_query_does_not_reenter_context_lock() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.events.push(egui::Event::Key {
                key: Key::A,
                physical_key: Some(Key::A),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            });
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let buttons = GamepadPanel::keyboard_buttons(ui);
                    assert_ne!(buttons & BTN_MASK_A, 0);
                    assert_eq!(buttons & BTN_MASK_DPAD_LEFT, 0);
                });
            });
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("egui context deadlocked");
    }
}
