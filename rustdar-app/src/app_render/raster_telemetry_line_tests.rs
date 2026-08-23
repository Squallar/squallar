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
