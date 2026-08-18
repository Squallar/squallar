//! Tier-1 browser gate: the first wasm tests in this repository's history.
//!
//! Run inside a real browser by `wasm-pack test --headless --firefox` and
//! `--chrome` (`.github/workflows/web.yaml`, `tier1` job). Headless is fine
//! here -- none of these needs WebGL.
//!
//! Scope is deliberately exactly these four tests. The wasm-bindgen-test
//! harness never serves `worker.js` + `pkg/`, so the real rasterization
//! worker cannot spawn here; the real spawn/HELLO handshake and the
//! doctored-token respawn are Tier 2's assertions
//! (`.github/browser-rig/run_tier2.sh` against the full served PWA).

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

/// Frame 1 always uploads the font atlas, so a non-empty `textures_delta.set`
/// is a concrete a-frame-really-ran witness -- a completion without it would
/// prove only that nothing panicked. (Mirrors the host-side precedent in
/// `rustdar-egui/src/ui_map/tests.rs`.) The second frame proves the first
/// left the `Gui` in a state that can run again.
#[wasm_bindgen_test]
fn the_first_egui_frame_ever_executed_on_wasm() {
    let mut gui = rustdar_egui::Gui::new();
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
/// ArrayBuffer-transfer machinery -- a `MessageChannel`, not a mock: encode,
/// post with the buffer in the transfer list, receive on the far port,
/// decode, compare.
///
/// The mid-flight assertion is that the sender's buffer is detached
/// (`byteLength == 0`) after the post: the transfer MOVED the bytes, the
/// zero-copy contract the worker wire depends on.
#[wasm_bindgen_test]
async fn job_request_bytes_survive_a_real_message_channel_transfer() {
    use rustdar_frontend::offload::JobRequest;
    use wasm_bindgen::{JsCast, JsValue, closure::Closure};

    let archive: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
    let request = JobRequest::Decode {
        archive: std::sync::Arc::new(archive.clone()),
    };
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

    // `from_bytes` returning `None` is the clean-refusal contract for bytes
    // from ANOTHER build; for bytes this build just encoded it is a failed
    // test, never a refusal to tolerate.
    let decoded = JobRequest::from_bytes(&back)
        .expect("this build's own bytes must decode; None here is a broken codec");
    match decoded {
        JobRequest::Decode { archive: got } => assert_eq!(*got, archive),
        other => panic!("decoded to the wrong variant: {other:?}"),
    }
}

/// The build-token compare reads REAL JS values: a token written onto a
/// `js_sys::Object` with `set_field` reads back verbatim through
/// `string_field` (the exact helpers `worker_port::handle_message` reads a
/// HELLO with), and a doctored token compares unequal -- the `theirs != ours`
/// branch that terminates and respawns the worker.
///
/// Both shapes `build_token` can yield ride the same path: CI-shaped
/// (`version/sha`) and dev-shaped (`version/wire-<digest>`),
/// equal-compares-equal and ANY single-character doctoring compares unequal.
/// (The real `build_token()` above already exercises whichever shape this
/// run was built under; the two literals are the deterministic pair.)
#[wasm_bindgen_test]
fn the_token_compare_reads_real_js_values() {
    use rustdar_web::worker_protocol::{TOKEN, build_token, set_field, string_field};
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

/// A config value stored through [`LocalStorageConfigStore`] lands under the
/// raw browser key exactly `rustdar.ui` -- read back through
/// `window.localStorage` directly, which pins `storage_key`'s prefix mapping
/// against the real `Storage` object rather than against itself.
///
/// [`LocalStorageConfigStore`]: rustdar_web::config_store::LocalStorageConfigStore
#[wasm_bindgen_test]
fn local_storage_round_trips_through_the_config_store() {
    use rustdar_egui::config_store::ConfigStore;

    let store = rustdar_web::config_store::LocalStorageConfigStore::new()
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
        .get_item("rustdar.ui")
        .expect("get_item");
    assert_eq!(
        raw.as_deref(),
        Some(sentinel),
        "the raw browser key must be exactly `rustdar.ui`; anything else orphans every saved layout"
    );
}
