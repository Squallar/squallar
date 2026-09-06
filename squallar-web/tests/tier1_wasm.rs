//! Tier-1 browser gate: the first wasm tests in this repository's history.
//!
//! Run inside a real browser by `wasm-pack test --headless --firefox` and
//! `--chrome` (`.github/workflows/web.yaml`, `tier1` job).
//!
//! Scope is deliberately exactly these five tests: the wasm-bindgen-test
//! harness never serves `worker.js` + `pkg/`, so the real spawn/HELLO handshake
//! and the doctored-token respawn are Tier 2's (`.github/browser-rig/run_tier2.sh`).
//!
//! The fifth is the canvas's graphics-context listeners, here for the same
//! reason as the others: it needs a real DOM `EventTarget` and a browser that
//! reads `preventDefault()` back, neither of which a host test has. The
//! decision half of that path is `squallar_web::context_loss`, tested on the
//! host beside it.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

/// Frame 1 always uploads the font atlas, so a non-empty `textures_delta.set`
/// is a concrete a-frame-really-ran witness. The second frame proves the first
/// left the `Gui` in a state that can run again.
#[wasm_bindgen_test]
fn the_first_egui_frame_ever_executed_on_wasm() {
    let mut gui = squallar_egui::Gui::new();
    let ctx = egui::Context::default();
    let out = ctx.run_ui(egui::RawInput::default(), |ctx| {
        let _ = gui.ui(ctx);
    });
    assert!(
        !out.textures_delta.set.is_empty(),
        "frame 1 must upload the font atlas; an empty textures_delta means no frame really ran"
    );
    let _second = ctx.run_ui(egui::RawInput::default(), |ctx| {
        let _ = gui.ui(ctx);
    });
}

/// `JobRequest` bytes survive the browser's REAL structured-clone +
/// ArrayBuffer-transfer machinery — a `MessageChannel`, not a mock. The
/// mid-flight assertion is that the sender's buffer is detached
/// (`byteLength == 0`) after the post: the transfer MOVED the bytes.
#[wasm_bindgen_test]
async fn job_request_bytes_survive_a_real_message_channel_transfer() {
    use squallar_worker::offload::JobRequest;
    use wasm_bindgen::{JsCast, JsValue, closure::Closure};

    let archive: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
    // Constructed the way every dispatch site constructs one: a typed input under
    // an envelope. A decode carries no raster geometry, so the envelope is zeroes.
    let request = JobRequest::describe(
        squallar_radar::jobs::DecodeJob {
            archive: std::sync::Arc::new(archive.clone()),
        },
        squallar_source::job::JobGeometry {
            width: 0,
            height: 0,
            bounds: squallar_geo::GeoBounds {
                min_lat: 0.0,
                max_lat: 0.0,
                min_lon: 0.0,
                max_lon: 0.0,
            },
            side_ceiling_px: 0,
        },
    );
    let bytes = request.to_bytes();

    let array = js_sys::Uint8Array::from(bytes.as_slice());
    let buffer = array.buffer();

    let channel = web_sys::MessageChannel::new().expect("MessageChannel::new");
    let port1 = channel.port1();
    let port2 = channel.port2();

    let receiver = port2.clone();
    let reply = js_sys::Promise::new(&mut move |resolve, _reject| {
        let on_message = Closure::once_into_js(move |event: web_sys::MessageEvent| {
            let _ = resolve.call1(&JsValue::NULL, &event.data());
        });
        // The `onmessage` setter implicitly starts the port.
        receiver.set_onmessage(Some(on_message.unchecked_ref()));
    });

    let transfer = js_sys::Array::of1(&buffer);
    port1
        .post_message_with_transferable(&array, &transfer)
        .expect("post_message_with_transferable");
    assert_eq!(
        buffer.byte_length(),
        0,
        "the ArrayBuffer must be MOVED by the transfer (detached buffers read byteLength 0), \
         not copied"
    );

    let received = wasm_bindgen_futures::JsFuture::from(reply)
        .await
        .expect("the transferred message must arrive");
    let received = js_sys::Uint8Array::new(&received);
    let mut back = vec![0u8; received.length() as usize];
    received.copy_to(&mut back[..]);

    // `from_bytes` returning `None` is the clean-refusal contract for bytes from
    // ANOTHER build; here it is a failed test.
    let decoded = JobRequest::from_bytes(&back)
        .expect("this build's own bytes must decode; None here is a broken codec");
    // `JobRequest` equality is value equality down through the type-erased input,
    // so the whole request survived, not just a prefix.
    assert_eq!(
        decoded, request,
        "the decoded request must equal the one encoded before the transfer"
    );
}

