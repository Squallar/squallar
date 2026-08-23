//! **The browser rig reads the lines this app actually writes.**
//!
//! The two running-total sentences are the whole readout on the web target:
//! `rustdar-gpu` and `rustdar-app` count, `App::report_raster_telemetry` says
//! it once a frame, and `.github/browser-rig/drive.py` scrapes the sentence
//! back out of the page's console ring with a regex. Nothing else connects
//! them. They are in two languages, in two directories, and neither one's test
//! suite compiles the other.
//!
//! **This exact seam has already broken once during this work**, and in the
//! worst possible way: a line-continuation backslash was eaten while writing
//! the source, the Rust string gained eighteen spaces in the middle, every
//! Rust test stayed green, and the rig reported the overlay reading as `null`
//! — which is indistinguishable from "the overlay path never ran", the very
//! reading the gate exists to detect. The failure was silent in both
//! directions at once.
//!
//! So the pattern is read out of `drive.py` rather than restated, on the same
//! terms `rustdar-web`'s `pwa_assets` reads `worker_port.rs`: a copy of a
//! literal is a second place for it to be wrong.

use rustdar_egui::overlay_cache::ledger;
use rustdar_gpu::egui_renderer::texture_upload::UploadTotals;

/// The rig driver, read at compile time so a moved or deleted file is a build
/// failure rather than a skipped test.
const DRIVE_PY: &str = include_str!("../../../.github/browser-rig/drive.py");

/// The body of a `var <name> = /…/;` regex literal in `drive.py`.
fn pattern(name: &str) -> String {
    let head = format!("var {name} = /");
    let at = DRIVE_PY.find(&head).unwrap_or_else(|| {
        panic!(
            "drive.py no longer declares `{head}…`; the rig's probe for this \
             line moved and this test can no longer read it"
        )
    });
    let rest = &DRIVE_PY[at + head.len()..];
    let end = rest
        .find("/;")
        .expect("the regex literal is not closed on its own line");
    rest[..end].to_string()
}

/// The sentence a pattern of literal text and `(\d+)` groups describes, given
/// what each group should capture.
///
/// Deliberately **not** a regex match. A match would answer "the rig could
/// read something", and what is wanted is "the rig reads exactly this" — a
/// pattern that had grown a `.*` would satisfy a match and would quietly stop
/// pinning the sentence. Substituting instead makes the pattern's own literal
/// text the assertion, and the check below that the pattern contains no other
/// metacharacter is what keeps the substitution honest.
fn rendered(pattern: &str, groups: &[u64]) -> String {
    const GROUP: &str = r"(\d+)";
    assert_eq!(
        pattern.matches(GROUP).count(),
        groups.len(),
        "the pattern has {} capture groups and {} values were offered",
        pattern.matches(GROUP).count(),
        groups.len(),
    );
    let bare = pattern.replace(GROUP, "");
    assert!(
        !bare.contains(['\\', '(', ')', '[', ']', '*', '+', '?', '|', '^', '$']),
        "the pattern has a metacharacter outside its `(\\d+)` groups, so \
         substituting the numbers into it no longer produces the sentence it \
         matches and this test would be comparing two different things: {bare:?}",
    );
    let mut out = String::new();
    let mut rest = pattern;
    for value in groups {
        let at = rest.find(GROUP).expect("counted above");
        out.push_str(&rest[..at]);
        out.push_str(&value.to_string());
        rest = &rest[at + GROUP.len()..];
    }
    out.push_str(rest);
    out
}

/// **Every field of both sentences, with a distinct value in each position.**
///
/// Distinct on purpose: a line whose fields were transposed — `dropped` and
/// `superseded` swapped, `staged` and `blocking` swapped — reads identically
/// to a correct one under any pattern if the numbers agree, and the rig would
/// then report the wrong figure under the right name for as long as nobody
/// looked. There is no repeated value below, and no value that is a prefix of
/// another.
#[test]
fn the_rig_reads_the_lines_the_app_actually_writes() {
    let rasters = ledger::Totals {
        dispatched: 11,
        arrived: 22,
        pictures: 33,
        picture_bytes: 44_000_000,
        shown: 55,
        promoted: 66,
        dropped: 77,
        superseded: 88,
    };
    assert_eq!(
        super::overlay_raster_line(&rasters),
        rendered(
            &pattern("rasters_re"),
            &[11, 22, 33, 44_000_000, 55, 66, 77, 88],
        ),
        "the `overlay rasters:` line and the rig's own probe for it have \
         drifted. The rig will report the overlay reading as null, which is \
         the same thing it reports when the overlay path never ran at all",
    );

    let uploads = UploadTotals {
        deltas: 13,
        unbanded_bytes: 25,
        bands: 37,
        staged_bytes: 49,
        blocking_bytes: 61,
    };
    // `bytes()` is derived, not a field, so it is named here at its own value
    // rather than trusted to appear: 25 + 49 + 61.
    assert_eq!(uploads.bytes(), 135);
    assert_eq!(
        super::texture_upload_line(&uploads),
        rendered(&pattern("uploads_re"), &[13, 135, 25, 37, 49, 61]),
        "the `texture uploads:` line and the rig's own probe for it have drifted",
    );
}

