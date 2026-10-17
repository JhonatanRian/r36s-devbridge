use crate::connection::ConnectionManager;
use crate::protocol::*;
use egui::{
    pos2, vec2, Color32, FontFamily, FontId, Key, Pos2, Rect, Rounding, Sense, Stroke, Ui,
};
use std::collections::HashSet;
use std::sync::atomic::Ordering;

/// Painel interativo do Gamepad R36S
/// Reproduz fielmente a geometria do console portátil R36S através de desenho vetorial com egui::Painter.
/// Suporta:
/// 1. Escuta em tempo real dos botões físicos e analógicos do console via daemon r36s-agent.
/// 2. Injeção de comandos (cliques do mouse no console virtual e mapeamento por teclado).
pub struct GamepadPanel {
    pressed_keys_cache: HashSet<Key>,
    dragged_stick: Option<ActiveStickDrag>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveStickDrag {
    Left,
    Right,
}

impl GamepadPanel {
    pub fn new() -> Self {
        Self {
            pressed_keys_cache: HashSet::new(),
            dragged_stick: None,
        }
    }

    pub fn render(&mut self, ui: &mut Ui, connection: &ConnectionManager) {
        let is_listening = connection.gamepad_listening.load(Ordering::Relaxed);
        let input_enabled = connection.gamepad_input_enabled.load(Ordering::Relaxed);

        ui.vertical(|ui| {
            // Header e Switches de Controle
            ui.horizontal(|ui| {
                ui.heading("🎮 Gamepad R36S");
            });

            ui.add_space(4.0);

            // Master Switch: Escutar Controles
            ui.horizontal(|ui| {
                let mut listening_toggle = is_listening;
                if ui.checkbox(&mut listening_toggle, "🎧 Escutar Controles").changed() {
                    connection.toggle_gamepad_listening();
                }

                if is_listening {
                    ui.colored_label(Color32::from_rgb(0, 220, 100), "● Ao Vivo");
                } else {
                    ui.colored_label(Color32::from_rgb(140, 140, 140), "○ Parado");
                }
            });

            // Switch: Enviar Comandos (Injeção de Input)
            ui.horizontal(|ui| {
                let mut inject_toggle = input_enabled;
                if ui.checkbox(&mut inject_toggle, "🖱 Enviar Comandos").changed() {
                    connection.gamepad_input_enabled.store(inject_toggle, Ordering::Relaxed);
                }
                if input_enabled {
                    ui.colored_label(Color32::from_rgb(0, 180, 255), "Injeção Ativa");
                }
            });

            if !is_listening && !input_enabled {
                ui.label(
                    egui::RichText::new("Ative 'Escutar Controles' para visualizar botões e analógicos em tempo real.")
                        .small()
                        .color(Color32::from_rgb(150, 150, 150)),
                );
            }

            ui.add_space(8.0);

            // Obtenção do estado atual do Gamepad
            let gamepad_state = if is_listening {
                connection.gamepad_state.lock().unwrap().clone()
            } else {
                GamepadState::default()
            };

            // Desenho do Console R36S e Processamento de Interações
            self.draw_r36s_console(ui, &gamepad_state, input_enabled, connection);

            // Se injeção estiver habilitada, processa atalhos do teclado
            if input_enabled {
                self.handle_keyboard_passthrough(ui, connection);
                ui.add_space(4.0);
                ui.collapsing("⌨ Mapeamento do Teclado", |ui| {
                    ui.label("• D-Pad: Setas direcionais ou W/A/S/D");
                    ui.label("• Botões A/B/X/Y: K / J / I / U");
                    ui.label("• SELECT / START / FN: Espaço / Enter / Tab (ou F)");
                    ui.label("• L1 / R1 / L2 / R2: Q / E / 1 / 3");
                    ui.label("• Mouse: Clique nos botões ou arraste os analógicos");
                });
            }
        });
    }

