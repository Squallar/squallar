use super::*;
use crate::platform_double::TestBridge;
use squallar_egui::overlay_cache::OverlayTexturePlan;
use squallar_geo::GeoBounds;
use squallar_kv::MemoryKvStore;
use squallar_source::id::{LayerId, known};
use std::sync::atomic::{AtomicBool, Ordering};

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -100.0,
        max_lon: -90.0,
    }
}

/// The browser build asks for both browser APIs, and for no native one.
///
/// Asking is all this function does. Which of the two the build ends up on is
/// `create_instance`'s answer and cannot be reached from a host test — it turns
/// on a `requestAdapter()` the browser has to run.
#[test]
fn the_browser_build_asks_for_webgpu_and_webgl2_and_no_native_backend() {
    // A base that is deliberately *neither*, so "the browser arm restricts to the two
    // browser backends" cannot be satisfied by the base already being them.
    let base = |backends| wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    };

    let browser = wgpu::Backends::BROWSER_WEBGPU.union(wgpu::Backends::GL);

    for offered in [
        wgpu::Backends::all(),
        wgpu::Backends::VULKAN,
        wgpu::Backends::BROWSER_WEBGPU,
        wgpu::Backends::VULKAN.union(wgpu::Backends::BROWSER_WEBGPU),
        wgpu::Backends::empty(),
    ] {
        let web = backends_for(true, base(offered)).backends;
        assert_eq!(
            web, browser,
            "offered {offered:?}, the browser build asked for {web:?} rather \
                 than WebGPU and WebGL2 together"
        );
        // Both halves are load-bearing and neither is the other's spare: without
        // `BROWSER_WEBGPU` a blocklisted-driver Chromium is stuck on SwiftShader,
        // and without `GL` Firefox/Linux — which governs here — has no renderer
        // at all until Mozilla ships WebGPU there.
        assert!(web.contains(wgpu::Backends::BROWSER_WEBGPU));
        assert!(web.contains(wgpu::Backends::GL));
        // And nothing that could never run in a browser rode along.
        assert!(
            web.difference(browser).is_empty(),
            "web mask carries {web:?}"
        );

        // Native is deliberately unrestricted: it passes the base through untouched, which
        // is what keeps `WGPU_BACKEND` working.
        let native = backends_for(false, base(offered)).backends;
        assert_eq!(native, offered, "the native arm altered the base");
    }

    // And the shipped path really does read the environment, which is the claim the
    // parameter moved out of `backends_for` and into its caller.
    assert_eq!(
        backends_for(
            false,
            wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        )
        .backends,
        wgpu::Backends::all().with_env()
    );
}

/// The browser's two APIs are chosen between by the DETECTING constructor, not
/// by `Instance::new`.
///
/// Scraped, because the difference is invisible to a host `cargo test` and
/// silent in a browser: `Instance::new` commits to WebGPU on the mere presence
/// of `navigator.gpu`, so a build that called it would bind the canvas to a
/// context whose `requestAdapter()` may then return null — and a canvas bound
/// once cannot be rebound. The fallback would be gone with nothing red.
#[test]
fn the_instance_is_built_by_the_detecting_constructor() {
    let source = include_str!("../app.rs");
    let (code, _) = source
        .split_once("#[cfg(test)]")
        .expect("app.rs no longer has a test module");

    let n = code.matches("new_instance_with_webgpu_detection(").count();
    assert_eq!(
        n, 1,
        "expected exactly one `new_instance_with_webgpu_detection(` in app.rs, \
         found {n}. It is the only constructor that decides between the two \
         browser APIs by asking for an adapter rather than by looking for \
         `navigator.gpu`."
    );
    assert!(
        !code.contains("wgpu::Instance::new("),
        "app.rs builds a wgpu instance with `Instance::new`. On wasm32 that \
         binds the WebGPU context whenever `navigator.gpu` exists, including on \
         the browsers where `requestAdapter()` then answers null — and the \
         surface it creates afterwards is what makes the choice permanent."
    );
}

/// And that this build asks on its own behalf.
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

    // The fork is reached, and reached with the *environment* as its base — the half
    // `backends_for` no longer reads for itself.
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

/// A request as `process_gui_actions` builds one: unexpanded viewport bounds plus a texture
/// plan.
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

fn entry(pane: usize, id: LayerId) -> (usize, LayerId, fetch::OverlayRenderRequest) {
    (pane, id, req(800, 600, 1.0, 1, 10))
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
    let result = deduplicate_overlay_renders(vec![entry(0, known::RADAR)], true);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, vec![0]);
    assert_eq!(result[0].1, known::RADAR);
    assert_eq!(result[0].2.texture.width, 800);

    let result = deduplicate_overlay_renders(vec![entry(0, known::RADAR)], false);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, vec![0]);
}

#[test]
fn test_dedup_no_grouping() {
    let input = vec![
        entry(0, known::RADAR),
        entry(1, known::RADAR),
        entry(2, known::NWS_ALERTS),
    ];

    let result = deduplicate_overlay_renders(input, false);
    assert_eq!(result.len(), 3);
    for e in &result {
        assert_eq!(e.0.len(), 1);
    }
}

#[test]
fn test_dedup_groups_same_key() {
    let input = vec![entry(0, known::RADAR), entry(1, known::RADAR)];

    let result = deduplicate_overlay_renders(input, true);
    assert_eq!(result.len(), 1);
    let mut panes = result[0].0.clone();
    panes.sort();
    assert_eq!(panes, vec![0, 1]);
    assert_eq!(result[0].1, known::RADAR);
}

#[test]
fn test_dedup_different_keys() {
    let input = vec![entry(0, known::RADAR), entry(1, known::NWS_ALERTS)];

    let result = deduplicate_overlay_renders(input, true);
    assert_eq!(result.len(), 2);
}

#[test]
fn test_dedup_duplicate_pane_idx() {
    let input = vec![entry(0, known::RADAR), entry(0, known::RADAR)];

    let result = deduplicate_overlay_renders(input, true);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, vec![0]);
}

/// Panes of different sizes must not share one render: the survivor's plan would be applied
/// to a pane it was not sized for.
#[test]
fn test_dedup_keeps_differently_sized_panes_apart() {
    let input = vec![
        (0, known::RADAR, req(2048, 600, 0.28, 1, 10)),
        (1, known::RADAR, req(2400, 600, 1.0, 1, 10)),
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

/// A bridge that consumes every back press, as Android's does: it installs a handler at
/// startup and `handle_back` reports `true` from then on.
fn minimising_bridge() -> TestBridge {
    let mut bridge = TestBridge::android();
    // Deliberately not `record_back_press`: that one's flag belongs to
    // `the_injected_callbacks_reach_the_bridge` alone.
    bridge.set_back_handler(|| {});
    bridge
}

/// Back with something open closes it; only a second press, with nothing open, minimises.
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

/// The two tests above exercise the decision; nothing can exercise the call that reaches
/// it, because `handle_input_events` takes an `ActiveEventLoop` and winit will not hand one
/// out except from inside a running loop.
fn fn_body(name: &str) -> &'static str {
    let (_, rest) = include_str!("../app.rs")
        .split_once(name)
        .unwrap_or_else(|| panic!("{name} is no longer a method here"));
    rest.split_once("\n    }")
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("{name} has no recognisable body"))
}

/// The block of the `match` arm `pattern` opens, brace-matched.
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
// `OnBackInvokedDispatcher` does not go through the input queue, so none of the pins above
// see it. It is also not a route this app is reachable through today -- the manifest does not
// opt in, on a measurement recorded there -- so everything below pins the SOURCE of a route
// kept correct and dormant, not a behaviour a device would exhibit.

/// The Kotlin half of the route, so a rename on either side is a build failure rather than an
/// `UnsatisfiedLinkError` on a device.
const BACK_HANDLER_KT: &str =
    include_str!("../../../packaging/android/app/src/main/kotlin/app/squallar/BackHandler.kt");

/// The Rust half: the one module file that CONTAINS the exported JNI symbol
/// (`squallar/src/android/back.rs`) -- NOT the crate root, which since the android fold
/// holds only module mounts and would silently pin wrong text.
const ANDROID_BACK: &str = include_str!("../../../squallar/src/android/back.rs");

/// `src` with its comments removed. Kotlin's `//` and `/* */` are Java's, so the
/// same stripper serves both; KDoc is a `/** */`, which `/*` already opens.
fn jvm_code(src: &str) -> String {
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
            // A lone '/' opens nothing.
            out.push('/');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// A press delivered outside the input queue has to reach the same funnel, and only when
/// there *is* one.
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
/// `@JvmStatic` is checked here because it is load-bearing twice and invisible
/// both times: without it an `object`'s members are not static methods of
/// `BackHandler`, so `Java_app_squallar_BackHandler_nativeBackPressed` binds to
/// nothing and `call_static_method("setClaimed")` throws `NoSuchMethodError` —
/// neither of which a host build can notice. This grep only proves the spelling;
/// WO-RP-SPIKE is where the binding itself was proven on a device.
#[test]
fn the_kotlin_callback_calls_the_symbol_rust_exports() {
    let kt = jvm_code(BACK_HANDLER_KT);
    assert!(
        kt.contains("package app.squallar")
            && kt.contains("object BackHandler")
            && kt.contains("external fun nativeBackPressed()"),
        "the Kotlin side no longer declares app.squallar.BackHandler.nativeBackPressed",
    );
    for (marker, why) in [
        ("external fun nativeBackPressed", "the native symbol binds"),
        ("fun setClaimed", "the claim call reaches a static method"),
        (
            "fun register",
            "register_java_helper's (Landroid/app/Activity;)V call lands",
        ),
    ] {
        let at = kt
            .find(marker)
            .unwrap_or_else(|| panic!("BackHandler.kt no longer declares `{marker}`"));
        let line_start = kt[..at].rfind('\n').map_or(0, |nl| nl + 1);
        let preceding = &kt[..line_start];
        assert!(
            preceding.trim_end().ends_with("@JvmStatic"),
            "`{marker}` lost its @JvmStatic, so {why} no longer holds",
        );
    }
    assert!(
        ANDROID_BACK.contains("fn Java_app_squallar_BackHandler_nativeBackPressed("),
        "nothing exports the symbol BackHandler.nativeBackPressed() binds to",
    );
}

/// Three spellings of one library, and the native method binds only if they agree.
///
/// `BackHandler.kt` declares a native method, so ART has to find
/// `libsquallar_native.so` on the JavaVM's library list under the app's own
/// ClassLoader — and it is not there, because NativeActivity dlopen()s the file
/// by path instead of going through `System.loadLibrary`. The `init` block that
/// repairs that names the library as a bare string, so this pins the string
/// against the two declarations that actually decide the filename: squallar's
/// `[lib] name` (which is what cargo calls the cdylib) and the manifest's
/// `android.app.lib_name` (which is what NativeActivity resolves). A rename of
/// either without the third is an `UnsatisfiedLinkError` on a device and nothing
/// at all on a host.
#[test]
fn the_kotlin_helper_loads_the_library_the_manifest_names() {
    const CARGO: &str = include_str!("../../../squallar/Cargo.toml");
    const MANIFEST: &str =
        include_str!("../../../packaging/android/app/src/main/AndroidManifest.xml");

    let lib_name = CARGO
        .split_once("[lib]")
        .and_then(|(_, rest)| rest.split_once("name = \""))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(name, _)| name)
        .expect("squallar/Cargo.toml no longer declares a [lib] name");
    assert_eq!(
        lib_name, "squallar_native",
        "the cdylib was renamed; the manifest and BackHandler.kt below have to move with it",
    );
    assert!(
        MANIFEST.contains(&format!("android:value=\"{lib_name}\"")),
        "AndroidManifest's android.app.lib_name is no longer {lib_name}, so \
         NativeActivity dlopen()s a different file than the one BackHandler loads",
    );
    assert!(
        jvm_code(BACK_HANDLER_KT).contains(&format!("System.loadLibrary(\"{lib_name}\")")),
        "BackHandler.kt no longer loads {lib_name} under its own ClassLoader. \
         NativeActivity's dlopen does not register the library with ART, so \
         without this the native method binds to nothing and every claimed back \
         press is dropped on the UI thread",
    );
}

