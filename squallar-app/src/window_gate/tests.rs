//! The cache's two obligations: never ask the window twice for an answer no
//! event has retired, and never serve an answer an event HAS retired.
//!
//! The second is the correctness half and is the only one with a user-visible
//! failure: a `minimized = true` that outlives the restore freezes the window.
//! See the module header for why the four retiring events are the ones that
//! define the value rather than a guess at which ones look related.

use super::{Reading, WindowGate, invalidated_by};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{
    DeviceId, ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};

/// A reading, and a counter saying how many times the gate went and asked for
/// one. The whole point of the cache is the second number.
struct Server {
    reading: std::cell::Cell<Reading>,
    asks: std::cell::Cell<u32>,
}

impl Server {
    fn new(minimized: bool, size: (u32, u32)) -> Self {
        Self {
            reading: std::cell::Cell::new(Reading { minimized, size }),
            asks: std::cell::Cell::new(0),
        }
    }

    /// What the window would answer if asked right now.
    fn set(&self, minimized: bool, size: (u32, u32)) {
        self.reading.set(Reading { minimized, size });
    }

    fn read(&self, gate: &mut WindowGate) -> Reading {
        gate.read_or_else(|| {
            self.asks.set(self.asks.get() + 1);
            self.reading.get()
        })
    }
}

fn live(size: (u32, u32)) -> Reading {
    Reading {
        minimized: false,
        size,
    }
}

#[test]
fn the_first_read_asks_the_window() {
    let server = Server::new(false, (1920, 1080));
    let mut gate = WindowGate::default();

    assert_eq!(server.read(&mut gate), live((1920, 1080)));
    assert_eq!(server.asks.get(), 1, "the first read must reach the window");
}

/// **The defect this exists for.** Two X11 round trips per frame — measured at
/// a steady 159 µs on every interact frame — to re-ask a question no event has
/// answered differently.
#[test]
fn a_warm_reading_is_served_without_asking_again() {
    let server = Server::new(false, (1920, 1080));
    let mut gate = WindowGate::default();

    for _ in 0..200 {
        assert_eq!(server.read(&mut gate), live((1920, 1080)));
    }
    assert_eq!(
        server.asks.get(),
        1,
        "200 frames asked the window more than once, which is the poll this \
         cache replaced",
    );
}

/// The frames the cache exists for must not retire it, or it saves nothing.
#[test]
fn the_input_families_do_not_retire_a_reading() {
    let server = Server::new(false, (1920, 1080));
    let mut gate = WindowGate::default();
    server.read(&mut gate);

    let device_id = DeviceId::dummy();
    for event in [
        WindowEvent::CursorMoved {
            device_id,
            position: PhysicalPosition::new(10.0, 10.0),
        },
        WindowEvent::MouseWheel {
            device_id,
            delta: MouseScrollDelta::LineDelta(0.0, 1.0),
            phase: TouchPhase::Moved,
        },
        WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::CursorEntered { device_id },
        WindowEvent::CursorLeft { device_id },
        WindowEvent::Moved(PhysicalPosition::new(4, 4)),
        WindowEvent::RedrawRequested,
        WindowEvent::CloseRequested,
    ] {
        assert!(
            !invalidated_by(&event),
            "{event:?} retires the reading, so an interact frame pays the \
             window query this cache exists to remove",
        );
        gate.note_event(&event);
        server.read(&mut gate);
    }

    assert_eq!(server.asks.get(), 1, "an input event re-read the window");
}

