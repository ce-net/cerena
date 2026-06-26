//! Input capture: raw device events -> one [`InputFrame`] per simulation tick.
//!
//! The golden rule from `arena-protocol`: the client asserts **intent**, never
//! outcome. So this module accumulates pressed keys, mouse-look deltas and the
//! selected ability slot, and emits exactly one [`InputFrame`] per fixed tick
//! ([`arena_protocol::TICK_HZ`]). The server derives all movement and casting
//! results from that intent.
//!
//! ## Control mapping (a first-person mage)
//!
//! | Physical input        | Intent bit / field             | Meaning                         |
//! |-----------------------|--------------------------------|---------------------------------|
//! | W / A / S / D         | FORWARD / LEFT / BACK / RIGHT  | planar movement                 |
//! | Space                 | JUMP                           | jump / (held) levitate          |
//! | Left Ctrl             | CROUCH                         | crouch / slide                  |
//! | Left Shift            | SPRINT                         | sprint (movement ability)       |
//! | Mouse Left            | FIRE                           | **primary cast** of the slotted spell |
//! | Mouse Right           | ALT_FIRE                       | **secondary cast** (charge/aim) |
//! | R                     | RELOAD                         | recharge / reattune the focus   |
//! | E                     | USE                            | interact / channel              |
//! | Q                     | MELEE                          | melee / staff strike            |
//! | 1..=6                 | weapon_slot                    | select the active ability slot  |
//! | Mouse move            | yaw / pitch                    | look (intent direction)         |
//!
//! Look angles are integrated here from raw mouse motion (pointer-locked) and fed
//! to both the outgoing frame and the camera, so what you aim at is what the
//! server hit-tests against.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use winit::event::{ElementState, MouseButton};
use winit::keyboard::KeyCode;

use arena_protocol::Tick;
use arena_protocol::input::{Buttons, InputFrame};

/// Mouse sensitivity in radians of look per pixel of motion. Tunable in settings.
const DEFAULT_SENSITIVITY: f32 = 0.0022;

/// The number of selectable ability slots (mapped to keys 1..=6).
pub const ABILITY_SLOTS: u8 = 6;