    /// Desenha o console R36S vetorialmente e lida com cliques e arrastos do mouse
    fn draw_r36s_console(
        &mut self,
        ui: &mut Ui,
        state: &GamepadState,
        input_enabled: bool,
        connection: &ConnectionManager,
    ) {
        // Largura disponível no painel lateral
        let available_width = ui.available_width().clamp(220.0, 380.0);
        // Proporção física típica do R36S: largura ~82mm, altura ~130mm (~1 : 1.58)
        let console_width = available_width - 8.0;
        let console_height = console_width * 1.52;

        let (rect, response) = ui.allocate_exact_size(
            vec2(console_width, console_height),
            Sense::click_and_drag(),
        );

        let painter = ui.painter_at(rect);

        // Cores de acabamento do console R36S (Plástico cinza escuro / preto fosco)
        let body_color = Color32::from_rgb(28, 30, 34);
        let bevel_color = Color32::from_rgb(45, 48, 54);
        let screen_bezel = Color32::from_rgb(16, 17, 20);
        let screen_glass = Color32::from_rgb(22, 28, 36);

        // 1. Corpo principal com bordas arredondadas e chanfro estético
        let corner_radius = console_width * 0.08;
        painter.rect_filled(rect, Rounding::same(corner_radius), bevel_color);

        let inner_rect = rect.shrink(3.0);
        painter.rect_filled(inner_rect, Rounding::same(corner_radius - 1.0), body_color);
        painter.rect_stroke(
            inner_rect,
            Rounding::same(corner_radius - 1.0),
            Stroke::new(1.0_f32, Color32::from_rgb(60, 64, 72)),
        );

        // 2. Botões de Ombro Superiores (L1, L2, R1, R2) integrados no topo
        let top_shoulder_height = console_height * 0.045;
        let shoulder_y = rect.top() + 4.0;
        let btn_w = console_width * 0.18;

        let l2_rect = Rect::from_min_size(pos2(rect.left() + 10.0, shoulder_y), vec2(btn_w, top_shoulder_height));
        let l1_rect = Rect::from_min_size(pos2(rect.left() + 14.0 + btn_w, shoulder_y), vec2(btn_w, top_shoulder_height));
        let r1_rect = Rect::from_min_size(pos2(rect.right() - 14.0 - btn_w * 2.0, shoulder_y), vec2(btn_w, top_shoulder_height));
        let r2_rect = Rect::from_min_size(pos2(rect.right() - 10.0 - btn_w, shoulder_y), vec2(btn_w, top_shoulder_height));

        Self::draw_pill_button(&painter, l2_rect, "L2", (state.buttons & BTN_MASK_L2) != 0);
        Self::draw_pill_button(&painter, l1_rect, "L1", (state.buttons & BTN_MASK_L1) != 0);
        Self::draw_pill_button(&painter, r1_rect, "R1", (state.buttons & BTN_MASK_R1) != 0);
        Self::draw_pill_button(&painter, r2_rect, "R2", (state.buttons & BTN_MASK_R2) != 0);

        // 3. Área da Tela (LCD 3.5 polegadas 4:3)
        let screen_top = rect.top() + top_shoulder_height + 10.0;
        let screen_w = console_width * 0.86;
        let screen_h = screen_w * 0.75; // Proporção 4:3
        let screen_rect = Rect::from_center_size(
            pos2(rect.center().x, screen_top + screen_h * 0.5),
            vec2(screen_w, screen_h),
        );

        // Moldura da tela
        painter.rect_filled(screen_rect, Rounding::same(6.0), screen_bezel);
        painter.rect_stroke(screen_rect, Rounding::same(6.0), Stroke::new(1.0_f32, Color32::from_rgb(50, 54, 62)));

        // Display interno (Painel HUD de telemetria rápida do R36S)
        let display_rect = screen_rect.shrink(6.0);
        painter.rect_filled(display_rect, Rounding::same(3.0), screen_glass);

        // Conteúdo HUD na tela do console
        let hud_font = FontId::new(console_width * 0.034, FontFamily::Monospace);
        let hud_title_font = FontId::new(console_width * 0.040, FontFamily::Proportional);

        let title = if state.device_name.is_empty() {
            "R36S HANDHELD CONSOLE"
        } else {
            &state.device_name
        };

        painter.text(
            pos2(display_rect.center().x, display_rect.top() + 12.0),
            egui::Align2::CENTER_CENTER,
            title,
            hud_title_font,
            Color32::from_rgb(0, 200, 255),
        );

        let norm_lx = (state.left_x as f32) / 32767.0;
        let norm_ly = (state.left_y as f32) / 32767.0;
        let norm_rx = (state.right_x as f32) / 32767.0;
        let norm_ry = (state.right_y as f32) / 32767.0;

        let stick_l_text = format!("LS: ({:+0.2}, {:+0.2})", norm_lx, norm_ly);
        let stick_r_text = format!("RS: ({:+0.2}, {:+0.2})", norm_rx, norm_ry);

        painter.text(
            pos2(display_rect.left() + 8.0, display_rect.top() + 32.0),
            egui::Align2::LEFT_TOP,
            stick_l_text,
            hud_font.clone(),
            Color32::from_rgb(180, 230, 255),
        );

        painter.text(
            pos2(display_rect.left() + 8.0, display_rect.top() + 48.0),
            egui::Align2::LEFT_TOP,
            stick_r_text,
            hud_font.clone(),
            Color32::from_rgb(180, 230, 255),
        );

        // Exibe os botões ativos na tela do HUD
        let active_btns = Self::format_active_buttons(state.buttons);
        let btns_display = if active_btns.is_empty() {
            "BOTOES: [Nenhum]".to_string()
        } else {
            format!("BOTOES: {}", active_btns.join(" "))
        };

        painter.text(
            pos2(display_rect.left() + 8.0, display_rect.top() + 66.0),
            egui::Align2::LEFT_TOP,
            btns_display,
            hud_font.clone(),
            Color32::from_rgb(0, 255, 170),
        );

        // 4. Logo / Inscrição central abaixo da tela
        let logo_y = screen_rect.bottom() + console_height * 0.022;
        painter.text(
            pos2(rect.center().x, logo_y),
            egui::Align2::CENTER_CENTER,
            "ArkOS • RK3326",
            FontId::new(console_width * 0.032, FontFamily::Proportional),
            Color32::from_rgb(100, 105, 115),
        );

        // LED indicador central de status
        let led_color = if (state.buttons & BTN_MASK_POWER) != 0 {
            Color32::from_rgb(255, 80, 80)
        } else {
            Color32::from_rgb(0, 220, 100)
        };
        painter.circle_filled(pos2(rect.center().x, logo_y + 11.0), 2.5, led_color);

        // 5. Bloco de Controles: D-Pad, Botões Centrais e Face Buttons ABXY
        let controls_center_y = logo_y + console_height * 0.135;

        // D-Pad à Esquerda (Cruz direcional clássica)
        let dpad_center = pos2(rect.left() + console_width * 0.23, controls_center_y);
        let dpad_radius = console_width * 0.15;
        let (up_rect, down_rect, left_rect, right_rect) =
            Self::draw_dpad(&painter, dpad_center, dpad_radius, state.buttons);

        // Botões Centrais: SELECT, FN, START
        let center_btns_y = controls_center_y + console_height * 0.08;
        let fn_center = pos2(rect.center().x, center_btns_y - 12.0);
        let select_center = pos2(rect.center().x - console_width * 0.15, center_btns_y + 4.0);
        let start_center = pos2(rect.center().x + console_width * 0.15, center_btns_y + 4.0);

        let select_rect = Rect::from_center_size(select_center, vec2(console_width * 0.11, console_height * 0.022));
        let fn_rect = Rect::from_center_size(fn_center, vec2(console_width * 0.08, console_height * 0.020));
        let start_rect = Rect::from_center_size(start_center, vec2(console_width * 0.11, console_height * 0.022));

        Self::draw_pill_button(&painter, select_rect, "SELECT", (state.buttons & BTN_MASK_SELECT) != 0);
        Self::draw_pill_button(&painter, fn_rect, "FN", (state.buttons & BTN_MASK_FN) != 0);
        Self::draw_pill_button(&painter, start_rect, "START", (state.buttons & BTN_MASK_START) != 0);

        // Botões de Ação ABXY à Direita (Disposição em Diamante)
        let abxy_center = pos2(rect.right() - console_width * 0.23, controls_center_y);
        let abxy_radius = console_width * 0.15;
        let btn_radius = console_width * 0.052;

        let x_pos = pos2(abxy_center.x, abxy_center.y - abxy_radius * 0.65);
        let b_pos = pos2(abxy_center.x, abxy_center.y + abxy_radius * 0.65);
        let y_pos = pos2(abxy_center.x - abxy_radius * 0.65, abxy_center.y);
        let a_pos = pos2(abxy_center.x + abxy_radius * 0.65, abxy_center.y);

        let x_rect = Rect::from_center_size(x_pos, vec2(btn_radius * 2.0, btn_radius * 2.0));
        let b_rect = Rect::from_center_size(b_pos, vec2(btn_radius * 2.0, btn_radius * 2.0));
        let y_rect = Rect::from_center_size(y_pos, vec2(btn_radius * 2.0, btn_radius * 2.0));
        let a_rect = Rect::from_center_size(a_pos, vec2(btn_radius * 2.0, btn_radius * 2.0));

        Self::draw_face_button(&painter, x_pos, btn_radius, "X", Color32::from_rgb(30, 144, 255), (state.buttons & BTN_MASK_X) != 0);
        Self::draw_face_button(&painter, y_pos, btn_radius, "Y", Color32::from_rgb(46, 204, 113), (state.buttons & BTN_MASK_Y) != 0);
        Self::draw_face_button(&painter, b_pos, btn_radius, "B", Color32::from_rgb(241, 196, 15), (state.buttons & BTN_MASK_B) != 0);
        Self::draw_face_button(&painter, a_pos, btn_radius, "A", Color32::from_rgb(231, 76, 60), (state.buttons & BTN_MASK_A) != 0);

        // 6. Analógicos Duplos Inferiores (Left Stick / Right Stick)
        let stick_y = rect.bottom() - console_height * 0.14;
        let stick_well_radius = console_width * 0.125;
        let stick_cap_radius = console_width * 0.095;

        let left_stick_center = pos2(rect.left() + console_width * 0.25, stick_y);
        let right_stick_center = pos2(rect.right() - console_width * 0.25, stick_y);

        Self::draw_analog_stick(
            &painter,
            left_stick_center,
            stick_well_radius,
            stick_cap_radius,
            norm_lx,
            norm_ly,
            (state.buttons & BTN_MASK_L3) != 0,
            "L3",
        );

        Self::draw_analog_stick(
            &painter,
            right_stick_center,
            stick_well_radius,
            stick_cap_radius,
            norm_rx,
            norm_ry,
            (state.buttons & BTN_MASK_R3) != 0,
            "R3",
        );

        // 7. Grelhas dos Alto-Falantes Estéreo no rodapé
        let speaker_y = rect.bottom() - 14.0;
        let spk_center_x = rect.center().x;
        for i in -2..=2 {
            let offset_x = (i as f32) * 6.0;
            painter.line_segment(
                [pos2(spk_center_x + offset_x, speaker_y - 4.0), pos2(spk_center_x + offset_x, speaker_y + 4.0)],
                Stroke::new(1.5_f32, Color32::from_rgb(18, 20, 22)),
            );
        }

        // 8. Interações do Mouse: Injeção de Comandos
        if input_enabled {
            self.handle_mouse_interactions(
                ui,
                &response,
                connection,
                state,
                &[
                    (up_rect, BTN_MASK_DPAD_UP),
                    (down_rect, BTN_MASK_DPAD_DOWN),
                    (left_rect, BTN_MASK_DPAD_LEFT),
                    (right_rect, BTN_MASK_DPAD_RIGHT),
                    (a_rect, BTN_MASK_A),
                    (b_rect, BTN_MASK_B),
                    (x_rect, BTN_MASK_X),
                    (y_rect, BTN_MASK_Y),
                    (l1_rect, BTN_MASK_L1),
                    (r1_rect, BTN_MASK_R1),
                    (l2_rect, BTN_MASK_L2),
                    (r2_rect, BTN_MASK_R2),
                    (select_rect, BTN_MASK_SELECT),
                    (fn_rect, BTN_MASK_FN),
                    (start_rect, BTN_MASK_START),
                ],
                left_stick_center,
                right_stick_center,
                stick_well_radius,
            );
        }
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
        painter.circle_stroke(center, radius * 1.05, Stroke::new(1.0_f32, Color32::from_rgb(45, 48, 54)));

        let up_rect = Rect::from_center_size(pos2(center.x, center.y - arm_len * 0.5), vec2(arm_w, arm_len));
        let down_rect = Rect::from_center_size(pos2(center.x, center.y + arm_len * 0.5), vec2(arm_w, arm_len));
        let left_rect = Rect::from_center_size(pos2(center.x - arm_len * 0.5, center.y), vec2(arm_len, arm_w));
        let right_rect = Rect::from_center_size(pos2(center.x + arm_len * 0.5, center.y), vec2(arm_len, arm_w));

        let base_dpad_color = Color32::from_rgb(38, 41, 46);
        let active_color = Color32::from_rgb(0, 220, 150);

        // Braços da cruz
        let is_up = (buttons & BTN_MASK_DPAD_UP) != 0;
        let is_down = (buttons & BTN_MASK_DPAD_DOWN) != 0;
        let is_left = (buttons & BTN_MASK_DPAD_LEFT) != 0;
        let is_right = (buttons & BTN_MASK_DPAD_RIGHT) != 0;

        painter.rect_filled(left_rect, Rounding::same(3.0), if is_left { active_color } else { base_dpad_color });
        painter.rect_filled(right_rect, Rounding::same(3.0), if is_right { active_color } else { base_dpad_color });
        painter.rect_filled(up_rect, Rounding::same(3.0), if is_up { active_color } else { base_dpad_color });
        painter.rect_filled(down_rect, Rounding::same(3.0), if is_down { active_color } else { base_dpad_color });

        // Centro do D-Pad
        painter.rect_filled(Rect::from_center_size(center, vec2(arm_w, arm_w)), Rounding::ZERO, base_dpad_color);
        painter.circle_filled(center, arm_w * 0.35, Color32::from_rgb(28, 30, 34));

        // Setinhas no D-Pad
        let arrow_font = FontId::new(radius * 0.32, FontFamily::Proportional);
        painter.text(pos2(center.x, up_rect.top() + 4.0), egui::Align2::CENTER_TOP, "▲", arrow_font.clone(), Color32::from_rgb(180, 185, 195));
        painter.text(pos2(center.x, down_rect.bottom() - 4.0), egui::Align2::CENTER_BOTTOM, "▼", arrow_font.clone(), Color32::from_rgb(180, 185, 195));
        painter.text(pos2(left_rect.left() + 4.0, center.y), egui::Align2::LEFT_CENTER, "◀", arrow_font.clone(), Color32::from_rgb(180, 185, 195));
        painter.text(pos2(right_rect.right() - 4.0, center.y), egui::Align2::RIGHT_CENTER, "▶", arrow_font, Color32::from_rgb(180, 185, 195));

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
            theme_color
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
        painter.circle_stroke(button_center, radius, Stroke::new(1.2_f32, if is_pressed { Color32::WHITE } else { theme_color }));

        painter.text(
            button_center,
            egui::Align2::CENTER_CENTER,
            label,
            FontId::new(radius * 1.05, FontFamily::Proportional),
            text_color,
        );
    }