/// **Every transition that can move either answer, driven through the event
/// that carries it, with the window's truth CHANGED FIRST.**
///
/// This is the work-held half, and the vacuity it is built against is
/// specific: a cache makes the cost vanish whether or not the value it serves
/// is right, and a test that asserts "the cached size equals the live size"
/// passes trivially on a window nothing has resized. So every row here moves
/// what the window would answer and then REQUIRES the gate to serve the new
/// answer. A cache that never refreshed fails the row; it cannot pass it by
/// doing nothing.
///
/// The restore rows are doubled because the event that carries a restore
/// differs by backend: X11 unmaps an iconified window, so de-iconifying is a
/// `MapNotify` and winit re-issues it as `Focused`; macOS changes occlusion
/// state and becomes key; Windows sends `WM_SIZE`, which is a `Resized`.
#[test]
fn every_transition_reaches_the_next_frame() {
    let live = |w, h| Reading {
        minimized: false,
        size: (w, h),
    };
    let hidden = |w, h| Reading {
        minimized: true,
        size: (w, h),
    };

    for (what, before, event, after) in [
        (
            "a drag-resize",
            live(1920, 1080),
            WindowEvent::Resized(PhysicalSize::new(1280, 720)),
            live(1280, 720),
        ),
        (
            "maximise",
            live(1280, 720),
            WindowEvent::Resized(PhysicalSize::new(2560, 1440)),
            live(2560, 1440),
        ),
        (
            "restore from maximised",
            live(2560, 1440),
            WindowEvent::Resized(PhysicalSize::new(1280, 720)),
            live(1280, 720),
        ),
        (
            "a resize to nothing",
            live(1920, 1080),
            WindowEvent::Resized(PhysicalSize::new(0, 0)),
            live(0, 0),
        ),
        (
            "a monitor change that resized the window",
            live(1920, 1080),
            WindowEvent::Resized(PhysicalSize::new(3440, 1440)),
            live(3440, 1440),
        ),
        (
            "minimise, seen as a focus loss",
            live(1920, 1080),
            WindowEvent::Focused(false),
            hidden(1920, 1080),
        ),
        (
            "minimise, seen as an occlusion",
            live(1920, 1080),
            WindowEvent::Occluded(true),
            hidden(1920, 1080),
        ),
        (
            "un-minimise on X11, which arrives as a MapNotify re-issued focus",
            hidden(1920, 1080),
            WindowEvent::Focused(true),
            live(1920, 1080),
        ),
        (
            "un-minimise on macOS, which arrives as an occlusion change",
            hidden(1920, 1080),
            WindowEvent::Occluded(false),
            live(1920, 1080),
        ),
        (
            "un-minimise on Windows, which arrives as WM_SIZE",
            hidden(1920, 1080),
            WindowEvent::Resized(PhysicalSize::new(1920, 1080)),
            live(1920, 1080),
        ),
        (
            "the window closing",
            live(1920, 1080),
            WindowEvent::Destroyed,
            live(0, 0),
        ),
    ] {
        let server = Server::new(before.minimized, before.size);
        let mut gate = WindowGate::default();
        assert_eq!(server.read(&mut gate), before, "{what}: setup did not take");

        server.set(after.minimized, after.size);
        gate.note_event(&event);

        assert_eq!(
            server.read(&mut gate),
            after,
            "after {what} the gate served the reading from BEFORE it. A stale \
             size draws into a surface that is not there; a stale minimized \
             flag abandons every frame of a window the user is looking at.",
        );
        assert_eq!(
            server.asks.get(),
            2,
            "{what} did not send the gate back to the window at all",
        );
    }
}

/// The zero-area arm of the gate specifically: a window resized to nothing is
/// one `handle_redraw` abandons the frame on, and it has to see the zero on
/// the frame the resize lands rather than later.
#[test]
fn a_resize_to_zero_area_is_visible_on_the_next_frame() {
    let server = Server::new(false, (1920, 1080));
    let mut gate = WindowGate::default();
    assert!(!server.read(&mut gate).zero_area());

    server.set(false, (0, 0));
    gate.note_event(&WindowEvent::Resized(PhysicalSize::new(0, 0)));

    assert!(
        server.read(&mut gate).zero_area(),
        "a zero-area window went on being drawn into after its resize",
    );
}

/// A new window is a new set of answers, and the backends that satisfy
/// `request_inner_size` synchronously emit no event for it — so the creation
/// path invalidates by hand rather than waiting for one.
#[test]
fn an_explicit_invalidate_retires_the_reading() {
    let server = Server::new(false, (1920, 1080));
    let mut gate = WindowGate::default();
    server.read(&mut gate);

    server.set(false, (1280, 720));
    gate.invalidate();

    assert_eq!(server.read(&mut gate), live((1280, 720)));
    assert_eq!(server.asks.get(), 2);
}

/// `ScaleFactorChanged` carries an `InnerSizeWriter`, so the inner size can
/// move across it with no `Resized` beside it.
///
/// Held on the source text rather than on a call, because
/// `InnerSizeWriter::new` is `pub(crate)` to winit and the event cannot be
/// built from outside it. The exhaustive `match` is what makes this readable:
/// every variant is named exactly once, so "before the `=> true`" is the whole
/// of the retiring set.
#[test]
fn the_scale_factor_change_is_on_the_retiring_side() {
    const SRC: &str = include_str!("../window_gate.rs");
    let body = SRC
        .split_once("pub(crate) fn invalidated_by(")
        .map(|(_, rest)| rest)
        .expect("`invalidated_by` is no longer a free fn in this module");
    let (retiring, rest) = body
        .split_once("=> true,")
        .expect("`invalidated_by` no longer has a `=> true` arm");

    for name in [
        "WindowEvent::Resized",
        "WindowEvent::ScaleFactorChanged",
        "WindowEvent::Occluded",
        "WindowEvent::Focused",
        "WindowEvent::Destroyed",
    ] {
        assert!(
            retiring.contains(name),
            "{name} is not on the retiring side of `invalidated_by`; a \
             reading can now outlive the event that defines it",
        );
        assert!(
            !rest.contains(&format!("| {name}")),
            "{name} is named on both sides of `invalidated_by`",
        );
    }
}

