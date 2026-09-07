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

/// Every non-comment spelling of a window query under `squallar-app/src`,
/// counted per file, with the test modules left out.
///
/// Calls, not prose: a `//` line naming a query is documentation and costs no
/// round trip.
fn spellings_of(needle: &str) -> Vec<(String, usize)> {
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
            let hits = text
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .map(|line| line.matches(needle).count())
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
    found
}

/// And the query has exactly one spelling in the crate outside its own module,
/// so a second per-frame poll cannot be added somewhere the test above does
/// not look.
///
/// `inner_size()` gets its own census below rather than sharing this one: it
/// has legitimate one-shot spellings that this query does not, so the expected
/// set is a different shape.
#[test]
fn the_minimized_query_has_one_spelling_in_this_crate() {
    assert_eq!(
        spellings_of(concat!(".is_", "minimized(")),
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

/// **The size query's census, and the reason it can have one now.** Until the
/// pump's spelling moved onto the gate, `inner_size()` had a frame-path call
/// site and the expected set here would have had to name it; the remaining
/// ones are all one-shot bring-up, so the set is closed and any new spelling
/// is a new poll.
///
/// The `ensure_rendering_state` half is what makes this a *frame-path* gate
/// rather than a count: a spelling that moved out of that fn and onto the
/// frame is a regression the totals alone would not see.
#[test]
fn the_size_query_is_spelled_only_in_the_gate_and_at_bring_up() {
    assert_eq!(
        spellings_of(concat!(".inner_", "size()")),
        vec![("app.rs".to_string(), 2), ("window_gate.rs".to_string(), 1),],
        "the inner-size query has a spelling this crate did not have. On X11 \
         it is a `GetGeometry` round trip to the display server, and the \
         frame path must reach it only through `WindowGate`.",
    );

    const APP: &str = include_str!("../app.rs");
    let needle = concat!(".inner_", "size()");
    let mut at = 0;
    let mut sites = Vec::new();
    while let Some(hit) = APP[at..].find(needle) {
        let offset = at + hit;
        at = offset + needle.len();
        // The enclosing method: the last `fn` header at method indentation
        // before this call.
        let header = APP[..offset]
            .rfind("\n    fn ")
            .expect("an inner_size() call outside any method in app.rs");
        let name = APP[header + "\n    fn ".len()..]
            .split(['(', '<'])
            .next()
            .unwrap_or_default();
        sites.push(name.to_string());
    }
    assert_eq!(
        sites,
        vec![
            "ensure_rendering_state".to_string(),
            "ensure_rendering_state".to_string(),
        ],
        "app.rs asks the window its size somewhere other than the two \
         `ensure_rendering_state` bring-ups. Both of those run once per \
         renderer; anything else is a per-frame round trip.",
    );
}

/// **The cost, as a gate rather than a stopwatch.** The frame now takes TWO
/// readings — `handle_redraw`'s minimized/zero-area gate and
/// `setup_egui_frame`'s zoom factor — and both have to come out of one ask.
///
/// A timing figure could not tell success from a cache that had simply gone
/// stale; this counts the round trips directly, so it is load-independent and
/// says nothing about how fast the box was.
#[test]
fn both_of_a_frames_readings_share_one_ask() {
    let server = Server::new(false, (1920, 1080));
    let mut gate = WindowGate::default();

    for _ in 0..200 {
        // `handle_redraw`: minimized and zero-area.
        let gated = server.read(&mut gate);
        assert!(!gated.minimized && !gated.zero_area());
        // `setup_egui_frame`: the width the zoom factor divides by.
        assert_eq!(server.read(&mut gate).size, (1920, 1080));
    }

    assert_eq!(
        server.asks.get(),
        1,
        "200 frames made more than one round trip across 400 readings. Before \
         the gate this was two queries per frame; the pump's third would have \
         made it three.",
    );
}

/// **Work held at the second read.** Every transition that moves the size,
/// driven through the event that carries it, with the window's truth CHANGED
/// FIRST — and then asserted at the reading `setup_egui_frame` divides by,
/// not just at the gate's.
///
/// The vacuity this is built against: a cached size that goes stale is
/// *faster* than a correct one, and "the cached size equals the live size"
/// passes trivially on a window nothing has resized. So every row moves the
/// window, and every row checks the ratio and not only the size — a stale
/// width is wrong by exactly the resize ratio, and the zoom factor feeds
/// rendering geometry.
///
/// The surface follows the window through `handle_resized` →
/// `AppState::resize_surface` on the same `Resized` event that retires the
/// reading, so a fresh gate makes the two terms agree and the ratio comes out
/// 1.0. That is the row's real assertion; the stale gate's answer is named
/// beside it so the row cannot pass by accident.
#[test]
fn the_zoom_factors_width_survives_every_size_transition() {
    // The one arithmetic `setup_egui_frame` does with the width. Pinned to
    // the source below so this copy cannot drift from the real one.
    let zoom = |surface_w: u32, window_w: u32| surface_w as f32 / window_w.max(1) as f32;

    for (what, before, after) in [
        ("a drag-resize", (1920u32, 1080u32), (1280u32, 720u32)),
        ("maximise", (1280, 720), (2560, 1440)),
        ("restore from maximised", (2560, 1440), (1280, 720)),
        (
            "a monitor change that resized the window",
            (1920, 1080),
            (3440, 1440),
        ),
    ] {
        let server = Server::new(false, before);
        let mut gate = WindowGate::default();
        assert_eq!(
            server.read(&mut gate).size,
            before,
            "{what}: setup did not take"
        );

        // The window moves, and the surface is reconfigured off the same
        // event.
        server.set(false, after);
        gate.note_event(&WindowEvent::Resized(PhysicalSize::new(after.0, after.1)));
        let surface_w = after.0;

        // The frame: the gate's reading, then the pump's.
        server.read(&mut gate);
        let pump = server.read(&mut gate);

        assert_eq!(
            zoom(surface_w, pump.size.0),
            zoom(surface_w, after.0),
            "after {what} the zoom factor was computed off the width from \
             BEFORE it. The cached read and a live read must agree here — a \
             stale width scales the whole frame's geometry by the resize \
             ratio, which is visible on screen and not merely wrong in a \
             counter.",
        );
        assert_eq!(
            zoom(surface_w, pump.size.0),
            1.0,
            "{what}: the surface and the window disagree on the frame after \
             the resize, so one of the two terms is a frame behind",
        );
        assert_ne!(
            zoom(surface_w, before.0),
            1.0,
            "{what} does not move the width far enough for a stale reading to \
             be distinguishable, so this row cannot fail",
        );
    }
}

/// The scale-factor change's row, driven the only way it can be. Winit's
/// `InnerSizeWriter` has no public constructor, so `ScaleFactorChanged` cannot
/// be built from outside winit and no test in this crate can hand one to
/// `note_event`. What is checked instead is the pair that composes to the same
/// thing: `the_scale_factor_change_is_on_the_retiring_side` holds that the
/// variant reaches `invalidate`, and this holds that an `invalidate` is seen
/// at the pump's reading and not only the gate's.
///
/// The same pair carries the monitor change that moves DPI without resizing,
/// and `create_window`'s hand-invalidate.
#[test]
fn a_retired_reading_is_retired_for_the_pump_too() {
    let server = Server::new(false, (1920, 1080));
    let mut gate = WindowGate::default();
    server.read(&mut gate);
    server.read(&mut gate);

    // What `note_event` does for `ScaleFactorChanged`, and what
    // `create_window` does by hand.
    server.set(false, (3840, 2160));
    gate.invalidate();

    assert_eq!(
        server.read(&mut gate).size,
        (3840, 2160),
        "the gate's own reading survived the invalidate",
    );
    assert_eq!(
        server.read(&mut gate).size,
        (3840, 2160),
        "the pump's reading survived the invalidate. A scale-factor change \
         moves the inner size with no `Resized` beside it, so this is the \
         only thing standing between a DPI change and a frame drawn at the \
         old geometry.",
    );
    assert_eq!(
        server.asks.get(),
        2,
        "the retired reading cost more than one fresh ask across both of the \
         frame's readings",
    );
}

/// **The precondition the pump's read depends on, held on the source text.**
///
/// `setup_egui_frame` reading the cache is only correct if the cache is
/// current for THIS frame. It is, because `handle_redraw` reads the gate
/// before it decides the frame is worth building and `setup_egui_frame` is
/// that same call's callee — so the pump's read is the second read of one
/// reading, never a first read of an older one. Reorder those two and the
/// round trip would still vanish while the value went stale, which is exactly
/// the failure a timing figure cannot see.
///
/// Held on the text because the property is an ordering between two calls, and
/// neither one can observe the other from inside itself.
#[test]
fn the_pump_reads_the_gate_after_the_frame_gate_has() {
    const APP: &str = include_str!("../app.rs");
    const RENDER: &str = include_str!("../app_render.rs");

    let redraw = APP
        .split_once("fn handle_redraw(&mut self) {")
        .map(|(_, rest)| rest)
        .expect("`handle_redraw` is no longer a method on App");
    let redraw = redraw
        .split_once("\n    fn ")
        .map(|(head, _)| head)
        .unwrap_or(redraw);

    let gate_read = redraw
        .find(concat!("self.window_gate.", "read("))
        .expect("`handle_redraw` no longer takes the frame's reading");
    let guard = redraw
        .find("self.window.is_none()")
        .expect("`handle_redraw` no longer returns early on a missing window");
    let setup = redraw
        .find("self.setup_egui_frame(")
        .expect("`handle_redraw` no longer lays out a frame");

    assert!(
        gate_read < setup,
        "`setup_egui_frame` runs before the frame's reading is taken, so the \
         zoom factor divides by a width from a previous frame — or, on the \
         first frame, pays the round trip the gate exists to remove",
    );
    assert!(
        gate_read < guard && guard < setup,
        "the missing-window early return no longer sits between the reading \
         and the pump. `handle_redraw` takes no reading at all when \
         `self.window` is `None`, and that guard is what stops \
         `setup_egui_frame` being reached in that state and asking the window \
         itself.",
    );

    let setup_body = RENDER
        .split_once("fn setup_egui_frame(")
        .map(|(_, rest)| rest)
        .expect("`setup_egui_frame` is no longer a method here");
    let setup_body = setup_body
        .split_once("\n    }")
        .map(|(body, _)| body)
        .expect("`setup_egui_frame` has no recognisable body");

    assert!(
        !setup_body.contains(concat!(".inner_", "size()")),
        "`setup_egui_frame` asks the window its size again. On X11 that is a \
         `GetGeometry` round trip on every frame — read it off the gate.",
    );
    assert!(
        setup_body.contains(concat!("self.window_gate.", "read(window).size")),
        "`setup_egui_frame` no longer reads the width off the gate, so this \
         test is asserting an ordering over a frame path that has moved",
    );
    assert!(
        setup_body.contains("state.surface_config.width as f32 / window_w.max(1) as f32"),
        "the zoom factor is no longer `surface width / window width`, so the \
         copy of that arithmetic in \
         `the_zoom_factors_width_survives_every_size_transition` has drifted \
         from the real one and is proving nothing",
    );
}