    /// Desenha botões em formato de pílula (L1/L2/R1/R2, SELECT, START, FN)
    fn draw_pill_button(
        painter: &egui::Painter,
        rect: Rect,
        label: &str,
        is_pressed: bool,
    ) {
        let rounding = Rounding::same(rect.height() * 0.45);
        let bg_color = if is_pressed {
            Color32::from_rgb(0, 220, 150)
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
            if is_pressed { Color32::BLACK } else { Color32::from_rgb(200, 205, 215) },
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
        painter.circle_stroke(well_center, well_radius, Stroke::new(1.5_f32, Color32::from_rgb(38, 41, 46)));

        // Marcadores de quadrante do analógico
        painter.line_segment(
            [pos2(well_center.x - well_radius, well_center.y), pos2(well_center.x + well_radius, well_center.y)],
            Stroke::new(0.5_f32, Color32::from_rgb(28, 30, 35)),
        );
        painter.line_segment(
            [pos2(well_center.x, well_center.y - well_radius), pos2(well_center.x, well_center.y + well_radius)],
            Stroke::new(0.5_f32, Color32::from_rgb(28, 30, 35)),
        );

        // Deslocamento máximo da tampa em pixels
        let max_travel = well_radius - cap_radius * 0.85;
        let offset_x = (axis_x.clamp(-1.0, 1.0)) * max_travel;
        let offset_y = (axis_y.clamp(-1.0, 1.0)) * max_travel;
        let cap_center = pos2(well_center.x + offset_x, well_center.y + offset_y);

        // Sombra sob a tampa
        painter.circle_filled(pos2(cap_center.x + 1.0, cap_center.y + 1.5), cap_radius, Color32::from_black_alpha(120));

        // Tampa de borracha do analógico
        let cap_color = if is_clicked {
            Color32::from_rgb(0, 200, 255)
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
            if is_clicked { Color32::BLACK } else { Color32::from_rgb(180, 185, 195) },
        );
    }

    /// Mapeia o teclado do computador para ações de entrada no R36S
    fn handle_keyboard_passthrough(&mut self, ui: &mut Ui, connection: &ConnectionManager) {
        ui.input(|input| {
            let key_mappings: &[(Key, u32)] = &[
                (Key::ArrowUp, BTN_MASK_DPAD_UP),
                (Key::W, BTN_MASK_DPAD_UP),
                (Key::ArrowDown, BTN_MASK_DPAD_DOWN),
                (Key::S, BTN_MASK_DPAD_DOWN),
                (Key::ArrowLeft, BTN_MASK_DPAD_LEFT),
                (Key::A, BTN_MASK_DPAD_LEFT),
                (Key::ArrowRight, BTN_MASK_DPAD_RIGHT),
                (Key::D, BTN_MASK_DPAD_RIGHT),
                // Botões de Ação: K=A, J=B, I=X, U=Y
                (Key::K, BTN_MASK_A),
                (Key::J, BTN_MASK_B),
                (Key::I, BTN_MASK_X),
                (Key::U, BTN_MASK_Y),
                // Ombro: Q=L1, E=R1, 1=L2, 3=R2
                (Key::Q, BTN_MASK_L1),
                (Key::E, BTN_MASK_R1),
                (Key::Num1, BTN_MASK_L2),
                (Key::Num3, BTN_MASK_R2),
                // Centrais
                (Key::Space, BTN_MASK_SELECT),
                (Key::Enter, BTN_MASK_START),
                (Key::Tab, BTN_MASK_FN),
                (Key::F, BTN_MASK_FN),
            ];

            for &(key, mask) in key_mappings {
                let is_down = input.key_down(key);
                let was_down = self.pressed_keys_cache.contains(&key);

                if is_down && !was_down {
                    self.pressed_keys_cache.insert(key);
                    connection.send_gamepad_action(GamepadInputAction::Button { mask, pressed: true });
                } else if !is_down && was_down {
                    self.pressed_keys_cache.remove(&key);
                    connection.send_gamepad_action(GamepadInputAction::Button { mask, pressed: false });
                }
            }
        });
    }

    /// Lida com cliques do mouse nos botões e arrasto dos sticks analógicos
    fn handle_mouse_interactions(
        &mut self,
        ui: &mut Ui,
        response: &egui::Response,
        connection: &ConnectionManager,
        current_state: &GamepadState,
        buttons: &[(Rect, u32)],
        left_stick_center: Pos2,
        right_stick_center: Pos2,
        stick_radius: f32,
    ) {
        let mouse_pos = ui.input(|i| i.pointer.interact_pos());

        // 1. Clique nos botões virtuais
        if response.clicked() {
            if let Some(pos) = mouse_pos {
                for &(btn_rect, mask) in buttons {
                    if btn_rect.contains(pos) {
                        // Envia pulso de clique (Press seguido de Release)
                        connection.pulse_gamepad_button(mask, 80);
                        break;
                    }
                }
            }
        }

        // 2. Arrasto dos Analógicos com o mouse
        if response.drag_started() {
            if let Some(pos) = mouse_pos {
                if pos.distance(left_stick_center) <= stick_radius * 1.2 {
                    self.dragged_stick = Some(ActiveStickDrag::Left);
                } else if pos.distance(right_stick_center) <= stick_radius * 1.2 {
                    self.dragged_stick = Some(ActiveStickDrag::Right);
                }
            }
        }

        if response.dragged() {
            if let (Some(drag), Some(pos)) = (self.dragged_stick, mouse_pos) {
                let (center, is_left) = match drag {
                    ActiveStickDrag::Left => (left_stick_center, true),
                    ActiveStickDrag::Right => (right_stick_center, false),
                };

                let delta = pos - center;
                let max_travel = stick_radius * 0.85;
                let norm_x = (delta.x / max_travel).clamp(-1.0, 1.0);
                let norm_y = (delta.y / max_travel).clamp(-1.0, 1.0);
                let axis_x = (norm_x * 32767.0) as i16;
                let axis_y = (norm_y * 32767.0) as i16;

                if is_left {
                    connection.send_gamepad_action(GamepadInputAction::Axis {
                        left_x: axis_x,
                        left_y: axis_y,
                        right_x: current_state.right_x,
                        right_y: current_state.right_y,
                    });
                } else {
                    connection.send_gamepad_action(GamepadInputAction::Axis {
                        left_x: current_state.left_x,
                        left_y: current_state.left_y,
                        right_x: axis_x,
                        right_y: axis_y,
                    });
                }
            }
        }

        if response.drag_stopped() || !response.dragged() {
            if let Some(drag) = self.dragged_stick.take() {
                // Centraliza o analógico ao soltar o mouse
                match drag {
                    ActiveStickDrag::Left => {
                        connection.send_gamepad_action(GamepadInputAction::Axis {
                            left_x: 0,
                            left_y: 0,
                            right_x: current_state.right_x,
                            right_y: current_state.right_y,
                        });
                    }
                    ActiveStickDrag::Right => {
                        connection.send_gamepad_action(GamepadInputAction::Axis {
                            left_x: current_state.left_x,
                            left_y: current_state.left_y,
                            right_x: 0,
                            right_y: 0,
                        });
                    }
                }
            }
        }
    }

    /// Converte a bitmask de botões em lista de nomes legíveis
    fn format_active_buttons(mask: u32) -> Vec<&'static str> {
        let mut list = Vec::new();
        if (mask & BTN_MASK_DPAD_UP) != 0 { list.push("UP"); }
        if (mask & BTN_MASK_DPAD_DOWN) != 0 { list.push("DOWN"); }
        if (mask & BTN_MASK_DPAD_LEFT) != 0 { list.push("LEFT"); }
        if (mask & BTN_MASK_DPAD_RIGHT) != 0 { list.push("RIGHT"); }
        if (mask & BTN_MASK_A) != 0 { list.push("A"); }
        if (mask & BTN_MASK_B) != 0 { list.push("B"); }
        if (mask & BTN_MASK_X) != 0 { list.push("X"); }
        if (mask & BTN_MASK_Y) != 0 { list.push("Y"); }
        if (mask & BTN_MASK_L1) != 0 { list.push("L1"); }
        if (mask & BTN_MASK_R1) != 0 { list.push("R1"); }
        if (mask & BTN_MASK_L2) != 0 { list.push("L2"); }
        if (mask & BTN_MASK_R2) != 0 { list.push("R2"); }
        if (mask & BTN_MASK_SELECT) != 0 { list.push("SELECT"); }
        if (mask & BTN_MASK_START) != 0 { list.push("START"); }
        if (mask & BTN_MASK_FN) != 0 { list.push("FN"); }
        if (mask & BTN_MASK_L3) != 0 { list.push("L3"); }
        if (mask & BTN_MASK_R3) != 0 { list.push("R3"); }
        if (mask & BTN_MASK_POWER) != 0 { list.push("PWR"); }
        list
    }
}