/// The one manifest attribute that decides whether this app can be reopened.
///
/// `android:enableOnBackInvokedCallback="true"` opts the app into the
/// predictive-back dispatcher, and that is the whole reason `BackHandler.kt`
/// exists: only a registered callback can decline a press, and only a declined
/// press buys the platform's own back-to-home preview.
///
/// It was ABSENT for one release, on a measurement: with it present, an
/// unclaimed press did exactly what the design wanted and the app could then
/// never be reopened (WO-RP-SPIKE leg 4b). That was never an
/// Activity-recreation problem. android-activity's `notify_destroyed()` blocks
/// the Java **UI thread** until the Rust `android_main` thread reports
/// `Stopped`, and winit 0.30's Android backend does not act on
/// `MainEvent::Destroy` — an upstream `TODO` that logs and returns — so
/// `android_main` never returned and the UI thread stayed blocked until
/// ActivityTaskManager gave up with an "Activity destroy timeout".
///
/// [`App::suspended`] ends the loop itself now, on the
/// `Activity.isFinishing()` probe that tells a finish from a backgrounding.
/// Measured on the emulator, 2026-08-21, one build one change apart: three
/// close-and-reopen cycles, three `android_main` starts, zero destroy
/// timeouts, zero panics — against a count that stayed at 1 before it.
///
/// This test now guards the attribute's **presence**, because losing it is a
/// silent regression in the other direction: back would go back to the legacy
/// `KEYCODE_BACK` route, the preview would disappear, and nothing else in a
/// host build would notice. From targetSdk 36 the dispatcher is not opt-in and
/// this attribute stops being a choice at all.
#[test]
fn the_manifest_opts_into_predictive_back() {
    const MANIFEST: &str =
        include_str!("../../../packaging/android/app/src/main/AndroidManifest.xml");

    // XML comments carry the reasoning above, including the attribute's own
    // name, so the document has to be read without them.
    let mut markup = String::with_capacity(MANIFEST.len());
    let mut rest = MANIFEST;
    while let Some(open) = rest.find("<!--") {
        markup.push_str(&rest[..open]);
        let after = &rest[open + 4..];
        match after.find("-->") {
            Some(close) => rest = &after[close + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    markup.push_str(rest);

    // Presence control: a stripper that ate the document, or one that left the
    // comments in, would make the assertion below true for the wrong reason.
    for anchor in [
        "<application",
        "android:hasCode=\"true\"",
        "android.app.lib_name",
    ] {
        assert!(
            markup.contains(anchor),
            "the comment stripper ate the manifest: `{anchor}` is gone, so what \
             this test asserts would hold for the wrong reason",
        );
    }

    assert_eq!(
        markup
            .matches("enableOnBackInvokedCallback=\"true\"")
            .count(),
        1,
        "the manifest no longer opts into the predictive-back dispatcher. \
         BackHandler.kt's callback is then never invoked, back falls back to the \
         legacy KEYCODE_BACK route, and the platform's back-to-home preview is \
         gone. If this was removed because the app became unopenable again, the \
         thing to check is App::suspended's terminal-suspend exit, not this \
         attribute",
    );
}

/// The decision this class is not allowed to make.
///
/// The old Java version minimised for itself when it could not route a press,
/// and the one allowance was argued as a fallback. Under the claim design there
/// is nothing left to fall back to: the callback is registered only while Rust
/// says something is open, so a press that reaches it always has a funnel, and a
/// press with nothing open never reaches it at all — that is what leaves the
/// platform free to draw its own back-to-home preview. A `moveTaskToBack` here
/// would take that animation away and re-open the "one press with the drawer
/// open minimises the app" hole from the other side.
///
/// Live today only as a property of the source: the app is not opted into the
/// dispatcher, so the callback is never invoked (the manifest carries why).
/// The pin is what keeps the class re-openable — a minimise added into it while
/// it is dormant is a bug that would ship the day the opt-in does.
#[test]
fn the_predictive_back_callback_never_minimises() {
    let kt = jvm_code(BACK_HANDLER_KT);
    assert!(
        kt.contains("registerOnBackInvokedCallback")
            && kt.contains("unregisterOnBackInvokedCallback"),
        "BackHandler no longer registers AND unregisters its callback; a claim \
         it cannot withdraw is a claim it cannot make truthfully",
    );
    assert!(
        kt.contains("nativeBackPressed()"),
        "BackHandler declares the native funnel but never calls it",
    );
    assert_eq!(
        kt.matches("moveTaskToBack").count(),
        0,
        "BackHandler minimises for itself again; the minimise belongs to \
         App::resolve_back_press, which is the only side that knows whether \
         the press closed anything",
    );
    assert_eq!(
        kt.matches("OnBackAnimationCallback").count(),
        0,
        "an OnBackAnimationCallback appeared: this app draws no in-app back \
         animation, and registering one takes the system's own preview away",
    );
}

/// A claim that has gone stale must still land somewhere safe.
///
/// The claim is pushed at the end of a frame and the press arrives whenever the
/// user makes it, so the two can disagree for a frame. The design's answer is
/// that the claimed route is not a second decision path: it parks a flag that
/// `about_to_wait` funnels into the very same `back_out` the legacy KEYCODE_BACK
/// route ends in, and that funnel minimises through `handle_back` when it finds
/// nothing to dismiss. This pins the funnel, not the prose.
#[test]
fn a_stale_back_claim_still_resolves_through_the_app() {
    assert!(
        ANDROID_BACK.contains("BACK_PRESS_PENDING.store(true"),
        "the JNI callback no longer parks a press for the loop to collect",
    );
    assert!(
        ANDROID_BACK.contains("pub(super) fn take_back_press() -> bool"),
        "nothing exposes the parked press to the bridge's poll_back_press",
    );

    let about_to_wait = fn_body("fn about_to_wait(");
    assert!(
        about_to_wait.contains("if self.platform.poll_back_press() {")
            && about_to_wait.contains("self.back_out("),
        "the parked press no longer reaches back_out: {about_to_wait}",
    );

    let resolve = fn_body("fn resolve_back_press(");
    let dismisses = resolve
        .find("dismiss_top_layer()")
        .expect("resolve_back_press no longer asks the Gui to close anything");
    let minimises = resolve
        .find("platform.handle_back()")
        .expect("resolve_back_press no longer has a minimise for a press that closed nothing");
    assert!(
        minimises > dismisses,
        "resolve_back_press minimises before it tries to dismiss, so a stale \
         claim would background the app with a sheet open: {resolve}",
    );
}

/// The claim, and the fact that it is only pushed when it moves.
///
/// Android's dispatcher decides ownership of the gesture before it happens, so
/// the claim has to be published ahead of the press and has to be true at every
/// transition. It also has to be published on transitions ONLY: the far end is a
/// JNI static call and this runs at the end of every frame, so a push per frame
/// is a JNI hop at the display's refresh rate for a value that changes when the
/// user opens a sheet.
#[test]
fn the_back_claim_is_pushed_when_it_changes_and_only_then() {
    let bridge = TestBridge::android();
    let claims = bridge.back_claim_log();
    let mut app = headless(bridge);

    app.push_back_claim();
    app.push_back_claim();
    assert!(
        claims.borrow().is_empty(),
        "a claim of `false` was pushed with nothing open; the platform starts \
         unregistered, so that push says nothing and costs a JNI call: {:?}",
        claims.borrow(),
    );

    app.gui.open_settings();
    app.push_back_claim();
    app.push_back_claim();
    app.push_back_claim();
    assert_eq!(
        &*claims.borrow(),
        &[true],
        "opening the inspector must claim the next press exactly once",
    );

    assert!(
        app.gui.dismiss_top_layer(),
        "precondition: the inspector was the open layer",
    );
    app.push_back_claim();
    app.push_back_claim();
    assert_eq!(
        &*claims.borrow(),
        &[true, false],
        "closing the last open layer must WITHDRAW the claim, or the app keeps \
         swallowing back presses the platform should have taken home",
    );
}

/// Set by `one_press` below.
static PARKED_BACK_PRESS: AtomicBool = AtomicBool::new(false);

fn one_press() -> bool {
    PARKED_BACK_PRESS.swap(false, Ordering::Relaxed)
}

/// The taker has to reach the bridge, and it has to *consume*.
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

/// No bridge may invent a press.
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

/// The same press on a platform with no back handler: Escape on the desktop and the
/// browser's back. It closes what is open and then does **nothing** — it used to quit the
/// app outright, unconfirmed, from a key pressed to leave a text field.
#[test]
fn escape_with_nothing_open_does_not_quit() {
    for (name, platform) in [
        ("desktop", TestBridge::desktop()),
        // The browser's Back and GoBack arrive as the same press (`input.rs`), and its
        // bridge takes neither, so the web build reaches this line too.
        ("web", TestBridge::web()),
    ] {
        let mut gui = Gui::new();
        gui.open_settings();

        assert_eq!(
            App::resolve_back_press(&mut gui, &platform),
            BackPress::Dismissed,
            "{name}: escape must close the window rather than quit, same as back"
        );
        assert_eq!(
            App::resolve_back_press(&mut gui, &platform),
            BackPress::Ignored,
            "{name}: a second press, with nothing left open, quit the app"
        );
        assert_eq!(
            App::resolve_back_press(&mut gui, &platform),
            BackPress::Ignored,
            "{name}: and it stays inert — the first inert press did not arm anything"
        );
    }
}

/// The exit route is not deleted, only unclaimed: a platform that says an unhandled back
/// leaves still gets `Exit`, and it says so through the bridge, not a `cfg` in the
/// resolver. Nothing that ships answers `true` — see the trait — so this is what keeps the
/// arm reachable.
#[test]
fn a_platform_that_asks_to_quit_on_back_still_does() {
    let mut gui = Gui::new();
    let platform = TestBridge::desktop().that_quits_on_unhandled_back();
    gui.open_settings();

    assert_eq!(
        App::resolve_back_press(&mut gui, &platform),
        BackPress::Dismissed,
        "the opt-in must not skip the dismissal it is behind",
    );
    assert_eq!(
        App::resolve_back_press(&mut gui, &platform),
        BackPress::Exit,
    );

    // And the platform that takes the press wins over the opt-in: Android minimises even
    // where both are set, because the press never reaches the last line.
    let mut gui = Gui::new();
    let minimising = minimising_bridge().that_quits_on_unhandled_back();
    assert_eq!(
        App::resolve_back_press(&mut gui, &minimising),
        BackPress::PlatformHandled,
        "the platform consumed the press and the app quit anyway",
    );
}

/// An `App` with no GPU behind it.
///
/// `App::new` is the whole of it now: it builds no wgpu instance, so there is
/// no longer an empty-backends one to pass in. What keeps this headless is that
/// nothing here gives the app a window, and the instance is built beside the
/// surface — `initialize_rendering_state` is the only thing that asks for
/// either.
pub(super) fn headless(mut platform: TestBridge) -> App {
    crate::test_sites::install();
    let location = squallar_location::LocationFacade::new(Box::new(platform.location_provider()));
    App::new(Box::new(platform), location)
}

/// A loop speed no default produces, so finding it can only mean the stored config was
/// read.
const STORED_FPS: f32 = 9.25;

/// Write a config the way the app writes one, rather than by hand: a literal blob would
/// stop matching the format the moment it changed and would then be testing nothing.
fn seed_config(store: &MemoryKvStore, fps: f32) {
    let mut gui = Gui::new();
    gui.loop_speed_fps = fps;
    gui.save_ui_config(store);
}

/// What a bridge's store holds, read back through the same parser the app loads with.
fn stored_fps(store: &MemoryKvStore) -> f32 {
    let mut reloaded = Gui::new();
    reloaded.load_ui_config(store);
    reloaded.loop_speed_fps
}

/// The site every pane opens on, which is what a user actually sees.
fn opening_site(app: &App) -> String {
    app.gui.pane(0).expect("a pane exists").site().to_string()
}

// ── First-run site selection ────────────────────────────────────────

/// The complaint this feature answers: a first run in Minnesota opened on Oklahoma's radar
/// because the default was compiled in.
#[test]
fn a_first_run_opens_on_the_radar_nearest_the_devices_timezone() {
    let app = headless(TestBridge::desktop().with_timezone("America/Chicago"));
    assert_eq!(opening_site(&app), "KLOT");
}

/// Two devices in different timezones must not open on the same site, which is the failure
/// mode a hardcoded default has by construction.
#[test]
fn different_timezones_open_on_different_sites() {
    let west = headless(TestBridge::desktop().with_timezone("America/Los_Angeles"));
    let east = headless(TestBridge::desktop().with_timezone("America/New_York"));
    assert_ne!(opening_site(&west), opening_site(&east));
}

/// A platform that cannot report a timezone keeps the compiled-in default rather than
/// ending up on an empty or invented site.
#[test]
fn a_platform_with_no_timezone_keeps_the_built_in_default() {
    let app = headless(TestBridge::desktop());
    assert_eq!(opening_site(&app), Gui::new().pane(0).unwrap().site());
}

/// The precedence rule, and the one that matters most: a returning user's stored site is
/// never second-guessed, however far the timezone disagrees.
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

/// The silent upgrade: the timezone puts the user in the right region for the first paint,
/// and a fix — which only arrives where location was already granted — resolves the actual
/// nearest radar.
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
        .send(squallar_location::Fix::from_lat_lon(46.7867, -92.1005))
        .unwrap();
    app.poll_platform_state();

    assert_eq!(opening_site(&app), "KDLH");
}

/// Naming the new site is only the visible part of moving to it.
#[test]
fn a_refined_site_actually_requests_its_radar_data() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);

    fixes
        .send(squallar_location::Fix::from_lat_lon(46.7867, -92.1005))
        .unwrap();
    app.poll_platform_state();

    let pane = app.gui.pane(0).expect("a pane exists");
    assert_eq!(pane.site(), "KDLH");
    assert_eq!(
        pane.loading_site.as_deref(),
        Some("KDLH"),
        "the site changed without anything fetching for it, so the pane has \
             no scan_info and the map stays at its no-data centre"
    );
}

