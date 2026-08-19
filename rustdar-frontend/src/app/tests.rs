use super::*;
use crate::platform_double::TestBridge;
use rustdar_egui::overlay_cache::OverlayTexturePlan;
use rustdar_geo::GeoBounds;
use rustdar_kv::MemoryKvStore;
use rustdar_overlays::render::overlay_state::OverlayKind;
use std::sync::atomic::{AtomicBool, Ordering};

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -100.0,
        max_lon: -90.0,
    }
}

/// The browser build asks for WebGL2, and for nothing else.
///
/// The `const _` further up this file asserts wgpu's `webgl` feature is
/// *compiled in*. It does not assert that this build asks for it, and the
/// gap is not academic: delete the `backends: wgpu::Backends::GL` line and
/// the const assert still passes, `cargo check --target
/// wasm32-unknown-unknown` still exits 0, and every browser silently falls
/// back to `Backends::all()`. Chrome then picks WebGPU while Firefox stays
/// on WebGL2, and one binary runs two different, separately-broken
/// rendering paths — exactly what `instance_descriptor`'s own doc says it
/// exists to prevent.
#[test]
fn the_browser_build_asks_for_webgl2_and_refuses_webgpu() {
    // A base that is deliberately *not* GL, so "the browser arm restricts
    // to GL" cannot be satisfied by the base already being GL. Supplying it
    // is the whole reason `backends_for` takes a base: an earlier version
    // read the environment inline and could only compare against whatever
    // `WGPU_BACKEND` said, so with `WGPU_BACKEND=gl` exported the
    // `backends` line could be deleted with the gate still green. Measured,
    // not hypothetical.
    let base = |backends| wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    };

    for offered in [
        wgpu::Backends::all(),
        wgpu::Backends::VULKAN,
        wgpu::Backends::BROWSER_WEBGPU,
        wgpu::Backends::VULKAN.union(wgpu::Backends::BROWSER_WEBGPU),
        wgpu::Backends::empty(),
    ] {
        let web = backends_for(true, base(offered)).backends;
        assert_eq!(
            web,
            wgpu::Backends::GL,
            "offered {offered:?}, the browser build asked for {web:?} \
                 rather than WebGL2 alone"
        );
        assert!(!web.contains(wgpu::Backends::BROWSER_WEBGPU));

        // Native is deliberately unrestricted: it passes the base through
        // untouched, which is what keeps `WGPU_BACKEND` working. That is
        // the other half of the fork and the reason the browser arm cannot
        // simply be the default.
        let native = backends_for(false, base(offered)).backends;
        assert_eq!(native, offered, "the native arm altered the base");
    }

    // And the shipped path really does read the environment, which is the
    // claim the parameter moved out of `backends_for` and into its caller.
    assert_eq!(
        backends_for(
            false,
            wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        )
        .backends,
        wgpu::Backends::all().with_env()
    );
}

/// And that this build asks on its own behalf.
///
/// Both arms above run from one host binary, so the remaining unchecked
/// claim is which one `instance_descriptor` selects. That is one `cfg!` on
/// one line, and every way of getting it wrong — another arch,
/// `target_family = "wasm"` (also true for WASI), a hardcoded `false` —
/// evaluates identically on this host and differently in a browser. So the
/// line is scraped, from the shipped half of the file only: the assertions
/// quote the strings they search for.
///
/// Every needle is counted before it is read. One occurrence is the claim;
/// a second would mean the scrape is reading whichever came first, and a
/// decoy in a doc comment or a string literal would be one.
#[test]
fn the_backend_choice_is_made_on_the_wasm32_arch_and_nothing_else() {
    let source = include_str!("../app.rs");
    let (code, _) = source
        .split_once("#[cfg(test)]")
        .expect("app.rs no longer has a test module");

    let unique = |needle: &str| {
        let n = code.matches(needle).count();
        assert_eq!(n, 1, "expected exactly one `{needle}` in app.rs, found {n}");
    };

    unique("const WEB: bool =");
    let definition = code
        .split_once("const WEB: bool =")
        .and_then(|(_, rest)| rest.split_once(';'))
        .map(|(value, _)| value.trim())
        .expect("`WEB` is no longer defined in app.rs");
    assert_eq!(
        definition, r#"cfg!(target_arch = "wasm32")"#,
        "`WEB` is defined as `{definition}`, which is not the browser arch. \
             No host build can tell the difference."
    );

    // The fork is reached, and reached with the *environment* as its base —
    // the half `backends_for` no longer reads for itself. Whitespace is
    // collapsed first so this survives `cargo fmt` rewrapping the call.
    let flat = code.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "backends_for( WEB,",
        "wgpu::InstanceDescriptor::new_without_display_handle_from_env(), )",
    ] {
        let n = flat.matches(needle).count();
        assert_eq!(
            n, 1,
            "expected exactly one `{needle}` in app.rs, found {n}. \
                 `instance_descriptor` must fork on `WEB` and hand \
                 `backends_for` the environment's own descriptor; without \
                 either, the browser backend restriction is not reached on the \
                 arm it is for."
        );
    }
}

/// A request as `process_gui_actions` builds one: unexpanded viewport bounds
/// plus a texture plan.
fn req(w: u32, h: u32, overdraw: f32, data_gen: u64, zoom: i32) -> fetch::OverlayRenderRequest {
    fetch::OverlayRenderRequest {
        geo_bounds: bounds(),
        texture: OverlayTexturePlan {
            width: w,
            height: h,
            overdraw,
            pixels_per_point: 1.0,
        },
        data_generation: data_gen,
        zoom,
    }
}

fn entry(pane: usize, kind: OverlayKind) -> (usize, OverlayKind, fetch::OverlayRenderRequest) {
    (pane, kind, req(800, 600, 1.0, 1, 10))
}

#[test]
fn test_dedup_empty() {
    let result = deduplicate_overlay_renders(vec![], true);
    assert!(result.is_empty());
    let result = deduplicate_overlay_renders(vec![], false);
    assert!(result.is_empty());
}

#[test]
fn test_dedup_single_render() {
    let result = deduplicate_overlay_renders(vec![entry(0, OverlayKind::Radar)], true);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, vec![0]);
    assert_eq!(result[0].1, OverlayKind::Radar);
    assert_eq!(result[0].2.texture.width, 800);

    let result = deduplicate_overlay_renders(vec![entry(0, OverlayKind::Radar)], false);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, vec![0]);
}

#[test]
fn test_dedup_no_grouping() {
    let input = vec![
        entry(0, OverlayKind::Radar),
        entry(1, OverlayKind::Radar),
        entry(2, OverlayKind::NwsAlerts),
    ];

    let result = deduplicate_overlay_renders(input, false);
    assert_eq!(result.len(), 3);
    for e in &result {
        assert_eq!(e.0.len(), 1);
    }
}

#[test]
fn test_dedup_groups_same_key() {
    let input = vec![entry(0, OverlayKind::Radar), entry(1, OverlayKind::Radar)];

    let result = deduplicate_overlay_renders(input, true);
    assert_eq!(result.len(), 1);
    let mut panes = result[0].0.clone();
    panes.sort();
    assert_eq!(panes, vec![0, 1]);
    assert_eq!(result[0].1, OverlayKind::Radar);
}

#[test]
fn test_dedup_different_keys() {
    let input = vec![
        entry(0, OverlayKind::Radar),
        entry(1, OverlayKind::NwsAlerts),
    ];

    let result = deduplicate_overlay_renders(input, true);
    assert_eq!(result.len(), 2);
}

#[test]
fn test_dedup_duplicate_pane_idx() {
    let input = vec![entry(0, OverlayKind::Radar), entry(0, OverlayKind::Radar)];

    let result = deduplicate_overlay_renders(input, true);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, vec![0]);
}

/// Panes of different sizes must not share one render: the survivor's plan would
/// be applied to a pane it was not sized for. Width is part of the key, and the
/// overdraw that travels with it has to survive grouping intact.
#[test]
fn test_dedup_keeps_differently_sized_panes_apart() {
    let input = vec![
        (0, OverlayKind::Radar, req(2048, 600, 0.28, 1, 10)),
        (1, OverlayKind::Radar, req(2400, 600, 1.0, 1, 10)),
    ];

    let mut result = deduplicate_overlay_renders(input, true);
    assert_eq!(
        result.len(),
        2,
        "different texture widths are different renders"
    );
    result.sort_by_key(|e| e.2.texture.width);
    assert_eq!(result[0].2.texture.width, 2048);
    assert_eq!(
        result[0].2.texture.overdraw, 0.28,
        "the clamped plan's overdraw survived grouping"
    );
    assert_eq!(result[1].2.texture.overdraw, 1.0);
}

/// A bridge that consumes every back press, as Android's does: it installs
/// a handler at startup and `handle_back` reports `true` from then on.
fn minimising_bridge() -> TestBridge {
    let mut bridge = TestBridge::android();
    // Deliberately not `record_back_press`: that one's flag belongs to
    // `the_injected_callbacks_reach_the_bridge` alone. Tests run in
    // parallel, and a second writer could set it while that test is
    // asserting — which would only ever make it pass, which is worse.
    bridge.set_back_handler(|| {});
    bridge
}

/// Back with something open closes it; only a second press, with nothing
/// open, minimises.
///
/// The bug is an *ordering* one, which is why the platform here consumes
/// everything: `handle_back` used to be asked first, and on Android a
/// handler is always installed, so it always said yes — the UI was never
/// consulted and one press with the drawer open went straight to minimise.
///
/// Opens the settings (the inspector's App › Settings body) rather than the
/// drawer only because `open_settings` is the dismissible state this crate
/// can reach. `dismiss_top_layer`'s own coverage of the drawer, and of the
/// one-layer-per-press rule, is in `rustdar-egui`'s `ui_menu` tests.
#[test]
fn back_closes_what_is_open_before_it_minimises() {
    let mut gui = Gui::new();
    let platform = minimising_bridge();
    gui.open_settings();

    assert_eq!(
        App::resolve_back_press(&mut gui, &platform),
        BackPress::Dismissed,
        "the first press left the app with a window still open"
    );
    assert!(!gui.settings_visible(), "the settings body is still open");

    assert_eq!(
        App::resolve_back_press(&mut gui, &platform),
        BackPress::PlatformHandled,
        "with nothing open, back must reach the platform and minimise"
    );
}

/// The two tests above exercise the decision; nothing can exercise the
/// call that reaches it, because `handle_input_events` takes an
/// `ActiveEventLoop` and winit will not hand one out except from inside a
/// running loop. Reading the source is the only handle, as it is for
/// `egui_renderer`'s `begin_frame`.
fn fn_body(name: &str) -> &'static str {
    let (_, rest) = include_str!("../app.rs")
        .split_once(name)
        .unwrap_or_else(|| panic!("{name} is no longer a method here"));
    rest.split_once("\n    }")
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("{name} has no recognisable body"))
}

/// The block of the `match` arm `pattern` opens, brace-matched.
///
/// Ending the slice at the *next* arm's pattern instead would tie the probe
/// to the order the arms happen to be written in: reorder them and the end
/// marker lands behind the start, the slice falls back to the whole
/// function, and the assertion stops saying anything about the arm it
/// names. Braces are the arm's own structure and move with it.
fn arm_body<'a>(body: &'a str, pattern: &str) -> &'a str {
    let at = body
        .find(pattern)
        .unwrap_or_else(|| panic!("there is no {pattern} arm here"));
    let open = at
        + body[at..]
            .find("=> {")
            .unwrap_or_else(|| panic!("the {pattern} arm is no longer a block"))
        + "=> ".len();
    let mut depth = 0usize;
    for (i, c) in body[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[open..=open + i];
                }
            }
            _ => {}
        }
    }
    panic!("the {pattern} arm's block is unterminated");
}

/// A press has to actually reach the funnel.
///
/// Both keys go through one call and one route, so this is the whole
/// wiring: drop either and Escape and back do nothing at all, with the
/// decision tests still green because they call `resolve_back_press`
/// directly.
///
/// `take_back_out_press` rather than a plain read is part of the claim —
/// `handle_input_events` runs on every keyboard press, so a non-consuming
/// read spends one press on two layers.
#[test]
fn every_back_out_press_reaches_the_funnel_exactly_once() {
    let body = fn_body("fn handle_input_events(");
    for call in ["take_back_out_press(", "self.back_out("] {
        assert!(
            body.contains(call),
            "handle_input_events no longer calls {call}, so Escape and the \
                 back button reach nothing: {body}"
        );
    }
}

/// A press the UI is about to take must not also back the app out.
///
/// `InputHandler` reads the raw `WindowEvent`, before egui and independently
/// of what egui consumes, so Escape with a text field focused unfocused the
/// field *and* dismissed the layer behind it — or, with nothing else open,
/// quit — on one press.
///
/// Two claims, and the second is the one a bare "contains the gate" missed.
/// The press has to be *taken* whether or not it is spent: `&&`
/// short-circuits left to right, so `!self.ui_is_taking_keys() &&
/// self.input.take_back_out_press()` leaves the flag latched, and
/// `handle_input_events` runs on every keyboard press — the next key of any
/// kind then spends it, which is the same double dismissal one keystroke
/// later.
#[test]
fn a_press_the_ui_is_taking_does_not_also_back_the_app_out() {
    let body = fn_body("fn handle_input_events(");
    assert!(
        body.contains("if self.input.take_back_out_press() && !self.ui_is_taking_keys() {"),
        "the funnel no longer takes the press first and then asks whether \
             egui wanted it: {body}",
    );
    assert!(
        fn_body("fn ui_is_taking_keys(").contains("egui_wants_keyboard_input()"),
        "ui_is_taking_keys no longer asks egui what it has focused, so it \
             is answering from something else",
    );
}

/// A dismissal has to schedule the frame that shows it.
///
/// Nothing else consumed the press, so nothing else requests a redraw: drop
/// this and the drawer stays on screen until something unrelated repaints.
/// `WindowRef` cannot be built without a window, so the source is again the
/// only handle.
#[test]
fn a_dismissal_asks_for_the_frame_that_shows_it() {
    let body = fn_body("fn back_out(");
    let dismissed = body
        .find("BackPress::Dismissed")
        .expect("back_out no longer handles a dismissal");
    let arm_end = body[dismissed..]
        .find('\n')
        .map(|i| dismissed + i)
        .unwrap_or(body.len());
    assert!(
        body[dismissed..arm_end].contains("notify_redraw("),
        "the Dismissed arm does not request a redraw: {}",
        &body[dismissed..arm_end]
    );
}

// ── The second delivery route: Android's predictive back ────────────
//
// `OnBackInvokedDispatcher` does not go through the input queue, so none of
// the pins above see it. It also does not go through this process's main
// thread: the press lands on a Java callback, which parks it and wakes the
// loop, and `about_to_wait` collects it. What has to hold is that it ends
// in the *same* `resolve_back_press` — which the decision tests above
// already cover once a press gets there.

/// The Java half of the route, so a rename on either side is a build
/// failure rather than an `UnsatisfiedLinkError` on a device.
const BACK_HANDLER_JAVA: &str =
    include_str!("../../../packaging/android/app/src/main/java/com/rustdar/BackHandler.java");

