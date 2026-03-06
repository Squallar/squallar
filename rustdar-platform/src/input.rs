use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// A simple input handler for keyboard events.
/// Tracks keys pressed this frame (no held-key tracking needed).
/// Handles Escape on desktop and the Android back button (GoBack).
#[derive(Default)]
pub struct InputHandler {
    escape_pressed: bool,
    back_pressed: bool,
}

impl InputHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear per-frame input state. Should be called once at the start of each frame.
    pub fn clear_frame_state(&mut self) {
        self.escape_pressed = false;
        self.back_pressed = false;
    }

    /// Process a single window event for input handling.
    /// Accepts `&WindowEvent` directly to avoid cloning the entire event.
    /// Returns true if the event was an input-related event that was handled.
    pub fn process_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                // Check physical key for Escape
                if let PhysicalKey::Code(keycode) = key_event.physical_key {
                    if keycode == KeyCode::Escape {
                        self.escape_pressed = true;
                    }
                }
                // Check logical key for Android back button and browser back
                match &key_event.logical_key {
                    Key::Named(NamedKey::GoBack | NamedKey::BrowserBack) => {
                        self.back_pressed = true;
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    /// Check if a key was just pressed this frame
    pub fn key_pressed(&self, keycode: KeyCode) -> bool {
        match keycode {
            KeyCode::Escape => self.escape_pressed,
            _ => false,
        }
    }

    /// Check if the Android back button was pressed this frame
    pub fn back_pressed(&self) -> bool {
        self.back_pressed
    }
}