/// The floor under the test above: `rendered` really can disagree.
///
/// Without this, a `pattern` that silently returned the sentence itself — or a
/// `rendered` that ignored its argument — would make the equality above hold
/// whatever the app wrote, which is the exact shape of the four vacuous checks
/// this campaign has already caught.
#[test]
fn a_line_that_drifted_by_one_space_is_not_accepted() {
    let rasters = ledger::Totals::default();
    let good = rendered(&pattern("rasters_re"), &[0; 8]);
    assert_eq!(super::overlay_raster_line(&rasters), good);

    let drifted = good.replacen(" B, ", " B,  ", 1);
    assert_ne!(
        drifted, good,
        "the substitution produced a sentence with no ` B, ` in it, so the \
         perturbation below is not perturbing anything",
    );
    assert_ne!(
        super::overlay_raster_line(&rasters),
        drifted,
        "a line with one extra space compared equal to the real one, so the \
         test above cannot fail",
    );
}

/// The rig driver's launcher, read at compile time for the same reason
/// [`DRIVE_PY`] is.
const RUN_TIER2: &str = include_str!("../../../.github/browser-rig/run_tier2.sh");

/// **The rig can still hear the lines it scrapes.**
///
/// The two sentences are `debug` on an ordinary install, and the browser boots
/// `console_log` at `Level::Info`, so on the web target they exist only where
/// this key is seeded. That makes `run_tier2.sh`'s `SEED_LS` load-bearing for
/// `--expect-overlay-rasters` in exactly the way the format string is
/// load-bearing for the regex: rename either side and the rig reports the
/// overlay reading as `null`, which is what it also reports when the overlay
/// path never ran.
///
/// The `localStorage` name is checked and not just the logical key, because
/// the seed is written in `localStorage`'s vocabulary and the app reads in the
/// config store's; `rustdar_web::kv::storage_key` is the prefix between them
/// and its own test pins that.
#[test]
fn the_rig_seeds_the_key_that_makes_the_lines_loud() {
    let seeded = format!("\"rustdar.{}\": \"1\"", super::RASTER_TELEMETRY_KEY);
    assert!(
        RUN_TIER2.contains(&seeded),
        "run_tier2.sh no longer seeds {seeded}, so the app writes both \
         running-total lines at `debug`, `console_log` drops them before the \
         console ring, and `--expect-overlay-rasters` reads the overlay path \
         as null -- which is what it reports when that path never ran at all",
    );
}

/// The floor under the test above: the app really does read that key, and
/// really does refuse anything else.
///
/// Without this, renaming the key on BOTH sides at once would keep the seam
/// test green while the switch pointed at a key nothing sets.
#[test]
fn only_the_seeded_value_makes_the_lines_loud() {
    use rustdar_kv::{KvStore, MemoryKvStore};

    assert!(
        !super::raster_telemetry_is_loud(None),
        "an install with nowhere to persist must not be loud",
    );

    let store = MemoryKvStore::default();
    assert!(
        !super::raster_telemetry_is_loud(Some(&store)),
        "an install that never set the key must not be loud -- this is the \
         default a user is in, and it is the whole point of the switch",
    );

    store
        .store(super::RASTER_TELEMETRY_KEY, "0")
        .expect("the memory store always accepts a write");
    assert!(
        !super::raster_telemetry_is_loud(Some(&store)),
        "an explicit `0` turned the lines on",
    );

    store
        .store(super::RASTER_TELEMETRY_KEY, "1")
        .expect("the memory store always accepts a write");
    assert!(
        super::raster_telemetry_is_loud(Some(&store)),
        "the value the rig seeds did not turn the lines on, so Tier-2 hears \
         nothing",
    );
}

/// **The reading is periodic, and the period is asked rather than waited on.**
///
/// A wall-clock test here would red-gate this file under load for a reason
/// that has nothing to do with the property; `telemetry_is_due` takes both
/// instants so the question can be put directly.
#[test]
fn a_reading_is_due_once_a_period_and_not_once_a_frame() {
    let t0 = web_time::Instant::now();
    assert!(
        super::telemetry_is_due(None, t0),
        "the first reading must go out at once, or a short rig leg hears \
         nothing at all",
    );

    let frame = std::time::Duration::from_micros(16_667);
    let mut n = 0u32;
    let mut at = t0;
    // A whole period of 60 Hz frames, every one of them after a reading taken
    // at `t0`. Under the per-frame reporter this counts 120.
    while at.duration_since(t0) < super::RASTER_TELEMETRY_PERIOD {
        if super::telemetry_is_due(Some(t0), at) {
            n += 1;
        }
        at += frame;
    }
    assert_eq!(
        n, 0,
        "{n} of the frames inside one period would have written a line; the \
         running totals are back to being a per-frame event",
    );
    assert!(
        super::telemetry_is_due(Some(t0), t0 + super::RASTER_TELEMETRY_PERIOD),
        "the frame exactly one period later was still not due, so the last \
         reading of a run can be withheld for ever",
    );
}
