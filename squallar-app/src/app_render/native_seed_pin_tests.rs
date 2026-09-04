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

/// This crate's own `App::handle_resized`, which prints the geometry readback
/// for every surface CHANGE.
const APP_RS: &str = include_str!("../app.rs");

/// `AppState::new`, which prints the surface the app OPENED at. The other half
/// of the readback: a resize line cannot describe a window that was never
/// resized, and refusing that leg threw away the correct case.
const APP_STATE_RS: &str = include_str!("../app_state.rs");

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
/// Not cosmetic. The whole-picture overlay raster's size follows the surface,
/// so a runner default that drifted from the app's own default would make
/// every native leg resize its window at boot, and a resize that raced would
/// produce exactly the pair of incomparable rows (17,971,200 B against
/// 43,344,000 B) this protocol was written to stop.
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

/// The surface check reads the picture sizes this app reports; it does not
/// model them.
///
/// It used to encode `(W * 1.5) * ((H - 40) * 1.5) * 4`, and this test held
/// that as "the picture size this app's overdraw and top bar actually
/// produce". It was — on 2026-08-31, at three surfaces, to the byte — because
/// those legs ran at display scale 1.0, where a point is a pixel. The bar
/// lays out at 40 points then and now; what the model has no term for is the
/// scale factor. A headed X11 leg on 2026-09-02 ran at 13/12, which puts the
/// bar at 43.33 px: a one-pane leg at the default window uploaded 2880x1555
/// pictures (17,913,600 B) against the model's 17,971,200 B, and every
/// multi-pane native row read INVALID against a figure the app no longer
/// produced. A figure in points read as a figure in pixels. What is pinned now is the mechanism that
/// replaced it: the analyser scrapes the app's `overlay pictures:` line
/// (`budget_telemetry::overlay_pictures_line`) in the spelling the app prints
/// — prefix, field order, and an empty `px=` at `n=0` — every group
/// mandatory, and the model is gone.
#[test]
fn the_surface_check_reads_the_apps_own_picture_sizes() {
    assert!(
        NATIVE_ROW.contains(
            r#"OVERLAY_PICTURES_RE = re.compile(r"overlay pictures: n=(\d+), px=((?:\d+x\d+(?:;\d+x\d+)*)?), bytes=(\d+)")"#
        ),
        "native_row.py no longer scrapes the app's `overlay pictures:` line in \
         the spelling the app prints. That line is what the surface check \
         compares a bracket's uploads against; without it every native row \
         reads UNCHECKED and no native surface is ever confirmed",
    );
    assert!(
        !NATIVE_ROW.contains("int((h - 40) * 1.5)"),
        "native_row.py models the overlay picture from a 40-point top bar \
         again. 40 is the bar in points, and a leg's pixels are that times a \
         display scale the harness never sees; the model was exact only \
         while every leg ran at scale 1, and then refused every multi-pane \
         row against a picture the app no longer drew",
    );
}