/// Accumulates raw input between ticks and produces [`InputFrame`]s.
pub struct Input {
    /// Live button intent, rebuilt as keys/mouse buttons go down/up.
    buttons: Buttons,
    /// Currently selected ability slot (0-based; wire field is `weapon_slot`).
    slot: u8,
    /// Integrated look angles, radians. `yaw` wraps to [-pi, pi]; `pitch` clamps.
    yaw: f32,
    pitch: f32,
    /// Mouse motion accumulated since the last tick (consumed by [`Input::end_tick`]).
    mouse_dx: f32,
    mouse_dy: f32,
    /// Look sensitivity (radians per pixel).
    pub sensitivity: f32,
    /// Monotonic input sequence number stamped on each frame for reconciliation.
    seq: u32,
    /// Whether the pointer is currently locked (mouse-look active).
    pub pointer_locked: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            buttons: Buttons::default(),
            slot: 0,
            yaw: 0.0,
            pitch: 0.0,
            mouse_dx: 0.0,
            mouse_dy: 0.0,
            sensitivity: DEFAULT_SENSITIVITY,
            seq: 0,
            pointer_locked: false,
        }
    }
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a key down/up. `pressed` is true on press, false on release.
    pub fn on_key(&mut self, code: KeyCode, pressed: bool) {
        match code {
            KeyCode::KeyW => self.buttons.set(Buttons::FORWARD, pressed),
            KeyCode::KeyS => self.buttons.set(Buttons::BACK, pressed),
            KeyCode::KeyA => self.buttons.set(Buttons::LEFT, pressed),
            KeyCode::KeyD => self.buttons.set(Buttons::RIGHT, pressed),
            KeyCode::Space => self.buttons.set(Buttons::JUMP, pressed),
            KeyCode::ControlLeft => self.buttons.set(Buttons::CROUCH, pressed),
            KeyCode::ShiftLeft => self.buttons.set(Buttons::SPRINT, pressed),
            KeyCode::KeyR => self.buttons.set(Buttons::RELOAD, pressed),
            KeyCode::KeyE => self.buttons.set(Buttons::USE, pressed),
            KeyCode::KeyQ => self.buttons.set(Buttons::MELEE, pressed),
            // Ability-slot selection on key-down only (edge, not hold).
            KeyCode::Digit1 if pressed => self.slot = 0,
            KeyCode::Digit2 if pressed => self.slot = 1,
            KeyCode::Digit3 if pressed => self.slot = 2,
            KeyCode::Digit4 if pressed => self.slot = 3,
            KeyCode::Digit5 if pressed => self.slot = 4,
            KeyCode::Digit6 if pressed => self.slot = 5,
            _ => {}
        }
    }

    /// Apply a mouse-button down/up. Left = primary cast, Right = secondary cast.
    pub fn on_mouse_button(&mut self, button: MouseButton, state: ElementState) {
        let pressed = state == ElementState::Pressed;
        match button {
            MouseButton::Left => self.buttons.set(Buttons::FIRE, pressed),
            MouseButton::Right => self.buttons.set(Buttons::ALT_FIRE, pressed),
            _ => {}
        }
    }

    /// Accumulate raw mouse motion (device-space pixels). Only consumed while the
    /// pointer is locked, so UI interaction never moves the view.
    pub fn on_mouse_motion(&mut self, dx: f32, dy: f32) {
        if self.pointer_locked {
            self.mouse_dx += dx;
            self.mouse_dy += dy;
        }
    }

    /// Convenience accessor for the winit/keyboard glue.
    pub fn key_state(state: ElementState) -> bool {
        state == ElementState::Pressed
    }

    /// Integrate the accumulated mouse motion into the look angles, then produce the
    /// frame for tick `client_tick`. Call once per fixed tick. The look is folded in
    /// here (not per mouse event) so the frame reflects all motion since last tick.
    pub fn end_tick(&mut self, client_tick: Tick) -> InputFrame {
        // Yaw increases to the right; pitch increases looking up. The minus on dy
        // gives the conventional "mouse up => look up" feel.
        self.yaw += self.mouse_dx * self.sensitivity;
        self.pitch += -self.mouse_dy * self.sensitivity;
        self.mouse_dx = 0.0;
        self.mouse_dy = 0.0;

        // Wrap yaw to [-pi, pi] and clamp pitch so the camera cannot flip over.
        self.yaw = (self.yaw + PI).rem_euclid(TAU) - PI;
        self.pitch = self.pitch.clamp(-FRAC_PI_2 + 0.001, FRAC_PI_2 - 0.001);

        self.seq = self.seq.wrapping_add(1);
        InputFrame {
            seq: self.seq,
            client_tick,
            buttons: self.buttons,
            yaw: self.yaw,
            pitch: self.pitch,
            weapon_slot: self.slot,
        }
        // The server re-sanitizes this (`InputFrame::sanitized`); we send honest
        // intent and let authority be authority.
    }

    /// Current look angles, for snapping the camera between ticks (so the view is
    /// smooth at render rate even though frames are produced at tick rate).
    pub fn look(&self) -> (f32, f32) {
        (self.yaw, self.pitch)
    }

    /// Selected ability slot.
    pub fn slot(&self) -> u8 {
        self.slot
    }

    /// Latest input sequence stamped (diagnostics / HUD netgraph).
    pub fn last_seq(&self) -> u32 {
        self.seq
    }
}

// ---------------------------------------------------------------------------
// Pointer lock (wasm)
// ---------------------------------------------------------------------------

/// Request pointer lock on the canvas so the browser feeds us raw, unbounded mouse
/// motion (the native build gets this for free from `DeviceEvent::MouseMotion`).
/// Typically called from a click handler, as browsers require a user gesture.
#[cfg(target_arch = "wasm32")]
pub fn request_pointer_lock() {
    use wasm_bindgen::JsCast;
    if let Some(canvas) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("cerena-canvas"))
        .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
    {
        canvas.request_pointer_lock();
    }
}