/// **The regression gate on the frame path.** The two queries this cache
/// replaced were a steady 159 µs on every interact frame; the cheapest way for
/// them to come back is for somebody to spell one in `handle_redraw` again.
///
/// Held on the source text because the property is "this call is not made
/// here", and an absence cannot be observed from inside a call that has
/// already returned.
#[test]
fn handle_redraw_asks_the_window_nothing() {
    const APP: &str = include_str!("../app.rs");
    let body = APP
        .split_once("fn handle_redraw(&mut self) {")
        .map(|(_, rest)| rest)
        .expect("`handle_redraw` is no longer a method on App");
    let body = body
        .split_once("\n    fn ")
        .map(|(head, _)| head)
        .expect("`handle_redraw` is the last fn in app.rs; bound the slice");

    for query in ["is_minimized(", "inner_size()"] {
        assert!(
            !body.contains(query),
            "`handle_redraw` spells `{query}` again. On X11 that is a \
             synchronous round trip to the display server on every frame — \
             the 159 µs floor `WindowGate` exists to remove. Read it off the \
             gate instead.",
        );
    }
    assert!(
        body.contains("window_gate"),
        "`handle_redraw` no longer reads the gate at all, so this test is \
         asserting an absence over a frame path that has moved",
    );
}

/// And the query has exactly one spelling in the crate outside its own module,
/// so a second per-frame poll cannot be added somewhere the test above does
/// not look.
///
/// `inner_size()` is deliberately NOT counted here: three of its four call
/// sites are one-shot bring-up (`ensure_rendering_state`) or live in a
/// different segment's cut, and only the frame gate's pair was the floor.
#[test]
fn the_minimized_query_has_one_spelling_in_this_crate() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("squallar-app/src is readable") {
            let path = entry.expect("a readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.contains("test") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a readable .rs file");
            // Calls, not prose: a `//` line naming the query is documentation
            // and costs no round trip.
            let hits = text
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .map(|line| line.matches(concat!(".is_", "minimized(")).count())
                .sum::<usize>();
            if hits > 0 {
                found.push((
                    path.strip_prefix(&root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                    hits,
                ));
            }
        }
    }
    found.sort();
    assert_eq!(
        found,
        vec![("window_gate.rs".to_string(), 1)],
        "the minimized query is spelled outside `WindowGate`. On X11 it is a \
         display-server round trip, and every spelling on a per-frame path is \
         a floor under every frame.",
    );
}

/// **The wiring, held where the behaviour cannot see it.** Every test above
/// drives `note_event` by hand, so all of them stay green on an app that never
/// calls it — and an app that never calls it serves the boot reading forever.
///
/// Two positions, both load-bearing. `note_event` must be the FIRST thing
/// `window_event` does, because everything after it in that handler may read
/// the gate (`RedrawRequested` runs the whole frame from inside it). And
/// `create_window` must invalidate by hand: a new window is a new pair of
/// answers, and `request_inner_size` is satisfied synchronously with no event
/// on the backends that can do it.
#[test]
fn the_app_retires_the_reading_where_it_has_to() {
    const APP: &str = include_str!("../app.rs");

    let body = |head: &str| {
        let rest = APP
            .split_once(head)
            .unwrap_or_else(|| panic!("`{head}` is gone from app.rs"))
            .1;
        rest.split_once("\n    fn ")
            .map(|(body, _)| body.to_string())
            .unwrap_or_else(|| rest.to_string())
    };

    let handler = body("fn window_event(");
    let note = handler
        .find(concat!("self.window_gate.", "note_event(&event)"))
        .expect(
            "`window_event` no longer tells the gate about the event, so the              cached minimized flag and inner size are never retired and the              app serves its boot reading for the life of the window",
        );
    let first_read = handler
        .find(concat!("self.input.", "process_event"))
        .expect("`window_event` no longer opens with the input pump; re-anchor this test");
    assert!(
        note < first_read,
        "`window_event` reads before it retires. `RedrawRequested` runs the          whole frame from inside this handler, so an event that moved the          window would be seen one frame late.",
    );

    assert!(
        body("fn create_window(").contains(concat!("self.window_gate.", "invalidate()")),
        "`create_window` no longer retires the reading. A new window is a new          pair of answers, and `request_inner_size` is satisfied synchronously          with no event on the backends that can satisfy it at once.",
    );
}
