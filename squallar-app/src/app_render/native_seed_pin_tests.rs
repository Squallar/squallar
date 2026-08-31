//! **The native rig seeds the files this app actually reads.**
//!
//! `run_measure.sh` seeds a scene as `localStorage` entries named
//! `squallar.<key>`; `run_measure_native.sh` seeds the same scene by writing
//! `<key>.json` into a redirected `XDG_CONFIG_HOME`, and the app reads it back
//! through `FileKvStore`, whose `path_for` is `dir.join("{key}.json")`.
//!
//! So one scene passes through four places in three languages, and nothing
//! else connects them:
//!
//!   1. the seeded key, in `run_measure.sh` (shell),
//!   2. the prefix strip, in `native_row.py` (python),
//!   3. the on-disk name, in `squallar/src/kv.rs` (rust, another crate),
//!   4. the key constant this module's parent declares (rust, here).
//!
//! A drift anywhere in that chain **fails silently and in the worst
//! direction**: the seed lands under a name nothing reads, the app boots on
//! its defaults with telemetry OFF, and the leg produces a row describing a
//! scene it never ran. Nothing is missing from the output — the numbers are
//! simply of something else. That is strictly worse than a crash, and it is
//! why this is pinned rather than trusted.
//!
//! Read out of the other files rather than restated, on the same terms
//! `raster_telemetry_line_tests` reads `drive.py`: a copy of a literal is a
//! second place for it to be wrong.

use squallar_device_profile::constants::{RENDER_HEIGHT, RENDER_WIDTH};

/// The web rig, read at compile time so a moved or deleted file is a build
/// failure rather than a skipped test.
const RUN_MEASURE: &str = include_str!("../../../.github/browser-rig/run_measure.sh");

/// The native rig's analyser, which owns the prefix strip.
const NATIVE_ROW: &str = include_str!("../../../.github/browser-rig/native_row.py");

/// The native rig's runner, which owns the launch environment.
const RUN_NATIVE: &str = include_str!("../../../.github/browser-rig/run_measure_native.sh");

/// The key/value store the seeded files land in — a different crate, so this
/// is the only way to hold its filename rule from here.
const KV_RS: &str = include_str!("../../../squallar/src/kv.rs");

/// The `squallar.`-prefixed keys scene A's seed sets, prefix stripped.
///
/// Scene A is the one that seeds every switch the rig uses (both telemetry
/// keys and the gesture script), so it is the widest single seed to pin.
fn scene_a_keys() -> Vec<String> {
    let at = RUN_MEASURE.find("    A) echo '{\"squallar.ui\"").expect(
        "run_measure.sh no longer defines scene A's seed in the shape the \
             native runner reuses; the scenes moved and the native rig is \
             seeding something else",
    );
    let line_end = RUN_MEASURE[at..]
        .find('\n')
        .map(|e| at + e)
        .unwrap_or(RUN_MEASURE.len());
    let line = &RUN_MEASURE[at..line_end];

    let mut keys = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("\"squallar.") {
        let tail = &rest[i + "\"squallar.".len()..];
        let end = tail
            .find('"')
            .expect("an unterminated seed key in run_measure.sh");
        keys.push(tail[..end].to_string());
        rest = &tail[end..];
    }
    keys
}

/// Every key the web rig seeds is a key this app reads, and the file the
/// native rig writes for it is the file the store looks for.
#[test]
fn the_native_rig_seeds_the_keys_this_app_reads() {
    let keys = scene_a_keys();

    // Non-vacuity first: a parse that found nothing would satisfy every
    // `contains` below without asserting anything at all.
    assert!(
        keys.len() >= 4,
        "only {} seed keys were parsed out of run_measure.sh's scene A; the \
         parse broke and this pin would otherwise pass without checking \
         anything: {keys:?}",
        keys.len(),
    );

    // The app's own constants, by name. A rename here without the same
    // rename in the two rigs is exactly the silent drift this pins against.
    for expected in [
        squallar_egui::UI_CONFIG_KEY,
        super::FRAME_TELEMETRY_KEY,
        super::RASTER_TELEMETRY_KEY,
        super::GESTURE_SCRIPT_KEY,
    ] {
        assert!(
            keys.iter().any(|k| k == expected),
            "the rig no longer seeds `{expected}`. On the web that is a \
             localStorage entry `squallar.{expected}`; on native it is \
             `{expected}.json`. Whatever it seeds instead, this app does not \
             read it, and the leg will measure a default app while reporting \
             the scene's name: {keys:?}",
        );
    }
}

