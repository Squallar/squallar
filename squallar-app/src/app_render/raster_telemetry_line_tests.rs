//! **The browser rig reads the lines this app actually writes.**
//!
//! The running-total sentences are the whole readout on the web target:
//! `squallar-gpu`, `squallar-egui` and `squallar-app` count,
//! `App::report_raster_telemetry` says them once a frame, and
//! `.github/browser-rig/drive.py` scrapes the sentence back out of the page's
//! console ring with a regex. Nothing else connects them. They are in two
//! languages, in two directories, and neither one's test suite compiles the
//! other.
//!
//! All four sentences are scraped and are pinned here. The `basemap tiles:`
//! one is the only one the rig **gates** on with a single conjunct
//! (`--expect-basemap-tiles`, `vector_tiles > 0`), so a drift there is not
//! merely a `null` in a report: it is a gate that reads "the basemap decoded
//! nothing" for every run until someone notices. `floor strips:` was the one
//! with no rig probe and so nothing to be pinned against; it gained one with
//! the floor cause counters, and is pinned here in the same shape.
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
//! terms `squallar-web`'s `pwa_assets` reads `worker_port.rs`: a copy of a
//! literal is a second place for it to be wrong.

use squallar_egui::basemap_ledger;
use squallar_egui::overlay_cache::ledger;
use squallar_gpu::egui_renderer::texture_upload::UploadTotals;

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