/// A fix must not move a site the user chose.
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
        .send(squallar_location::Fix::from_lat_lon(32.7767, -96.7970))
        .unwrap();
    app.poll_platform_state();

    assert_eq!(
        opening_site(&app),
        "KICT",
        "a late fix yanked the user away from the site they chose"
    );
}

/// Once a guess has been refined it stops being a guess.
#[test]
fn only_the_first_fix_refines_the_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);

    fixes
        .send(squallar_location::Fix::from_lat_lon(46.7867, -92.1005))
        .unwrap();
    app.poll_platform_state();
    assert_eq!(opening_site(&app), "KDLH");

    // The same user, now in Denver.
    fixes
        .send(squallar_location::Fix::from_lat_lon(39.7392, -104.9903))
        .unwrap();
    app.poll_platform_state();
    assert_eq!(
        opening_site(&app),
        "KDLH",
        "a second fix moved a site that was already settled"
    );
}

/// The OS location services all report a fused position and decline to name the source, so
/// none of them can honestly claim `Gps`.
#[test]
fn an_os_fix_refines_a_guessed_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);
    assert_eq!(opening_site(&app), "KLOT");

    fixes
        .send(squallar_location::Fix {
            // What the location portal measured on the developer's own machine: an
            // IP/ichnaea lookup, and comfortably good enough to choose among sites 200 km
            // apart.
            accuracy_m: Some(25_000.0),
            ..squallar_location::Fix::from_device_position(46.7867, -92.1005)
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

/// The shape the android location module now produces from the network provider, end to
/// end.
#[test]
fn an_android_network_fix_refines_the_opening_site() {
    let mut bridge = TestBridge::android().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);
    assert_eq!(opening_site(&app), "KLOT");

    fixes
        .send(squallar_location::Fix {
            accuracy_m: Some(32.0),
            ..squallar_location::Fix::from_device_position(46.7867, -92.1005)
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

/// A GPS simulator is a real thing on the serial path — GGA quality 8, and quality 7 is a
/// position somebody typed into the receiver.
#[test]
fn a_simulated_fix_does_not_move_the_radar_site() {
    for quality in [
        squallar_location::FixQuality::Simulation,
        squallar_location::FixQuality::Manual,
        squallar_location::FixQuality::None,
    ] {
        let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
        let fixes = bridge.gps_channel();
        let mut app = headless(bridge);

        fixes
            .send(squallar_location::Fix {
                fix_quality: quality,
                ..squallar_location::Fix::from_lat_lon(46.7867, -92.1005)
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

/// The threshold is enormous on purpose — see `MAX_RELOCATION_ACCURACY_M`, where the
/// measurements are — so this is about the absurd end: a fix whose stated uncertainty is
/// wider than the region the timezone guess already resolved must not spend the one
/// upgrade.
#[test]
fn a_low_accuracy_fix_does_not_spend_the_provisional_site() {
    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);

    fixes
        .send(squallar_location::Fix {
            accuracy_m: Some(squallar_location::MAX_RELOCATION_ACCURACY_M * 2.0),
            ..squallar_location::Fix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();

    assert_eq!(opening_site(&app), "KLOT");
    assert!(
        app.site_is_provisional,
        "a fix too coarse to use was still spent, so the good one that \
             follows it can never refine anything"
    );

    // And the good fix that follows still works, which is the half that makes the rejection
    // worth anything.
    fixes
        .send(squallar_location::Fix {
            accuracy_m: Some(25_000.0),
            ..squallar_location::Fix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();
    assert_eq!(opening_site(&app), "KDLH");
}

/// The upgrade asks **pane 0** whether the move would be a no-op and then moves the
/// **active** pane. With one pane those are the same pane; with two they need not be, and
/// the short-circuit then answers about a pane nobody was going to move — so the pane the
/// switch names never moves, and `site_is_provisional` is spent regardless, so no later fix
/// retries it.
#[test]
fn a_fix_moves_the_active_pane_even_when_pane_zero_is_already_there() {
    use squallar_egui::UI_CONFIG_KEY;
    use squallar_kv::KvStore;

    let mut bridge = TestBridge::desktop().with_timezone("America/Chicago");
    let fixes = bridge.gps_channel();
    let mut app = headless(bridge);
    assert!(
        app.site_is_provisional,
        "precondition: the timezone guess must still be the one a fix may refine",
    );

    // Pane 0 sits on the radar the fix is about to name; the active pane does not.
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"active_pane":1,"site":"KDLH",
                "panes":[{"site":"KDLH"},{"site":"KLOT"}]}"#,
        )
        .expect("the memory store always accepts a write");
    assert!(
        app.gui.load_ui_config(&store),
        "the two-pane fixture config did not parse"
    );
    app.render.ensure_pane_count(2);
    assert_eq!(
        (app.gui.active_pane_idx(), opening_site(&app)),
        (1, "KDLH".to_string()),
        "precondition: pane 0 on the fix's radar, and the active pane elsewhere",
    );

    fixes
        .send(squallar_location::Fix {
            accuracy_m: Some(25.0),
            ..squallar_location::Fix::from_device_position(46.7867, -92.1005)
        })
        .unwrap();
    app.poll_platform_state();

    assert_eq!(
        app.gui.pane(1).expect("the fixture built two panes").site(),
        "KDLH",
        "the fix refined nothing: the no-op check asked pane 0, which was \
         already there, so the pane the switch names never moved",
    );
}

// ── The location permission gate, from the App's side ───────────────
// `squallar_location::gate` owns the state machine and tests it against a clock it controls.

/// The gate is stepped from `poll_platform_state`, and what it sees is pushed to the `Gui`
/// — which is the only copy the settings pane can read, since `squallar-egui` cannot see a
/// `PlatformBridge`.
#[test]
fn what_the_platform_says_about_location_reaches_the_settings_pane() {
    let bridge =
        TestBridge::desktop().with_permission(squallar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    assert_eq!(
        app.gui.location_permission(),
        squallar_location::LocationPermission::Unknown,
        "the cache starts inert, before anything has been polled"
    );

    app.poll_platform_state();
    // What the gate observed reaches the UI on the frame's compose; no renderer exists
    // here, so the test drives the compose itself.
    app.push_frame_inputs();

    assert_eq!(
        app.gui.location_permission(),
        squallar_location::LocationPermission::Granted
    );
    assert!(
        app.gui.location_active(),
        "a grant with no stream is where every desktop process starts; \
             something has to turn it on"
    );
    assert_eq!(location.requests.get(), 1);
}

/// Consent went away, so the position drawn under it must go too.
#[test]
fn a_revoked_permission_stops_delivery_and_clears_the_dot() {
    let bridge =
        TestBridge::desktop().with_permission(squallar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    // A delivered fix lands in the App's own field, stamped at arrival, and reaches the UI
    // on the next compose — the same route production takes.
    app.user_gps = Some((
        squallar_location::Fix::from_device_position(35.25, -97.5),
        web_time::Instant::now(),
    ));
    app.push_frame_inputs();
    assert!(app.gui.gps_fix().is_some());

    // Revoked in system settings, with no process restart — which is what happens on every
    // desktop OS.
    location
        .permission
        .set(squallar_location::LocationPermission::Denied);
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

/// The serial dongle is not covered by this permission — it is a device the user plugged in
/// — so a location denial must not take its dot away.
#[test]
fn a_revoked_permission_leaves_a_serial_dongles_dot_alone() {
    let bridge =
        TestBridge::desktop().with_permission(squallar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    app.location
        .start_serial(&squallar_nmea_serial::SerialConfig::default());
    app.user_gps = Some((
        squallar_location::Fix::from_lat_lon(35.25, -97.5),
        web_time::Instant::now(),
    ));

    location
        .permission
        .set(squallar_location::LocationPermission::Denied);
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
/// `shouldShowRequestPermissionRationale` is `false` for both — so the memo on this side
/// has to tell it, and this is the wire that does.
#[test]
fn a_bridge_that_needs_the_attempt_count_is_told_it() {
    let bridge =
        TestBridge::android().with_permission(squallar_location::LocationPermission::Prompt);
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

/// Turning location off in the settings pane stops the stream and takes the dot with it, at
/// the moment of the click rather than at the next poll.
#[test]
fn turning_location_off_stops_the_stream_and_clears_the_dot() {
    let bridge =
        TestBridge::desktop().with_permission(squallar_location::LocationPermission::Granted);
    let location = bridge.location_record();
    let mut app = headless(bridge);
    app.poll_platform_state();
    app.user_gps = Some((
        squallar_location::Fix::from_device_position(35.25, -97.5),
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
#[test]
fn the_bridge_gets_the_apps_own_waker_before_any_window_exists() {
    let bridge = TestBridge::desktop();
    let handed_to_the_bridge = bridge.waker_record();
    let app = headless(bridge);

    // Stands in for what `create_window` installs; no test can build the `Window` it
    // captures, so that half is read off the source below.
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

/// The entry points' own producers — `android_main`'s location and compass threads, the
/// browser's `watchPosition` watch — are not the bridge's, and take their handle from here.
#[test]
fn every_handle_the_app_gives_out_is_the_same_slot() {
    let app = headless(TestBridge::desktop());
    let woke = count_wakes(&app.redraw_waker());

    // What `android_main` and `entry::start` keep: a clone taken at startup, several
    // seconds before the first `resumed()`.
    app.redraw_waker().wake();

    assert_eq!(woke.load(Ordering::SeqCst), 1);
}

/// The window half of the wiring.
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

/// And the teardown.
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

/// The bug this exists for.
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

/// An idle app must not rewrite an unchanged config every three seconds for the life of the
/// process.
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

/// The interval is what keeps this cheap, so it has to actually gate.
#[test]
fn autosave_respects_its_interval() {
    let bridge = TestBridge::desktop();
    let writes = bridge.write_count();
    let mut app = headless(bridge);

    // The first unforced call has no previous check to compare against and establishes the
    // baseline.
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

/// A timezone-guessed site has to reach storage like any other, or a first run guesses
/// again every launch and a returning user is never recognised.
#[test]
fn a_guessed_site_is_persisted() {
    let bridge = TestBridge::desktop().with_timezone("America/Denver");
    let store = bridge.store();
    let mut app = headless(bridge);

    app.autosave_config(true);

    let mut reloaded = Gui::new();
    assert!(reloaded.load_ui_config(store.as_ref()));
    assert_eq!(reloaded.pane(0).unwrap().site(), "KFTG");
}

/// The state a pan leaves behind: an event has been seen, and the last autosave check was
/// `ago` in the past.
fn owes_a_save_from(app: &mut App, ago: std::time::Duration) {
    app.autosave.last_check = Some(web_time::Instant::now() - ago);
    app.autosave.touched = true;
}

/// Everything an expired `WaitUntil` actually dispatches, and nothing more.
fn wake_on_the_timer(app: &mut App) -> ControlFlow {
    app.autosave_config(false);
    app.wakeup_control_flow()
}

/// A wake-up asked for and granted has to end in the write it was asked for.
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

/// `about_to_wait` is where the save has to happen, and it takes an `ActiveEventLoop` — so
/// this is a source probe for the same reason
/// `a_back_press_from_the_platform_reaches_the_funnel_too` is.
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

/// A deadline in the past must put the loop back to sleep, not re-arm at zero.
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

/// The positive control for the test above: closing the spin must not be done by switching
/// the timer off.
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

/// An app nothing has touched has to be left free to sleep indefinitely, which is the whole
/// reason `touched` exists.
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
    // The terms that do belong: each one ends, and its ending asks for the frame that
    // notices.
    for kept in [
        "any_render_in_flight",
        "any_loop_active",
        "chunk_feeds.any_in_flight",
        // The handshake, which times out; not the backoff, which does not.
        "chunk_notify.handshake_pending",
        // Memory the app has already decided it does not want, waiting for a frame to free
        // it.
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

/// A poll is checked inside the egui pass, so its wake-up has to end in a real frame.
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

/// …and a spent one lets it sleep again.
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

/// Silence every other term, so what `auto_poll_delay` answers can only have come from the
/// one under test.
fn silence_the_other_timers(app: &mut App) {
    for idx in 0..app.gui.remembered_pane_count() {
        {
            let pane = app.gui.pane_mut(idx).expect("a remembered pane");
            for id in squallar_egui::sources::default_draw_order() {
                pane.set_overlay_enabled(id, false);
            }
        }
        // The archive poll is gated on a pane VIEWING LIVE, not on the radar
        // layer being enabled — a pane scrubbed to an archive time still has
        // radar on — so turning the layer off above does not silence it. What
        // does is nothing on screen asking for live data.
        app.gui
            .apply(squallar_egui::shell_api::GuiEvent::ViewingLiveForPane {
                pane_idx: idx,
                live: false,
            });
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

/// The chunk feed's five-second round is a timer checked on a frame, and it used to ride on
/// the auto-poll re-arm keeping frames coming at 60 Hz.
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
        &Ok(squallar_radar::chunks::PollOutcome::default()),
    );
    let delay = app
        .auto_poll_delay()
        .expect("a feed between rounds owes itself another one");
    assert!(
        !delay.is_zero() && delay <= squallar_radar::chunks::QUIET_INTERVAL,
        "the next round is scheduled {delay:?} out, which is not this feed's \
             own cadence"
    );
    // The same answer, not merely a plausible one — both are read off a live clock, so they
    // agree to within the microseconds between the two calls.
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

/// A notification socket waiting out its backoff is scheduled for, not spun on.
#[test]
fn a_notifier_backoff_is_slept_through_rather_than_spun_on() {
    use squallar_radar::chunk_notify::Feed;

    // Loopback on a closed port: `ewebsock` opens a socket that will never finish its
    // handshake, which is the state a blocked network leaves.
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

    // Past `CONNECT_TIMEOUT`: the next sync tears the socket down and the wait becomes a
    // backoff.
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

/// Set by the back handler the app installs, so a test can see it *ran* rather than merely
/// being held somewhere.
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

/// The app opens showing what the last session left, and it can only get that from the
/// bridge — this crate has no idea where config lives.
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

/// iOS cannot quit, and the menu must not offer to.
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

/// Android learns its data directory only after startup, so the load in `App::new` had
/// nothing to read and the second one is the only one that ever runs there.
#[test]
fn learning_where_config_lives_loads_it() {
    let bridge = TestBridge::android();
    seed_config(&bridge.store(), STORED_FPS);

    let mut app = headless(bridge);
    assert_eq!(
        app.gui.loop_speed_fps,
        squallar_egui::pane::DEFAULT_LOOP_SPEED_FPS,
        "precondition: nowhere to load from yet",
    );

    app.set_config_dir(std::path::PathBuf::from("/data/user/0/squallar"));

    assert_eq!(
        app.gui.loop_speed_fps, STORED_FPS,
        "the config directory arrived and nothing was read from it",
    );
}

/// The save has to happen before the platform gets to refuse the exit.
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

/// An exit asked for during a redraw has no event loop to hand, so it is deferred rather
/// than dropped.
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

/// The menu's Exit is one of the four ways out and goes through the same gate as the rest:
/// it saves, and it respects a platform that cannot quit.
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

/// A fix and a heading are separate readings from separate sensors and must stay that way:
/// the map draws the dot from one and rotates it by the other.
#[test]
fn the_platforms_sensors_reach_the_map() {
    let mut bridge = TestBridge::android();
    let fix_tx = bridge.gps_channel();
    let mut app = headless(bridge);
    let (heading_tx, heading_rx) = std::sync::mpsc::channel();
    app.set_heading_receiver(heading_rx);

    fix_tx
        .send(squallar_location::Fix::from_lat_lon(35.3331, -97.2778))
        .unwrap();
    heading_tx.send(214.5).unwrap();

    app.handle_redraw();
    // `handle_redraw` polls the producers and then returns before it needs a renderer; the
    // compose that carries the polled facts to the UI lives on the renderer's side of that
    // return, so the test drives it itself.
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

#[test]
fn a_deferred_teardown_is_freed_by_the_frame_loop() {
    let mut app = headless(TestBridge::android());
    // Emptied first: the queue is thread-local and the harness reuses threads.
    while squallar_worker::offload::drain_deferred_drops(std::time::Duration::from_secs(30)) > 0 {}

    let held: Vec<std::sync::Arc<()>> = (0..3).map(|_| std::sync::Arc::new(())).collect();
    let watched: Vec<std::sync::Arc<()>> = held.iter().map(std::sync::Arc::clone).collect();
    // Straight onto the queue, which is where a browser's `discard` puts every payload —
    // the native routing would hand these to the pool instead, and the frame loop is what
    // this test is about.
    for payload in held {
        squallar_worker::offload::defer_drop("test-teardown", Box::new(payload));
    }
    assert!(squallar_worker::offload::has_deferred_drops());

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
        !squallar_worker::offload::has_deferred_drops(),
        "the queue outlived the frames that were supposed to empty it",
    );
}

/// A theme change has to invalidate the site labels, and only a *change* may.
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

/// **A cut sealing is not a reason to re-rasterize every site marker**, and
/// only a spinner that was really up may be.
///
/// `Gui::clear_loading_site_for_site` is called on every sealed cut of the
/// live chunk feed — `App::apply_chunk_outcome` calls it in both arms — and on
/// every scan result and failed fetch besides. The site layer's whole cache
/// token is `radar_sites_render_gen`, so a bump there is a full-size raster
/// spent for a picture nothing changed. Measured on the Tier-2 rig before the
/// guard: `overlay/sites` ran 13-16 times in a ~40 s leg on a scene where the
/// site table never moved, against `overlay/alerts`' 2-3.
///
/// Counted, not timed: twelve calls, zero bumps.
#[test]
fn a_cut_sealing_does_not_re_rasterize_the_site_markers() {
    let mut app = headless(TestBridge::desktop());
    let site = app.gui.pane(0).unwrap().site().to_string();
    app.gui.pane_mut(0).unwrap().loading_site = Some(site.clone());
    let before = app.gui.pane(0).unwrap().radar_sites_render_gen;

    // The switch completing, which is the one transition that really changes
    // the picture: the spinner comes down.
    app.gui.clear_loading_site_for_site(&site);
    let settled = app.gui.pane(0).unwrap().radar_sites_render_gen;
    assert_eq!(
        settled,
        before.wrapping_add(1),
        "the spinner came down and the markers were not redrawn, so the pane \
         keeps painting a spinner that is over",
    );
    assert_eq!(
        app.gui.pane(0).unwrap().loading_site,
        None,
        "precondition for the loop below: the spinner must really be down, or \
         every call there is the transition above repeated",
    );

    for seal in 1..=12 {
        app.gui.clear_loading_site_for_site(&site);
        assert_eq!(
            app.gui.pane(0).unwrap().radar_sites_render_gen,
            settled,
            "sealed cut {seal} re-keyed the site raster with nothing to \
             redraw: a whole picture rasterized, uploaded and promoted, once \
             per cut, for as long as the feed runs",
        );
    }
}

/// Every scan response queued for a frame is spent in it.
#[test]
fn every_queued_scan_response_is_spent_in_the_frame_it_arrives_in() {
    let mut app = headless(TestBridge::desktop());
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.set_site("KTLX".to_string());
        pane.loading_site = Some("KTLX".to_string());
    }

    for site in ["KOUN", "KTLX"] {
        app.channels
            .scan_sender
            .send(crate::channels::ScanResponse {
                generation: 1,
                site: site.to_string(),
                requester: crate::channels::FetchRequester::Site,
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
pub(super) fn two_pane_app(first: &str, second: &str) -> App {
    use squallar_egui::UI_CONFIG_KEY;
    use squallar_kv::KvStore;

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
    assert_eq!(app.gui.pane(1).map(|p| p.site()), Some(second));
    app.render.ensure_pane_count(2);
    app
}

/// An `App` with `n` map panes, every one of them on `site`.
pub(super) fn n_pane_app(n: usize, site: &str) -> App {
    use squallar_egui::UI_CONFIG_KEY;
    use squallar_kv::KvStore;

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

/// Every whole-texture upload egui has been handed since this was last called, with the
/// pixels it was handed.
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
pub(crate) fn scan_info_for(site: &str) -> ScanInfo {
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
#[test]
fn a_volume_no_pane_is_showing_is_dropped() {
    let mut app = headless(TestBridge::desktop());
    app.gui.pane_mut(0).unwrap().set_site("KTLX".to_string());
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: scan_info_for("KTLX"),
        });
    for site in ["KTLX", "KOUN"] {
        drop(app.volumes.install_still(
            site.to_string(),
            scan_info_for(site).timestamp,
            (Arc::new(empty_scan()), Default::default()),
        ));
        app.volumes.install_base(
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
        app.volumes
            .holds_still("KTLX", scan_info_for("KTLX").timestamp),
        "the volume the pane is drawing from was evicted",
    );
    assert!(
        !app.volumes.holds_any_still("KOUN"),
        "a radar no pane is on is still holding its whole decoded volume",
    );
    assert!(
        app.volumes.holds_base("KTLX"),
        "the base volume the site's whole-volume panes build from was \
             evicted, so none of them can ever be handed one",
    );
    assert!(
        !app.volumes.holds_base("KOUN"),
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
    // All three holders, named individually: a hand-over that covered one of them and
    // left the other two on `retain` would look closed here while leaving most of the
    // teardown on the frame. The first two are the inventory's, which takes them out
    // owned under its own names; the third is still a bare map on the `App`.
    for holder in [
        "self.volumes.retain_still(&wanted)",
        "self.volumes.evict_base(&unshown)",
        "evicted(&mut self.latest_cached_scans, &unshown)",
    ] {
        assert!(
            body.contains(holder),
            "`{holder}` is not in the eviction pass, so that holder's volumes \
             are still freed on the frame: {body}"
        );
    }
    // The extractions above prove the values come out owned; this proves where they go.
    assert_eq!(
        body.matches("squallar_worker::offload::discard_each(")
            .count(),
        3,
        "one of the three volume holders stopped handing its evictions over \
         to the deferred-drop path: {body}"
    );
}

#[test]
fn the_loop_caches_evictions_are_handed_over_and_the_sweep_is_called() {
    assert!(
        fn_body("fn evict_unshown_scans(").contains("self.evict_unneeded_loop_scans();"),
        "the loop cache's sweep is no longer reached from the once-a-frame \
         eviction, so nothing bounds it — which is the defect it closed",
    );
    let body = fn_body("fn evict_unneeded_loop_scans(");
    assert_eq!(
        body.matches("squallar_worker::offload::discard_each(")
            .count(),
        3,
        "one of the loop's three holders frees its evictions where it evicted \
         them — on the frame thread, ~47 MiB median and 58.3 MiB worst case for a \
         volume (measured, `volume_inventory`), a decoded \
         message plus its own bytes for an object, a day's bucket keys for a \
         listing: {body}"
    );
    // The grace rule, named rather than described: without it a loop whose listing is in
    // flight names no frame, and the sweep takes its whole window one frame before the
    // listing would have saved it.
    assert!(
        body.contains("listing_wait(now)"),
        "the grace rule for a loop still fetching its scan listing is gone, so \
         every product switch and loop re-init re-downloads its window: {body}"
    );
    // And the clock on it.
    assert!(
        body.contains("LOOP_LISTING_GRACE"),
        "the grace exemption is unbounded again: on wasm32 a listing future \
         that never completes then exempts its site for the life of the tab, \
         and the leak resumes at full rate: {body}"
    );
    // The queues are swept by the same predicate as the cache.
    assert!(
        body.contains("retain_plan_frames(keep)") && body.contains("retain_scans(keep)"),
        "the frame plan and the cache are no longer swept by one predicate, so \
         a re-plan can queue a download for a volume the sweep evicts: {body}"
    );
    // And the Level III cache, by the same predicate object rather than a second rule free
    // to disagree with the first.
    assert!(
        body.contains("retain_l3(keep)"),
        "the Level III cache is no longer swept, or is swept by a rule of its \
         own: one `Level3Product` per frame per AWIPS code, removed by nothing \
         else, is the sibling of the leak this pass exists for: {body}"
    );
}

/// A pane is under two site names and eviction has to honour both.
#[test]
fn the_volume_a_switching_pane_is_still_drawing_survives() {
    let mut app = headless(TestBridge::desktop());
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: scan_info_for("KTLX"),
        });
    app.gui.pane_mut(0).unwrap().set_site("KOUN".to_string());
    drop(app.volumes.install_still(
        "KTLX".to_string(),
        scan_info_for("KTLX").timestamp,
        (Arc::new(empty_scan()), Default::default()),
    ));
    app.volumes.install_base(
        "KTLX".to_string(),
        (
            Arc::new(empty_scan()),
            Default::default(),
            scan_info_for("KTLX").timestamp,
        ),
    );

    app.evict_unshown_scans();

    assert!(
        app.volumes
            .holds_still("KTLX", scan_info_for("KTLX").timestamp),
        "the pane's own scan info still names KTLX, which is what the \
             render path looks the volume up by",
    );
    assert!(
        app.volumes.holds_base("KTLX"),
        "the base volume was pulled out from under a 3D pane that is \
             still building from it",
    );
}

/// A result thrown away still ends the wait it belonged to.
#[test]
fn a_discarded_scan_result_still_takes_down_the_wait_it_belonged_to() {
    let mut app = headless(TestBridge::desktop());
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.set_site("KTLX".to_string());
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
            requester: crate::channels::FetchRequester::Site,
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
        app.gui.pane(0).unwrap().scan_info.is_none() && app.volumes.holds_no_still(),
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

/// The two theme routes a desktop actually takes, neither of which can be driven here:
/// winit answers `window.theme()` on Windows and macOS, and it reports a flip as
/// `ThemeChanged`.
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

/// Where the injected querier says the system bars are.
static ROTATED: AtomicBool = AtomicBool::new(false);

fn cutout() -> (f32, f32, f32, f32) {
    if ROTATED.load(Ordering::Relaxed) {
        (0.0, 0.0, 96.0, 0.0)
    } else {
        (96.0, 0.0, 0.0, 0.0)
    }
}

/// Turning the device sideways moves the cutout to another edge, and the app has to ask
/// again.
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

/// Both query sites have to stay wired.
#[test]
fn both_inset_queries_are_still_wired() {
    for f in ["fn resumed(", "fn handle_resized("] {
        assert!(
            fn_body(f).contains("refresh_safe_area_insets("),
            "{f} no longer asks the platform for insets",
        );
    }
}

/// The window's own close button is the fourth exit trigger and the last one with no other
/// handle on it: `window_event` takes an `ActiveEventLoop`, so the arm can only be read.
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

/// The save on the way out has to be in `exit_now`, not only where the exit was requested.
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
        BackPress::Ignored,
        "precondition: with no handler installed, back reaches nothing",
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
#[test]
fn starting_gps_hands_the_bridge_the_config_the_action_carried() {
    let bridge = TestBridge::desktop();
    let started = bridge.gps_record();
    let mut app = headless(bridge);

    app.handle_gui_action(
        GuiAction::StartGps {
            config: squallar_nmea_serial::SerialConfig {
                port_path: Some("/dev/ttyPROBE".to_string()),
                baud_rate: 38400,
            },
        },
        None,
    );

    assert!(app.location.serial_active(), "the reader was never started");
    {
        let record = started.borrow();
        let config = record.as_ref().expect("start_serial was not reached");
        assert_eq!(
            config.port_path.as_deref(),
            Some("/dev/ttyPROBE"),
            "the reader opened a different port than the action asked for",
        );
        assert_eq!(config.baud_rate, 38400);
    }

    app.handle_gui_action(GuiAction::StopGps, None);
    assert!(
        !app.location.serial_active(),
        "the reader kept the serial port open after being told to stop",
    );
}

/// The classification the loop acts on: zero-delay requests (animations) repaint
/// immediately, timed ones (cursor blink) schedule a wake, and egui's `Duration::MAX` idle
/// marker — or anything indistinguishable from it — leaves the loop parked.
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

/// The contract the fix stands on: a mid-flight `animate_bool_with_time` puts a zero
/// `repaint_delay` on the root viewport's output — the value `end_pass_and_upload` now
/// carries out and `handle_redraw` spends.
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
    // Seed the animation at `false`, then flip the target: the second pass is mid-
    // interpolation for a 0.2 s animation.
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
    // (The settled side — no repaint request once the animation ends — is real-clock timing
    // and lives with `repaint_action`'s own mapping pins.)
}

// ── Learning where the radars are ───────────────────────────────────

/// A volume stating its own position, as `scan::decoded` builds one out of the first
/// Message 31's Volume Data Block.
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

/// The learned position survives the process, and is applied before anything is drawn from
/// it.
#[test]
fn a_position_a_volume_taught_survives_a_restart() {
    use squallar_radar::site_position::SitePositionSource;

    let store = std::rc::Rc::new(MemoryKvStore::default());
    // Before the read, not merely before the app: `headless` installs the fixture, and it
    // is not called until further down.
    crate::test_sites::install();
    let table = squallar_radar::sites::get_radar_site("KTLX").expect("the fixture places KTLX");
    // A quarter of a degree from the row: far enough that resolving to the wrong one is
    // unmistakable, and in the direction a re-survey would move.
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
            squallar_kv::KvStore::load(store.as_ref(), crate::site_positions::SITE_POSITIONS_KEY,)
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
#[test]
fn a_taught_position_moves_the_maps_marker_and_not_only_the_data() {
    use squallar_radar::site_position::SitePositionSource;

    const SITE: &str = "KMQT";
    let store = std::rc::Rc::new(MemoryKvStore::default());
    // Marquette, Michigan, at the position and heights its own volume reports — the row
    // this test starts from, because nothing is compiled in.
    squallar_radar::sites::resolve([(
        SITE,
        squallar_radar::sites::SiteFix::Learned(squallar_radar::site_position::SitePosition {
            lat_udeg: 46_531_110,
            lon_udeg: -87_548_330,
            site_height_m: 430,
            tower_height_m: 20,
        }),
    )]);
    // Copied out rather than borrowed: `sites::resolve` below displaces the row this names,
    // and a `&'static RadarSite` held across it goes on describing where the radar was
    // *believed* to be — which is the very thing under test.
    let (seeded_lat, seeded_lon) = {
        let row = squallar_radar::sites::get_radar_site(SITE).expect("this test placed it");
        (row.lat, row.lon)
    };
    // A quarter of a degree, in the direction a re-survey would move: far past anything a
    // rounding could produce.
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

    let row = squallar_radar::sites::get_radar_site(SITE).expect("still a row");
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
        squallar_radar::sites::radars()
            .iter()
            .any(|r| r.name == SITE && (r.lat - info.site.lat).abs() < 1e-9),
        "the table's `get` moved but the walk `visible_radar_sites` and \
         `rasterize_radar_sites` both take did not",
    );
    // Nothing else moved with it.
    assert_eq!(
        squallar_radar::sites::radars()
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

    // Restating the same position is not a fresh lesson: no second resolve, no second
    // invalidation, so a session does not re-key the texture every volume.
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

/// The hole the compiled-in table used to cover, and the reason
/// `scan_info_learning_position` resolves as well as persists.
#[test]
fn a_volume_this_session_decoded_gives_its_radar_a_height_this_session() {
    use squallar_radar::sites::Datum;

    // An identifier nothing else in this workspace names, at a coordinate no other test in
    // this binary places a radar near — the site table is process-wide, and
    // `first_launch_tests` puts one at (-30, -140).
    const SITE: &str = "ZZQE";
    const LAT: f32 = -55.0;
    const LON: f32 = -120.0;

    assert!(
        squallar_radar::sites::get_radar_site(SITE).is_none(),
        "precondition: nothing may have placed {SITE}",
    );
    assert_eq!(
        squallar_radar::eet::radar_height_ft_near(f64::from(LAT), f64::from(LON), Datum::Feedhorn),
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
        squallar_radar::site_position::SitePositionSource::Volume,
        "precondition: the volume states its own position",
    );

    // 370 m of ground under 20 m of tower, in feet: the figure every beam height in this
    // session is now measured above.
    let want = f64::from(
        squallar_radar::sites::get_radar_site(SITE)
            .expect("the volume placed it in the live table")
            .height_ft(Datum::Feedhorn)
            .expect("a learned row records both datums"),
    );
    assert_eq!(
        squallar_radar::eet::radar_height_ft_near(f64::from(LAT), f64::from(LON), Datum::Feedhorn),
        Some(want),
        "the render path anchors on sea level for the radar it is drawing",
    );
    assert!(
        want > 1000.0,
        "and it is a real elevation, not zero: {want} ft"
    );
}

/// With nowhere to write, the app still works and simply forgets.
#[test]
fn a_run_with_no_kv_still_applies_the_volumes_own_position() {
    use squallar_radar::site_position::SitePositionSource;

    const SITE: &str = "KMBX";
    // Placed here rather than by the shared fixture, so no sibling can move it.
    squallar_radar::sites::resolve([(
        SITE,
        squallar_radar::sites::SiteFix::Learned(squallar_radar::site_position::SitePosition {
            lat_udeg: 48_392_500,
            lon_udeg: -100_864_720,
            site_height_m: 455,
            tower_height_m: 30,
        }),
    )]);
    let table = squallar_radar::sites::get_radar_site(SITE)
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

    // Within the run it is still remembered — the map is in memory and the store is only
    // where it is *written* — so a chunk-fed volume arriving after an archive one is placed
    // correctly even here.
    let same_run = app.scan_info_learning_position(&empty_scan(), SITE, at);
    assert_eq!(same_run.site_source, SitePositionSource::Learned);
    assert_eq!(same_run.site.lat.to_bits(), info.site.lat.to_bits());

    // And nothing outlives the process: a fresh app remembers nothing, so its `ScanInfo`
    // falls back to the table rather than recalling anything.
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

/// A radar the compiled-in seed has never heard of is in the table by the time `App::new`
/// returns — before any frame exists.
#[test]
fn a_learned_radar_the_seed_never_had_is_in_the_table_before_the_first_frame() {
    use squallar_kv::KvStore;

    const SITE: &str = "ZZZF";

    let store = std::rc::Rc::new(MemoryKvStore::default());
    // A previous session learned this radar from its own volume.
    let learned = serde_json::to_string(&std::collections::BTreeMap::from([(
        SITE.to_owned(),
        squallar_radar::site_position::SitePosition {
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
        squallar_radar::sites::get_radar_site(SITE).is_none(),
        "precondition: {SITE} must not be a seed row, or this proves nothing",
    );

    let _app = headless(TestBridge::desktop().with_store(std::rc::Rc::clone(&store)));

    let row = squallar_radar::sites::get_radar_site(SITE).unwrap_or_else(|| {
        panic!(
            "constructing the app must resolve the table: {SITE} was learned \
             in an earlier session and is still unknown",
        )
    });
    assert_eq!(row.name, SITE, "it carries its own ICAO, not UNKNOWN");
    assert_eq!((row.lat, row.lon), (-34.0, -144.0));
    assert!(
        squallar_radar::sites::radars()
            .iter()
            .any(|r| r.name == SITE),
        "and the walk the map and the site list both do reaches it",
    );
}

/// Android's second resolution is the one that has anything to resolve.
#[test]
fn android_resolves_the_table_when_the_config_directory_arrives() {
    use squallar_kv::KvStore;

    const SITE: &str = "ZZZG";

    let bridge = TestBridge::android();
    let learned = serde_json::to_string(&std::collections::BTreeMap::from([(
        SITE.to_owned(),
        squallar_radar::site_position::SitePosition {
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
        squallar_radar::sites::get_radar_site(SITE).is_none(),
        "precondition: there was nowhere to read {SITE} from yet",
    );

    app.set_config_dir(std::path::PathBuf::from("/data/user/0/squallar"));

    let row = squallar_radar::sites::get_radar_site(SITE).unwrap_or_else(|| {
        panic!(
            "the config directory arrived and the table was not resolved from \
             it: {SITE} is still unknown",
        )
    });
    assert_eq!((row.lat, row.lon), (-35.0, -145.0));
}

// ── The one arrival path: SourceEvent (WO-M11) ────────────────────────────

/// A layer that does nothing but write down what arrived. Registered in place
/// of the production twelve for the drain test below, so each of the three
/// `SourceEvent` arms is observed reaching a **handler**, not merely being
/// consumed by the `while let`.
#[derive(Default, Debug, PartialEq)]
struct Recorded {
    data: Vec<String>,
    listings: Vec<(squallar_source::time::FrameListing, String)>,
    frames: Vec<(squallar_source::time::FrameStamp, String)>,
}

struct FrameRecorder {
    id: LayerId,
    /// Shared with the test rather than downcast back out of the registry:
    /// `OverlayRegistry` hands out `&dyn OverlayHandler`, and adding a
    /// downcast door to production for one test is the wrong trade.
    seen: std::sync::Arc<std::sync::Mutex<Recorded>>,
}

impl squallar_overlays::render::overlay_state::OverlayHandler for FrameRecorder {
    fn id(&self) -> LayerId {
        self.id.clone()
    }
    /// It implements `FrameSource` and records the frames it is asked for, so
    /// this is what it is. It used to inherit `Live` from the trait's default
    /// body while doing that — a frame source declaring it had no frames —
    /// which is the class of silence removing that default exists to end.
    fn time_axis(&self) -> squallar_source::time::TimeAxis {
        squallar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(300),
            extends_future: false,
        }
    }
    fn surface(&self) -> squallar_overlays::render::overlay_state::Surface {
        squallar_overlays::render::overlay_state::Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        999
    }
    fn display_name(&self) -> &str {
        "Frame Recorder"
    }
    fn render_mode(&self) -> squallar_overlays::render::overlay_state::RenderMode {
        squallar_overlays::render::overlay_state::RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        self.seen.lock().expect("no poisoned lock").data.len() as u64
    }
    fn has_data(&self, _pane: &squallar_source::handler::PaneRef<'_>) -> bool {
        !self.seen.lock().expect("no poisoned lock").data.is_empty()
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _f: bool, _pane: &squallar_source::handler::PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(
        &mut self,
        result: squallar_overlays::render::overlay_state::FetchPayload,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
        if let Ok(tag) = result.downcast::<String>() {
            self.seen.lock().expect("no poisoned lock").data.push(*tag);
        }
    }
    fn retain_selections(
        &self,
        _selections: &mut Vec<
            std::sync::Arc<dyn squallar_overlays::render::overlay_state::OverlayItem>,
        >,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
    }
    fn frames(&self) -> Option<&dyn squallar_source::time::FrameSource> {
        Some(self)
    }
    fn frames_mut(&mut self) -> Option<&mut dyn squallar_source::time::FrameSource> {
        Some(self)
    }
}

/// **The two arrival doors are what this double is for**; the other seven are
/// written out and empty, because the suite's whole claim is that an arriving
/// listing and an arriving frame each reach *a handler*. A supply that
/// answered anything would give the drain a second way to be satisfied.
impl squallar_source::time::FrameSource for FrameRecorder {
    fn latest_at(
        &self,
        _pane: &squallar_source::handler::PaneRef<'_>,
        _t: chrono::NaiveDateTime,
    ) -> Option<squallar_source::time::FrameStamp> {
        None
    }
    fn list_frames(
        &self,
        _ctx: &squallar_overlays::render::overlay_state::FetchConfig,
        _pane: &squallar_source::handler::PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> squallar_source::time::FrameListing {
        squallar_source::time::FrameListing::empty(range)
    }
    fn create_frame_list_task(
        &self,
        _ctx: &squallar_overlays::render::overlay_state::FetchConfig,
        _pane: &squallar_source::handler::PaneRef<'_>,
        _range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<squallar_overlays::render::overlay_state::FetchTask> {
        None
    }
    fn fetch_frame(
        &self,
        _ctx: &squallar_overlays::render::overlay_state::FetchConfig,
        _pane: &squallar_source::handler::PaneRef<'_>,
        _stamp: &squallar_source::time::FrameStamp,
    ) -> Option<squallar_overlays::render::overlay_state::FetchTask> {
        None
    }
    fn frames_resident(
        &self,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) -> Vec<squallar_source::time::FrameStamp> {
        Vec::new()
    }
    fn retain_frames(
        &mut self,
        _pane: &squallar_source::handler::PaneRef<'_>,
        _keep: &[squallar_source::time::FrameStamp],
    ) {
    }
    fn frame_horizon(&self, _pane: &squallar_source::handler::PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::zero()
    }
    fn apply_frame_listing(
        &mut self,
        listing: squallar_source::time::FrameListing,
        scope: squallar_overlays::render::overlay_state::FetchPayload,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
        let scope = scope.downcast::<String>().map(|t| *t).unwrap_or_default();
        self.seen
            .lock()
            .expect("no poisoned lock")
            .listings
            .push((listing, scope));
    }
    fn apply_frame(
        &mut self,
        stamp: squallar_source::time::FrameStamp,
        data: squallar_overlays::render::overlay_state::FetchPayload,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
        let tag = data.downcast::<String>().map(|t| *t).unwrap_or_default();
        self.seen
            .lock()
            .expect("no poisoned lock")
            .frames
            .push((stamp, tag));
    }
}

fn a_stamp(hour: u32) -> squallar_source::time::FrameStamp {
    squallar_source::time::FrameStamp {
        valid: chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap(),
        run: None,
    }
}

/// **Every arm of the one arrival path reaches the layer it names.**
///
/// The channel carries `SourceEvent` since WO-M11, and the drain is one
/// `match`: `Data` still lands in `apply_fetch_result`, and `Frames` /
/// `FrameReady` land in the two frame hooks. Those two have **no producer**
/// until WO-E7/WO-M12 — this is the test that says the route they will use is
/// already wired and already routes by layer id.
///
/// A fourth event names a layer that is not registered, so the drain is also
/// shown to survive an arrival with no owner rather than panicking or wedging.
#[test]
fn every_source_event_arm_reaches_the_layer_it_names() {
    use squallar_overlays::render::overlay_state::{
        OverlayFetchResult, OverlayRegistry, SourceEvent,
    };
    use squallar_source::time::FrameListing;

    let mine = LayerId::new("test/frame-recorder");
    let other = LayerId::new("test/nobody-owns-this");
    let mut app = n_pane_app(1, "KTLX");
    let seen: std::sync::Arc<std::sync::Mutex<Recorded>> = Default::default();
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(FrameRecorder {
        id: mine.clone(),
        seen: std::sync::Arc::clone(&seen),
    })]);

    let range = (a_stamp(12).valid, a_stamp(18).valid);
    let listing = FrameListing {
        range,
        frames: vec![a_stamp(13), a_stamp(14)],
        complete: false,
    };
    for event in [
        SourceEvent::Data(OverlayFetchResult {
            kind: mine.clone(),
            data: Box::new("round-1".to_owned()),
        }),
        SourceEvent::Frames {
            id: mine.clone(),
            listing: listing.clone(),
            scope: Box::new("listed-for-mine".to_owned()),
        },
        SourceEvent::FrameReady {
            id: mine.clone(),
            stamp: a_stamp(13),
            data: Box::new("frame-13".to_owned()),
        },
        SourceEvent::Frames {
            id: other,
            listing: listing.clone(),
            scope: Box::new("listed-for-nobody".to_owned()),
        },
    ] {
        app.channels
            .overlay_fetch_sender
            .send(event)
            .expect("the receiver is alive");
    }

    app.poll_overlay_fetch_results();

    let recorder = seen.lock().expect("no poisoned lock");

    assert_eq!(
        recorder.data,
        vec!["round-1".to_owned()],
        "the Data arm no longer reaches apply_fetch_result",
    );
    assert_eq!(
        recorder.listings,
        vec![(listing, "listed-for-mine".to_owned())],
        "the Frames arm did not reach apply_frame_listing exactly once, carrying \
         the scope it was dispatched with — a listing for a layer nobody owns \
         must be dropped, not delivered here",
    );
    assert_eq!(
        recorder.frames,
        vec![(a_stamp(13), "frame-13".to_owned())],
        "the FrameReady arm did not carry both the stamp and the payload",
    );
    assert!(
        app.channels.overlay_fetch_receiver.try_recv().is_err(),
        "the drain left an arrival in the channel",
    );
}

// ── The frame supply, end to end (WO-M12b) ────────────────────────────────

/// **A radar frame listing that arrives on the source path builds the loop
/// that was waiting for it — through the production registry, not a stub.**
///
/// This is the whole re-point in one pass: the arrival lands in `Ingest` and
/// is filed by `RadarSource` under the site the SCOPE names; the pane's loop
/// is built in `Apply` from what `list_frames` answers, which is the layer's
/// own cache and not the arrival's payload. Nothing between the two holds an
/// archive identifier.
#[test]
fn a_listing_that_arrives_on_the_source_path_builds_the_loop_waiting_for_it() {
    use squallar_overlays::render::overlay_state::SourceEvent;
    use squallar_source::time::{FrameListing, FrameStamp};

    /// Built here rather than looked up: a process-global site table makes a
    /// test mean one thing alone and another beside its neighbours.
    const SITE: squallar_radar::sites::RadarSite = squallar_radar::sites::RadarSite {
        name: "KTLX",
        network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
        lat: 35.33,
        lon: -97.27,
        heights: None,
    };
    fn at(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(i64::from(minute))
    }

    let mut app = n_pane_app(1, "KTLX");
    let span_secs = 600u64;
    let range = (at(0) - chrono::Duration::seconds(span_secs as i64), at(0));
    let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
    pane.scan_info = Some(scan_info_for("KTLX"));
    *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
        span_secs,
        &SITE,
        squallar_radar::types::RenderView::PlanView,
    );
    pane.time_state_mut(&known::RADAR).asked_range = Some(range);
    assert_eq!(
        pane.time_state(&known::RADAR).phase,
        squallar_egui::pane::LoopPhase::FetchingScanList,
        "precondition: the pane must be waiting on a listing, or the arrival \
         has nothing to answer",
    );
    assert!(
        pane.time_state(&known::RADAR).frames.is_empty(),
        "precondition: the loop has no frames yet",
    );

    let minutes = [-8i64, -4, 0];
    let scans: Vec<(chrono::NaiveDateTime, squallar_radar::archive::Identifier)> = minutes
        .iter()
        .map(|&m| {
            let ts = at(0) + chrono::Duration::minutes(m);
            (
                ts,
                squallar_radar::archive::Identifier::new(format!("KTLX{m}")),
            )
        })
        .collect();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: squallar_source::id::known::RADAR,
            listing: FrameListing {
                range,
                frames: scans
                    .iter()
                    .map(|(valid, _)| FrameStamp {
                        valid: *valid,
                        run: None,
                    })
                    .collect(),
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: "KTLX".to_string(),
                range,
                scans: scans.clone(),
            }),
        })
        .expect("the receiver is alive");

    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();

    let loop_state = app
        .gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR);
    assert_eq!(
        loop_state
            .frames
            .iter()
            .map(|frame| frame.timestamp)
            .collect::<Vec<_>>(),
        scans.iter().map(|(ts, _)| *ts).collect::<Vec<_>>(),
        "the listing did not reach the pane's frame list through the contract",
    );
    assert_eq!(
        loop_state.phase,
        squallar_egui::pane::LoopPhase::Rendering,
        "the loop was left waiting on a listing that had already landed",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        scans.len(),
        "the frame plan the downloads are derived from was not stored",
    );
}

/// **A task is built against the site the pane is on NOW.**
///
/// `with_layer_pane` hydrates before it hands the view over, and this is the
/// property that hydrate carries: `set_site` is a side-effect-free assignment
/// by its own written contract, so what publishes a pane's selection into the
/// slot a handler reads it from is the hydrate and nothing else. Without it a
/// task is built against whatever site the pane's config last named — the
/// load-time site for the whole session.
///
/// Registered as unpinned by WO-M12b (its tamper stayed green because
/// `Gui::across_panes` hydrates every visible pane on the ARRIVAL path); this
/// is the dispatch path, where nothing else hydrates first.
#[test]
fn a_task_is_built_against_the_site_the_pane_is_on_now() {
    let mut app = n_pane_app(1, "KTLX");
    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .set_site("KOUN".to_string());

    let named = app
        .with_layer_pane(0, &squallar_source::id::known::RADAR, |_, pane_ref| {
            pane_ref
                .config
                .get("site")
                .and_then(|site| site.as_str())
                .map(str::to_owned)
        })
        .expect("pane 0 is in the layout");

    assert_eq!(
        named.as_deref(),
        Some("KOUN"),
        "the layer was handed the site the pane carried when its config was \
         loaded, not the site it is on now",
    );
}

/// **A pane the layout is not showing is hydrated too, before its frames are
/// read.**
///
/// `accept_loop_scan_listings` walks EVERY pane; `Gui::across_panes` — which
/// hydrates on the arrival path — walks only the visible ones, and shrinking
/// the layout does not drop the panes above the count. So a hidden pane that
/// is still looping reaches this walk unhydrated, and the hydrate inside it is
/// the only thing that publishes its site before `list_frames` is scoped to
/// it.
///
/// This is the second of WO-M12b's two green-tamper findings, pinned rather
/// than deleted: the redundancy it reported holds for visible panes only.
#[test]
fn a_hidden_panes_loop_is_still_built_from_the_site_it_is_on_now() {
    use squallar_overlays::render::overlay_state::SourceEvent;
    use squallar_source::time::{FrameListing, FrameStamp};

    /// Built here rather than looked up: a process-global site table makes a
    /// test mean one thing alone and another beside its neighbours.
    const KOUN: squallar_radar::sites::RadarSite = squallar_radar::sites::RadarSite {
        name: "KOUN",
        network: squallar_radar::sites::RadarNetwork::of_id("KOUN"),
        lat: 35.23,
        lon: -97.46,
        heights: None,
    };
    fn at(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(i64::from(minute))
    }

    let mut app = n_pane_app(2, "KTLX");
    // The layout shrinks; the pane does not go away. Neither `set_pane_count`
    // nor a config load ever shortens the vector, which is what puts pane 1
    // outside every visible walk and inside this one.
    {
        use squallar_egui::UI_CONFIG_KEY;
        use squallar_kv::KvStore;
        let store = MemoryKvStore::default();
        store
            .store(UI_CONFIG_KEY, r#"{"pane_count":1,"site":"KTLX"}"#)
            .expect("the memory store always accepts a write");
        assert!(
            app.gui.load_ui_config(&store),
            "the one-pane layout did not parse"
        );
    }
    assert_eq!(
        app.gui.pane_count(),
        1,
        "precondition: the layout must be showing one pane",
    );
    assert!(
        app.gui.pane_mut(1).is_some(),
        "precondition: the pane above the count must still be in the vector, \
         or this test is about nothing",
    );

    let span_secs = 600u64;
    let range = (at(0) - chrono::Duration::seconds(span_secs as i64), at(0));
    let pane = app.gui.pane_mut(1).expect("the fixture built two panes");
    pane.set_site("KOUN".to_string());
    pane.scan_info = Some(scan_info_for("KOUN"));
    *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
        span_secs,
        &KOUN,
        squallar_radar::types::RenderView::PlanView,
    );
    pane.time_state_mut(&known::RADAR).asked_range = Some(range);
    assert_eq!(
        pane.time_state(&known::RADAR).phase,
        squallar_egui::pane::LoopPhase::FetchingScanList,
        "precondition: the hidden pane must be waiting on a listing",
    );

    let scans = vec![(
        at(0),
        squallar_radar::archive::Identifier::new("KOUN-00".to_string()),
    )];
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: squallar_source::id::known::RADAR,
            listing: FrameListing {
                range,
                frames: vec![FrameStamp {
                    valid: at(0),
                    run: None,
                }],
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: "KOUN".to_string(),
                range,
                scans,
            }),
        })
        .expect("the receiver is alive");

    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();

    let loop_state = app
        .gui
        .pane(1)
        .expect("the fixture built two panes")
        .time_state(&known::RADAR);
    assert_eq!(
        loop_state
            .frames
            .iter()
            .map(|frame| frame.timestamp)
            .collect::<Vec<_>>(),
        vec![at(0)],
        "the hidden pane's loop was scoped to a site it is no longer on, so \
         its own listing answered nothing",
    );
}

/// **A listing for one site does not build a loop a second pane is running on
/// another** — the fixed bug, at the seam where the arrival names no pane.
#[test]
fn a_listing_for_one_site_leaves_another_sites_pane_waiting() {
    use squallar_overlays::render::overlay_state::SourceEvent;
    use squallar_source::time::{FrameListing, FrameStamp};

    const KOUN: squallar_radar::sites::RadarSite = squallar_radar::sites::RadarSite {
        name: "KOUN",
        network: squallar_radar::sites::RadarNetwork::of_id("KOUN"),
        lat: 35.23,
        lon: -97.46,
        heights: None,
    };
    fn at(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(i64::from(minute))
    }

    let mut app = n_pane_app(1, "KOUN");
    let span_secs = 600u64;
    let range = (at(0) - chrono::Duration::seconds(span_secs as i64), at(0));
    let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
    pane.scan_info = Some(scan_info_for("KOUN"));
    *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
        span_secs,
        &KOUN,
        squallar_radar::types::RenderView::PlanView,
    );
    // The very window the listing below covers, so the refusal observed is
    // the SITE guard's and not the window match's.
    pane.time_state_mut(&known::RADAR).asked_range = Some(range);

    // KTLX's listing, arriving while the only pane is on KOUN.
    let scans = vec![(
        at(0),
        squallar_radar::archive::Identifier::new("KTLX-00".to_string()),
    )];
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: squallar_source::id::known::RADAR,
            listing: FrameListing {
                range,
                frames: vec![FrameStamp {
                    valid: at(0),
                    run: None,
                }],
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: "KTLX".to_string(),
                range,
                scans,
            }),
        })
        .expect("the receiver is alive");

    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();

    let loop_state = app
        .gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR);
    assert!(
        loop_state.frames.is_empty(),
        "another site's listing was poured into this pane's frame list",
    );
    assert_eq!(
        loop_state.phase,
        squallar_egui::pane::LoopPhase::FetchingScanList,
        "another site's listing retired this pane's loop, which is still owed \
         a listing of its own",
    );
}

/// **Two panes looping one site with two spans ask two questions, and neither
/// is answered with the other's.**
///
/// The arrival names no pane, so what selects the panes a listing builds is
/// the window it covered. Same site on both, so the site guard cannot be what
/// separates them — only the recorded ask can.
#[test]
fn a_listing_over_one_window_does_not_build_a_loop_asking_about_another() {
    use squallar_overlays::render::overlay_state::SourceEvent;
    use squallar_source::time::{FrameListing, FrameStamp};

    const SITE: squallar_radar::sites::RadarSite = squallar_radar::sites::RadarSite {
        name: "KTLX",
        network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
        lat: 35.33,
        lon: -97.27,
        heights: None,
    };
    fn at(minute: i64) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(2, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(minute)
    }

    let mut app = n_pane_app(2, "KTLX");
    // Pane 0 asks about ten minutes; pane 1 about half an hour.
    for (pane_idx, span_secs) in [(0usize, 600u64), (1, 1800)] {
        let pane = app
            .gui
            .pane_mut(pane_idx)
            .expect("the fixture built two panes");
        pane.scan_info = Some(scan_info_for("KTLX"));
        *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
            span_secs,
            &SITE,
            squallar_radar::types::RenderView::PlanView,
        );
        pane.time_state_mut(&known::RADAR).asked_range =
            Some((at(-((span_secs / 60) as i64)), at(0)));
    }

    let range = (at(-10), at(0));
    let scans: Vec<(chrono::NaiveDateTime, squallar_radar::archive::Identifier)> = [-8i64, -4, 0]
        .iter()
        .map(|&m| {
            (
                at(m),
                squallar_radar::archive::Identifier::new(format!("KTLX{m}")),
            )
        })
        .collect();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: squallar_source::id::known::RADAR,
            listing: FrameListing {
                range,
                frames: scans
                    .iter()
                    .map(|(valid, _)| FrameStamp {
                        valid: *valid,
                        run: None,
                    })
                    .collect(),
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: "KTLX".to_string(),
                range,
                scans: scans.clone(),
            }),
        })
        .expect("the receiver is alive");

    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();

    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built two panes")
            .time_state(&known::RADAR)
            .frames
            .len(),
        scans.len(),
        "the pane that asked about this window was not built from it",
    );
    assert!(
        app.gui
            .pane(1)
            .expect("the fixture built two panes")
            .time_state(&known::RADAR)
            .frames
            .is_empty(),
        "a ten-minute listing was poured into a loop asking about half an hour",
    );
    assert_eq!(
        app.gui
            .pane(1)
            .expect("the fixture built two panes")
            .time_state(&known::RADAR)
            .phase,
        squallar_egui::pane::LoopPhase::FetchingScanList,
        "the half-hour loop was retired by a listing that never answered it",
    );
}
