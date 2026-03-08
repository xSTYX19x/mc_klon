use glam::Vec3;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window};

use crate::camera::Camera;
use crate::settings::{InputAction, InputBindings};

pub(crate) struct CameraController {
    pub(crate) move_forward: bool,
    pub(crate) move_backward: bool,
    pub(crate) move_left: bool,
    pub(crate) move_right: bool,
    pub(crate) move_up: bool,
    pub(crate) move_down: bool,
    pub(crate) sprint: bool,
    pub(crate) sprint_held: bool,
    pub(crate) sprint_latched: bool,
    pub(crate) jump_requested: bool,
    pub(crate) mouse_sens: f32,
    pub(crate) mouse_captured: bool,
}

impl Default for CameraController {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraController {
    pub(crate) fn new() -> Self {
        Self {
            move_forward: false,
            move_backward: false,
            move_left: false,
            move_right: false,
            move_up: false,
            move_down: false,
            sprint: false,
            sprint_held: false,
            sprint_latched: false,
            jump_requested: false,
            mouse_sens: 0.002,
            mouse_captured: false,
        }
    }

    pub(crate) fn process_key(&mut self, event: &KeyEvent, bindings: &InputBindings) {
        let pressed = event.state == ElementState::Pressed;
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };

        for (action, target) in [
            (InputAction::MoveForward, &mut self.move_forward),
            (InputAction::MoveBackward, &mut self.move_backward),
            (InputAction::MoveLeft, &mut self.move_left),
            (InputAction::MoveRight, &mut self.move_right),
            (InputAction::Jump, &mut self.move_up),
            (InputAction::Sneak, &mut self.move_down),
            (InputAction::Sprint, &mut self.sprint_held),
        ] {
            if bindings.key_for(action) == code {
                *target = pressed;
            }
        }
        self.refresh_sprint();
    }

    pub(crate) fn process_mouse_motion(&mut self, camera: &mut Camera, dx: f64, dy: f64) {
        if !self.mouse_captured {
            return;
        }

        camera.yaw += (dx as f32) * self.mouse_sens;
        camera.pitch -= (dy as f32) * self.mouse_sens;

        let limit = 89.0_f32.to_radians();
        camera.pitch = camera.pitch.clamp(-limit, limit);
    }

    pub(crate) fn horizontal_move_input(&self, camera: &Camera) -> Vec3 {
        let forward_flat = camera.forward_flat();
        let right_flat = camera.right_flat();
        let mut horizontal = Vec3::ZERO;
        if self.move_forward {
            horizontal += forward_flat;
        }
        if self.move_backward {
            horizontal -= forward_flat;
        }
        if self.move_right {
            horizontal += right_flat;
        }
        if self.move_left {
            horizontal -= right_flat;
        }
        horizontal
    }

    pub(crate) fn consume_jump_requested(&mut self) -> bool {
        let requested = self.jump_requested;
        self.jump_requested = false;
        requested
    }

    pub(crate) fn request_jump(&mut self) {
        self.jump_requested = true;
    }

    pub(crate) fn clear_jump_requested(&mut self) {
        self.jump_requested = false;
    }

    pub(crate) fn clear_inputs(&mut self) {
        self.move_forward = false;
        self.move_backward = false;
        self.move_left = false;
        self.move_right = false;
        self.move_up = false;
        self.move_down = false;
        self.sprint_held = false;
        self.sprint_latched = false;
        self.sprint = false;
        self.jump_requested = false;
    }

    pub(crate) fn set_sprint_latched(&mut self, active: bool) {
        self.sprint_latched = active;
        self.refresh_sprint();
    }

    fn refresh_sprint(&mut self) {
        self.sprint = self.sprint_held || self.sprint_latched;
    }

    pub(crate) fn set_mouse_capture(&mut self, window: &Window, capture: bool) {
        self.mouse_captured = capture;

        if capture {
            window.set_cursor_visible(false);
            let _ = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
        }
    }
}

pub(crate) fn hotbar_slot_for_key(code: KeyCode) -> Option<usize> {
    match code {
        KeyCode::Digit1 => Some(0),
        KeyCode::Digit2 => Some(1),
        KeyCode::Digit3 => Some(2),
        KeyCode::Digit4 => Some(3),
        KeyCode::Digit5 => Some(4),
        KeyCode::Digit6 => Some(5),
        KeyCode::Digit7 => Some(6),
        KeyCode::Digit8 => Some(7),
        KeyCode::Digit9 => Some(8),
        _ => None,
    }
}
