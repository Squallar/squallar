use std::collections::HashSet;
use winit::event::{ElementState, Event, KeyEvent, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

/// Type alias for a set of keyboard keys
type KeySet = HashSet<KeyCode>;

/// A simple input handler to replace winit_input_helper
#[derive(Default)]
pub struct InputHandler {
    keys_pressed: KeySet,
    keys_held: KeySet,
}

impl InputHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear per-frame input state. Should be called once at the start of each frame.
    pub fn clear_frame_state(&mut self) {
        self.keys_pressed.clear();
    }

    /// Process a single input event.
    /// Returns true if the event was an input-related event that was handled.
    pub fn process_event<T>(&mut self, event: &Event<T>) -> bool {
        match event {
            Event::WindowEvent {
                event:
                    WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                physical_key: PhysicalKey::Code(keycode),
                                state,
                                ..
                            },
                        ..
                    },
                ..
            } => {
                match state {
                    ElementState::Pressed => {
                        if !self.keys_held.contains(keycode) {
                            self.keys_pressed.insert(*keycode);
                        }
                        self.keys_held.insert(*keycode);
                    }
                    ElementState::Released => {
                        self.keys_held.remove(keycode);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Check if a key was just pressed this frame
    pub fn key_pressed(&self, keycode: KeyCode) -> bool {
        self.keys_pressed.contains(&keycode)
    }
}