/// The Rust half: the one module file that CONTAINS the exported JNI symbol
/// (`rustdar/src/android/back.rs`) -- NOT the crate root, which since the
/// android fold holds only module mounts and would silently pin wrong text.
/// The android modules are `cfg(target_os = "android")`, so they compile to
/// nothing on a host and can hold no test of their own; this crate owns the
/// funnel both halves are about, so the pins live here.
const ANDROID_BACK: &str = include_str!("../../../rustdar/src/android/back.rs");

/// `src` with its Java comments removed.
///
/// The pins below are about the order two calls happen in, and the prose
/// around them necessarily names both — the first draft failed on its own
/// javadoc. Deliberately naive: it would mangle a `//` inside a string
/// literal, and there is none in this file.
fn java_code(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(slash) = rest.find('/') {
        let (kept, tail) = rest.split_at(slash);
        out.push_str(kept);
        if let Some(body) = tail.strip_prefix("/*") {
            rest = body.split_once("*/").map_or("", |(_, after)| after);
        } else if let Some(body) = tail.strip_prefix("//") {
            rest = body.split_once('\n').map_or("", |(_, after)| after);
        } else {
            // A lone '/' opens nothing. Keep it and move past it.
            out.push('/');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// A press delivered outside the input queue has to reach the same funnel,
/// and only when there *is* one.
///
/// `about_to_wait` takes an `ActiveEventLoop`, so this is a source probe for
/// the same reason `handle_input_events` is. Three claims, and the third is
/// the one a substring pair missed: without `poll_back_press` the press is
/// never collected; without `self.back_out` it is collected and thrown
/// away; and with the poll demoted out of the condition — `let _ =
/// self.platform.poll_back_press(); self.back_out(event_loop);` — this runs
/// on *every* iteration of the loop and the UI dismantles itself. So the
/// call is pinned as the `if`, not merely as present.
#[test]
fn a_back_press_from_the_platform_reaches_the_funnel_too() {
    let body = fn_body("fn about_to_wait(");
    assert!(
        body.contains("if self.platform.poll_back_press() {"),
        "the platform back press is no longer what gates the funnel, so \
             about_to_wait either drops it or backs out on every iteration: \
             {body}"
    );
    assert!(
        body.contains("self.back_out("),
        "about_to_wait collects the press and does nothing with it: {body}"
    );
}

/// The two ends of the JNI hop must agree on one name.
///
/// It is resolved by string at runtime and by nothing at build time, so a
/// rename on either side compiles, links, ships, and then throws
/// `UnsatisfiedLinkError` on the first back press — where the Java
/// fallback catches it and minimises, which is indistinguishable from the
/// bug this route exists to remove.
#[test]
fn the_java_callback_calls_the_symbol_rust_exports() {
    let java = java_code(BACK_HANDLER_JAVA);
    assert!(
        java.contains("package com.rustdar;")
            && java.contains("class BackHandler")
            && java.contains("native boolean nativeBackPressed()"),
        "the Java side no longer declares com.rustdar.BackHandler.nativeBackPressed",
    );
    assert!(
        ANDROID_BACK.contains("fn Java_com_rustdar_BackHandler_nativeBackPressed("),
        "nothing exports the symbol BackHandler.nativeBackPressed() binds to",
    );
}

/// Offsets of every *call* to `name`, skipping the line that declares it.
///
/// The declaration and the call are spelled the same, and an earlier draft
/// of the pin below matched the first of either. A review moved
/// `private static native boolean nativeBackPressed();` above the method and
/// rewrote the body to minimise first and ask second — the regression the
/// pin is named for — and it passed, because the declaration was now the
/// first match. A `native` keyword on the line is what tells them apart.
fn call_sites(java: &str, name: &str) -> Vec<usize> {
    java.match_indices(name)
        .map(|(at, _)| at)
        .filter(|at| {
            let line = java[..*at].rfind('\n').map_or(0, |nl| nl + 1);
            !java[line..*at].contains("native ")
        })
        .collect()
}

/// The bomb this route was built to defuse.
///
/// The callback used to be `() -> activity.moveTaskToBack(true)`: no route
/// into Rust at all, inert only because the manifest has not opted in and
/// targetSdk is 34. Raising targetSdk opts the app in, and back would have
/// gone straight back to minimising on the first press with the drawer
/// open — no test failing, nothing logged.
///
/// So: every minimise in this class must come after the class has asked
/// Rust. The one `moveTaskToBack` left is the fallback for a press with no
/// event loop to route to, and it sits after the call that asks.
///
/// Deliberately ordered across the whole class rather than within one
/// method: a minimise hoisted into a helper *defined earlier in the file*
/// would fail this even if it still ran after the call. That is the safe
/// direction to be wrong in, and the class is sixty lines of code.
#[test]
fn the_predictive_back_callback_asks_rust_before_it_minimises() {
    let java = java_code(BACK_HANDLER_JAVA);
    assert!(
        java.contains("registerOnBackInvokedCallback"),
        "BackHandler no longer registers a callback",
    );

    let asks = *call_sites(&java, "nativeBackPressed(")
        .first()
        .expect("BackHandler declares the native funnel but never calls it");

    for minimises in call_sites(&java, "moveTaskToBack(") {
        assert!(
            minimises > asks,
            "BackHandler minimises before it asks Rust, so one press with \
                 the drawer open minimises the app",
        );
    }
    assert!(
        java.matches("moveTaskToBack(").count() <= 1,
        "a second minimise appeared in BackHandler; the one this class is \
             allowed is the fallback for a press with no event loop to route to",
    );
}

/// Set by `one_press` below. A `fn` pointer closes over nothing, which is
/// the constraint the real taker is under too — it reads a `static` a JNI
/// entry point on the UI thread wrote.
static PARKED_BACK_PRESS: AtomicBool = AtomicBool::new(false);

fn one_press() -> bool {
    PARKED_BACK_PRESS.swap(false, Ordering::Relaxed)
}

/// The taker has to reach the bridge, and it has to *consume*.
///
/// `about_to_wait` runs every loop iteration, so a non-consuming read would
/// spend one gesture on every layer the UI has open — the drawer, the
/// settings window and the time dialog would all vanish together, and then
/// the app would minimise.
#[test]
fn a_parked_back_press_is_collected_once() {
    let mut app = headless(TestBridge::android());
    PARKED_BACK_PRESS.store(true, Ordering::Relaxed);
    assert!(
        !app.platform.poll_back_press(),
        "precondition: nothing injected yet, so there is nothing to collect",
    );

    app.set_back_press_taker(one_press);

    assert!(
        app.platform.poll_back_press(),
        "the parked press never reached the bridge",
    );
    assert!(
        !app.platform.poll_back_press(),
        "the press was not consumed, so it fires again on the next iteration",
    );
}

/// No bridge may invent a press. `about_to_wait` runs on every iteration of
/// every platform's loop, so a bridge answering `true` on its own would
/// close a layer per iteration and then minimise, for a gesture nobody
/// made. Desktop and iOS never get a taker at all; Android has none until
/// `android_main` injects one.
#[test]
fn no_bridge_invents_a_back_press() {
    for (name, mut bridge) in [
        ("desktop", TestBridge::desktop()),
        ("ios", TestBridge::ios()),
        (
            "android, before android_main injects the taker",
            TestBridge::android(),
        ),
    ] {
        assert!(
            !bridge.poll_back_press(),
            "{name} reported a back press nobody delivered",
        );
    }
}

/// The same press on a platform with no back handler: Escape on the desktop
/// and the browser's back. Nothing open means quit, and quitting must stay
/// reachable — a dismissal that reported itself with nothing open would
/// make the app unquittable.
#[test]
fn escape_with_nothing_open_still_exits() {
    let mut gui = Gui::new();
    let platform = TestBridge::desktop();
    gui.open_settings();

    assert_eq!(
        App::resolve_back_press(&mut gui, &platform),
        BackPress::Dismissed,
        "escape must close the window rather than quit, same as back"
    );
    assert_eq!(
        App::resolve_back_press(&mut gui, &platform),
        BackPress::Exit
    );
}

// ── Driving a whole `App` ───────────────────────────────────────────
//
// Everything below builds one. Two things used to make that impossible and
// only one of them was real: `App::new` builds a `wgpu::Instance` and a
// Tokio runtime, and it needs a `PlatformBridge`. The bridge is now
// `platform_double::TestBridge`; the instance is built with no backends,
// which is the whole of `with_instance`'s reason to exist. A texture upload
// was also blamed and is not an obstacle at all — a bare `egui::Context`
// uploads perfectly well with no renderer behind it, which is what
// `app_render`'s tests rely on.

/// An `App` with no GPU behind it, wired the way `App::new` wires one.
pub(super) fn headless(platform: TestBridge) -> App {
    crate::test_sites::install();
    App::with_instance(
        egui_wgpu::wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::empty(),
            ..instance_descriptor()
        }),
        Box::new(platform),
    )
}

/// A loop speed no default produces, so finding it can only mean the stored
/// config was read.
const STORED_FPS: f32 = 9.25;

/// Write a config the way the app writes one, rather than by hand: a
/// literal blob would stop matching the format the moment it changed and
/// would then be testing nothing.
fn seed_config(store: &MemoryKvStore, fps: f32) {
    let mut gui = Gui::new();
    gui.loop_speed_fps = fps;
    gui.save_ui_config(store);
}

/// What a bridge's store holds, read back through the same parser the app
/// loads with.
fn stored_fps(store: &MemoryKvStore) -> f32 {
    let mut reloaded = Gui::new();
    reloaded.load_ui_config(store);
    reloaded.loop_speed_fps
}

/// The site every pane opens on, which is what a user actually sees.
fn opening_site(app: &App) -> String {
    app.gui.pane(0).expect("a pane exists").site.clone()
}

// ── First-run site selection ────────────────────────────────────────

/// The complaint this feature answers: a first run in Minnesota opened on
/// Oklahoma's radar because the default was compiled in.
#[test]
fn a_first_run_opens_on_the_radar_nearest_the_devices_timezone() {
    let app = headless(TestBridge::desktop().with_timezone("America/Chicago"));
    assert_eq!(opening_site(&app), "KLOT");
}

/// Two devices in different timezones must not open on the same site, which
/// is the failure mode a hardcoded default has by construction.
#[test]
fn different_timezones_open_on_different_sites() {
    let west = headless(TestBridge::desktop().with_timezone("America/Los_Angeles"));
    let east = headless(TestBridge::desktop().with_timezone("America/New_York"));
    assert_ne!(opening_site(&west), opening_site(&east));
}

/// A platform that cannot report a timezone keeps the compiled-in default
/// rather than ending up on an empty or invented site.
#[test]
fn a_platform_with_no_timezone_keeps_the_built_in_default() {
    let app = headless(TestBridge::desktop());
    assert_eq!(opening_site(&app), Gui::new().pane(0).unwrap().site);
}

/// The precedence rule, and the one that matters most: a returning user's
/// stored site is never second-guessed, however far the timezone disagrees.
#[test]
fn a_stored_site_outranks_the_timezone_guess() {
    let bridge = TestBridge::desktop().with_timezone("America/Los_Angeles");
    let store = bridge.store();
    {
        let mut gui = Gui::new();
        gui.set_initial_site("KMPX");
        gui.save_ui_config(store.as_ref());
    }

    let app = headless(bridge);
    assert_eq!(
        opening_site(&app),
        "KMPX",
        "a stored choice was overwritten by the timezone guess"
    );
}

// ── Refining a guess with a real fix ────────────────────────────────

/// The silent upgrade: the timezone puts the user in the right region for
/// the first paint, and a fix — which only arrives where location was
/// already granted — resolves the actual nearest radar.
#[test]
fn a_location_fix_refines_a_guessed_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);
    assert_eq!(
        opening_site(&app),
        "KLOT",
        "the guess is the starting point"
    );

    // Duluth, Minnesota: same timezone, a different radar.
    fixes
        .send(rustdar_location::Fix::from_lat_lon(46.7867, -92.1005))
        .unwrap();
    app.poll_platform_state();

    assert_eq!(opening_site(&app), "KDLH");
}

/// Naming the new site is only the visible part of moving to it. The first
/// version of this feature assigned `pane.site` and nothing else, so no
/// volume was ever requested: the pane sat on a site with no `scan_info`,
/// which is the state the map draws at the geographic centre of the
/// contiguous US — leaving the user looking at Kansas with the right radar
/// named in the picker.
///
/// `loading_site` is the observable, because it is raised by the same
/// `SwitchRadarSite` handling that spawns the fetch and cleared only when a
/// scan for that site arrives. Asserting on the site name alone passes on
/// the broken version, which is how this shipped.
#[test]
fn a_refined_site_actually_requests_its_radar_data() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);

    fixes
        .send(rustdar_location::Fix::from_lat_lon(46.7867, -92.1005))
        .unwrap();
    app.poll_platform_state();

    let pane = app.gui.pane(0).expect("a pane exists");
    assert_eq!(pane.site, "KDLH");
    assert_eq!(
        pane.loading_site.as_deref(),
        Some("KDLH"),
        "the site changed without anything fetching for it, so the pane has \
             no scan_info and the map stays at its no-data centre"
    );
}

/// A fix must not move a site the user chose. Someone in Dallas watching a
/// storm over Kansas keeps the Kansas radar.
#[test]
fn a_location_fix_does_not_move_a_stored_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let store = bridge.store();
    {
        let mut gui = Gui::new();
        gui.set_initial_site("KICT");
        gui.save_ui_config(store.as_ref());
    }

    let mut app = headless(bridge);
    fixes
        .send(rustdar_location::Fix::from_lat_lon(32.7767, -96.7970))
        .unwrap();
    app.poll_platform_state();

    assert_eq!(
        opening_site(&app),
        "KICT",
        "a late fix yanked the user away from the site they chose"
    );
}

/// Once a guess has been refined it stops being a guess. A later fix — from
/// someone travelling with the app open — must not keep re-homing the map.
#[test]
fn only_the_first_fix_refines_the_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);

    fixes
        .send(rustdar_location::Fix::from_lat_lon(46.7867, -92.1005))
        .unwrap();
    app.poll_platform_state();
    assert_eq!(opening_site(&app), "KDLH");

    // The same user, now in Denver.
    fixes
        .send(rustdar_location::Fix::from_lat_lon(39.7392, -104.9903))
        .unwrap();
    app.poll_platform_state();
    assert_eq!(
        opening_site(&app),
        "KDLH",
        "a second fix moved a site that was already settled"
    );
}

