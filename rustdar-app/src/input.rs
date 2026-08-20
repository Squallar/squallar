use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

#[derive(Default)]
pub struct InputHandler {
    escape_pressed: bool,
    back_pressed: bool,
}

impl InputHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear_frame_state(&mut self) {
        self.escape_pressed = false;
        self.back_pressed = false;
    }

    pub fn process_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.note_key(
                    key_event.physical_key,
                    &key_event.logical_key,
                    key_event.repeat,
                );
                true
            }
            _ => false,
        }
    }

    fn note_key(&mut self, physical: PhysicalKey, logical: &Key, repeat: bool) {
        // Auto-repeat is not a second press.
        if repeat {
            return;
        }
        // Escape by *physical* key, so it survives a layout that remaps it.
        if let PhysicalKey::Code(keycode) = physical
            && keycode == KeyCode::Escape
        {
            self.escape_pressed = true;
        }
        // Back by *logical* key: Android's button and the browser's are named keys with no
        // physical keycode at all.
        if let Key::Named(NamedKey::GoBack | NamedKey::BrowserBack) = logical {
            self.back_pressed = true;
        }
    }

    /// Take this frame's "back out of the thing I am in" press — Escape or the back button
    /// — if there was one.
    pub fn take_back_out_press(&mut self) -> bool {
        let pressed = self.escape_pressed || self.back_pressed;
        self.escape_pressed = false;
        self.back_pressed = false;
        pressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    /// A key-down, decoded by the shipped `note_key`.
    fn press(handler: &mut InputHandler, physical: PhysicalKey, logical: Key, repeat: bool) {
        handler.note_key(physical, &logical, repeat);
    }

    fn escape() -> (PhysicalKey, Key) {
        (
            PhysicalKey::Code(KeyCode::Escape),
            Key::Named(NamedKey::Escape),
        )
    }

    /// Android's back button and the browser's, which arrive as different named keys and
    /// must both count.
    fn back_keys() -> [(&'static str, (PhysicalKey, Key)); 2] {
        [
            (
                "GoBack",
                (
                    PhysicalKey::Code(KeyCode::BrowserBack),
                    Key::Named(NamedKey::GoBack),
                ),
            ),
            (
                "BrowserBack",
                (
                    PhysicalKey::Code(KeyCode::BrowserBack),
                    Key::Named(NamedKey::BrowserBack),
                ),
            ),
        ]
    }

    /// Every key that must count as a back-out, named for assertion messages.
    fn back_out_keys() -> Vec<(&'static str, (PhysicalKey, Key))> {
        std::iter::once(("escape", escape()))
            .chain(back_keys())
            .collect()
    }

    fn letter_a() -> (PhysicalKey, Key) {
        (
            PhysicalKey::Code(KeyCode::KeyA),
            Key::Character(SmolStr::new("a")),
        )
    }

    /// Escape and back must both mean "back out".
    #[test]
    fn either_key_counts_as_a_back_out() {
        for (name, (physical, logical)) in back_out_keys() {
            let mut input = InputHandler::new();
            press(&mut input, physical, logical, false);
            assert!(
                input.take_back_out_press(),
                "{name} did not count as a back-out"
            );
        }

        let mut neither = InputHandler::new();
        let (physical, logical) = letter_a();
        press(&mut neither, physical, logical, false);
        assert!(
            !neither.take_back_out_press(),
            "an ordinary key spent a back press, so typing would close dialogs"
        );
    }

    /// Auto-repeat is not a second press.
    #[test]
    fn holding_a_key_down_is_still_one_press() {
        for (name, (physical, logical)) in back_out_keys() {
            let mut input = InputHandler::new();
            press(&mut input, physical, logical.clone(), false);
            assert!(
                input.take_back_out_press(),
                "{name}: the first press was lost"
            );

            for repeat in 0..5 {
                press(&mut input, physical, logical.clone(), true);
                assert!(
                    !input.take_back_out_press(),
                    "{name}: auto-repeat {repeat} spent another press, so \
                     holding the key closes every open layer and then leaves \
                     the app"
                );
            }
        }
    }

    /// One physical press must be spendable once.
    #[test]
    fn a_back_out_press_is_spent_when_it_is_taken() {
        for (name, (physical, logical)) in back_out_keys() {
            let mut input = InputHandler::new();
            press(&mut input, physical, logical, false);
            assert!(input.take_back_out_press(), "{name}");
            assert!(
                !input.take_back_out_press(),
                "{name}: the press was still there to be spent a second time, \
                 so the next key of any kind dismisses another layer"
            );
        }
    }

    /// Escape is read from the physical key and back from the logical one.
    #[test]
    fn each_key_is_read_from_the_side_that_carries_it() {
        let mut logical_only = InputHandler::new();
        press(
            &mut logical_only,
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
            Key::Named(NamedKey::GoBack),
            false,
        );
        assert!(
            logical_only.take_back_out_press(),
            "a back button with no physical keycode was dropped"
        );

        let mut physical_only = InputHandler::new();
        press(
            &mut physical_only,
            PhysicalKey::Code(KeyCode::Escape),
            Key::Character(SmolStr::new("q")),
            false,
        );
        assert!(
            physical_only.take_back_out_press(),
            "Escape was dropped because the layout reported another character"
        );
    }

    /// `process_event` must hand `note_key` the event's own three fields.
    #[test]
    fn process_event_forwards_the_events_own_fields() {
        let (_, rest) = include_str!("input.rs")
            .split_once("pub fn process_event(")
            .expect("process_event is no longer a method here");
        let body = rest
            .split_once("\n    }")
            .map(|(body, _)| body)
            .expect("process_event has no recognisable body");

        for field in [
            "key_event.physical_key",
            "key_event.logical_key",
            "key_event.repeat",
        ] {
            assert!(
                body.contains(field),
                "process_event no longer forwards {field}, so note_key decides \
                 on a value the event never carried: {body}"
            );
        }
    }
}
