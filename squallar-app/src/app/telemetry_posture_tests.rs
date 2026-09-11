//! The posture row's rendered text, and its field count.
//!
//! The field count is pinned here and nowhere else. The log-line enumeration
//! gates on main key on the prefix ahead of the first colon, so they are blind
//! to a fourth field appearing on an existing row — a reader whose regex ends
//! in a greedy tail would swallow it silently. The producer side is the only
//! place that can catch it.

use super::telemetry_posture_line;

/// Every combination, spelled out. A table rather than a loop over booleans
/// because the thing under test is the literal text a reader on the far side
/// of a console scrape matches, and a loop that built the expectation from the
/// same `format!` would assert nothing.
#[test]
fn the_posture_row_renders_one_digit_per_gate() {
    for (frame, raster, store, want) in [
        (
            false,
            false,
            false,
            "telemetry posture: frame=0 raster=0 store=0",
        ),
        (
            true,
            false,
            true,
            "telemetry posture: frame=1 raster=0 store=1",
        ),
        (
            false,
            true,
            true,
            "telemetry posture: frame=0 raster=1 store=1",
        ),
        (
            true,
            true,
            true,
            "telemetry posture: frame=1 raster=1 store=1",
        ),
    ] {
        assert_eq!(
            telemetry_posture_line(frame, raster, store),
            want,
            "the posture row's text moved; a rig reader matches it literally \
             and reads `null` for a leg, not an error"
        );
    }
}

/// The gate no enumeration test can be: a *fourth* field.
///
/// A reader that has to be told about a new field will not be, so the
/// producer refuses to grow one quietly. If a field is genuinely wanted, this
/// number changes in the same commit as the reader.
#[test]
fn the_posture_row_carries_exactly_three_fields() {
    let line = telemetry_posture_line(true, false, true);
    let head = "telemetry posture: ";
    let tail = line
        .strip_prefix(head)
        .expect("the posture row's head is the part a reader anchors on");

    let fields: Vec<&str> = tail.split(' ').collect();
    assert_eq!(
        fields.len(),
        3,
        "the posture row grew or lost a field ({tail:?}); the enumeration \
         gates key on the prefix before the first colon and cannot see this, \
         so a reader on the far side would silently keep matching a shape \
         that no longer holds"
    );
    assert_eq!(
        fields
            .iter()
            .map(|f| f.split('=').next().expect("a non-empty field has a head"))
            .collect::<Vec<_>>(),
        ["frame", "raster", "store"],
        "the posture row's field order or names moved; the order is fixed \
         because a reader positions on it"
    );
    for field in &fields {
        let value = field
            .split_once('=')
            .expect("every posture field is `name=value`")
            .1;
        assert!(
            value == "0" || value == "1",
            "the posture row's {field:?} is not a single digit; `0` means the \
             predicate resolved false and absence of the row means the build \
             predates it, and nothing else may occupy that slot"
        );
    }
}

/// The whole point of the third field: it is `platform.kv().is_some()`, so a
/// process with no store at all says `store=0` and a reader knows neither gate
/// could have been true — rather than chasing an absent key and a wrong value
/// as well.
#[test]
fn no_store_reads_zero_on_every_field() {
    assert_eq!(
        telemetry_posture_line(false, false, false),
        "telemetry posture: frame=0 raster=0 store=0"
    );
}

/// The pins above drive the builder, which proves the text and proves nothing
/// about whether anything says it. A cut whose call site was deleted delivers
/// exactly nothing and every assertion above still passes.
///
/// Source-scraped rather than log-captured on purpose: installing a global
/// logger in this crate's lib test binary is process-global state shared with
/// 1,300 other tests. The two ways this row dies are a deleted call and a
/// migration into `say_telemetry`, and both are visible in the text.
fn app_rs() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app.rs");
    std::fs::read_to_string(&path).expect("squallar-app/src/app.rs is readable")
}

/// Non-comment calls only: a `//` line naming the builder is prose and emits
/// nothing.
fn calls_in_app_rs(needle: &str) -> usize {
    app_rs()
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| !line.trim_start().starts_with("///"))
        .filter(|line| line.contains(needle))
        .count()
}

#[test]
fn something_actually_says_the_posture_row() {
    let calls = calls_in_app_rs("telemetry_posture_line(");
    assert!(
        calls >= 2,
        "the posture row has {calls} non-comment call site(s) in app.rs and \
         needs at least two: its definition and at least one emission. A \
         deleted emission leaves every text pin above green and every leg \
         without the row"
    );
}