/// The OS location services all report a fused position and decline to name
/// the source, so none of them can honestly claim `Gps`. Requiring that
/// variant — as this gate used to — meant a desktop, iOS or Android network
/// fix drew a blue dot and never refined the site it was drawn on.
#[test]
fn an_os_fix_refines_a_guessed_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);
    assert_eq!(opening_site(&app), "KLOT");

    fixes
        .send(rustdar_location::Fix {
            // What the location portal measured on the developer's own machine: an
            // IP/ichnaea lookup, and comfortably good enough to choose
            // among sites 200 km apart.
            accuracy_m: Some(25_000.0),
            ..rustdar_location::Fix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();

    assert_eq!(
        opening_site(&app),
        "KDLH",
        "a platform location fix drew a dot and left the map on the \
             timezone's guess"
    );
}

/// The shape the android location module now produces from the network
/// provider, end
/// to end.
///
/// Two things about that shape changed, and only one of them is visible
/// here. The quality moved from `Estimated` to `Device`, which is a label
/// correction — `can_relocate` admits both. The accuracy moved from `None`
/// to whatever `Location.getAccuracy()` said, and *that* is what turns the
/// gate below from a formality into a judgement: until this fix every
/// Android reading passed unconditionally, because there was nothing to
/// weigh.
///
/// 32 m is a typical Wi-Fi-assisted network fix. It refines; the absurd one
/// in `a_low_accuracy_fix_does_not_spend_the_provisional_site` does not, and
/// before this it would have.
#[test]
fn an_android_network_fix_refines_the_opening_site() {
    let mut bridge = TestBridge::android().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);
    assert_eq!(opening_site(&app), "KLOT");

    fixes
        .send(rustdar_location::Fix {
            accuracy_m: Some(32.0),
            ..rustdar_location::Fix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();

    assert_eq!(
        opening_site(&app),
        "KDLH",
        "an Android network fix drew a dot and left the map on the \
             timezone's guess"
    );
}

/// A GPS simulator is a real thing on the serial path — GGA quality 8, and
/// quality 7 is a position somebody typed into the receiver. Both carry
/// well-formed coordinates and neither is a place, so neither may move the
/// user's radar.
#[test]
fn a_simulated_fix_does_not_move_the_radar_site() {
    for quality in [
        rustdar_location::FixQuality::Simulation,
        rustdar_location::FixQuality::Manual,
        rustdar_location::FixQuality::None,
    ] {
        let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
        let fixes = bridge.gps_channel();
        let mut app = headless(bridge);

        fixes
            .send(rustdar_location::Fix {
                fix_quality: quality,
                ..rustdar_location::Fix::from_lat_lon(46.7867, -92.1005)
            })
            .unwrap();
        app.poll_platform_state();

        assert_eq!(
            opening_site(&app),
            "KLOT",
            "a {quality:?} fix relocated the user's radar site"
        );
        assert!(
            app.site_is_provisional,
            "a {quality:?} fix spent the one upgrade a real fix was owed"
        );
    }
}

/// The threshold is enormous on purpose — see `MAX_RELOCATION_ACCURACY_M`,
/// where the measurements are — so this is about the absurd end: a fix
/// whose stated uncertainty is wider than the region the timezone guess
/// already resolved must not spend the one upgrade.
#[test]
fn a_low_accuracy_fix_does_not_spend_the_provisional_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);

    fixes
        .send(rustdar_location::Fix {
            accuracy_m: Some(rustdar_location::MAX_RELOCATION_ACCURACY_M * 2.0),
            ..rustdar_location::Fix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();

    assert_eq!(opening_site(&app), "KLOT");
    assert!(
        app.site_is_provisional,
        "a fix too coarse to use was still spent, so the good one that \
             follows it can never refine anything"
    );

    // And the good fix that follows still works, which is the half that
    // makes the rejection worth anything.
    fixes
        .send(rustdar_location::Fix {
            accuracy_m: Some(25_000.0),
            ..rustdar_location::Fix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();
    assert_eq!(opening_site(&app), "KDLH");
}

// ── The location permission gate, from the App's side ───────────────
//
// `rustdar_location::gate` owns the state machine and tests it against a
// clock it controls. What belongs here is the wiring: that the gate is
// stepped at all, that what it observes reaches the UI, and that a
// revocation takes the dot with it.

/// The gate is stepped from `poll_platform_state`, and what it sees is
/// pushed to the `Gui` — which is the only copy the settings pane can read,
/// since `rustdar-egui` cannot see a `PlatformBridge`.
#[test]
fn what_the_platform_says_about_location_reaches_the_settings_pane() {
    let bridge =
        TestBridge::desktop().with_permission(rustdar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    assert_eq!(
        app.gui.location_permission(),
        rustdar_location::LocationPermission::Unknown,
        "the cache starts inert, before anything has been polled"
    );

    app.poll_platform_state();
    // What the gate observed reaches the UI on the frame's compose; no
    // renderer exists here, so the test drives the compose itself.
    app.push_frame_inputs();

    assert_eq!(
        app.gui.location_permission(),
        rustdar_location::LocationPermission::Granted
    );
    assert!(
        app.gui.location_active(),
        "a grant with no stream is where every desktop process starts; \
             something has to turn it on"
    );
    assert_eq!(location.requests.get(), 1);
}

/// Consent went away, so the position drawn under it must go too. Leaving
/// it is the app showing a location it has just been told it may not know.
#[test]
fn a_revoked_permission_stops_delivery_and_clears_the_dot() {
    let bridge =
        TestBridge::desktop().with_permission(rustdar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    // A delivered fix lands in the App's own field, stamped at arrival, and
    // reaches the UI on the next compose — the same route production takes.
    app.user_gps = Some((
        rustdar_location::Fix::from_device_position(35.25, -97.5),
        web_time::Instant::now(),
    ));
    app.push_frame_inputs();
    assert!(app.gui.gps_fix().is_some());

    // Revoked in system settings, with no process restart — which is what
    // happens on every desktop OS.
    location
        .permission
        .set(rustdar_location::LocationPermission::Denied);
    app.location.resumed();
    app.poll_platform_state();
    app.push_frame_inputs();

    assert!(!location.active.get(), "the stream was left running");
    assert!(
        app.gui.gps_fix().is_none(),
        "the blue dot is still on the map at a position the user has \
             withdrawn consent for"
    );
}

/// The serial dongle is not covered by this permission — it is a device the
/// user plugged in — so a location denial must not take its dot away.
#[test]
fn a_revoked_permission_leaves_a_serial_dongles_dot_alone() {
    let bridge =
        TestBridge::desktop().with_permission(rustdar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    app.platform
        .start_gps(&rustdar_nmea_serial::SerialConfig::default());
    app.user_gps = Some((
        rustdar_location::Fix::from_lat_lon(35.25, -97.5),
        web_time::Instant::now(),
    ));

    location
        .permission
        .set(rustdar_location::LocationPermission::Denied);
    app.location.resumed();
    app.poll_platform_state();
    app.push_frame_inputs();

    assert!(
        app.gui.gps_fix().is_some(),
        "denying the OS location service took the serial receiver's dot \
             off the map with it"
    );
}

/// Android cannot tell "never asked" from "permanently denied" on its own —
/// `shouldShowRequestPermissionRationale` is `false` for both — so the memo
/// on this side has to tell it, and this is the wire that does.
#[test]
fn a_bridge_that_needs_the_attempt_count_is_told_it() {
    let bridge =
        TestBridge::android().with_permission(rustdar_location::LocationPermission::Prompt);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    // Android has no config dir until `android_main` supplies one.
    app.platform
        .set_config_dir(std::path::PathBuf::from("/data"));

    app.poll_platform_state();

    assert_eq!(
        location.attempts.get(),
        Some(1),
        "the bridge was asked to prompt and never told it had been"
    );
}

/// Turning location off in the settings pane stops the stream and takes the
/// dot with it, at the moment of the click rather than at the next poll.
#[test]
fn turning_location_off_stops_the_stream_and_clears_the_dot() {
    let bridge =
        TestBridge::desktop().with_permission(rustdar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    app.user_gps = Some((
        rustdar_location::Fix::from_device_position(35.25, -97.5),
        web_time::Instant::now(),
    ));
    app.push_frame_inputs();
    assert!(location.active.get());

    app.handle_gui_action(GuiAction::StopLocation, None);
    // The click's effect crosses to the UI on the frame it triggers.
    app.push_frame_inputs();

    assert!(!location.active.get(), "the off switch did not switch off");
    assert!(app.gui.gps_fix().is_none(), "the dot outlived the stream");
    assert!(!app.gui.location_active(), "the pane still reads 'On.'");
}

// ── Waking the loop from a thread that is not this one ──────────────
//
// The tests above hand a fix straight to `poll_platform_state`, which is
// the frame's own drain. In production nothing calls that until a frame
// happens, and under `ControlFlow::Wait` nothing schedules a frame unless
// something asks — so the five sensor producers each need a way to ask.
// `RedrawWaker`'s own guarantees are pinned in `platform.rs`; what belongs
// here is that the `App` fills it, empties it, and hands it out.

/// How many times `waker` has fired since this was called.
fn count_wakes(waker: &RedrawWaker) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let probe = std::sync::Arc::clone(&count);
    waker.install(move || {
        probe.fetch_add(1, Ordering::SeqCst);
    });
    count
}

/// The ordering the desktop and Android bridges both depend on.
///
/// `DesktopPlatform::start_gps` spawns the serial reader from a menu toggle
/// and `AndroidPlatform::set_theme_detector` spawns the theme poller during
/// `android_main`; neither call carries a window, and on Android the second
/// happens before `run_app`. So the bridge has to be holding the waker from
/// construction, and it has to be the *same* slot the window later fills —
/// a bridge handed a private copy would spawn threads that wake nothing for
/// the life of the process.
#[test]
fn the_bridge_gets_the_apps_own_waker_before_any_window_exists() {
    let bridge = TestBridge::desktop();
    let handed_to_the_bridge = bridge.waker_record();
    let app = headless(bridge);

    // Stands in for what `create_window` installs; no test can build the
    // `Window` it captures, so that half is read off the source below.
    let woke = count_wakes(&app.redraw_waker());

    handed_to_the_bridge.borrow().wake();

    assert_eq!(
        woke.load(Ordering::SeqCst),
        1,
        "the bridge is holding a waker the app does not fill, so every \
             thread it starts — the serial GPS reader, the Android theme \
             poller — asks for frames that nobody hears"
    );
}

/// The entry points' own producers — `android_main`'s location and compass
/// threads, the browser's `watchPosition` watch — are not the bridge's, and
/// take their handle from here. Same slot, same reasoning.
#[test]
fn every_handle_the_app_gives_out_is_the_same_slot() {
    let app = headless(TestBridge::desktop());
    let woke = count_wakes(&app.redraw_waker());

    // What `android_main` and `entry::start` keep: a clone taken at
    // startup, several seconds before the first `resumed()`.
    app.redraw_waker().wake();

    assert_eq!(woke.load(Ordering::SeqCst), 1);
}

/// The window half of the wiring. `create_window` takes an
/// `ActiveEventLoop`, so this is a source probe for the reason
/// `both_inset_queries_are_still_wired` is.
///
/// Two claims. That the slot is filled at all — without it every producer's
/// wake is a no-op forever and the app is exactly as broken as before, with
/// the tests above still green because they install their own. And that
/// what goes in is `notify_redraw`: a wake that reaches anything *other*
/// than a redraw request produces an iteration, and the sensor channels are
/// drained on a frame.
#[test]
fn the_window_teaches_every_outstanding_waker_what_a_wake_means() {
    let body = fn_body("fn create_window(");
    assert!(
        body.contains("self.redraw_waker.install("),
        "the window came up without filling the waker slot, so every sensor \
             thread's wake is a no-op for the life of the process: {body}"
    );
    assert!(
        body.contains("notify_redraw("),
        "the waker no longer ends in a redraw request, so a fix wakes the \
             loop for an iteration that never drains the channel: {body}"
    );
}

/// And the teardown. `suspended` clears `window` and `state` precisely so
/// no wgpu surface outlives the destroyed window; the waker is the third
/// holder of that window and the only one this thread does not own outright
/// — five sensor threads have a clone. Surviving the suspend is the bug.
///
/// Probed rather than driven: `suspended` takes an `ActiveEventLoop`.
#[test]
fn a_waker_stops_holding_the_window_once_the_app_is_suspended() {
    let body = fn_body("fn suspended(");
    assert!(
        body.contains("self.window = None"),
        "the premise of this test is gone: suspend no longer drops the \
             window, so there is nothing for the waker to be holding past it"
    );
    assert!(
        body.contains("self.redraw_waker.detach()"),
        "the waker keeps the destroyed window alive across a suspend, so \
             every sensor thread holds an Arc<Window> whose ANativeWindow is \
             gone: {body}"
    );
}

// ── Autosave ────────────────────────────────────────────────────────

/// The bug this exists for. Config used to be written only from
/// `request_exit` and `suspended`; a browser tab close runs neither, so the
/// web build persisted nothing at all.
#[test]
fn config_is_persisted_without_an_exit_or_a_suspend() {
    let bridge = TestBridge::desktop();
    let store = bridge.store();
    let mut app = headless(bridge);

    app.gui.loop_speed_fps = STORED_FPS;
    app.autosave_config(true);

    assert_eq!(
        stored_fps(&store),
        STORED_FPS,
        "a change was lost because nothing but exit and suspend ever saved"
    );
}

/// An idle app must not rewrite an unchanged config every three seconds for
/// the life of the process.
#[test]
fn an_unchanged_config_is_not_rewritten() {
    let bridge = TestBridge::desktop();
    let writes = bridge.write_count();
    let mut app = headless(bridge);

    app.gui.loop_speed_fps = STORED_FPS;
    app.autosave_config(true);
    let after_change = writes.get();
    assert!(after_change > 0, "the change was never written at all");

    for _ in 0..10 {
        app.autosave_config(true);
    }
    assert_eq!(
        writes.get(),
        after_change,
        "an unchanged config is being rewritten on every tick"
    );
}

/// Having saved once must not stop the next change being saved.
#[test]
fn a_later_change_is_written_after_an_idle_period() {
    let bridge = TestBridge::desktop();
    let store = bridge.store();
    let mut app = headless(bridge);

    app.gui.loop_speed_fps = STORED_FPS;
    app.autosave_config(true);
    app.autosave_config(true);

    app.gui.loop_speed_fps = 3.5;
    app.autosave_config(true);

    assert_eq!(stored_fps(&store), 3.5);
}

/// The interval is what keeps this cheap, so it has to actually gate. A
/// forced call is the only one allowed through immediately.
#[test]
fn autosave_respects_its_interval() {
    let bridge = TestBridge::desktop();
    let writes = bridge.write_count();
    let mut app = headless(bridge);

    // The first unforced call has no previous check to compare against and
    // establishes the baseline.
    app.autosave_config(false);
    let baseline = writes.get();

    app.gui.loop_speed_fps = STORED_FPS;
    app.autosave_config(false);
    assert_eq!(
        writes.get(),
        baseline,
        "a change was written before the interval elapsed, so the timer is \
             not gating and this runs every frame"
    );
}

/// A timezone-guessed site has to reach storage like any other, or a first
/// run guesses again every launch and a returning user is never recognised.
#[test]
fn a_guessed_site_is_persisted() {
    let bridge = TestBridge::desktop().with_timezone("America/Denver");
    let store = bridge.store();
    let mut app = headless(bridge);

    app.autosave_config(true);

    let mut reloaded = Gui::new();
    assert!(reloaded.load_ui_config(store.as_ref()));
    assert_eq!(reloaded.pane(0).unwrap().site, "KFTG");
}

/// The state a pan leaves behind: an event has been seen, and the last
/// autosave check was `ago` in the past. `ago` past [`AUTOSAVE_INTERVAL`]
/// is a save that is due; short of it is one still waiting.
fn owes_a_save_from(app: &mut App, ago: std::time::Duration) {
    app.autosave.last_check = Some(web_time::Instant::now() - ago);
    app.autosave.touched = true;
}

/// Everything an expired `WaitUntil` actually dispatches, and nothing more.
///
/// Deliberately not `handle_redraw`: the whole bug is that the timer never
/// produces a frame, so a test that renders one is testing the path that
/// was already working.
fn wake_on_the_timer(app: &mut App) -> ControlFlow {
    app.autosave_config(false);
    app.wakeup_control_flow()
}

/// A wake-up asked for and granted has to end in the write it was asked
/// for.
///
/// It did not. `autosave_config` was reachable only from `handle_redraw`,
/// and a `WaitUntil` deadline expiring dispatches `new_events` and
/// `about_to_wait` — never `RedrawRequested`. So the timer woke the app,
/// found no route to the save, and re-armed; the change survived only if
/// some unrelated event later drew a frame, and a user who panned and
/// walked away lost it.
#[test]
fn a_timed_wakeup_actually_saves_the_change_it_was_scheduled_for() {
    let bridge = TestBridge::desktop();
    let store = bridge.store();
    let mut app = headless(bridge);

    // The frame the pan ended on: it checked, so nothing was owed yet.
    app.autosave_config(true);
    app.gui.loop_speed_fps = STORED_FPS;
    owes_a_save_from(&mut app, AUTOSAVE_INTERVAL);

    wake_on_the_timer(&mut app);

    assert_eq!(
        stored_fps(&store),
        STORED_FPS,
        "the wake-up spent itself on a reschedule: the change it was \
             scheduled to save is still unwritten"
    );
}

/// `about_to_wait` is where the save has to happen, and it takes an
/// `ActiveEventLoop` — so this is a source probe for the same reason
/// `a_back_press_from_the_platform_reaches_the_funnel_too` is.
///
/// The behavioural tests either side of this one drive `autosave_config`
/// and `wakeup_control_flow` themselves, which says nothing about
/// whether the event loop ever reaches them. Drop the call and they all
/// stay green while the timer goes back to waking for nothing.
#[test]
fn the_autosave_wakeup_is_spent_on_a_save_not_only_on_a_reschedule() {
    let body = fn_body("fn about_to_wait(");
    assert!(
        body.contains("self.autosave_config("),
        "about_to_wait no longer saves, so the only dispatch a WaitUntil \
             expiry produces cannot reach the config write it was armed for: \
             {body}"
    );
    assert!(
        body.contains("self.schedule_wakeup("),
        "about_to_wait no longer re-arms, so one missed interval ends the \
             autosave for the life of the process: {body}"
    );
}

/// A deadline in the past must put the loop back to sleep, not re-arm at
/// zero.
///
/// `set_control_flow` is sticky and `WaitUntil` is compared against the
/// clock every iteration, so an expired deadline left in place — or
/// re-armed with a saturated-to-zero delay — is a timeout of zero forever:
/// measured at ~164,000 iterations per second on one X11 core, with the
/// config still unwritten. This is the half that burns the battery, and it
/// survives the save being wired up: the save clears `touched`, and an
/// early return that leaves the stale `WaitUntil` alone spins just as hard
/// (measured: ~162,000/s).
#[test]
fn a_passed_autosave_deadline_does_not_re_arm_at_zero_delay() {
    let mut app = headless(TestBridge::desktop());

    // Well past due, so a deadline recomputed from `last_check` saturates.
    owes_a_save_from(&mut app, AUTOSAVE_INTERVAL * 4);

    let flow = wake_on_the_timer(&mut app);

    assert_eq!(
        flow,
        ControlFlow::Wait,
        "the loop was left on an expired WaitUntil, which is a zero timeout \
             on every following iteration — a busy loop that saves nothing"
    );
}

/// The positive control for the test above: closing the spin must not be
/// done by switching the timer off.
#[test]
fn a_change_inside_the_interval_still_arms_a_timer_for_the_rest_of_it() {
    let mut app = headless(TestBridge::desktop());

    // A third of the way in, so two thirds are still owed.
    owes_a_save_from(&mut app, AUTOSAVE_INTERVAL / 3);

    app.autosave_config(false);
    let Some(delay) = app.autosave_delay() else {
        panic!(
            "a change less than one interval old got no wake-up at all, so \
                 an app that goes quiet now sleeps on it forever"
        );
    };
    assert!(
        !delay.is_zero() && delay <= AUTOSAVE_INTERVAL,
        "the re-arm is not the remainder of the interval: {delay:?}"
    );
    assert!(
        matches!(app.wakeup_control_flow(), ControlFlow::WaitUntil(_)),
        "the delay is owed but the loop is not being woken to spend it"
    );
}

/// An app nothing has touched has to be left free to sleep indefinitely,
/// which is the whole reason `touched` exists.
#[test]
fn an_untouched_app_is_left_free_to_sleep() {
    let mut app = headless(TestBridge::desktop());
    app.autosave_config(true);
    assert!(
        !app.autosave.touched,
        "the check did not account for itself"
    );

    assert_eq!(
        app.wakeup_control_flow(),
        ControlFlow::Wait,
        "an idle app is being woken on a timer for a change nobody made"
    );
}

// ── Auto-poll scheduling ────────────────────────────────────────────

/// The bug this section exists for, stated where the loop is left in a
/// state: the end-of-frame re-arm used to include
/// `self.gui.is_auto_poll_active()`, which is `enabled &&
/// initial_fetch_done` — true from the first frame of the default
/// configuration and never false again. Every frame therefore asked for
/// another, so the app rendered at 60, 120 or 144 Hz for the life of the
/// process, with no user input and nothing in flight, to service a poll
/// that fires once a minute.
///
/// The re-arm must now hold only things that *finish*. A term that is
/// permanently true is a repaint loop however it is spelled.
#[test]
fn the_frame_re_arm_holds_only_work_that_finishes() {
    let body = fn_body("fn handle_redraw(");
    let start = body
        .find("if self.render.any_render_in_flight()")
        .expect("the end-of-frame re-arm is gone from handle_redraw");
    let arm = &body[start
        ..start
            + body[start..]
                .find("notify_redraw(")
                .expect("the re-arm no longer ends in a redraw request")];
    assert!(
        !arm.contains("is_auto_poll_active"),
        "the re-arm asks for another frame whenever a poll timer is running, \
             which is always: {arm}"
    );
    assert!(
        !arm.contains("auto_poll"),
        "an auto-poll term is back in the unconditional re-arm; it belongs in \
             the scheduled wake (`auto_poll_at`): {arm}"
    );
    // The terms that do belong: each one ends, and its ending asks for the
    // frame that notices.
    for kept in [
        "any_render_in_flight",
        "any_loop_active",
        "chunk_feeds.any_in_flight",
        // The handshake, which times out; not the backoff, which does not.
        // `a_down_socket_is_retried_regardless_of_other_activity` holds that
        // distinction and the schedule the other half moved to.
        "chunk_notify.handshake_pending",
        // Memory the app has already decided it does not want, waiting for a
        // frame to free it. It finishes — `drain_deferred_drops` takes at least
        // one payload per call — and it is the term with the least else to fall
        // back on: an eviction is exactly the moment when no render is in
        // flight and no loop is running, so without it a teardown drains once
        // and the rest waits on the user. No behavioural test can stand in for
        // this line: every harness that drives `handle_redraw` is windowless
        // and returns before the re-arm is reached.
        "has_deferred_drops",
    ] {
        assert!(
            arm.contains(kept),
            "the re-arm dropped `{kept}`, so that work now depends on \
                 something unrelated waking the loop: {arm}"
        );
    }
    assert!(
        body.contains("self.auto_poll_at ="),
        "the frame no longer records when auto-poll next needs one, so the \
             loop sleeps through every poll: {body}"
    );
}

/// A poll is checked inside the egui pass, so its wake-up has to end in a
/// real frame. The autosave's wake deliberately does not — it is spent
/// directly in `about_to_wait` — and copying that shape here would wake
/// the loop for an iteration that polls nothing.
///
/// A source probe for the same reason
/// `the_autosave_wakeup_is_spent_on_a_save_not_only_on_a_reschedule` is:
/// `about_to_wait` takes an `ActiveEventLoop`.
#[test]
fn the_auto_poll_wakeup_is_spent_on_a_frame() {
    let body = fn_body("fn about_to_wait(");
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("{needle} is gone from about_to_wait: {body}"))
    };
    assert!(
        at("self.auto_poll_at = None") < at("self.schedule_wakeup("),
        "the auto-poll deadline is not spent before the loop is re-armed on \
             it, so an expired one is re-armed at a zero delay every \
             iteration: {body}"
    );
    let spend = &body[at("self.auto_poll_at = None")..];
    assert!(
        spend.starts_with("self.auto_poll_at = None;\n            notify_redraw("),
        "the auto-poll wake-up no longer ends in a redraw request, so the \
             frame that would run `check_auto_polls` never happens: {spend}"
    );
}

/// An owed poll is a timer, not a repaint — the whole shape of the fix.
#[test]
fn an_owed_poll_leaves_the_loop_on_a_timer() {
    let mut app = headless(TestBridge::desktop());
    let owed = std::time::Duration::from_secs(42);
    app.auto_poll_at = Some(web_time::Instant::now() + owed);

    let ControlFlow::WaitUntil(until) = app.wakeup_control_flow() else {
        panic!(
            "a poll due in {owed:?} left the loop free to sleep indefinitely, \
                 so it will not fire until something unrelated happens"
        );
    };
    let delay = until.saturating_duration_since(web_time::Instant::now());
    assert!(
        delay > owed - std::time::Duration::from_secs(1) && delay <= owed,
        "the loop was woken for {delay:?} against a poll owed in {owed:?}"
    );
}

/// …and a spent one lets it sleep again. `set_control_flow` is sticky and a
/// `WaitUntil` is compared against the clock afresh every iteration, so a
/// deadline left behind after it passes is a zero timeout forever — the
/// same ~164,000 iterations per second the autosave's expired deadline
/// produced.
#[test]
fn a_spent_poll_wakeup_lets_the_loop_sleep_again() {
    let mut app = headless(TestBridge::desktop());
    app.auto_poll_at = Some(web_time::Instant::now() - std::time::Duration::from_secs(1));

    // What `about_to_wait` does with a deadline that has passed.
    app.auto_poll_at = None;

    assert_eq!(
        app.wakeup_control_flow(),
        ControlFlow::Wait,
        "the loop was left on an expired WaitUntil, which is a zero timeout \
             on every following iteration"
    );
}

/// Silence every other term, so what `auto_poll_delay` answers can only have
/// come from the one under test.
///
/// This is not scene-setting. A fresh `Gui` opens with layers that refresh on
/// timers of their own, and a layer that has never been fetched is due *now*
/// — so `auto_poll_delay` is `Some(MIN_WAKE)` whatever else is or is not in
/// the fold, and a test asserting that value alone passes with a term
/// deleted. Measured: dropping `self.chunk_feeds.next_round_delay()` from
/// `auto_poll_delay` left the entire workspace green.
///
/// The radar term is already `None` here — `AutoPollState::poll_delay` needs
/// a `last_fetch_time`, and only a frame writes one — and so is the status
/// bar's, which a headless app never draws. Both are asserted rather than
/// assumed, so a change that gives either an opinion fails here instead of
/// quietly making the tests below vacuous again.
fn silence_the_other_timers(app: &mut App) {
    for idx in 0..app.gui.remembered_pane_count() {
        let pane = app.gui.pane_mut(idx).expect("a remembered pane");
        for &kind in OverlayKind::all() {
            pane.enabled_overlays.insert(kind, false);
        }
    }
    assert_eq!(
        app.gui.auto_poll_delay(),
        None,
        "the GUI still owes a poll, so this fixture cannot attribute what \
             `auto_poll_delay` answers to the chunk feed"
    );
    assert_eq!(
        app.gui.status_tick_delay(),
        None,
        "a headless app drew no status bar, yet something is owed for one"
    );
}

/// The chunk feed's five-second round is a timer checked on a frame, and it
/// used to ride on the auto-poll re-arm keeping frames coming at 60 Hz.
/// Taking that away without scheduling for it would strand every live site
/// between rounds — the feed would stop, silently, and the site would fall
/// back to archive volumes minutes old.
///
/// Asserted through `App::auto_poll_delay` at every step rather than through
/// `ChunkFeedManager::next_round_delay`, because the defect this guards is
/// the *wiring*: a `next_round_delay` that is correct and not folded into the
/// wake is exactly as stranding as one that is wrong.
#[test]
fn a_chunk_feed_between_rounds_still_gets_its_frame() {
    let mut app = headless(TestBridge::desktop());
    silence_the_other_timers(&mut app);
    app.chunk_feeds.ensure("KTLX");

    assert_eq!(
        app.auto_poll_delay(),
        Some(MIN_WAKE),
        "a feed with no round yet is due now, and a zero-length sleep \
             re-armed every iteration is a busy loop, not a wake"
    );

    let poller = app
        .chunk_feeds
        .take_for_round("KTLX")
        .expect("the first round is available immediately");
    assert_eq!(
        app.auto_poll_delay(),
        None,
        "a round in flight is already holding the loop awake through \
             `any_in_flight`; scheduling for it as well would wake it twice"
    );

    app.chunk_feeds.finish_round(
        "KTLX",
        poller,
        &Ok(rustdar_radar::chunks::PollOutcome::default()),
    );
    let delay = app
        .auto_poll_delay()
        .expect("a feed between rounds owes itself another one");
    assert!(
        !delay.is_zero() && delay <= rustdar_radar::chunks::QUIET_INTERVAL,
        "the next round is scheduled {delay:?} out, which is not this feed's \
             own cadence"
    );
    // The same answer, not merely a plausible one — both are read off a live
    // clock, so they agree to within the microseconds between the two calls.
    let asked = app
        .chunk_feeds
        .next_round_delay()
        .expect("the feed still owes itself a round");
    assert!(
        delay.abs_diff(asked) < std::time::Duration::from_millis(50),
        "the loop will sleep {delay:?} against the {asked:?} the feed asked \
             for, so the wake is coming from something else"
    );
}

/// A notification socket waiting out its backoff is scheduled for, not spun
/// on.
///
/// This was the app's last unconditional spinner and the sharpest remaining
/// case of the bug this branch is about. The backoff doubles from 5 s to a
/// 300 s ceiling and never gives up — deliberately, because a service down
/// for an hour is what it exists to survive — so on a machine that cannot
/// reach the notifier at all (offline, a restrictive network) the boolean the
/// frame loop re-armed on was true for the entire session, and the app drew at
/// the display's refresh rate for as long as it ran.
///
/// Driven through `App::auto_poll_delay` rather than the notifier's own
/// accessor, because the defect is the wiring; `chunk_notify`'s own tests own
/// the arithmetic, where the backoff constants are in scope.
#[test]
fn a_notifier_backoff_is_slept_through_rather_than_spun_on() {
    use rustdar_radar::chunk_notify::Feed;

    // Loopback on a closed port: `ewebsock` opens a socket that will never
    // finish its handshake, which is the state a blocked network leaves.
    const ENDPOINT: &str = "wss://127.0.0.1:1";
    let sites = ["KTLX".to_string()];

    let mut app = headless(TestBridge::desktop());
    silence_the_other_timers(&mut app);
    app.chunk_notify
        .sync_sites(&sites, &Feed::ALL, ENDPOINT, || {});
    assert!(
        app.chunk_notify.handshake_pending(),
        "precondition: a handshake is in flight, which the re-arm carries"
    );
    assert_eq!(
        app.auto_poll_delay(),
        None,
        "a handshake still inside its timeout is being scheduled for as well \
             as re-armed on, so the loop wakes twice for one socket"
    );

    // Past `CONNECT_TIMEOUT`: the next sync tears the socket down and the
    // wait becomes a backoff.
    for feed in Feed::ALL {
        app.chunk_notify
            .backdate_handshake("KTLX", feed, std::time::Duration::from_secs(120));
    }
    app.chunk_notify
        .sync_sites(&sites, &Feed::ALL, ENDPOINT, || {});
    assert!(
        !app.chunk_notify.handshake_pending(),
        "precondition: the handshake timed out, so nothing re-arms for it now"
    );

    let delay = app
        .auto_poll_delay()
        .expect("a socket waiting out a backoff must still be retried");
    assert!(
        !delay.is_zero(),
        "the backoff was scheduled as a zero-length sleep, which is the spin \
             it was supposed to replace"
    );

    // And it goes quiet with the site, rather than outliving it.
    app.chunk_notify
        .sync_sites(&[], &Feed::ALL, ENDPOINT, || {});
    assert_eq!(
        app.auto_poll_delay(),
        None,
        "a retired site's backoff is still waking the app"
    );
}

/// Set by the back handler the app installs, so a test can see it *ran*
/// rather than merely being held somewhere.
static BACK_PRESS_REACHED_THE_HANDLER: AtomicBool = AtomicBool::new(false);

fn record_back_press() {
    BACK_PRESS_REACHED_THE_HANDLER.store(true, Ordering::Relaxed);
}

fn always_dark() -> bool {
    true
}

fn always_light() -> bool {
    false
}

/// The app opens showing what the last session left, and it can only get
/// that from the bridge — this crate has no idea where config lives.
#[test]
fn the_app_opens_with_the_config_its_platform_kept() {
    let bridge = TestBridge::desktop();
    seed_config(&bridge.store(), STORED_FPS);

    let app = headless(bridge);

    assert_eq!(
        app.gui.loop_speed_fps, STORED_FPS,
        "the stored config never reached the UI, so every session starts \
             on defaults",
    );
}

/// iOS cannot quit, and the menu must not offer to. The flag is pushed in
/// from here because `rustdar-egui` cannot see a bridge; what it then does
/// with it — dropping the Exit entry — is covered there.
#[test]
fn the_ui_is_told_whether_this_platform_can_quit() {
    assert!(
        !headless(TestBridge::ios()).gui.supports_exit(),
        "iOS would draw an Exit button that does nothing",
    );
    assert!(
        headless(TestBridge::desktop()).gui.supports_exit(),
        "the desktop menu lost its Exit entry",
    );
}

/// Android learns its data directory only after startup, so the load in
/// `App::new` had nothing to read and the second one is the only one that
/// ever runs there.
///
/// Also the strongest available statement that the directory *reached the
/// bridge*: the double, like Android's, has no store to hand out until it
/// has been told where one lives, so a dropped forward leaves the UI on
/// defaults just as a dropped load does.
#[test]
fn learning_where_config_lives_loads_it() {
    let bridge = TestBridge::android();
    seed_config(&bridge.store(), STORED_FPS);

    let mut app = headless(bridge);
    assert_eq!(
        app.gui.loop_speed_fps, 5.0,
        "precondition: nowhere to load from yet",
    );

    app.set_config_dir(std::path::PathBuf::from("/data/user/0/rustdar"));

    assert_eq!(
        app.gui.loop_speed_fps, STORED_FPS,
        "the config directory arrived and nothing was read from it",
    );
}

/// The save has to happen before the platform gets to refuse the exit.
///
/// On iOS the refusal is unconditional, so a `supports_exit` check hoisted
/// above the save would mean that platform never persists anything on quit
/// at all — and it would look completely fine on every other platform.
#[test]
fn a_platform_that_cannot_quit_still_saves_on_the_way_out() {
    let bridge = TestBridge::ios();
    let store = bridge.store();
    let mut app = headless(bridge);
    app.gui.loop_speed_fps = STORED_FPS;

    app.request_exit(None);

    assert_eq!(
        stored_fps(&store),
        STORED_FPS,
        "nothing was persisted; on iOS this is the only exit path there is",
    );
    assert!(
        !app.exit_requested,
        "iOS has no quit, so nothing may be scheduled on the next event",
    );
}

/// An exit asked for during a redraw has no event loop to hand, so it is
/// deferred rather than dropped.
#[test]
fn an_exit_with_no_event_loop_is_deferred_to_the_next_event() {
    let mut app = headless(TestBridge::desktop());
    assert!(!app.exit_requested, "precondition");

    app.request_exit(None);

    assert!(
        app.exit_requested,
        "the request was swallowed and the app never quits",
    );
}

/// The menu's Exit is one of the four ways out and goes through the same
/// gate as the rest: it saves, and it respects a platform that cannot quit.
///
/// The other three — `CloseRequested`, Escape and the Android back button —
/// all reach `request_exit` holding an `ActiveEventLoop`, which winit will
/// not hand out except from inside a running loop. Their routes are pinned
/// by the source probes above and below; only this one can be driven.
#[test]
fn the_menus_exit_goes_through_the_same_gate() {
    let mut app = headless(TestBridge::desktop());
    app.handle_gui_action(GuiAction::Exit, None);
    assert!(
        app.exit_requested,
        "Exit from the menu no longer reaches request_exit",
    );

    let bridge = TestBridge::ios();
    let store = bridge.store();
    let mut app = headless(bridge);
    app.gui.loop_speed_fps = STORED_FPS;

    app.handle_gui_action(GuiAction::Exit, None);

    assert!(!app.exit_requested, "iOS took the exit path anyway");
    assert_eq!(
        stored_fps(&store),
        STORED_FPS,
        "the menu's Exit skipped the config save",
    );
}

/// A fix and a heading are separate readings from separate sensors and must
/// stay that way: the map draws the dot from one and rotates it by the
/// other.
///
/// Both arrive over channels the app installs on the bridge, which is how
/// Android and the browser deliver them. Nothing here could be reached at
/// all until those two setters stopped being `#[cfg(target_os = "android")]`.
///
/// Driven through `handle_redraw` rather than `poll_platform_state`
/// directly. Nothing else polls the bridge, so calling the poller by hand
/// would leave the one line that schedules it — in the frame loop — free to
/// be deleted. With no window, `handle_redraw` polls and then returns
/// before it needs a renderer.
#[test]
fn the_platforms_sensors_reach_the_map() {
    let mut app = headless(TestBridge::android());
    let (fix_tx, fix_rx) = std::sync::mpsc::channel();
    let (heading_tx, heading_rx) = std::sync::mpsc::channel();
    app.set_gps_fix_receiver(fix_rx);
    app.set_heading_receiver(heading_rx);

    fix_tx
        .send(rustdar_location::Fix::from_lat_lon(35.3331, -97.2778))
        .unwrap();
    heading_tx.send(214.5).unwrap();

    app.handle_redraw();
    // `handle_redraw` polls the producers and then returns before it needs a
    // renderer; the compose that carries the polled facts to the UI lives on
    // the renderer's side of that return, so the test drives it itself.
    app.push_frame_inputs();

    let fix = app.gui.gps_fix().expect("no position reached the UI");
    assert_eq!((fix.point.lat, fix.point.lon), (35.3331, -97.2778));
    assert_eq!(
        app.gui.user_heading(),
        Some(214.5),
        "no compass reading reached the UI — note the fix carries no \
             heading of its own, so this cannot have come from it",
    );
}

/// **The frame loop frees what the application discarded.**
///
/// Driven through `handle_redraw` rather than by calling the drain directly,
/// for the reason `the_platforms_sensors_reach_the_map` gives: calling it by
/// hand would leave the one line that schedules it — in the frame loop, right
/// behind the eviction pass that fills the queue — free to be deleted with
/// every test in `offload::discard_tests` still green. With no window,
/// `handle_redraw` reaches the drain and returns before it needs a renderer.
///
/// **It proves the drain runs, and deliberately claims nothing about the
/// re-arm.** `headless` leaves `window` and `state` `None`, so `handle_redraw`
/// returns well before the end-of-frame re-arm; a name promising the loop stays
/// awake would be a name this harness cannot keep. That half is
/// `the_frame_re_arm_holds_only_work_that_finishes`, which reads the source of
/// the re-arm itself and is where `has_deferred_drops` is pinned.
#[test]
fn a_deferred_teardown_is_freed_by_the_frame_loop() {
    let mut app = headless(TestBridge::android());
    // Emptied first: the queue is thread-local and the harness reuses threads.
    while rustdar_worker::offload::drain_deferred_drops(std::time::Duration::from_secs(30)) > 0 {}

    let held: Vec<std::sync::Arc<()>> = (0..3).map(|_| std::sync::Arc::new(())).collect();
    let watched: Vec<std::sync::Arc<()>> = held.iter().map(std::sync::Arc::clone).collect();
    // Straight onto the queue, which is where a browser's `discard` puts every
    // payload — the native routing would hand these to the pool instead, and
    // the frame loop is what this test is about.
    for payload in held {
        rustdar_worker::offload::defer_drop("test-teardown", Box::new(payload));
    }
    assert!(rustdar_worker::offload::has_deferred_drops());

    for _ in 0..3 {
        app.handle_redraw();
    }
    assert!(
        watched
            .iter()
            .all(|item| std::sync::Arc::strong_count(item) == 1),
        "the frame loop never freed what was discarded, so the line that \
         drains the queue is not being reached",
    );
    assert!(
        !rustdar_worker::offload::has_deferred_drops(),
        "the queue outlived the frames that were supposed to empty it",
    );
}

/// A theme change has to invalidate the site labels, and only a *change*
/// may.
///
/// The labels are raster textures baked in the theme's colours, so they are
/// stale the moment it flips. But Android's theme poller re-sends its
/// reading every two seconds whether or not it moved — see
/// `spawn_state_poller` — so an unguarded bump would re-rasterise every
/// label on every pane twice a second, forever.
#[test]
fn a_theme_change_invalidates_the_site_labels_exactly_once() {
    let mut bridge = TestBridge::android();
    let theme = bridge.theme_channel();
    let mut app = headless(bridge);
    let before = app.gui.pane(0).unwrap().radar_sites_render_gen;

    theme.send(true).unwrap();
    app.handle_redraw();

    assert_eq!(
        app.cached_dark_theme,
        Some(true),
        "the change was not taken"
    );
    let after = app.gui.pane(0).unwrap().radar_sites_render_gen;
    assert_eq!(
        after,
        before.wrapping_add(1),
        "the site labels still carry the old theme's colours",
    );

    theme.send(true).unwrap();
    app.handle_redraw();

    assert_eq!(
        app.gui.pane(0).unwrap().radar_sites_render_gen,
        after,
        "a repeated reading re-rasterised every label; the poller sends \
             one of these every two seconds",
    );
}

/// Every scan response queued for a frame is spent in it.
///
/// They arrive in batches — auto-poll sends one `CheckForNewScans` per live
/// site, and two quick navigations queue two — while winit coalesces the
/// redraws each of them asks for into one `RedrawRequested`. Taking a single
/// response per frame left the rest in the channel with nothing scheduled to
/// come back for them: `handle_redraw`'s re-arm only fires for a render in
/// flight, auto-poll or an active loop.
///
/// The first response here is for a site no pane is showing, so only a drain
/// that goes past it reaches the one the pane is waiting on.
#[test]
fn every_queued_scan_response_is_spent_in_the_frame_it_arrives_in() {
    let mut app = headless(TestBridge::desktop());
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.site = "KTLX".to_string();
        pane.loading_site = Some("KTLX".to_string());
    }

    for site in ["KOUN", "KTLX"] {
        app.channels
            .scan_sender
            .send(crate::channels::ScanResponse {
                generation: 1,
                site: site.to_string(),
                result: Err("no data".to_string()),
                is_auto_poll: false,
            })
            .unwrap();
    }

    app.poll_data_channels();

    assert_eq!(
        app.gui.pane(0).unwrap().loading_site,
        None,
        "the second response was left in the channel, so the pane holds its \
             spinner until something unrelated wakes the loop",
    );
    assert!(
        app.channels.scan_receiver.try_recv().is_err(),
        "the frame ended with a scan response still queued",
    );
}

/// An app split into two panes, one on each named site.
///
/// `Gui::load_ui_config` is the only route to a multi-pane `Gui` that is
/// public to this crate: `Gui::set_pane_count_for_test` is `#[cfg(test)]`
/// inside `rustdar-egui`, so it exists for that crate's own tests and nowhere
/// else — which is why the pane loops here could previously only be covered
/// on their single-pane branches. Going through the config loader is not a
/// workaround either: it is the path a returning user's saved layout takes.
pub(super) fn two_pane_app(first: &str, second: &str) -> App {
    use rustdar_egui::UI_CONFIG_KEY;
    use rustdar_kv::KvStore;

    let mut app = headless(TestBridge::desktop());
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            &format!(
                r#"{{"pane_count":2,"site":"{first}",
                        "panes":[{{"site":"{first}"}},{{"site":"{second}"}}]}}"#
            ),
        )
        .expect("the memory store always accepts a write");
    assert!(
        app.gui.load_ui_config(&store),
        "the two-pane fixture config did not parse"
    );
    assert_eq!(
        app.gui.pane_count(),
        2,
        "precondition: the fixture must really have two panes"
    );
    assert_eq!(app.gui.pane(1).map(|p| p.site.as_str()), Some(second));
    app.render.ensure_pane_count(2);
    app
}

/// An `App` with `n` map panes, every one of them on `site`.
///
/// Built through the config loader for the reason [`two_pane_app`] gives, and
/// that is the only reason it is not written in terms of this: the two-site
/// case is what a split usually is, and the one-site case is what makes a
/// *shared* render observable. Both are one pane list either way.
pub(super) fn n_pane_app(n: usize, site: &str) -> App {
    use rustdar_egui::UI_CONFIG_KEY;
    use rustdar_kv::KvStore;

    let mut app = headless(TestBridge::desktop());
    let panes = (0..n)
        .map(|_| format!(r#"{{"site":"{site}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            &format!(r#"{{"pane_count":{n},"site":"{site}","panes":[{panes}]}}"#),
        )
        .expect("the memory store always accepts a write");
    assert!(
        app.gui.load_ui_config(&store),
        "the {n}-pane fixture config did not parse"
    );
    assert_eq!(
        app.gui.pane_count(),
        n,
        "precondition: the fixture must really have {n} panes"
    );
    app.render.ensure_pane_count(n);
    app
}

/// Every whole-texture upload egui has been handed since this was last called,
/// with the pixels it was handed.
///
/// `TexturesDelta::set` is the renderer's entire input — `egui_wgpu::Renderer`
/// turns each entry into one `queue.write_texture` and nothing else uploads at
/// all — so counting these counts uploads exactly, including the ones nobody
/// meant to make. Partial updates (`pos: Some(..)`) are the font atlas growing
/// and are not what any caller here is asking about.
///
/// This is also the only pixel readback egui offers: a `TextureHandle` gives an
/// id and a size and never its contents. Taking the delta is therefore how a
/// test says "these exact bytes reached the GPU" rather than "a texture of the
/// right shape exists".
pub(super) fn drain_uploads(ctx: &egui::Context) -> Vec<Arc<egui::ColorImage>> {
    ctx.tex_manager()
        .write()
        .take_delta()
        .set
        .into_iter()
        .filter(|(_, delta)| delta.pos.is_none())
        .map(|(_, delta)| {
            let egui::epaint::image::ImageData::Color(image) = delta.image;
            image
        })
        .collect()
}

/// A scan carrying no sweeps.
///
/// Nothing below reads a pixel: what is under test is whether a response was
/// applied at all, and an empty volume is the cheapest one this crate can
/// build. `ScanInfo::from_scan` handles it — it falls back to the requested
/// timestamp when there is no radial to date the volume from.
pub(super) fn empty_scan() -> nexrad_model::data::Scan {
    use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        ),
        Vec::new(),
    )
}

