// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Translate ArkTS input events into egui `RawInput` events.
//!
//! The NAPI layer reports touch / pointer / key / wheel events from the XComponent surface
//! as JSON; this module turns each into the egui events the original UI code consumes, so
//! the original `MailApp::logic` input handling runs unchanged. Mirrors the anicraft-hmos
//! input layer.
//!
//! egui 0.37 note: `Event::PointerButton` / `MouseWheel` shapes are validated against egui
//! 0.37 at compile time; adjust field names if egui 0.37 renamed them.

use serde::Deserialize;

/// One input event as reported by the NAPI layer (JSON shape matches `bridge.cpp`).
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    /// One or more touch points.
    Touch { points: Vec<TouchPoint> },
    /// Pointer (mouse / pen): action 0=down 1=up 2=move 3=cancel.
    Pointer {
        action: u8,
        #[serde(default)]
        button: u8,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
    },
    /// Key: action 0=up 1=down (matches the XComponent key codes).
    Key {
        action: u8,
        code: i32,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        text: String,
    },
    /// Wheel / pinch: action 0=down 1=up 2=move.
    Axis {
        action: u8,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
        #[serde(default)]
        dx: f32,
        #[serde(default)]
        dy: f32,
        #[serde(default)]
        zoom: f32,
    },
    /// View lost focus.
    Blur,
    /// View gained focus.
    Focus,
    /// A synthetic tap at egui *point* coordinates (a down+up in one frame). This is a self-test
    /// hook, not a real input path: it bypasses device-pixel routing (the NAPI layer feeds egui
    /// points directly), so it proves "click → select → repaint" regardless of window position.
    Tap { x: f32, y: f32 },
}

#[derive(Deserialize, Debug, Clone)]
pub struct TouchPoint {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub pressed: bool,
}

/// Translate one input event into egui's `RawInput` and feed it to the Context.
///
/// Coordinates are physical pixels; egui wants logical pixels, so divide by `scale`
/// (the device pixel ratio, supplied by the NAPI layer at runtime).
pub fn apply(ctx: &egui::Context, input: &Input, scale: f32) {
    let inv = 1.0 / scale.max(0.001);
    let mut raw = ctx.input_mut(|i| i.clone());
    raw.events.clear();
    match input {
        Input::Touch { points } => {
            for p in points {
                let pos = egui::pos2(p.x * inv, p.y * inv);
                if p.pressed {
                    raw.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    });
                } else {
                    raw.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                raw.events.push(egui::Event::PointerMoved(pos));
            }
        }
        Input::Pointer { action, button, x, y } => {
            let pos = egui::pos2(*x * inv, *y * inv);
            let btn = match button {
                1 => egui::PointerButton::Secondary,
                2 => egui::PointerButton::Middle,
                _ => egui::PointerButton::Primary,
            };
            let modifiers = egui::Modifiers::NONE;
            match action {
                0 => raw.events.push(egui::Event::PointerButton { pos, button: btn, pressed: true, modifiers }),
                1 => raw.events.push(egui::Event::PointerButton { pos, button: btn, pressed: false, modifiers }),
                2 => raw.events.push(egui::Event::PointerMoved(pos)),
                3 => raw.events.push(egui::Event::PointerGone),
                _ => {}
            }
        }
        Input::Key { action, code, ctrl, shift, alt, text } => {
            let key = xcode_to_egui(*code);
            let modifiers = egui::Modifiers {
                ctrl: *ctrl,
                shift: *shift,
                alt: *alt,
                ..Default::default()
            };
            if !text.is_empty() && *action == 1 {
                raw.events.push(egui::Event::Text(text.clone()));
            }
            if let Some(k) = key {
                raw.events.push(egui::Event::Key {
                    key: k,
                    physical_key: None,
                    pressed: *action == 1,
                    repeat: false,
                    modifiers,
                });
            }
        }
        Input::Axis { action, x, y, dx, dy, zoom } => {
            if *action == 2 {
                let _pos = egui::pos2(*x * inv, *y * inv);
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(*dx, *dy),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                });
            }
            let _zoom = zoom;
        }
        Input::Blur => {
            raw.events.push(egui::Event::PointerGone);
        }
        Input::Focus => {}
        Input::Tap { x, y } => {
            // A click at an egui point: move onto it, press, release. egui registers the click
            // on the release when a press was seen this frame.
            let pos = egui::pos2(*x, *y);
            raw.events.push(egui::Event::PointerMoved(pos));
            raw.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            raw.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }
    ctx.input_mut(|i| {
        i.events = raw.events;
    });
}

/// XComponent key code -> egui::Key (simplified; complements the char generation in bridge.cpp).
fn xcode_to_egui(code: i32) -> Option<egui::Key> {
    use egui::Key::*;
    match code {
        // Letters A-Z (XComponent key codes 2017-2042)
        2017..=2042 => {
            let idx = (code - 2017) as usize;
            let letters = [A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z];
            letters.get(idx).copied()
        }
        // Digits 0-9 (key codes 2000-2009)
        2000..=2009 => {
            let digits = [Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9];
            digits.get((code - 2000) as usize).copied()
        }
        2050 => Some(Space),
        2043 => Some(Comma),
        2044 => Some(Period),
        2056 => Some(Semicolon),
        2057 => Some(Quote),
        2058 => Some(OpenBracket),
        2059 => Some(CloseBracket),
        2060 => Some(Backslash),
        2061 => Some(Slash),
        2062 => Some(Backtick),
        2063 => Some(Minus),
        2064 => Some(Equals),
        2070 => Some(Escape),
        2071 => Some(Enter),
        2072 => Some(Tab),
        2073 => Some(Backspace),
        2074 => Some(Insert),
        2075 => Some(Delete),
        2076 => Some(Home),
        2077 => Some(End),
        2078 => Some(PageUp),
        2079 => Some(PageDown),
        2080 => Some(ArrowUp),
        2081 => Some(ArrowDown),
        2082 => Some(ArrowLeft),
        2083 => Some(ArrowRight),
        _ => None,
    }
}