/// **Two emissions, and the second is inside `set_config_dir`.**
///
/// Android resolves both gates twice — `App::new` against a bridge with no
/// store, then `set_config_dir` once the Activity has a data path — which is
/// the same reason the site catalogue resolves twice (`squallar-radar`'s
/// `sites.rs` says so at its own second read). So on Android **two emissions
/// is the contract and one is a defect**, and the two failures are
/// indistinguishable in the digits: with only the `App::new` row, a reader
/// taking the last match sees `frame=0 raster=0 store=0` and reads a seed
/// failure, when what actually happened is that `set_config_dir` never ran —
/// a lifecycle bug with a different owner.
///
/// What this gates is the half a producer can see: that the second emission
/// exists and sits in the function only `android_main` calls. That
/// `set_config_dir` was *reached* on a real device is not observable from
/// here and is a reader-side check.
#[test]
fn the_android_resolution_path_says_the_row_too() {
    let code = app_rs();
    let needle = "telemetry_posture_line(";
    let definition = "fn telemetry_posture_line(";

    let mut emissions = Vec::new();
    let mut at = 0;
    while let Some(found) = code[at..].find(needle) {
        let idx = at + found;
        at = idx + needle.len();
        if code[..idx + needle.len()].ends_with(definition) {
            continue;
        }
        let line_start = code[..idx].rfind('\n').map_or(0, |i| i + 1);
        if code[line_start..idx].trim_start().starts_with("//") {
            continue;
        }
        emissions.push(idx);
    }

    assert_eq!(
        emissions.len(),
        2,
        "the posture row has {} emission(s) and the contract is two: one in \
         `App::new` and one after the Android re-resolve in `set_config_dir`. \
         Losing the second makes an Android leg report the pre-store reading \
         as its posture — `frame=0 raster=0 store=0` with the keys correctly \
         seeded, which reads as a seed failure and is not one",
        emissions.len(),
    );

    // The window of `set_config_dir`: from its signature to the next method at
    // the same indent. Spelled this way rather than by line number because a
    // line number is stale the moment anything above it moves.
    let signature = "pub fn set_config_dir(";
    let start = code
        .find(signature)
        .expect("`App::set_config_dir` is where Android re-resolves the gates");
    let end = code[start + signature.len()..]
        .find("\n    pub fn ")
        .map_or(code.len(), |i| start + signature.len() + i);

    assert!(
        emissions.iter().any(|&idx| idx > start && idx < end),
        "neither emission of the posture row is inside `set_config_dir`, so \
         the Android re-resolve says nothing and an Android leg carries only \
         the row from before it had a store"
    );
    assert!(
        emissions.iter().any(|&idx| idx < start),
        "both emissions are inside `set_config_dir`, so the startup row is \
         gone: a leg that dies before any re-resolve — every web and desktop \
         leg — carries no posture at all"
    );
}

/// The load-bearing constraint, gated.
///
/// `say_telemetry(loud, line)` is `log::info!` when `loud` and `log::debug!`
/// otherwise, so routing the posture row through the telemetry seam would
/// silence it exactly when both gates are off — the one case it exists to
/// report. This is the one legitimate row that bypasses the seam, *because* it
/// reports the gate it would otherwise sit behind, and this test is what stops
/// a well-meant tidy-up from deleting it in effect while leaving it in the
/// tree.
///
/// **Asserted positively, over the enclosing statement rather than the line.**
/// A line-scoped "does not also say `say_telemetry`" check reads green on the
/// real migration: rustfmt splits the call across lines, so the seam's name and
/// the builder's never share one. Asked the other way — *what emits this?* — a
/// wrapper the row is passed to has nowhere to hide, because the answer stops
/// being `log::info!`.
#[test]
fn the_posture_row_is_emitted_by_a_bare_log_info_and_not_the_telemetry_seam() {
    // Comment lines out first, so the prose above and at the call site (which
    // names `say_telemetry` on purpose, so a migrator finds this) is not read
    // as code.
    let code: String = app_rs()
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            !t.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let needle = "telemetry_posture_line(";
    let definition = "fn telemetry_posture_line(";
    let mut emissions = 0;
    let mut at = 0;
    while let Some(found) = code[at..].find(needle) {
        let idx = at + found;
        at = idx + needle.len();
        // The definition is not an emission.
        if code[..idx + needle.len()].ends_with(definition) {
            continue;
        }
        emissions += 1;

        // Everything from the previous statement boundary up to this call: the
        // whole expression that emits it, however rustfmt wrapped it.
        let stmt_start = code[..idx].rfind(';').map_or(0, |i| i + 1);
        let emitter = &code[stmt_start..idx];

        assert!(
            emitter.contains("log::info!"),
            "the posture row is emitted by something other than a bare \
             `log::info!`: {emitter:?}. It must not be routed through \
             `say_telemetry` or any wrapper over it — through the seam this \
             row falls to `debug` exactly when both gates are off, which is \
             the case it exists to report"
        );
        assert!(
            !emitter.contains("say_telemetry"),
            "the posture row was routed through the telemetry seam: \
             {emitter:?}"
        );
        assert!(
            !emitter.contains("log::debug!") && !emitter.contains("log::trace!"),
            "the posture row was demoted below `info`: {emitter:?}"
        );
    }

    assert!(
        emissions >= 1,
        "no emission of the posture row was found at all, so this test \
         asserted nothing"
    );
}