/// **The geometry readback both rig halves take is the line this app prints.**
///
/// The window manager's answer is not portable and is not the same quantity:
/// `xdotool` answers on X, System Events answers on macOS, *nothing* answers on
/// Wayland, and all three describe a frame while the picture-bytes formula
/// predicts a **surface**. The app's own `Window resized to WxH` is the surface,
/// and it exists wherever the app runs — so it is what the byte cross-check is
/// taken against and what the runner's geometry solver converges on.
///
/// That makes a log line load-bearing for a *measurement*, which is the shape
/// that has already failed silently here once: a pattern drifts, the scrape
/// returns nothing, and the leg reports an unconfirmable surface —
/// indistinguishable from "the overlay path never ran". So the formatter and
/// both readers are held together from here, as `drive.py`'s patterns are.
///
/// # Two sentences, because a resize is a change and a surface is a fact
///
/// `Window resized to` fires on a resize EVENT, so an app whose window opens
/// at exactly the size it was asked for never prints it — under a bare X
/// server with no window manager, sizing a window to the size already in force
/// produces no event at all. The rig then read no surface and REFUSED the leg,
/// which is the *correct* case being thrown away; it cost two scene-A legs on
/// 2026-08-31, whose surface the picture-byte formula confirmed by hand
/// immediately afterwards. `Surface configured to` is printed unconditionally
/// where the surface is decided, so the readback exists on every leg, and both
/// readers take the newest of either.
#[test]
fn both_rig_halves_read_the_surface_lines_this_app_prints() {
    assert!(
        APP_RS.contains(r#"log::info!("Window resized to {}x{}", width, height)"#),
        "`App::handle_resized` no longer prints `Window resized to WxH`. That \
         line is the geometry readback the native rig has on every platform: \
         without it a macOS or Wayland leg cannot confirm a resize at all, and \
         an X leg falls back to a window manager that has already been caught \
         reporting a size the app was not rendering at",
    );
    assert!(
        APP_STATE_RS.contains(r#"log::info!("Surface configured to {}x{}", width, height)"#),
        "`AppState::new` no longer prints `Surface configured to WxH`. Without \
         it the only surface readback is a RESIZE, so a window that opens at \
         exactly the requested size reports nothing and the runner refuses the \
         leg that got its geometry right — the false negative that cost two \
         scene-A legs on 2026-08-31",
    );
    assert!(
        NATIVE_ROW.contains(
            r#"SURFACE_RE = re.compile(r"(?:Window resized to|Surface configured to) (\d+)x(\d+)")"#
        ),
        "native_row.py no longer scrapes both of the app's surface sentences, \
         so either the picture-bytes cross-check is back to trusting whatever \
         the runner was told the window was — which is how legs ran at \
         3440x1440 while believing they had asked for 1920x1080 — or a leg \
         that opened at the target reads back no surface at all",
    );
    assert!(
        RUN_NATIVE.contains(r#"grep -E "(Window resized to|Surface configured to) " "$1""#),
        "run_measure_native.sh no longer reads both of the app's surface \
         sentences. Its geometry solver converges by spending the residual \
         between the frame it asked for and the surface it got; with no \
         surface reading it can only ask once and hope, and with only the \
         resize sentence it refuses every leg that needed no resize",
    );
}

/// **A platform this runner does not know degrades by name; it does not exit.**
///
/// This file used to `exit 2` before its first leg if `xdotool` or `xrandr`
/// were missing — which is every macOS box. The consequence was not "no macOS
/// rows": it was **five** lanes each writing a private runner in a scratchpad
/// that died with the session, and no two lanes' native rows guaranteed
/// comparable. A hard preflight refusal is the defect, not the safety.
///
/// What replaced it is a plan: each capability is either present or absent
/// *with a stated reason*, the leg runs either way, and the row carries what it
/// had to do without. The refusals that remain are about the *measurement* —
/// an unconfirmable surface, a loud box — never about the machine.
#[test]
fn the_runner_degrades_by_name_instead_of_refusing_the_platform() {
    assert!(
        RUN_NATIVE.contains(r#""$ROW_PY" plan \"#),
        "run_measure_native.sh no longer resolves a platform plan. Every \
         platform-specific reading it takes — loadavg, window pinning, \
         refresh, CPU time — has a different source on Linux and macOS, and \
         without the plan the only way to handle that is the tool-presence \
         preflight that exited 2 on the wrong platform",
    );
    for tool in ["xdotool", "xrandr"] {
        assert!(
            !RUN_NATIVE.contains(&format!("command -v \"{tool}\"")),
            "run_measure_native.sh preflights `{tool}` again and exits when it \
             is missing. That is the exact gate that made this file useless on \
             macOS and cost five lanes a private reimplementation each; a \
             missing tool must cost the row its columns, not cost the run",
        );
    }
    assert!(
        RUN_NATIVE.contains("--allow-unpinned"),
        "the escape hatch for a leg that cannot reach its geometry is gone. \
         Without it the only options are `refuse` and `fabricate`, and an \
         operator who needs the row anyway will reach for a private runner \
         rather than a flag",
    );

    // macOS renamed the field the refresh is read from, and the old spelling
    // fails SILENTLY — an empty `hz~` column reads exactly like a display that
    // did not report, which is why the previous macOS lane ended up hardcoding
    // `--refresh 175` into a sweep script rather than noticing. Measured on
    // macOS 26.4 (2026-08-31): only `UI Looks like: … @ 175.00Hz` is printed.
    for spelling in ["Refresh Rate", "UI Looks like"] {
        assert!(
            RUN_NATIVE.contains(spelling),
            "run_measure_native.sh no longer reads macOS's `{spelling}` field. \
             The two spellings belong to different macOS releases and reading \
             only one gives a blank hz~ column on the other, with no error",
        );
    }
}

/// **Every row names the commit, and the caller may supply it.**
///
/// The rows that mattered most — a device measured on hardware there is one
/// pass at — printed `commit=unknown`, because the bundle shipped to that
/// machine had no `.git` for `rev-parse` to read. A measurement whose tree is
/// unknown cannot be put in a matrix beside one whose tree is known.
#[test]
fn the_runner_accepts_the_commit_it_cannot_look_up() {
    assert!(
        RUN_NATIVE.contains("--commit)"),
        "run_measure_native.sh no longer accepts `--commit`. On any machine \
         holding only a shipped binary, `git rev-parse` fails and every row \
         from it is stamped `commit=unknown`",
    );
    assert!(
        RUN_NATIVE.contains("--commit \"$COMMIT\""),
        "the commit is accepted but never passed to the analyser, so the ROW \
         line still carries the default",
    );
}

/// **The runner's decisions live in the analyser, where they can be tested.**
///
/// Four of the capabilities lanes kept rebuilding privately are *decisions*,
/// not mechanism: is this box quiet, is this silent process wedged, what order
/// do the legs run in, what may this platform do. A decision spelled in bash
/// can only be checked by running a leg — a display, a GPU, three minutes — so
/// in practice it is never checked, and the five private runners each decided
/// differently. Spelled in `native_row.py` they are pure functions with tests
/// that run anywhere in milliseconds.
///
/// This holds the split: each decision is declared there *and* called from the
/// runner. A decision that drifts back into the shell loses its test without
/// losing its behaviour, which is the change nobody would notice.
#[test]
fn the_runners_decisions_stay_where_they_can_be_tested() {
    for (decision, declared, called, loses) in [
        (
            "the quiet stamp",
            "def quiet_verdict(",
            r#"--load-file "$loadf""#,
            "a leg loud only in its MIDDLE is stamped quiet — the case a \
             start-of-leg gate is blind to, and load's error is one-sided so \
             it biases the ratio rather than adding noise to it",
        ),
        (
            "wedged-vs-working",
            "def cpu_liveness(",
            r#""$ROW_PY" cputime"#,
            "a quiet log is read as a hang and a healthy leg is killed, which \
             has happened",
        ),
        (
            "the leg order",
            "def leg_order(",
            r#""$ROW_PY" order"#,
            "base-first-in-every-pair returns, and a decaying box biases every \
             pair the same way — the one error repetition cannot average out",
        ),
        (
            "the platform plan",
            "def platform_plan(",
            r#""$ROW_PY" plan"#,
            "the tool-presence preflight returns and the wrong platform is an \
             exit again",
        ),
    ] {
        assert!(
            NATIVE_ROW.contains(declared),
            "native_row.py no longer declares {decision}; without it {loses}",
        );
        assert!(
            RUN_NATIVE.contains(called),
            "run_measure_native.sh no longer calls native_row.py for \
             {decision}. Either it stopped making that decision, or it makes \
             it in bash again — where it has no test, and where {loses}",
        );
    }

    // Declared and reachable is not the same as used. `quiet_verdict` is the
    // one the shell reaches only indirectly — it hands over a load file and
    // the analyser decides — so its own call site is checked here rather than
    // in the table above.
    assert!(
        NATIVE_ROW.contains("qv = quiet_verdict(load, quiet_max)"),
        "`build_row` no longer takes its quiet stamp from `quiet_verdict`, so \
         the tested rule and the printed stamp are two different rules",
    );

    // And the load file the shell hands over is sampled THROUGHOUT the leg.
    // A single reading, or two, describes the ends and says nothing about the
    // middle — which is the only interval the stamp is for.
    assert!(
        RUN_NATIVE.contains("while kill -0 \"$pid\" 2>/dev/null; do")
            && RUN_NATIVE.contains("$(plat_loadavg)"),
        "run_measure_native.sh no longer samples load for the duration of the \
         leg. A start-of-leg gate cannot see a compile that begins after it \
         passes, and that compile depresses only the cheaper-frame arm",
    );
}