/// The scan info a pane holds while it is drawing `site`'s volume.
fn scan_info_for(site: &str) -> ScanInfo {
    ScanInfo::from_scan(
        &empty_scan(),
        site,
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        None,
    )
}

/// A decoded volume nobody is showing is not kept.
///
/// One entry is tens of megabytes and nothing else in this crate ever
/// removes one, so every radar a session visits stayed resident until the
/// process ended — next to a render cache that is carefully bounded and a
/// loop cache with a written-down byte budget.
///
/// **All three maps, and none is the incidental one.** `base_scans` holds
/// whole decoded volumes on exactly the same terms as `scan_data` and is
/// swept by the same pass off the same `shown` set, so a sweep that covered
/// only `scan_data` would leave the leak in place while looking closed.
/// `latest_cached_scans` was the leak this shape predicts: written for
/// every historic-mode site whose feed delivered, removed only by
/// `handle_jump_to_live` for that one site, and — until this pass covered
/// it — never bounded at all, so a session touring sites in historic mode
/// kept every one of their latest volumes for the life of the process.
#[test]
fn a_volume_no_pane_is_showing_is_dropped() {
    let mut app = headless(TestBridge::desktop());
    app.gui.pane_mut(0).unwrap().site = "KTLX".to_string();
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: scan_info_for("KTLX"),
        });
    for site in ["KTLX", "KOUN"] {
        app.scan_data.insert(
            site.to_string(),
            (Arc::new(empty_scan()), Default::default()),
        );
        app.base_scans.insert(
            site.to_string(),
            (
                Arc::new(empty_scan()),
                Default::default(),
                scan_info_for(site).timestamp,
            ),
        );
        app.latest_cached_scans.insert(
            site.to_string(),
            (
                Arc::new(empty_scan()),
                Default::default(),
                scan_info_for(site),
                scan_info_for(site).timestamp,
            ),
        );
    }

    app.evict_unshown_scans();

    assert!(
        app.scan_data.contains_key("KTLX"),
        "the volume the pane is drawing from was evicted",
    );
    assert!(
        !app.scan_data.contains_key("KOUN"),
        "a radar no pane is on is still holding its whole decoded volume",
    );
    assert!(
        app.base_scans.contains_key("KTLX"),
        "the base volume the site's whole-volume panes build from was \
             evicted, so none of them can ever be handed one",
    );
    assert!(
        !app.base_scans.contains_key("KOUN"),
        "a radar no pane is on is still holding its whole decoded base \
             volume; nothing else in this crate ever removes one",
    );
    assert!(
        app.latest_cached_scans.contains_key("KTLX"),
        "the cached latest volume for a shown site was evicted, so \
             JumpToLive on its pane has nothing to jump to",
    );
    assert!(
        !app.latest_cached_scans.contains_key("KOUN"),
        "a radar no pane is on is still holding its cached latest volume; \
             only JumpToLive ever removed one, and it cannot fire for a site \
             no pane shows",
    );
}

