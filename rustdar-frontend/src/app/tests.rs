use super::*;
use crate::platform_double::TestBridge;
use rustdar_egui::config_store::MemoryConfigStore;
use rustdar_egui::overlay_cache::OverlayTexturePlan;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_overlays::types::GeoBounds;
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
    include_str!("../../../rustdar-android/android/app/src/main/java/com/rustdar/BackHandler.java");

/// The Rust half. `rustdar-android` is `#![cfg(target_os = "android")]`, so
/// it compiles to nothing on a host and can hold no test of its own; this
/// crate owns the funnel both halves are about, so the pins live here.
const ANDROID_ENTRY: &str = include_str!("../../../rustdar-android/src/lib.rs");

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
        ANDROID_ENTRY.contains("fn Java_com_rustdar_BackHandler_nativeBackPressed("),
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
fn seed_config(store: &MemoryConfigStore, fps: f32) {
    let mut gui = Gui::new();
    gui.loop_speed_fps = fps;
    gui.save_ui_config(store);
}

/// What a bridge's store holds, read back through the same parser the app
/// loads with.
fn stored_fps(store: &MemoryConfigStore) -> f32 {
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
        .send(rustdar_gps::GpsFix::from_lat_lon(46.7867, -92.1005))
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
        .send(rustdar_gps::GpsFix::from_lat_lon(46.7867, -92.1005))
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
        .send(rustdar_gps::GpsFix::from_lat_lon(32.7767, -96.7970))
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
        .send(rustdar_gps::GpsFix::from_lat_lon(46.7867, -92.1005))
        .unwrap();
    app.poll_platform_state();
    assert_eq!(opening_site(&app), "KDLH");

    // The same user, now in Denver.
    fixes
        .send(rustdar_gps::GpsFix::from_lat_lon(39.7392, -104.9903))
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
        .send(rustdar_gps::GpsFix {
            // What the location portal measured on the developer's own machine: an
            // IP/ichnaea lookup, and comfortably good enough to choose
            // among sites 200 km apart.
            accuracy_m: Some(25_000.0),
            ..rustdar_gps::GpsFix::from_device_position(46.7867, -92.1005)
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

/// The shape `rustdar-android` now produces from the network provider, end
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
        .send(rustdar_gps::GpsFix {
            accuracy_m: Some(32.0),
            ..rustdar_gps::GpsFix::from_device_position(46.7867, -92.1005)
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
        rustdar_gps::FixQuality::Simulation,
        rustdar_gps::FixQuality::Manual,
        rustdar_gps::FixQuality::None,
    ] {
        let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
        let fixes = bridge.gps_channel();
        let mut app = headless(bridge);

        fixes
            .send(rustdar_gps::GpsFix {
                fix_quality: quality,
                ..rustdar_gps::GpsFix::from_lat_lon(46.7867, -92.1005)
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
        .send(rustdar_gps::GpsFix {
            accuracy_m: Some(MAX_RELOCATION_ACCURACY_M * 2.0),
            ..rustdar_gps::GpsFix::from_device_position(46.7867, -92.1005)
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
        .send(rustdar_gps::GpsFix {
            accuracy_m: Some(25_000.0),
            ..rustdar_gps::GpsFix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();
    assert_eq!(opening_site(&app), "KDLH");
}

/// The measured portal number, pinned. It is an order of magnitude coarser
/// than a satellite fix and an order of magnitude better than it needs to
/// be: displacing a sample point by 25 km changed the chosen site in 5.5%
/// of probes. A threshold that rejected it would switch off the largest
/// single improvement this feature has.
#[test]
fn the_accuracy_gate_admits_a_coarse_but_usable_fix() {
    assert!(fix_is_accurate_enough_to_relocate(Some(25_000.0)));
    assert!(
        fix_is_accurate_enough_to_relocate(None),
        "the serial path reports no accuracy at all and has always been \
             trusted"
    );
    assert!(!fix_is_accurate_enough_to_relocate(Some(1_000_000.0)));
    assert!(
        !fix_is_accurate_enough_to_relocate(Some(f64::NAN)),
        "a NaN accuracy compares false against everything, so it has to be \
             rejected explicitly or it slips through as 'good enough'"
    );
}

// ── The location permission gate, from the App's side ───────────────
//
// `location_permission.rs` owns the state machine and tests it against a
// clock it controls. What belongs here is the wiring: that the gate is
// stepped at all, that what it observes reaches the UI, and that a
// revocation takes the dot with it.

/// The gate is stepped from `poll_platform_state`, and what it sees is
/// pushed to the `Gui` — which is the only copy the settings pane can read,
/// since `rustdar-egui` cannot see a `PlatformBridge`.
#[test]
fn what_the_platform_says_about_location_reaches_the_settings_pane() {
    let bridge = TestBridge::desktop().with_permission(rustdar_gps::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    assert_eq!(
        app.gui.location_permission(),
        rustdar_gps::LocationPermission::Unknown,
        "the cache starts inert, before anything has been polled"
    );

    app.poll_platform_state();

    assert_eq!(
        app.gui.location_permission(),
        rustdar_gps::LocationPermission::Granted
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
    let bridge = TestBridge::desktop().with_permission(rustdar_gps::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    app.gui
        .set_gps_fix(rustdar_gps::GpsFix::from_device_position(35.25, -97.5));
    assert!(app.gui.gps_fix().is_some());

    // Revoked in system settings, with no process restart — which is what
    // happens on every desktop OS.
    location
        .permission
        .set(rustdar_gps::LocationPermission::Denied);
    app.location.resumed();
    app.poll_platform_state();

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
    let bridge = TestBridge::desktop().with_permission(rustdar_gps::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    app.platform.start_gps(&rustdar_gps::GpsConfig::default());
    app.gui
        .set_gps_fix(rustdar_gps::GpsFix::from_lat_lon(35.25, -97.5));

    location
        .permission
        .set(rustdar_gps::LocationPermission::Denied);
    app.location.resumed();
    app.poll_platform_state();

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
    let bridge = TestBridge::android().with_permission(rustdar_gps::LocationPermission::Prompt);
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
    let bridge = TestBridge::desktop().with_permission(rustdar_gps::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    app.gui
        .set_gps_fix(rustdar_gps::GpsFix::from_device_position(35.25, -97.5));
    assert!(location.active.get());

    app.handle_gui_action(GuiAction::StopLocation, None);

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
    app.autosave_control_flow()
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
/// and `autosave_control_flow` themselves, which says nothing about
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
        body.contains("self.schedule_autosave_wakeup("),
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
        matches!(app.autosave_control_flow(), ControlFlow::WaitUntil(_)),
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
        app.autosave_control_flow(),
        ControlFlow::Wait,
        "an idle app is being woken on a timer for a change nobody made"
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
        .send(rustdar_gps::GpsFix::from_lat_lon(35.3331, -97.2778))
        .unwrap();
    heading_tx.send(214.5).unwrap();

    app.handle_redraw();

    let fix = app.gui.gps_fix().expect("no position reached the UI");
    assert_eq!((fix.latitude, fix.longitude), (35.3331, -97.2778));
    assert_eq!(
        app.gui.user_heading(),
        Some(214.5),
        "no compass reading reached the UI — note the fix carries no \
             heading of its own, so this cannot have come from it",
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
    use rustdar_egui::config_store::{ConfigStore, UI_CONFIG_KEY};

    let mut app = headless(TestBridge::desktop());
    let store = MemoryConfigStore::default();
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
    app.gui.set_scan_info_for_pane(0, scan_info_for("KTLX"));
    for site in ["KTLX", "KOUN"] {
        app.scan_data
            .insert(site.to_string(), Arc::new(empty_scan()));
        app.base_scans.insert(
            site.to_string(),
            (Arc::new(empty_scan()), scan_info_for(site).timestamp),
        );
        app.latest_cached_scans.insert(
            site.to_string(),
            (
                Arc::new(empty_scan()),
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

/// The window a site switch opens.
///
/// `SwitchRadarSite` moves `pane.site` immediately, but the pane goes on
/// drawing the old radar until the new volume lands — and
/// `dispatch_pane_renders` looks that volume up under `scan_info.site.name`,
/// not under `pane.site`. An eviction keyed on the live site alone therefore
/// pulls the scan out from under a pane still rendering from it, and the
/// symptom is a product change that silently does nothing until the switch
/// completes.
///
/// `base_scans` rides the same `shown` set for the same window: a 3D pane
/// mid-switch is still building from the old site's base volume, and an
/// eviction keyed on the live site alone would free it under the resampler.
#[test]
fn the_volume_a_switching_pane_is_still_drawing_survives() {
    let mut app = headless(TestBridge::desktop());
    app.gui.set_scan_info_for_pane(0, scan_info_for("KTLX"));
    app.gui.pane_mut(0).unwrap().site = "KOUN".to_string();
    app.scan_data
        .insert("KTLX".to_string(), Arc::new(empty_scan()));
    app.base_scans.insert(
        "KTLX".to_string(),
        (Arc::new(empty_scan()), scan_info_for("KTLX").timestamp),
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
    assert_eq!(
        app.gui.safe_area_insets(),
        (96.0, 0.0, 0.0, 0.0),
        "precondition: portrait puts the cutout along the top",
    );

    ROTATED.store(true, Ordering::Relaxed);
    app.handle_resized(2400, 1080);

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
            config: rustdar_gps::GpsConfig {
                port_path: Some("/dev/ttyPROBE".to_string()),
                baud_rate: 38400,
                ..Default::default()
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

// ── The floor retry ledger ──────────────────────────────────────────
//
// Both floor-dispatch failure paths used to lean on "the next completed
// voxel build for this scope tries again" — and a static archive volume
// completes no further builds, so either failure left the pane standing
// on a permanently missing floor.

/// A tiny real reflectivity scan — the same fixture shape as
/// `volume::bridge`'s test `ready_grid`, duplicated here because that one
/// lives in another module's `#[cfg(test)]` and cannot be imported. It
/// exists so `extract_current_volume` and the floor render's own
/// extraction have real sweeps to work from.
fn reflectivity_scan() -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };
    let sweep = |number: u8, elevation: f32| {
        let radials = (0..8u16)
            .map(|i| {
                Radial::new(
                    1_760_000_000_000 + i64::from(i),
                    i + 1,
                    f32::from(i) * 45.0,
                    45.0,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(MomentData::from_fixed_point(
                        4,
                        2125,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![120, 140, 160, 180],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    };
    let cut = |angle: f64| {
        nexrad_model::data::ElevationCut::new(
            angle,
            nexrad_model::data::ChannelConfiguration::ConstantPhase,
            nexrad_model::data::WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    };
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
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5), sweep(2, 1.5)],
    )
}

fn floor_stamp() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 5, 6)
        .unwrap()
        .and_hms_opt(22, 0, 0)
        .unwrap()
}

/// A floor reply with no image reopens the scope's dedupe entry and owes
/// the scope a retry; a reply **with** an image does neither.
///
/// The first half is the F4b fix for the `None` path: `floor_rendered`
/// records "dispatched", and keeping it after a dispatch that delivered
/// nothing meant `maybe_spawn_floor_render` refused every retry at that
/// stamp forever. Shown failing pre-fix (the reply was dropped with a
/// bare `continue`): the dedupe entry survived and nothing was owed.
#[test]
fn a_floor_reply_with_no_image_reopens_the_dedupe_and_owes_a_retry() {
    let mut app = headless(TestBridge::desktop());
    let scope = ("KTLX".to_string(), None);
    app.floor_rendered.push((scope.clone(), floor_stamp(), 0));

    app.channels
        .floor_sender
        .send(crate::channels::FloorResponse {
            site: "KTLX".to_string(),
            region: None,
            image: None,
        })
        .unwrap();
    app.poll_floor_results();
    assert!(
        !app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "a dispatch that delivered nothing must not stay recorded as \
             done — that is a floor missing for the volume's whole life",
    );
    assert_eq!(
        app.floor_owed,
        vec![scope.clone()],
        "the scope must be owed a retry the frame path can act on",
    );

    // The control: a delivered floor must keep its dedupe entry — one
    // render per (scope, stamp) is the whole point of the ledger.
    app.floor_owed.clear();
    app.floor_rendered.push((scope.clone(), floor_stamp(), 0));
    app.channels
        .floor_sender
        .send(crate::channels::FloorResponse {
            site: "KTLX".to_string(),
            region: None,
            image: Some(crate::volume::floor::FloorImage {
                size: [1, 1],
                rgba: vec![0, 0, 0, 255],
            }),
        })
        .unwrap();
    app.poll_floor_results();
    assert!(
        app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "a delivered floor must not reopen the dedupe",
    );
    assert!(
        app.floor_owed.is_empty(),
        "a delivered floor leaves nothing owed",
    );
}

/// A floor dispatch the render budget refused is owed, and the owed
/// retry dispatches it later **with no voxel build in between** — the
/// static-archive case, where no build ever completes again.
///
/// Shown failing pre-fix: the refusal recorded nothing anywhere, so
/// after the budget freed there was no path back to the dispatch short
/// of another completed build.
#[test]
fn a_budget_refused_floor_render_is_retried_without_a_new_build() {
    use crate::volume::bridge::VolumeEntry;
    use std::sync::atomic::Ordering;

    let mut app = headless(TestBridge::desktop());
    let stamp = floor_stamp();
    let scan = reflectivity_scan();
    let target = rustdar_egui::pane::VolumeTarget {
        region: None,
        volume: rustdar_egui::pane::VolumeStamp {
            site: "KTLX".to_string(),
            collected: stamp,
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
    };
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (35.33, -97.27),
        half_width_km: 40.0,
        base_km_msl: 0.0,
        top_km_msl: 10.0,
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::WASM_SHAPE,
        values_wanted: false,
    };
    let grid = Arc::new(
        rustdar_radar::voxel::build_voxels(&scan, &request, 35.33, -97.27)
            .expect("the fixture volume resamples"),
    );
    app.base_scans
        .insert("KTLX".to_string(), (Arc::new(scan), stamp));
    assert!(!app.volume_store.share(0, &target));
    app.volume_store.begin_build(0, &target);
    assert!(
        app.volume_store
            .complete(&target, VolumeEntry::Ready(Arc::clone(&grid)))
    );

    // The budget is full at dispatch time.
    app.render
        .renders_in_flight
        .store(crate::constants::MAX_CONCURRENT_RENDERS, Ordering::Relaxed);
    app.maybe_spawn_floor_render(&target, &grid);
    let scope = ("KTLX".to_string(), None);
    assert!(
        app.floor_rendered.is_empty(),
        "a refused dispatch must not be recorded as done",
    );
    assert_eq!(
        app.floor_owed,
        vec![scope.clone()],
        "the refused scope must be owed",
    );

    // A slot frees. No build completes. The frame-path retry alone must
    // reach the dispatch.
    app.render.renders_in_flight.store(0, Ordering::Relaxed);
    app.retry_owed_floor_renders();
    assert!(
        app.floor_rendered
            .iter()
            .any(|(s, at, _)| *s == scope && *at == stamp),
        "the owed retry must dispatch the floor render for the scope's \
             held grid without waiting for a build that will never come",
    );
    assert!(
        app.floor_owed.is_empty(),
        "a dispatched retry settles the debt",
    );
}

/// The floor's picture inputs changing after it was composited reopen
/// the scope — tiles landing, and the **warning set** moving — while a
/// floor whose signature still matches is left alone. One signature,
/// one drain: the warning choreography below runs through exactly the
/// mechanism the tile half does.
///
/// The floor renders once per sealed sweep, but its basemap and label
/// tiles download on their own schedule and warnings issue and expire
/// on the NWS's — without this check, whatever was in hand at the first
/// composite would be the box's ground for the volume's whole life.
/// Headless, `gather_floor_tiles` has no tiles, so its signature moves
/// only with the overlay content — which is what the second half
/// drives, through the production ingest path
/// (`OverlayRegistry::apply_fetch_result`).
#[test]
fn a_floor_is_recomposed_when_its_tiles_change_and_left_alone_when_not() {
    use crate::volume::bridge::VolumeEntry;

    let mut app = headless(TestBridge::desktop());
    let stamp = floor_stamp();
    let scan = reflectivity_scan();
    let target = rustdar_egui::pane::VolumeTarget {
        region: None,
        volume: rustdar_egui::pane::VolumeStamp {
            site: "KTLX".to_string(),
            collected: stamp,
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
    };
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (35.33, -97.27),
        half_width_km: 40.0,
        base_km_msl: 0.0,
        top_km_msl: 10.0,
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::WASM_SHAPE,
        values_wanted: false,
    };
    let grid = Arc::new(
        rustdar_radar::voxel::build_voxels(&scan, &request, 35.33, -97.27)
            .expect("the fixture volume resamples"),
    );
    app.base_scans
        .insert("KTLX".to_string(), (Arc::new(scan), stamp));
    assert!(!app.volume_store.share(0, &target));
    app.volume_store.begin_build(0, &target);
    assert!(
        app.volume_store
            .complete(&target, VolumeEntry::Ready(Arc::clone(&grid)))
    );
    let scope = ("KTLX".to_string(), None);

    // A floor composited while some tile set was in hand (signature 42),
    // and the tiles have since changed (headless now reports 0). The
    // stale floor is IN HAND — that is the bug's precondition: the drain
    // used to read the store's `Some` as "nothing to do", so the reopened
    // debt was dropped on the very next line of the frame, every frame,
    // and a static archive volume kept its tile-less floor forever.
    app.volume_store.set_floor(
        "KTLX",
        None,
        Arc::new(crate::volume::floor::FloorImage {
            size: [1, 1],
            rgba: vec![0, 0, 0, 255],
        }),
    );
    app.floor_rendered.push((scope.clone(), stamp, 42));
    app.recompose_floors_for_new_tiles();
    assert!(
        !app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "a changed tile signature must reopen the scope's dedupe entry",
    );
    assert_eq!(
        app.floor_owed,
        vec![scope.clone()],
        "the reopened scope must be owed a re-composite",
    );

    // Through the drain to the actual dispatch: with a render slot free,
    // the STALE floor in hand must not satisfy the retry — the whole
    // debt exists because that floor is the wrong picture.
    app.retry_owed_floor_renders();
    assert!(
        app.floor_rendered
            .iter()
            .any(|(s, at, _)| *s == scope && *at == stamp),
        "the reopened scope must re-dispatch through the drain — a stale \
             floor in hand satisfying the floor-in-hand guard is the leak \
             that starved static volumes of their tiles",
    );
    assert!(
        app.floor_owed.is_empty(),
        "a dispatched re-composite settles the debt",
    );

    // The signature matches what a composite now would use: untouched.
    let site = rustdar_radar::sites::get_radar_site("KTLX").expect("a known site");
    let current_sig = |app: &mut App| {
        let (_, _, sig) =
            app.gather_floor_tiles(site.lat, site.lon, grid.x_range_km(), grid.y_range_km());
        sig
    };
    app.floor_owed.clear();
    app.floor_rendered.clear();
    let quiet_sig = current_sig(&mut app);
    app.floor_rendered.push((scope.clone(), stamp, quiet_sig));
    app.recompose_floors_for_new_tiles();
    assert!(
        app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "an unchanged signature must leave the floor in hand",
    );
    assert!(
        app.floor_owed.is_empty(),
        "an unchanged signature owes nothing",
    );

    // ── The warning set is part of the same signature ────────────────
    // A tornado warning issues mid-session. No new tiles, no new build:
    // the same recompose + drain must reopen the scope and re-dispatch.
    let warning = |id: &str| {
        use rustdar_overlays::nws::alert::{AlertCategory, NwsAlert};
        use rustdar_overlays::types::{HatchPattern, OverlayFeature};
        NwsAlert {
            id: id.to_string(),
            event: "Tornado Warning".to_string(),
            category: AlertCategory::Warning,
            severity: "Extreme".parse().unwrap(),
            urgency: "Immediate".parse().unwrap(),
            certainty: "Observed".parse().unwrap(),
            headline: None,
            description: String::new(),
            instruction: None,
            area_desc: String::new(),
            sender_name: String::new(),
            effective: String::new(),
            expires: String::new(),
            onset: None,
            ends: None,
            affected_zones: Vec::new(),
            features: vec![OverlayFeature::new(
                vec![vec![vec![(35.5, -97.5), (35.9, -97.5), (35.9, -97.0)]]],
                [255, 0, 0, 80],
                [255, 0, 0, 255],
                "Tornado Warning".to_string(),
                String::new(),
                HatchPattern::None,
            )],
        }
    };
    let apply = |app: &mut App, alerts| {
        use rustdar_overlays::render::overlay_state::{
            OverlayFetchResult, OverlayKind, OverlayRegistry,
        };
        app.gui.overlays.apply_fetch_result(OverlayFetchResult {
            kind: OverlayKind::NwsAlerts,
            data: OverlayRegistry::nws_alerts_payload(alerts),
        });
    };
    apply(&mut app, vec![warning("w1")]);
    app.recompose_floors_for_new_tiles();
    assert!(
        !app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "a warning issuing must reopen the scope's dedupe entry — the \
             floor in hand shows a storm with no warning on it",
    );
    assert_eq!(
        app.floor_owed,
        vec![scope.clone()],
        "the reopened scope must be owed a re-composite",
    );
    app.retry_owed_floor_renders();
    assert!(
        app.floor_rendered
            .iter()
            .any(|(s, at, _)| *s == scope && *at == stamp),
        "the warning-driven reopen must re-dispatch through the same \
             drain the tile reopen uses",
    );
    assert!(
        app.floor_owed.is_empty(),
        "the re-dispatch settles the debt"
    );

    // The gathered vectors carry the warning to the composite, over the
    // radar where the pane stacks alerts.
    let vectors = app.gather_floor_vectors();
    assert_eq!(
        vectors.over_radar.len(),
        1,
        "the issued warning must reach the floor's over-radar layer",
    );
    assert!(
        vectors.range_ring && vectors.under_radar.is_empty(),
        "the ring always rides; no outlook data means no under-radar shapes",
    );

    // The next poll returns the SAME warning set: the signature names
    // the set, not the fetch, so nothing reopens — a floor recomposed
    // every two-minute poll is a floor recomposed for nothing. (This is
    // where a signature built on `data_generation` dies.)
    apply(&mut app, vec![warning("w1")]);
    app.recompose_floors_for_new_tiles();
    assert!(
        app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "a poll returning the same warning set must not reopen the scope",
    );
    assert!(
        app.floor_owed.is_empty(),
        "a poll returning the same warning set owes nothing",
    );

    // The warning expires out of the feed: reopened again.
    apply(&mut app, Vec::new());
    app.recompose_floors_for_new_tiles();
    assert!(
        !app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "a warning expiring must reopen the scope — the floor in hand \
             still shows it",
    );

    // ── The Mesoscale Discussion layer rides the same signature ──────
    // Settle the expiry debt, then an MD issues over the site: the same
    // reopen through the same drain, through the production ingest.
    let discussion = |number: u32| {
        use rustdar_overlays::spc::colors::{md_fill_color, md_stroke_color};
        use rustdar_overlays::spc::discussion::{MdType, SpcDiscussion};
        use rustdar_overlays::types::{HatchPattern, OverlayFeature};
        let md_type = MdType::Convective;
        let polygon = vec![vec![(35.0, -97.6), (35.8, -97.6), (35.8, -96.9)]];
        SpcDiscussion {
            number,
            title: format!("Mesoscale Discussion #{number:04}"),
            text: String::new(),
            link: String::new(),
            md_type,
            polygon: polygon.clone(),
            feature: OverlayFeature::new(
                vec![polygon],
                md_fill_color(&md_type),
                md_stroke_color(&md_type),
                format!("MD {number}"),
                String::new(),
                HatchPattern::None,
            ),
            concerning: None,
        }
    };
    let apply_md = |app: &mut App, mds| {
        use rustdar_overlays::render::overlay_state::{
            OverlayFetchResult, OverlayKind, OverlayRegistry,
        };
        app.gui.overlays.apply_fetch_result(OverlayFetchResult {
            kind: OverlayKind::SpcDiscussions,
            data: OverlayRegistry::spc_discussions_payload(mds),
        });
    };
    app.retry_owed_floor_renders();
    apply_md(&mut app, vec![discussion(101)]);
    app.recompose_floors_for_new_tiles();
    assert!(
        !app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "an MD issuing must reopen the scope — the floor in hand shows \
             no discussion polygon (this is where a signature that does not \
             fold `SpcDiscussions` dies)",
    );
    app.retry_owed_floor_renders();

    // The next poll returns the SAME MD set: nothing reopens. (This is
    // where the MD handler's `data_generation` as its signature dies.)
    apply_md(&mut app, vec![discussion(101)]);
    app.recompose_floors_for_new_tiles();
    assert!(
        app.floor_rendered.iter().any(|(s, _, _)| *s == scope),
        "a poll returning the same MD set must not reopen the scope",
    );

    // The planted MD reaches the gather at its slot in the pane's draw
    // order (`OverlayKind::all`): over the radar, under the alerts —
    // drawn first, so the alert blends over it. (This is where omitting
    // the MD layer from the gather dies.)
    apply(&mut app, vec![warning("w1")]);
    let vectors = app.gather_floor_vectors();
    assert_eq!(
        vectors.over_radar.len(),
        2,
        "the MD and the warning must both reach the over-radar layer",
    );
    assert_eq!(
        vectors.over_radar[0].fill_rgba,
        [255, 180, 50, 60],
        "the MD (convective fill) draws first — over the radar, under \
             the alerts, the pane's slot for SpcDiscussions",
    );
    assert_eq!(
        vectors.over_radar[1].fill_rgba,
        [255, 0, 0, 80],
        "the warning's fill blends over the MD's, as the pane draws them",
    );

    // The MD checkbox is global handler state, which the floor follows:
    // off, the layer gathers nothing.
    {
        use rustdar_overlays::render::overlay_state::OverlayKind;
        app.gui
            .overlays
            .set_enabled(OverlayKind::SpcDiscussions, false);
    }
    assert_eq!(
        app.gather_floor_vectors().over_radar.len(),
        1,
        "a disabled MD handler must put nothing on the floor",
    );
}