/// The build-token compare reads REAL JS values through the exact helpers
/// `worker_port::handle_message` reads a HELLO with, and a doctored token
/// compares unequal. Both shapes `build_token` can yield ride the same path.
#[wasm_bindgen_test]
fn the_token_compare_reads_real_js_values() {
    use squallar_web::worker_protocol::{TOKEN, build_token, set_field, string_field};
    use wasm_bindgen::JsValue;

    let ours = build_token();

    let hello = js_sys::Object::new();
    set_field(&hello, TOKEN, &JsValue::from_str(&ours));
    assert_eq!(
        string_field(&hello, TOKEN).as_deref(),
        Some(ours.as_str()),
        "a token written through set_field must read back verbatim through string_field"
    );

    let doctored = js_sys::Object::new();
    set_field(&doctored, TOKEN, &JsValue::from_str("doctored/0/deadbeef"));
    let theirs = string_field(&doctored, TOKEN).unwrap_or_default();
    assert_ne!(
        theirs, ours,
        "the doctored token must read as a different build, or the respawn path is dead code"
    );

    for shaped in ["1.2.3/abc123sha", "1.2.3/wire-00aa11bb22cc33dd"] {
        let object = js_sys::Object::new();
        set_field(&object, TOKEN, &JsValue::from_str(shaped));
        assert_eq!(
            string_field(&object, TOKEN).as_deref(),
            Some(shaped),
            "a {shaped:?}-shaped token must compare equal to itself through the real JS path"
        );
        for i in 0..shaped.len() {
            let mut doctored: Vec<u8> = shaped.bytes().collect();
            doctored[i] = if doctored[i] == b'x' { b'y' } else { b'x' };
            let doctored = String::from_utf8(doctored).expect("ASCII stays ASCII");
            let object = js_sys::Object::new();
            set_field(&object, TOKEN, &JsValue::from_str(&doctored));
            assert_ne!(
                string_field(&object, TOKEN).unwrap_or_default(),
                shaped,
                "doctoring byte {i} of a {shaped:?}-shaped token must compare unequal"
            );
        }
    }
}

/// A config value stored through [`LocalStorageKvStore`] lands under the raw
/// browser key exactly `squallar.ui`, read back through `window.localStorage`
/// directly.
///
/// [`LocalStorageKvStore`]: squallar_web::kv::LocalStorageKvStore
#[wasm_bindgen_test]
fn local_storage_round_trips_through_the_kv_store() {
    use squallar_kv::KvStore;

    let store = squallar_web::kv::LocalStorageKvStore::new()
        .expect("the test browser must expose localStorage");
    let sentinel = r#"{"tier1":"wasm-gate"}"#;
    store.store("ui", sentinel).expect("store must succeed");
    assert_eq!(
        store.load("ui").as_deref(),
        Some(sentinel),
        "the store must read back what it wrote"
    );

    let raw = web_sys::window()
        .expect("window")
        .local_storage()
        .expect("localStorage accessible")
        .expect("localStorage enabled")
        .get_item("squallar.ui")
        .expect("get_item");
    assert_eq!(
        raw.as_deref(),
        Some(sentinel),
        "the raw browser key must be exactly `squallar.ui`; anything else orphans every saved layout"
    );
}

/// **The canvas's two context listeners are really attached, and the loss one
/// really cancels** — the half of the restore path that no host test can
/// reach.
///
/// `squallar_web::context_loss` is host-tested and covers the once-only
/// delivery and the ask-for-a-frame; what it cannot cover is whether anything
/// is listening on the canvas at all, and whether `preventDefault()` is called
/// where the browser reads it. **That second one is the whole path**: the WebGL
/// spec makes the browser's attempt to restore conditional on the
/// `webglcontextlost` event having been cancelled, so an uncancelled loss is a
/// canvas that stays dead for the life of the tab — and nothing logs it.
///
/// Synthetic events rather than a forced GPU reset: `WEBGL_lose_context` is an
/// optional extension and a real reset is not something a test can ask for, so
/// what is under test is the wiring, dispatched through the browser's own
/// event machinery on a real canvas.
#[wasm_bindgen_test]
fn a_canvas_context_loss_is_cancelled_and_its_restore_reaches_the_bridge() {
    use squallar_app::platform::PlatformBridge;
    use wasm_bindgen::JsCast;

    let canvas = web_sys::window()
        .expect("window")
        .document()
        .expect("document")
        .create_element("canvas")
        .expect("create canvas")
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .expect("a <canvas> element");
    let mut bridge = squallar_web::bridge::WebPlatform::new(canvas.clone());
    assert!(
        !bridge.poll_graphics_restore(),
        "a fresh bridge reports a restore nothing caused",
    );

    let init = web_sys::EventInit::new();
    init.set_cancelable(true);
    let lost = web_sys::Event::new_with_event_init_dict("webglcontextlost", &init)
        .expect("construct a cancelable webglcontextlost");
    canvas.dispatch_event(&lost).expect("dispatch the loss");
    assert!(
        lost.default_prevented(),
        "the loss was not cancelled, so this browser would never offer the \
         page a replacement context and the canvas stays dead",
    );
    assert!(
        !bridge.poll_graphics_restore(),
        "a loss alone was reported as a restore; the app would tear its \
         graphics state down while there is nothing to rebuild onto",
    );

    let restored =
        web_sys::Event::new("webglcontextrestored").expect("construct a webglcontextrestored");
    canvas
        .dispatch_event(&restored)
        .expect("dispatch the restore");
    assert!(
        bridge.poll_graphics_restore(),
        "the restore listener is not attached to the canvas, so a recovered \
         context is never rebuilt onto",
    );
    assert!(
        !bridge.poll_graphics_restore(),
        "the restore was reported twice; the app would tear down the state it \
         had just rebuilt",
    );
}