/// **An evicted volume is handed to `offload::discard`, not freed on the frame
/// that evicted it.**
///
/// This is the claim the whole deferred-drop mechanism was built for, and
/// nothing else asserts it: `evict_unshown_scans` used three `HashMap::retain`
/// calls, which free in place — on the frame thread, tens of megabytes across
/// thousands of per-radial buffers — and reverting to them leaves every test in
/// `offload::discard_tests` green, because those exercise the queue and the
/// pool without caring who fills them.
///
/// **A source probe, because the behaviour is genuinely indistinguishable.**
/// A `retain` frees the volume on this thread and a `discard` sends it to the
/// pool's free lane, and both leave the caller holding no reference a moment
/// later — an `Arc::strong_count` assertion would pass on either and prove
/// nothing. What separates them is *where the walk happens*, which no
/// observation available to this thread can name. So the probe reads the
/// eviction's own source, the way
/// `the_frame_re_arm_holds_only_work_that_finishes` reads the re-arm's.
#[test]
fn an_evicted_volume_is_handed_over_rather_than_freed_on_the_frame() {
    let body = fn_body("fn evict_unshown_scans(");
    assert!(
        !body.contains(".retain("),
        "eviction is back to `retain`, which frees every evicted volume in \
         place — on the frame thread, tens of megabytes across thousands of \
         per-radial buffers, which is the cost `offload::discard` exists to \
         move: {body}"
    );
    // All three holders, named individually: a hand-over that covered
    // `scan_data` and left the other two on `retain` would look closed here
    // while leaving most of the teardown on the frame.
    //
    // The whole call expression rather than the map name alone — the name also
    // appears in this function's prose, and `rustfmt` reflows the calls
    // themselves (one of the three fits on a line and two do not), so the
    // extraction is the only part stable enough to assert on.
    for map in ["scan_data", "base_scans", "latest_cached_scans"] {
        assert!(
            body.contains(&format!("evicted(&mut self.{map}, &unshown)")),
            "{map}'s evictions are not handed over, so that map's volumes are \
             still freed on the frame: {body}"
        );
    }
    // The extractions above prove the values come out owned; this proves where
    // they go. The fully-qualified call, not the bare name, because this
    // function's own prose mentions `offload::discard_each` and a substring
    // count would have counted the sentence.
    assert_eq!(
        body.matches("rustdar_worker::offload::discard_each(")
            .count(),
        3,
        "one of the three volume holders stopped handing its evictions over \
         to the deferred-drop path: {body}"
    );
}