/// **Every field of all three scraped sentences, with a distinct value in each
/// position.**
///
/// Distinct on purpose: a line whose fields were transposed — `dropped` and
/// `superseded` swapped, `staged` and `blocking` swapped — reads identically
/// to a correct one under any pattern if the numbers agree, and the rig would
/// then report the wrong figure under the right name for as long as nobody
/// looked. There is no repeated value below, and no value that is a prefix of
/// another.
#[test]
fn the_rig_reads_the_lines_the_app_actually_writes() {
    // `inked` is a subset of `pictures` in the real ledger; here it is given a
    // value no other position holds, because what this test pins is the
    // POSITION of each field in the sentence and a plausible value would let a
    // transposition read as correct.
    let rasters = ledger::Totals {
        dispatched: 11,
        arrived: 22,
        pictures: 33,
        picture_bytes: 44_000_000,
        inked: 122,
        shown: 55,
        promoted: 66,
        dropped: 77,
        superseded: 88,
        cancelled: 99,
    };
    assert_eq!(
        super::overlay_raster_line(&rasters),
        rendered(
            &pattern("rasters_re"),
            &[11, 22, 33, 44_000_000, 122, 55, 66, 77, 88, 99],
        ),
        "the `overlay rasters:` line and the rig's own probe for it have \
         drifted. The rig will report the overlay reading as null, which is \
         the same thing it reports when the overlay path never ran at all",
    );

    // `whole` is a routing subset of `blocking` (25 of the 86 blocking bytes
    // moved whole), and the GPU total is the disjoint pair staged + blocking.
    let uploads = UploadTotals {
        deltas: 13,
        whole_bytes: 25,
        bands: 37,
        staged_bytes: 49,
        blocking_bytes: 86,
    };
    // `bytes()` is derived, not a field, so it is named here at its own value
    // rather than trusted to appear: 49 + 86.
    assert_eq!(uploads.bytes(), 135);
    assert_eq!(
        super::texture_upload_line(&uploads),
        rendered(&pattern("uploads_re"), &[13, 135, 25, 37, 49, 86]),
        "the `texture uploads:` line and the rig's own probe for it have drifted",
    );

    // Three more distinct values, and none of them a prefix of another: the
    // rig gates on the FIRST field of this line alone, so a transposition
    // here would make `--expect-basemap-tiles` read the hillshade counter --
    // or the sniff counter, which is expected to be zero on every archive
    // this app opens and would therefore fail every leg for ever.
    let basemap = basemap_ledger::Totals {
        vector_tiles: 17,
        raster_tiles: 29,
        sniffed_tiles: 43,
    };
    assert_eq!(
        super::basemap_tile_line(&basemap),
        rendered(&pattern("basemap_re"), &[17, 29, 43]),
        "the `basemap tiles:` line and the rig's own probe for it have \
         drifted. --expect-basemap-tiles will then read the basemap as \
         null, which it reports identically to a basemap that decoded \
         nothing -- the exact defect this gate was added for",
    );

    // Five more distinct values. The last three overlap `paints` and each
    // other by construction, so a transposition here reads as a plausible
    // line rather than an impossible one -- which is exactly why every
    // position gets a value no other position could hold.
    let floor = squallar_egui::floor_ledger::Totals {
        strip_paints: 101,
        mirror_renders: 202,
        key_moves: 303,
        paints_on_stable_key: 404,
        incomplete_paints: 505,
    };
    assert_eq!(
        super::floor_strip_line(&floor),
        rendered(&pattern("floor_re"), &[101, 202, 303, 404, 505]),
        "the `floor strips:` line and the rig's own probe for it have \
         drifted. The rig reports the floor reading as unknown, which is \
         what it also reports when no 3D floor was ever drawn",
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
    let good = rendered(&pattern("rasters_re"), &[0; 10]);
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

    // The same floor under the basemap pin, on the field the rig gates on:
    // the pattern really names `vector` and would not accept another word.
    let basemap = basemap_ledger::Totals::default();
    let good = rendered(&pattern("basemap_re"), &[0; 3]);
    assert_eq!(super::basemap_tile_line(&basemap), good);
    let drifted = good.replacen(" vector,", " vectors,", 1);
    assert_ne!(
        drifted, good,
        "the substitution produced a sentence with no ` vector,` in it, so \
         the perturbation below is not perturbing anything",
    );
    assert_ne!(
        super::basemap_tile_line(&basemap),
        drifted,
        "a basemap line with a drifted field name compared equal to the real \
         one, so the pin above cannot fail",
    );
}

/// **The rig reads the tile cache line the app actually writes**: the role
/// word and thirteen figures, each at a value no other position holds, for
/// both roles.
///
/// The role is a word group in the pattern's parenthesis and [`rendered`]
/// speaks `(\d+)` only, so the opening is checked by name and the rest of
/// the sentence is rendered on `rendered`'s terms — the honesty check over
/// the numeric part is kept intact rather than widened.
#[test]
fn the_rig_reads_the_tile_cache_line_the_app_actually_writes() {
    use squallar_egui::tile_source::cache_ledger::{CacheRole, ROLES, Totals};

    let totals = Totals {
        requests: 1001,
        restyle_asks: 12,
        refetch_after_eviction: 103,
        puts_first: 904,
        puts_restyle: 15,
        puts_duplicate: 26,
        puts_orphan: 37,
        evicted_pending: 48,
        evicted_resident: 59,
        evicted_bytes: 6_000_060,
        resident_entries: 71,
        resident_bytes: 8_000_082,
        parsed_entries: 93,
    };
    let pattern = pattern("tile_cache_re");
    let head = r"tile cache \(([a-z0-9-]+)\): ";
    assert!(
        pattern.starts_with(head),
        "the rig's tile cache pattern no longer opens with the role word: {pattern:?}",
    );
    let body = rendered(
        &pattern[head.len()..],
        &[
            1001, 12, 103, 904, 15, 26, 37, 48, 59, 6_000_060, 71, 8_000_082, 93,
        ],
    );
    for role in ROLES {
        assert_eq!(
            super::tile_cache_line(role, &totals),
            format!("tile cache ({}): {body}", role.label()),
            "the `tile cache ({}):` line and the rig's own probe for it have \
             drifted. The rig records the reading as absent, which is what it \
             also records for a bundle older than the line",
            role.label(),
        );
    }

    // The floor under the pin above, on the field the settle assertion
    // differences: the pattern really names `refetch after eviction`.
    let good = super::tile_cache_line(CacheRole::Base, &totals);
    let drifted = good.replacen(" refetch after eviction,", " refetches after eviction,", 1);
    assert_ne!(
        drifted, good,
        "the substitution produced a sentence with no ` refetch after eviction,` \
         in it, so the perturbation below is not perturbing anything",
    );
    assert_ne!(
        super::tile_cache_line(CacheRole::Base, &totals),
        drifted,
        "a tile cache line with a drifted field name compared equal to the \
         real one, so the pin above cannot fail",
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
/// config store's; `squallar_web::kv::storage_key` is the prefix between them
/// and its own test pins that.
#[test]
fn the_rig_seeds_the_key_that_makes_the_lines_loud() {
    let seeded = format!("\"squallar.{}\": \"1\"", super::RASTER_TELEMETRY_KEY);
    assert!(
        RUN_TIER2.contains(&seeded),
        "run_tier2.sh no longer seeds {seeded}, so the app writes both \
         running-total lines at `debug`, `console_log` drops them before the \
         console ring, and `--expect-overlay-rasters` reads the overlay path \
         as null -- which is what it reports when that path never ran at all",
    );
}

/// **The basemap gate is actually asked for.**
///
/// A `--expect-basemap-tiles` that `drive.py` defines and `run_tier2.sh` never
/// passes is a flag, not a gate: every leg goes green, the figure is printed,
/// and the reading it exists to force is optional again. This is the same
/// shape as the seed test above — the two halves live in two files and neither
/// one's suite compiles the other — so it is checked the same way, from the
/// side that owns the sentence.
///
/// Both halves are named, because either alone can be wrong: the flag has to
/// exist in the driver's argument parser AND ride a leg in the launcher.
#[test]
fn the_rig_asks_for_the_basemap_reading_it_can_now_take() {
    const FLAG: &str = "--expect-basemap-tiles";
    assert!(
        DRIVE_PY.contains(&format!("\"{FLAG}\"")),
        "drive.py no longer declares {FLAG}, so the `basemap tiles:` line is \
         a number nobody gates on and a basemap that decodes nothing passes \
         Tier-2 again",
    );
    assert!(
        RUN_TIER2.contains(FLAG),
        "run_tier2.sh never passes {FLAG}, so the gate exists and is never \
         asked for -- every leg stays green with a dead basemap, which is the \
         state this whole seam was added to end",
    );
}

/// **The seed-applied gate greps for lines this build really writes.**
///
/// `--expect-seed-applied` is four conjuncts over the console ring, and three
/// of them are `indexOf` against a literal in `drive.py`. Two of those are
/// **absences** — "the timezone fallback was never logged", "no config was
/// refused" — and an absence checked against a string the app no longer writes
/// is satisfied for ever, silently, by the very build it was meant to catch.
/// That is the vacuous shape this repo has already had to delete four
/// instances of, so the substrings are read out of the driver and matched
/// against the format strings here.
///
/// The fourth, `loop state:`, is the positive floor the two absences rest on,
/// and it is checked from `loop_telemetry`'s own sentence rather than restated.
#[test]
fn the_seed_applied_gate_greps_for_lines_this_build_writes() {
    const FLAG: &str = "--expect-seed-applied";
    assert!(
        DRIVE_PY.contains(&format!("\"{FLAG}\"")),
        "drive.py no longer declares {FLAG}",
    );
    assert!(
        RUN_TIER2.contains(FLAG),
        "run_tier2.sh never passes {FLAG}, so the gate exists and is never \
         asked for: a leg that navigated to /index.html, applied no seed and \
         opened on a timezone-derived site goes green again",
    );

    // The localStorage name the driver looks for among the keys the prelude
    // wrote. The app reads under the logical key and `squallar_web::kv` adds
    // the prefix; a rename on either side leaves the gate looking for a key
    // nobody sets, which reads as "the seed never landed" on every leg.
    assert!(
        DRIVE_PY.contains(&format!(
            "UI_CONFIG_SEED_KEY = \"squallar.{}\"",
            squallar_egui::UI_CONFIG_KEY
        )),
        "drive.py's UI_CONFIG_SEED_KEY is not `squallar.{}`",
        squallar_egui::UI_CONFIG_KEY,
    );

    // The three console substrings, each with the source line that writes it.
    // `assert!(contains)` on both sides: the driver must still look for it,
    // and this build must still say it.
    for (needle, written_by) in [
        (
            "nearest to timezone",
            "app.rs / app_render.rs, the only paths reachable when \
             `load_ui_config` returned false",
        ),
        ("Failed to parse config", "ui_config.rs's parse arm"),
        (
            "no radar could be called",
            "ui_config.rs, a seed naming a site this build has no radar for",
        ),
        (
            "loop state:",
            "loop_telemetry.rs, the frame-telemetry period",
        ),
    ] {
        assert!(
            DRIVE_PY.contains(needle),
            "drive.py's SEED_APPLIED_PROBE no longer greps for {needle:?}",
        );
        assert!(
            [APP, APP_RENDER, UI_CONFIG, LOOP_TELEMETRY]
                .iter()
                .any(|source| source.contains(needle)),
            "no source this test reads still writes {needle:?} ({written_by}). \
             The driver greps for it, finds nothing, and -- for the two \
             absence conjuncts -- passes for ever on the exact failure it \
             exists to catch",
        );
    }
}

/// The sources whose log lines `--expect-seed-applied` reads back.
const APP: &str = include_str!("../app.rs");
const APP_RENDER: &str = include_str!("../app_render.rs");
const UI_CONFIG: &str = include_str!("../../../squallar-egui/src/ui_config.rs");
const LOOP_TELEMETRY: &str = include_str!("../loop_telemetry.rs");

/// The floor under the test above: the app really does read that key, and
/// really does refuse anything else.
///
/// Without this, renaming the key on BOTH sides at once would keep the seam
/// test green while the switch pointed at a key nothing sets.
#[test]
fn only_the_seeded_value_makes_the_lines_loud() {
    use squallar_kv::{KvStore, MemoryKvStore};

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