/// The prefix the native seeding strips is the prefix the web store adds.
#[test]
fn the_native_rig_strips_the_prefix_the_web_store_adds() {
    assert!(
        NATIVE_ROW.contains(r#"WEB_KEY_PREFIX = "squallar.""#),
        "native_row.py no longer declares `WEB_KEY_PREFIX = \"squallar.\"`. \
         It derives every native config filename by stripping that prefix \
         from the web rig's seed keys; a different prefix silently produces \
         filenames nothing reads",
    );
    // And the keys it will strip really do carry it.
    let at = RUN_MEASURE
        .find("    A) echo '{\"squallar.ui\"")
        .expect("scene A's seed moved");
    assert!(RUN_MEASURE[at..].starts_with("    A) echo '{\"squallar.ui\""));
}

/// The on-disk name the native rig writes is the name the store reads.
#[test]
fn the_seeded_filename_is_the_one_the_store_looks_for() {
    assert!(
        KV_RS.contains(r#"self.dir.join(format!("{}.json", key))"#),
        "`FileKvStore::path_for` no longer maps a key to `<key>.json`. \
         native_row.py's `seed_files` writes exactly that name, so a change \
         here means the native rig seeds files the app never opens",
    );
    assert!(
        NATIVE_ROW.contains(r#"out[k[len(WEB_KEY_PREFIX):] + ".json"] = v"#),
        "native_row.py no longer builds its seed filenames as `<key>.json`, \
         which is what `FileKvStore::path_for` opens",
    );
}

/// The variable the runner arms the player with is the one the app reads.
#[test]
fn the_runner_arms_the_player_through_the_variable_the_app_reads() {
    assert!(
        RUN_NATIVE.contains("SQUALLAR_GESTURE_SCRIPT="),
        "run_measure_native.sh no longer sets `SQUALLAR_GESTURE_SCRIPT`; the \
         gesture player would not arm, the leg would log no markers, and \
         every window figure is bracketed by those markers",
    );
}

/// The window the runner pins by default is the window the app opens.
///
/// Not cosmetic. The whole-picture overlay raster is a pure function of the
/// surface — `(W * 1.5) * ((H - 40) * 1.5) * 4` — so a runner default that
/// drifted from the app's own default would make every native leg resize its
/// window at boot, and a resize that raced would produce exactly the pair of
/// incomparable rows (17,971,200 B against 43,344,000 B) this protocol was
/// written to stop.
#[test]
fn the_runner_pins_the_window_the_app_actually_opens() {
    let expected = format!("GEOM=\"{RENDER_WIDTH}x{RENDER_HEIGHT}\"");
    assert!(
        RUN_NATIVE.contains(&expected),
        "run_measure_native.sh's default geometry is no longer {expected}, \
         which is the app's own RENDER_WIDTH x RENDER_HEIGHT. Either the app \
         default moved and the runner should follow it, or the runner was \
         changed and every native leg now resizes at boot",
    );
}

/// The picture-bytes formula the surface check refuses legs on is the one
/// this app's overdraw and top bar actually produce.
///
/// Held here because the check lives in python and the constants it encodes
/// live in rust. The three surfaces are the ones verified exact on
/// 2026-08-31 and quoted in `run_measure.sh`'s header.
#[test]
fn the_surface_check_encodes_the_apps_own_picture_size() {
    assert!(
        NATIVE_ROW.contains("return int(w * 1.5) * int((h - 40) * 1.5) * 4"),
        "native_row.py's picture-bytes formula moved. It is what refuses a \
         leg whose window manager silently gave it another size — the failure \
         that ran legs at 3440x1440 while they believed they asked for \
         1920x1080 — and it can only do that while it matches the app",
    );
    // The formula, evaluated here, against the figure the campaign quotes for
    // the app's own default window.
    let (w, h) = (RENDER_WIDTH as u64, RENDER_HEIGHT as u64);
    let bytes = (w * 3 / 2) * ((h - 40) * 3 / 2) * 4;
    assert_eq!(
        bytes, 17_971_200,
        "the app's default window no longer produces the 17,971,200 B \
         picture every native row before today was measured against",
    );
}