/// **The loop cache's evictions are handed over too, and the sweep is wired in.**
///
/// The fourth holder of whole decoded volumes reached the same rule one commit
/// later, and needs the same two probes for the same reason: `retain_scans`
/// returning its values is only worth anything if the caller sends them
/// somewhere, and a `for` loop over the returned `Vec` that dropped them would
/// be indistinguishable at run time from one that handed them over.
///
/// The wiring half is not a source-probe nicety — `evict_unneeded_loop_scans`
/// is called from exactly one place, and a sweep nobody calls is the shape this
/// whole defect had.
#[test]
fn the_loop_caches_evictions_are_handed_over_and_the_sweep_is_called() {
    assert!(
        fn_body("fn evict_unshown_scans(").contains("self.evict_unneeded_loop_scans();"),
        "the loop cache's sweep is no longer reached from the once-a-frame \
         eviction, so nothing bounds it — which is the defect it closed",
    );
    let body = fn_body("fn evict_unneeded_loop_scans(");
    assert_eq!(
        body.matches("rustdar_worker::offload::discard_each(")
            .count(),
        3,
        "one of the loop's three holders frees its evictions where it evicted \
         them — on the frame thread, 47–69 MiB apiece for a volume, a decoded \
         message plus its own bytes for an object, a day's bucket keys for a \
         listing: {body}"
    );
    // The grace rule, named rather than described: without it a loop whose
    // listing is in flight names no frame, and the sweep takes its whole
    // window one frame before the listing would have saved it.
    assert!(
        body.contains("listing_wait(now)"),
        "the grace rule for a loop still fetching its scan listing is gone, so \
         every product switch and loop re-init re-downloads its window: {body}"
    );
    // And the clock on it. An exemption with no bound is not a milder bug than
    // no sweep at all on wasm32, where nothing else ever ends the wait — see
    // `constants::LOOP_LISTING_GRACE`.
    assert!(
        body.contains("LOOP_LISTING_GRACE"),
        "the grace exemption is unbounded again: on wasm32 a listing future \
         that never completes then exempts its site for the life of the tab, \
         and the leak resumes at full rate: {body}"
    );
    // The queues are swept by the same predicate as the cache. Split apart,
    // the download filter re-queues what the sweep just evicted.
    assert!(
        body.contains("retain_plan_frames(keep)") && body.contains("retain_scans(keep)"),
        "the frame plan and the cache are no longer swept by one predicate, so \
         a re-plan can queue a download for a volume the sweep evicts: {body}"
    );
    // And the Level III cache, by the same predicate object rather than a
    // second rule free to disagree with the first.
    assert!(
        body.contains("retain_l3(keep)"),
        "the Level III cache is no longer swept, or is swept by a rule of its \
         own: one `Level3Product` per frame per AWIPS code, removed by nothing \
         else, is the sibling of the leak this pass exists for: {body}"
    );
}

/// A pane is under two site names and eviction has to honour both.
///
/// `pane.site` is the radar the pane is aimed at; `scan_info.site.name` is the
/// radar the volume it is drawing came from, and it is the second that
/// `dispatch_pane_renders` looks the volume up under. Keyed on the live site
/// alone, eviction pulls the scan out from under a pane still rendering from it,
/// and the symptom is a product change that silently does nothing.
///
/// `base_scans` rides the same `shown` set for the same reason: a 3D pane builds
/// from the base volume of the site its scan came from, and an eviction keyed on
/// the live site alone would free it under the resampler.
///
/// Nothing writes the two names apart today — `SwitchRadarSite` drops the
/// `scan_info` with the site, and every path that copies a pane's site copies its
/// scan beside it — so the divergence here is built by hand. The union is what
/// keeps that a *property of the pane* rather than of one handler remembering.
#[test]
fn the_volume_a_switching_pane_is_still_drawing_survives() {
    let mut app = headless(TestBridge::desktop());
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: scan_info_for("KTLX"),
        });
    app.gui.pane_mut(0).unwrap().site = "KOUN".to_string();
    app.scan_data.insert(
        "KTLX".to_string(),
        (Arc::new(empty_scan()), Default::default()),
    );
    app.base_scans.insert(
        "KTLX".to_string(),
        (
            Arc::new(empty_scan()),
            Default::default(),
            scan_info_for("KTLX").timestamp,
        ),
    );

    app.evict_unshown_scans();

    assert!(
        app.scan_data.contains_key("KTLX"),
        "the pane's own scan info still names KTLX, which is what the \
             render path looks the volume up by",
    );
    assert!(
        app.base_scans.contains_key("KTLX"),
        "the base volume was pulled out from under a 3D pane that is \
             still building from it",
    );
}

/// A result thrown away still ends the wait it belonged to.
///
/// `SwitchRadarSite` raises a `loading_site` and sets no `fetching` flag, so
/// the gate that holds auto-poll off does not hold, and the very next frame
/// can emit a `CheckForNewScans` for the same site that bumps the generation
/// past it. The switch's own result then lands stale and is discarded — and
/// nothing else was ever going to take the spinner down, because
/// `check_and_fetch_latest` sends no response at all unless there is a newer
/// volume.
#[test]
fn a_discarded_scan_result_still_takes_down_the_wait_it_belonged_to() {
    let mut app = headless(TestBridge::desktop());
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.site = "KTLX".to_string();
        pane.loading_site = Some("KTLX".to_string());
    }

    // The fetch this response belongs to, then the one that supersedes it.
    let superseded = app.render.next_fetch_generation("KTLX");
    app.render.next_fetch_generation("KTLX");

    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation: superseded,
            site: "KTLX".to_string(),
            result: Ok(crate::channels::ScanData {
                scan: empty_scan(),
                declared_nyquist: Default::default(),
                site: "KTLX".to_string(),
                timestamp: chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            }),
            is_auto_poll: false,
        })
        .unwrap();

    app.poll_data_channels();

    assert!(
        app.gui.pane(0).unwrap().scan_info.is_none() && app.scan_data.is_empty(),
        "precondition: the superseded result was applied rather than \
             discarded, so nothing here is about the discard path",
    );
    assert_eq!(
        app.gui.pane(0).unwrap().loading_site,
        None,
        "the switch's spinner is still up with nothing left that would ever \
             take it down",
    );
}

/// The theme the frame resolves is the theme everything else rasterizes in.
///
/// `cached_dark_theme` is not a memo for a slow read: it is the *only*
/// answer the overlay rasterizers have, because they run on worker threads
/// with no window to ask (`RasterizeContext::is_dark`, and the `is_dark`
/// handed to `rasterize_radar_sites`). A frame that resolves a theme
/// without recording it leaves them on `unwrap_or(false)`.
///
/// Driven with no window, which is the arm Android and X11 take: winit has
/// no answer there, so the bridge is asked. The other arm is source-probed
/// below — a window cannot be built here.
#[test]
fn the_theme_the_frame_resolves_is_the_one_the_overlays_get() {
    let mut app = headless(TestBridge::android());
    app.set_theme_detector(always_dark);
    let before = app.gui.pane(0).unwrap().radar_sites_render_gen;
    assert_eq!(
        app.cached_dark_theme, None,
        "precondition: nothing read yet"
    );

    assert!(app.resolve_theme(), "the frame drew in the wrong theme");

    assert_eq!(
        app.cached_dark_theme,
        Some(true),
        "the frame resolved a theme and left every off-frame rasterizer \
             with none, so the overlays come back light under a dark UI",
    );
    assert_eq!(
        app.gui.pane(0).unwrap().radar_sites_render_gen,
        before.wrapping_add(1),
        "the site labels still carry the old theme's colours",
    );

    assert!(app.resolve_theme(), "the reading changed on a second look");
    assert_eq!(
        app.gui.pane(0).unwrap().radar_sites_render_gen,
        before.wrapping_add(1),
        "every frame re-rasterises every label",
    );
}

/// The two theme routes a desktop actually takes, neither of which can be
/// driven here: winit answers `window.theme()` on Windows and macOS, and it
/// reports a flip as `ThemeChanged`. Both must reach `adopt_theme`.
///
/// This is the shape the bug had. The `window.theme()` arm resolved a value
/// and returned it without recording it, and `ThemeChanged` *emptied* the
/// cache — which reads as "re-detect next frame" only on a platform whose
/// bridge detects anything. Desktop's `poll_theme` is hardwired `None`, so
/// there the cache simply stayed empty for good, and both defects were
/// invisible on the two platforms whose poll thread writes it anyway.
#[test]
fn the_desktop_theme_routes_record_what_they_read() {
    let body = fn_body("fn resolve_theme(");
    assert!(
        body.contains("self.adopt_theme(dark)"),
        "resolve_theme no longer records the theme it resolved: {body}",
    );
    assert!(
        !body.contains("return"),
        "an arm of resolve_theme answers on its own, so the theme it read \
             never reaches the cache: {body}",
    );

    let arm = arm_body(fn_body("fn window_event("), "WindowEvent::ThemeChanged");
    assert!(
        arm.contains("self.adopt_theme("),
        "a theme flip no longer goes through the funnel, so nothing \
             re-rasterises the site labels in the new theme's colours: {arm}",
    );
}

/// Where the injected querier says the system bars are. A `fn` pointer
/// closes over nothing, which is the constraint Android's real querier is
/// under too — it reaches the framework through a process-wide `JavaVM`.
static ROTATED: AtomicBool = AtomicBool::new(false);

fn cutout() -> (f32, f32, f32, f32) {
    if ROTATED.load(Ordering::Relaxed) {
        (0.0, 0.0, 96.0, 0.0)
    } else {
        (96.0, 0.0, 0.0, 0.0)
    }
}

/// Turning the device sideways moves the cutout to another edge, and the
/// app has to ask again.
///
/// It arrives as a resize, not as a resume, so insets queried once at
/// startup describe the orientation the app happened to open in for the
/// rest of the session — reserving a strip along the top while the notch is
/// down the left. The resize is also the signal that a layout has happened,
/// which is what `getRootWindowInsets` needs before it has anything current
/// to return.
#[test]
fn a_rotation_re_queries_the_insets_rather_than_keeping_the_old_edge() {
    ROTATED.store(false, Ordering::Relaxed);
    let mut app = headless(TestBridge::android());
    app.set_insets_querier(cutout);

    // What `resumed` does once the window exists.
    app.refresh_safe_area_insets();
    app.push_frame_inputs();
    assert_eq!(
        app.gui.safe_area_insets(),
        (96.0, 0.0, 0.0, 0.0),
        "precondition: portrait puts the cutout along the top",
    );

    ROTATED.store(true, Ordering::Relaxed);
    app.handle_resized(2400, 1080);
    app.push_frame_inputs();

    assert_eq!(
        app.gui.safe_area_insets(),
        (0.0, 0.0, 96.0, 0.0),
        "the device rotated and the app is still holding a strip clear at \
             the top while the cutout eats the left edge",
    );
}

/// Both query sites have to stay wired. The behavioural test above drives
/// `handle_resized`; `resumed` takes an `ActiveEventLoop` and cannot be
/// called, so its half is read off the source, as `back_out`'s is.
#[test]
fn both_inset_queries_are_still_wired() {
    for f in ["fn resumed(", "fn handle_resized("] {
        assert!(
            fn_body(f).contains("refresh_safe_area_insets("),
            "{f} no longer asks the platform for insets",
        );
    }
}

/// The window's own close button is the fourth exit trigger and the last
/// one with no other handle on it: `window_event` takes an
/// `ActiveEventLoop`, so the arm can only be read.
///
/// What it must reach is `request_exit` and not `event_loop.exit()` — the
/// config save and the `supports_exit` refusal both live inside it, and a
/// direct exit here would skip both while looking perfectly correct.
#[test]
fn closing_the_window_goes_through_request_exit() {
    let arm = arm_body(fn_body("fn window_event("), "WindowEvent::CloseRequested");
    assert!(
        arm.contains("self.request_exit("),
        "the close button bypasses request_exit, so it saves no config and \
             ignores a platform that cannot quit: {arm}",
    );
}

/// A deferred exit has to leave by the same door as an immediate one.
///
/// The menu's Exit is processed during a redraw, where there is no
/// `ActiveEventLoop` to hand out, so it parks a flag and the next
/// `RedrawRequested` spends it. That replay used to call `event_loop.exit()`
/// on its own, which drops the `process::exit` half — and Android, where the
/// loop never unwinds and the menu is the primary way out, is precisely the
/// platform that needs it. So the one route that *always* defers was the one
/// route that never ended the process.
///
/// `window_event` takes an `ActiveEventLoop` and `exit_now` ends the
/// process, so both halves are read off the source.
#[test]
fn a_deferred_exit_leaves_by_the_same_door_as_an_immediate_one() {
    let arm = arm_body(fn_body("fn window_event("), "WindowEvent::RedrawRequested");
    assert!(
        arm.contains("self.exit_now("),
        "the deferred exit no longer goes through exit_now, so on Android \
             it asks a loop that never unwinds to leave and the process stays \
             up: {arm}",
    );
    assert!(
        fn_body("fn exit_now(").contains("self.platform.needs_process_exit()"),
        "exit_now no longer ends the process on a platform whose event loop \
             never unwinds",
    );
}

/// The save on the way out has to be in `exit_now`, not only where the exit
/// was requested.
///
/// On the deferred route — Android's primary one, since the menu is processed
/// during a redraw with no event loop to hand out — `handle_redraw` runs
/// between the request and the replay. Its `autosave_config` *queues* a write,
/// because the config store hands writes to a writer thread, and the
/// `process::exit` in `exit_now` discards anything that thread has not reached.
/// The lost write is the one covering the last change the user made, in the
/// very redraw that processed Exit.
///
/// Read off the source because `exit_now` needs an `ActiveEventLoop` and ends
/// the process, which is the same reason the test above reads it that way.
#[test]
fn the_way_out_saves_the_config_where_the_process_actually_ends() {
    let body = fn_body("fn exit_now(");
    let save = body
        .find("save_ui_config")
        .expect("exit_now no longer saves the config, so a deferred exit loses the last change");
    let exit = body
        .find("needs_process_exit")
        .expect("exit_now no longer ends the process");
    assert!(
        save < exit,
        "the save must come before the process ends, or it is not a save: {body}",
    );
}

/// Two things the app hands the bridge that it can only get back by asking.
///
/// The theme read is Android's only source — NativeActivity never emits
/// `ThemeChanged` — and the back handler is what makes back minimise there
/// instead of quitting. Both are `fn` pointers because the JNI they end in
/// lives in a crate the bridge cannot depend on.
///
/// The theme half takes two apps rather than reading the uninjected state
/// first: with no detector, Android has no answer at all and both the real
/// bridge and the double `debug_assert!` there. Opposite detectors say more
/// anyway — that the read *follows* the injected function, not merely that
/// it changed.
#[test]
fn the_injected_callbacks_reach_the_bridge() {
    let mut app = headless(TestBridge::android());
    app.set_theme_detector(always_dark);
    assert!(
        app.platform.detect_dark_theme(),
        "the theme read never arrived, and Android has no other one",
    );

    let mut light = headless(TestBridge::android());
    light.set_theme_detector(always_light);
    assert!(
        !light.platform.detect_dark_theme(),
        "the read does not follow the detector it was handed",
    );

    light.set_theme_detector(always_dark);
    assert!(
        !light.platform.detect_dark_theme(),
        "a second detector was accepted; Android refuses one rather than \
             leave its poll thread calling the detector it has replaced",
    );

    BACK_PRESS_REACHED_THE_HANDLER.store(false, Ordering::Relaxed);
    assert_eq!(
        App::resolve_back_press(&mut app.gui, app.platform.as_ref()),
        BackPress::Exit,
        "precondition: with no handler installed, back quits",
    );

    app.set_back_handler(record_back_press);
    assert_eq!(
        App::resolve_back_press(&mut app.gui, app.platform.as_ref()),
        BackPress::PlatformHandled,
    );
    assert!(
        BACK_PRESS_REACHED_THE_HANDLER.load(Ordering::Relaxed),
        "the handler was installed but never run, so back reports the app \
             minimised and nothing minimises",
    );
}

/// The reader is started on the port the *action* names.
///
/// The settings pane edits a config and emits it with the action; the
/// bridge is the only thing that ever sees it, and opening the wrong serial
/// port is indistinguishable from a missing one at this level. So the
/// double keeps what it was handed — the one place in this suite where a
/// recorded argument is the only observable there is.
#[test]
fn starting_gps_hands_the_bridge_the_config_the_action_carried() {
    let bridge = TestBridge::desktop();
    let started = bridge.gps_record();
    let mut app = headless(bridge);

    app.handle_gui_action(
        GuiAction::StartGps {
            config: rustdar_nmea_serial::SerialConfig {
                port_path: Some("/dev/ttyPROBE".to_string()),
                baud_rate: 38400,
            },
        },
        None,
    );

    assert!(app.platform.gps_active(), "the reader was never started");
    {
        let record = started.borrow();
        let config = record.as_ref().expect("start_gps was not reached");
        assert_eq!(
            config.port_path.as_deref(),
            Some("/dev/ttyPROBE"),
            "the reader opened a different port than the action asked for",
        );
        assert_eq!(config.baud_rate, 38400);
    }

    app.handle_gui_action(GuiAction::StopGps, None);
    assert!(
        !app.platform.gps_active(),
        "the reader kept the serial port open after being told to stop",
    );
}

// ── egui repaint plumbing (the M9 animation fix) ─────────────────────────

/// The classification the loop acts on: zero-delay requests (animations)
/// repaint immediately, timed ones (cursor blink) schedule a wake, and
/// egui's `Duration::MAX` idle marker — or anything indistinguishable from
/// it — leaves the loop parked. The middle arm must never collapse into
/// `Now`: a 500 ms blink re-requests itself on every paint, so an immediate
/// redraw for it is a busy loop at frame rate.
#[test]
fn a_repaint_delay_maps_to_now_a_schedule_or_idle() {
    use std::time::Duration;
    assert_eq!(repaint_action(Duration::ZERO), RepaintAction::Now);
    assert_eq!(
        repaint_action(Duration::from_millis(500)),
        RepaintAction::After(Duration::from_millis(500))
    );
    assert_eq!(repaint_action(Duration::MAX), RepaintAction::Idle);
    assert_eq!(
        repaint_action(Duration::from_secs(3600)),
        RepaintAction::Idle,
        "an hour-scale request is idle for a loop every input wakes anyway"
    );
}

/// The contract the fix stands on: a mid-flight `animate_bool_with_time`
/// puts a zero `repaint_delay` on the root viewport's output — the value
/// `end_pass_and_upload` now carries out and `handle_redraw` spends. If
/// egui ever stopped reporting animations this way, the panel slides would
/// quietly go back to advancing only on input frames (the "close shudders,
/// then vanishes" finding), and this is the test that names it.
#[test]
fn a_mid_flight_animation_requests_an_immediate_repaint() {
    let ctx = egui::Context::default();
    let raw = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    // Seed the animation at `false`, then flip the target: the second pass
    // is mid-interpolation for a 0.2 s animation.
    ctx.begin_pass(raw());
    let _ = ctx.animate_bool_with_time(egui::Id::new("m9_anim"), false, 0.2);
    let _ = ctx.end_pass();
    ctx.begin_pass(raw());
    let value = ctx.animate_bool_with_time(egui::Id::new("m9_anim"), true, 0.2);
    let animating = ctx.end_pass();

    assert!(
        (0.0..1.0).contains(&value),
        "precondition: the animation is mid-flight, got {value}"
    );
    let delay = animating
        .viewport_output
        .get(&egui::viewport::ViewportId::ROOT)
        .map(|v| v.repaint_delay)
        .expect("the root viewport reports");
    assert_eq!(
        delay,
        std::time::Duration::ZERO,
        "egui no longer requests an immediate repaint mid-animation - the \
         chrome slides will only advance on input frames"
    );
    // (The settled side — no repaint request once the animation ends — is
    // real-clock timing and lives with `repaint_action`'s own mapping pins.)
}

// ── Learning where the radars are ───────────────────────────────────

/// A volume stating its own position, as `scan::decoded` builds one out of
/// the first Message 31's Volume Data Block.
///
/// The counterpart of [`empty_scan`], which states nothing — the shape a
/// chunk-fed or a pre-2010 volume arrives in.
fn scan_stating(lat: f32, lon: f32) -> nexrad_model::data::Scan {
    use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
    Scan::with_site(
        nexrad_model::meta::Site::new(*b"KTLX", lat, lon, 370, 20),
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        ),
        Vec::new(),
    )
}

/// The learned position survives the process, and is applied before anything
/// is drawn from it.
///
/// Two apps over one store, which is the only way to exercise something the
/// app is supposed to remember across restarts. The second one is handed a
/// volume that states *nothing* — the shape a chunk feed or a pre-2010 archive
/// produces — so the only thing that can place its site correctly is what the
/// first one wrote.
///
/// The read happens inside `ScanInfo::from_scan`, before the `ScanInfo`
/// exists and so before any pane can have painted from it. That is what keeps
/// this out of the "late correction that shifts a pane the user is looking at"
/// class the 1:1-on-reopen rule forbids.
#[test]
fn a_position_a_volume_taught_survives_a_restart() {
    use rustdar_radar::site_position::SitePositionSource;

    let store = std::rc::Rc::new(MemoryKvStore::default());
    // Before the read, not merely before the app: `headless` installs the
    // fixture, and it is not called until further down. Without this the read
    // below found whatever a *sibling* test's `headless` had left in the
    // process table, so the test passed in a full run and panicked under
    // `--exact`.
    crate::test_sites::install();
    let table = rustdar_radar::sites::get_radar_site("KTLX").expect("the fixture places KTLX");
    // A quarter of a degree from the row: far enough that resolving to the
    // wrong one is unmistakable, and in the direction a re-survey would move.
    let stated_lat = (table.lat + 0.25) as f32;
    let at = chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap();

    let learned_lat = {
        let mut app = headless(TestBridge::desktop().with_store(std::rc::Rc::clone(&store)));
        let info = app.scan_info_learning_position(
            &scan_stating(stated_lat, table.lon as f32),
            "KTLX",
            at,
        );
        assert_eq!(info.site_source, SitePositionSource::Volume);
        assert!(
            (info.site.lat - f64::from(stated_lat)).abs() < 1e-5,
            "the volume's own position must be what is drawn: {}",
            info.site.lat,
        );
        // Written already, with no autosave tick having run.
        assert!(
            rustdar_kv::KvStore::load(store.as_ref(), crate::site_positions::SITE_POSITIONS_KEY,)
                .is_some(),
            "the learned position must be durable the moment it is learned",
        );
        info.site.lat
    };

    // A second run, over the same blobs, handed a volume that states nothing.
    let mut next_run = headless(TestBridge::desktop().with_store(std::rc::Rc::clone(&store)));
    let recalled = next_run.scan_info_learning_position(&empty_scan(), "KTLX", at);

    assert_eq!(recalled.site_source, SitePositionSource::Learned);
    assert_eq!(
        recalled.site.lat.to_bits(),
        learned_lat.to_bits(),
        "the recalled position must be bit-identical to the one the pane was \
         drawn at, or reopening is not 1:1",
    );
    assert_ne!(
        recalled.site.lat, table.lat,
        "the compiled-in row must not have won",
    );
}

/// The map's marker moves with the data the moment a volume teaches a position.
///
/// Two tables answer "where is this radar", and until the volume lands they
/// agree. `ScanInfo::site` is where the *data* goes — `ImageBounds` frames the
/// raster on it, `render_radar_range_ring` draws the ring round it, the hover
/// readout measures bearing and range from it — and it takes the volume's word
/// the instant one arrives. `sites::radars()` is where the *marker* goes:
/// `visible_radar_sites` walks it every frame for the icons, the labels and the
/// hit-tests, and `rasterize_radar_sites` draws the texture from the same rows.
/// That one used to be resolved only at startup, so for the whole of the first
/// session that decoded a volume for a re-surveyed radar the icon sat beside
/// its own range ring.
///
/// # Why the assertions are on the table and not on a screenshot
///
/// `radars()` *is* what the map draws from, and both consumers are pure walks
/// of it. Checking the row and the walk is checking the marker, without a GPU;
/// what a frame adds is the rasterized icon, and `bump_all_radar_sites_gen` is
/// what makes it redraw rather than stay on the old coordinates until the next
/// thing that happens to bump it.
///
/// # `KMQT`
///
/// Load-bearing, exactly as `KMBX` is for
/// [`a_run_with_no_kv_still_applies_the_volumes_own_position`]: a fix
/// displaces the row it lands on for the whole process, so this test moves
/// `KMQT` for every test that runs after it. It must stay an identifier no
/// other test in the workspace names, and it is placed here rather than by
/// [`crate::test_sites`] for that reason — the binary carries no radars, so
/// the row this starts from has to come from somewhere, and a shared fixture
/// would be a row other tests could read after this one had moved it.
#[test]
fn a_taught_position_moves_the_maps_marker_and_not_only_the_data() {
    use rustdar_radar::site_position::SitePositionSource;

    const SITE: &str = "KMQT";
    let store = std::rc::Rc::new(MemoryKvStore::default());
    // Marquette, Michigan, at the position and heights its own volume reports —
    // the row this test starts from, because nothing is compiled in.
    rustdar_radar::sites::resolve([(
        SITE,
        rustdar_radar::sites::SiteFix::Learned(rustdar_radar::site_position::SitePosition {
            lat_udeg: 46_531_110,
            lon_udeg: -87_548_330,
            site_height_m: 430,
            tower_height_m: 20,
        }),
    )]);
    // Copied out rather than borrowed: `sites::resolve` below displaces the row
    // this names, and a `&'static RadarSite` held across it goes on describing
    // where the radar was *believed* to be — which is the very thing under test.
    let (seeded_lat, seeded_lon) = {
        let row = rustdar_radar::sites::get_radar_site(SITE).expect("this test placed it");
        (row.lat, row.lon)
    };
    // A quarter of a degree, in the direction a re-survey would move: far past
    // anything a rounding could produce.
    let stated_lat = (seeded_lat + 0.25) as f32;
    let at = chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap();

    let mut app = headless(TestBridge::desktop().with_store(std::rc::Rc::clone(&store)));
    let before = app.gui.pane(0).unwrap().radar_sites_render_gen;

    let info =
        app.scan_info_learning_position(&scan_stating(stated_lat, seeded_lon as f32), SITE, at);
    assert_eq!(info.site_source, SitePositionSource::Volume);
    assert!(
        (info.site.lat - f64::from(stated_lat)).abs() < 1e-5,
        "precondition: the data must have moved, or there is nothing for the \
         marker to disagree with: {}",
        info.site.lat,
    );

    let row = rustdar_radar::sites::get_radar_site(SITE).expect("still a row");
    assert!(
        (row.lat - info.site.lat).abs() < 1e-9,
        "the map draws {SITE} at {} while its own volume put the data — the \
         raster, the range ring and the hover readout — at {}",
        row.lat,
        info.site.lat,
    );
    assert_ne!(
        row.lat, seeded_lat,
        "the row this test placed is still standing, so nothing above was tested",
    );
    // The walk both marker consumers really do, rather than the `get` above.
    assert!(
        rustdar_radar::sites::radars()
            .iter()
            .any(|r| r.name == SITE && (r.lat - info.site.lat).abs() < 1e-9),
        "the table's `get` moved but the walk `visible_radar_sites` and \
         `rasterize_radar_sites` both take did not",
    );
    // Nothing else moved with it.
    assert_eq!(
        rustdar_radar::sites::radars()
            .iter()
            .filter(|r| r.name == SITE)
            .count(),
        1,
        "the fix added a second {SITE} instead of displacing the first",
    );

    assert_ne!(
        app.gui.pane(0).unwrap().radar_sites_render_gen,
        before,
        "the site texture was not invalidated, so the drawn icons stay on the \
         seeded coordinates until something else happens to bump it",
    );

    // Restating the same position is not a fresh lesson: no second resolve, no
    // second invalidation, so a session does not re-key the texture every
    // volume.
    let settled = app.gui.pane(0).unwrap().radar_sites_render_gen;
    let again =
        app.scan_info_learning_position(&scan_stating(stated_lat, seeded_lon as f32), SITE, at);
    assert_eq!(again.site_source, SitePositionSource::Volume);
    assert_eq!(
        app.gui.pane(0).unwrap().radar_sites_render_gen,
        settled,
        "a volume restating what is already known re-rasterized every site icon",
    );
}

/// **A volume decoded this session gives its radar an MSL datum this session.**
///
/// The hole the compiled-in table used to cover, and the reason
/// `scan_info_learning_position` resolves as well as persists.
///
/// `eet::radar_height_ft_near` is what the cross-section's `base_km_msl`, the
/// voxel grid's datum, hail's ARL and HCA all anchor on, and it searches the
/// **table** at coordinates that came from the volume. While the binary
/// carried a row for every radar it always found one. It does not any more, so
/// on a first run — before any catalogue has landed and before anything was
/// learned in an earlier session — the table has no row for the radar being
/// rendered, the lookup answers `None`, and `render_site_height_ft` falls back
/// to `0.0`.
///
/// Zero is not a visible failure. It is sea level, which is a perfectly
/// plausible reading for a coastal site, and it was 292 ft of silent error at
/// KLWX when six rows shipped without an elevation. This is the same defect
/// with the same shape at the scale of the whole network, and this test is
/// what stands between the two.
///
/// Fails on revert: drop the `sites::resolve` from
/// `scan_info_learning_position` and the lookup below answers `None`.
#[test]
fn a_volume_this_session_decoded_gives_its_radar_a_height_this_session() {
    use rustdar_radar::sites::Datum;

    // An identifier nothing else in this workspace names, at a coordinate no
    // other test in this binary places a radar near — the site table is
    // process-wide, and `first_launch_tests` puts one at (-30, -140). The
    // Southern Ocean, thousands of kilometres from all of them, so "the lookup
    // found this radar" cannot be confused with "the lookup found a
    // neighbour".
    const SITE: &str = "ZZQE";
    const LAT: f32 = -55.0;
    const LON: f32 = -120.0;

    assert!(
        rustdar_radar::sites::get_radar_site(SITE).is_none(),
        "precondition: nothing may have placed {SITE}",
    );
    assert_eq!(
        rustdar_radar::eet::radar_height_ft_near(f64::from(LAT), f64::from(LON), Datum::Feedhorn),
        None,
        "precondition: no radar is anywhere near here yet",
    );

    let at = chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(2, 15, 0)
        .unwrap();
    let mut app = headless(TestBridge::desktop());
    let info = app.scan_info_learning_position(&scan_stating(LAT, LON), SITE, at);
    assert_eq!(
        info.site_source,
        rustdar_radar::site_position::SitePositionSource::Volume,
        "precondition: the volume states its own position",
    );

    // 370 m of ground under 20 m of tower, in feet: the figure every beam
    // height in this session is now measured above.
    let want = f64::from(
        rustdar_radar::sites::get_radar_site(SITE)
            .expect("the volume placed it in the live table")
            .height_ft(Datum::Feedhorn)
            .expect("a learned row records both datums"),
    );
    assert_eq!(
        rustdar_radar::eet::radar_height_ft_near(f64::from(LAT), f64::from(LON), Datum::Feedhorn),
        Some(want),
        "the render path anchors on sea level for the radar it is drawing",
    );
    assert!(
        want > 1000.0,
        "and it is a real elevation, not zero: {want} ft"
    );
}

/// With nowhere to write, the app still works and simply forgets.
///
/// This is the degradation `LocationGate` chose for the same reason: no
/// `KvStore` must never mean "refuse", it means "ask again next time".
///
/// # Why not `KTLX`
///
/// A seeded row is no longer immovable: `sites::resolve` applies a learned fix
/// onto the row it lands on, so any sibling test that teaches a position — from
/// a store on construction, or from a volume mid-run — moves that radar in this
/// process's table. `a_position_a_volume_taught_survives_a_restart` does exactly
/// that, to `KTLX`, and running the two in either order made this test fail in
/// release and pass in debug: the worst kind, a test whose outcome is a
/// scheduling accident.
///
/// So this uses `KMBX`, which nothing else in the workspace names. The
/// identifier is load-bearing: it must stay one no other test learns a
/// position for. This test's own middle block teaches it, which is why the last
/// block asserts on the *source* rather than on coordinates — see there.
///
/// It also *places* it, rather than reading a row from
/// [`crate::test_sites`]: the binary carries no radars, so the row this starts
/// from has to come from somewhere, and a shared fixture would be a row other
/// tests could read after this one had moved it.
#[test]
fn a_run_with_no_kv_still_applies_the_volumes_own_position() {
    use rustdar_radar::site_position::SitePositionSource;

    const SITE: &str = "KMBX";
    // Placed here rather than by the shared fixture, so no sibling can move
    // it. Minot, North Dakota, at the position and heights its own volume
    // reports.
    rustdar_radar::sites::resolve([(
        SITE,
        rustdar_radar::sites::SiteFix::Learned(rustdar_radar::site_position::SitePosition {
            lat_udeg: 48_392_500,
            lon_udeg: -100_864_720,
            site_height_m: 455,
            tower_height_m: 30,
        }),
    )]);
    let table = rustdar_radar::sites::get_radar_site(SITE)
        .expect("this test just placed it")
        .clone();
    let stated_lat = (table.lat + 0.25) as f32;
    let at = chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap();

    let mut app = headless(TestBridge::desktop().without_kv());
    let info =
        app.scan_info_learning_position(&scan_stating(stated_lat, table.lon as f32), SITE, at);
    assert_eq!(info.site_source, SitePositionSource::Volume);
    assert!((info.site.lat - f64::from(stated_lat)).abs() < 1e-5);

    // Within the run it is still remembered — the map is in memory and the
    // store is only where it is *written* — so a chunk-fed volume arriving
    // after an archive one is placed correctly even here.
    let same_run = app.scan_info_learning_position(&empty_scan(), SITE, at);
    assert_eq!(same_run.site_source, SitePositionSource::Learned);
    assert_eq!(same_run.site.lat.to_bits(), info.site.lat.to_bits());

    // And nothing outlives the process: a fresh app remembers nothing, so its
    // `ScanInfo` falls back to the table rather than recalling anything.
    //
    // The *source* is the assertion, and it has to be: the site table is
    // process-global and `sites::resolve` never forgets, so the lesson above
    // moved this row for the whole process — deliberately, so that the marker
    // agrees with the data (`a_taught_position_moves_the_maps_marker_and_not_
    // only_the_data`). Comparing coordinates would therefore be comparing the
    // learned value with itself, which passes for the wrong reason. What "with
    // no store, nothing outlived the process" really means is that this app has
    // nothing to recall, and `Table` is exactly the answer that says so —
    // `Learned` is the one it would give if anything had been read back.
    let mut next_run = headless(TestBridge::desktop().without_kv());
    assert!(
        next_run.site_positions.is_empty(),
        "a run with nowhere to read from came up remembering {} positions",
        next_run.site_positions.len(),
    );
    let plain = next_run.scan_info_learning_position(&empty_scan(), SITE, at);
    assert_eq!(
        plain.site_source,
        SitePositionSource::Table,
        "with no store, the volume's own position must not have outlived the \
         process that decoded it — this run recalled one",
    );
}

/// A radar the compiled-in seed has never heard of is in the table by the time
/// `App::new` returns — before any frame exists.
///
/// # Why the timing is the assertion
///
/// The reopen-is-1:1 rule says a pane must look on its second opening exactly
/// as it looked on its first. A site's position or *name* that arrives late
/// breaks that in the most visible way there is: the map gains a marker, the
/// site list gains a row, and a cross-section's height datum moves, under a
/// user who is already looking at them. So the table is resolved beside the
/// config load, in `App::new`, and again in `set_config_dir` where Android
/// finally has a store — both of them ahead of the event loop.
///
/// The test therefore draws **no frames at all**. Constructing the app is the
/// entire act under test, and an assertion that only held after a warm-up
/// would be asserting the opposite of what this is for.
///
/// # Why it can run beside everything else
///
/// `sites::resolve` never *forgets* a radar, so no other test's app
/// construction can take `ZZZF` away again once this one has put it there. It
/// can move one — a fix now displaces the row it lands on — but only a row it
/// names, and `ZZZF` is named by nothing else. The identifier being unique to
/// this test is what makes both halves true, and it is load-bearing rather
/// than tidy; `ZZZF` also sits 5000 km from the nearest real radar, so no
/// nearest-search elsewhere can answer with it.
#[test]
fn a_learned_radar_the_seed_never_had_is_in_the_table_before_the_first_frame() {
    use rustdar_kv::KvStore;

    const SITE: &str = "ZZZF";

    let store = std::rc::Rc::new(MemoryKvStore::default());
    // A previous session learned this radar from its own volume. The blob is
    // written in the shape `SitePositions` persists, because going through the
    // real load is the point: a hand-installed table would not show that the
    // cache reaches the table at all.
    let learned = serde_json::to_string(&std::collections::BTreeMap::from([(
        SITE.to_owned(),
        rustdar_radar::site_position::SitePosition {
            lat_udeg: -34_000_000,
            lon_udeg: -144_000_000,
            site_height_m: 100,
            tower_height_m: 20,
        },
    )]))
    .expect("a SitePosition serializes");
    store
        .store(crate::site_positions::SITE_POSITIONS_KEY, &learned)
        .expect("the double's store cannot fail");

    assert!(
        rustdar_radar::sites::get_radar_site(SITE).is_none(),
        "precondition: {SITE} must not be a seed row, or this proves nothing",
    );

    let _app = headless(TestBridge::desktop().with_store(std::rc::Rc::clone(&store)));

    let row = rustdar_radar::sites::get_radar_site(SITE).unwrap_or_else(|| {
        panic!(
            "constructing the app must resolve the table: {SITE} was learned \
             in an earlier session and is still unknown",
        )
    });
    assert_eq!(row.name, SITE, "it carries its own ICAO, not UNKNOWN");
    assert_eq!((row.lat, row.lon), (-34.0, -144.0));
    assert!(
        rustdar_radar::sites::radars()
            .iter()
            .any(|r| r.name == SITE),
        "and the walk the map and the site list both do reaches it",
    );
}

/// Android's second resolution is the one that has anything to resolve.
///
/// `App::new` runs before the bridge will hand out a store, so it resolves
/// with nothing; `set_config_dir` is the first moment a returning user's
/// learned radars are readable at all. This pins that the table is resolved
/// *there* too, and it is the reason `sites::resolve` extends the table in
/// hand rather than rebuilding from the seed: had it been a `OnceLock`, the
/// empty first attempt would have won and the one platform that needs the
/// second call would be the one platform where a learned radar never arrived.
///
/// Still before the first frame — `set_config_dir` runs inside `android_main`,
/// ahead of the event loop.
#[test]
fn android_resolves_the_table_when_the_config_directory_arrives() {
    use rustdar_kv::KvStore;

    const SITE: &str = "ZZZG";

    let bridge = TestBridge::android();
    let learned = serde_json::to_string(&std::collections::BTreeMap::from([(
        SITE.to_owned(),
        rustdar_radar::site_position::SitePosition {
            lat_udeg: -35_000_000,
            lon_udeg: -145_000_000,
            site_height_m: 100,
            tower_height_m: 20,
        },
    )]))
    .expect("a SitePosition serializes");
    bridge
        .store()
        .store(crate::site_positions::SITE_POSITIONS_KEY, &learned)
        .expect("the double's store cannot fail");

    let mut app = headless(bridge);
    assert!(
        rustdar_radar::sites::get_radar_site(SITE).is_none(),
        "precondition: there was nowhere to read {SITE} from yet",
    );

    app.set_config_dir(std::path::PathBuf::from("/data/user/0/rustdar"));

    let row = rustdar_radar::sites::get_radar_site(SITE).unwrap_or_else(|| {
        panic!(
            "the config directory arrived and the table was not resolved from \
             it: {SITE} is still unknown",
        )
    });
    assert_eq!((row.lat, row.lon), (-35.0, -145.0));
}
