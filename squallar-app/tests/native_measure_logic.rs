//! **The native measurement runner's decisions are gated here, from CI.**
//!
//! `run_measure_native.sh` is not a gate and never will be — no millisecond it
//! prints may fail a build. But the runner is made of two different things, and
//! only one of them is a measurement:
//!
//!   * **mechanism** — launch a binary, move a window, sample a file. Checking
//!     it needs a display, a GPU and three minutes, so in practice nobody does.
//!   * **decisions** — is this box quiet, did this surface match, is this
//!     silent process wedged, what order do the legs run in, what may this
//!     platform do. Every one is a pure function over numbers already in hand.
//!
//! The decisions are the half that kept being rebuilt. Five lanes each wrote a
//! private native runner in a scratchpad that died with the session, and
//! because each re-decided these rules from scratch, no two lanes' native rows
//! were guaranteed comparable — the device matrix could not be assembled out of
//! them. So the decisions live in `native_row.py` as pure functions with tests,
//! and this test is what makes those tests *run*.
//!
//! Without this row they were a suite behind a `--selftest` flag that CI never
//! spells, which is a gate in appearance only. The six they cover are the six
//! that were re-derived differently by different lanes:
//!
//!   1. the quiet stamp is taken on the whole leg, not on its ends;
//!   2. the surface cross-check refuses a leg whose picture bytes disagree
//!      with the window it believes it had;
//!   3. divergence is adjudicated on unbinned counts, never binned
//!      percentiles;
//!   4. counterbalanced legs share a mean position;
//!   5. a platform without a tool degrades by name rather than exiting;
//!   6. an unreadable CPU-time reading is not a verdict;
//!   7. two rows that did not measure the same build are not a comparison.
//!
//! **What this does not check.** That the runner drives a real app correctly:
//! that needs a real app, and it is the runner's own output that shows it.
//! What it checks is that the rules those legs are judged by still hold — the
//! failure a passing measurement run cannot see, because a leg judged by a
//! broken rule still produces a number.

use std::path::PathBuf;
use std::process::Command;

/// The analyser, resolved from this file rather than from a working directory:
/// `cargo test` runs with the workspace root as cwd today, and that is a
/// convention rather than a promise.
fn native_row_py() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-app has no parent directory")
        .join(".github/browser-rig/native_row.py")
}

/// The interpreter, by the names a machine that can run the rig at all will
/// have. Deliberately **not** a skip: a gate that quietly stops checking when
/// its tool is absent is the vacuous form this tree has already been bitten by
/// four separate times in one night, and the whole rig — `drive.py`,
/// `serve.py`, `native_row.py` — is python already.
fn python() -> &'static str {
    for name in ["python3", "python"] {
        if Command::new(name).arg("--version").output().is_ok() {
            return name;
        }
    }
    panic!(
        "neither `python3` nor `python` is on PATH. The whole browser rig is \
         python, so this is not a missing optional extra: without it the \
         native measurement protocol's decision rules — the quiet stamp, the \
         surface cross-check, the divergence basis — are unchecked, and they \
         are the rules five separate lanes each got differently"
    );
}

#[test]
fn the_native_measurement_decision_rules_still_hold() {
    let script = native_row_py();
    assert!(
        script.is_file(),
        "the native rig's analyser is not at {}. Every native row's quiet \
         stamp, surface confirmation and divergence verdict comes from it",
        script.display(),
    );

    let out = Command::new(python())
        .arg(&script)
        .arg("selftest")
        .output()
        .expect("could not run native_row.py's selftest");

    // unittest writes its progress and its summary to stderr.
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Non-triviality floor FIRST. A suite that collected nothing exits 0 and
    // prints `Ran 0 tests ... OK`, which would satisfy the success check below
    // while checking nothing at all — the exact shape of a gate that cannot
    // fail. The floor is well under the roster (49 on 2026-08-31) so ordinary
    // growth does not touch it; what it catches is collapse.
    let ran = log
        .lines()
        .find_map(|l| l.strip_prefix("Ran ")?.split_whitespace().next())
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!(
                "native_row.py's selftest printed no `Ran N tests` line, so \
                 there is no evidence it collected any tests:\n{log}"
            )
        });
    assert!(
        ran >= 40,
        "native_row.py's selftest collected only {ran} tests. The roster was \
         49 when this floor was set; a collapse to a handful means the suite \
         stopped loading, and a suite that runs nothing reports OK:\n{log}"
    );

    assert!(
        out.status.success(),
        "native_row.py's selftest failed. These are the native measurement \
         protocol's decision rules — the ones that decide whether a leg's row \
         may be quoted at all — and a broken rule still produces a \
         plausible-looking number:\n{log}"
    );
}

/// The rig's own record of what a browser leg RAN, mapped to where each field
/// is checked — the same shape as `drive.py`'s frame-family table, and for the
/// same reason.
///
/// `drive.py` writes a `binary` block onto every browser leg saying which build
/// it drove. Those fields sat on disk unread: on 2026-09-04 the Mac's installed
/// Firefox moved 155.0 → 155.0.1 mid-campaign, and nothing in the rig would
/// have noticed a pair straddling it.
///
/// **A table, not a naming rule and not a blanket check.** A rule over fields
/// named `*_version` would demand `driver_version` agree, which is false by
/// construction on Firefox — geckodriver 0.37.1 is not Firefox 155, which is
/// why `version_match` is `None` on every Firefox leg — so it would refuse
/// every honest Firefox pair; and it would miss `commit`, which carries no
/// `version` in its name and is the commoner error of the two. A blanket
/// "something somewhere compares builds" is satisfiable by anything.
///
/// Each table row also PINS THE SHAPE its value is recorded in, which is a
/// stronger gate than equality: a field that quietly changes shape can go on
/// comparing equal while every comparison across the change is invalid. The
/// rig has already had to work around that one level down — an app line keeps
/// `since_boot=N us` as a literal token so prefix matches survive a change to
/// the rest of the sentence.
///
/// Because the table names what must be PRESENT, two absent fields cannot
/// satisfy a match. That is the property this suite keeps having to relearn: a
/// zero beside siblings at 14, 7 and 5 is a finding; the same zero alone is a
/// story.
#[test]
fn every_recorded_build_identity_field_is_claimed_by_the_subject_table() {
    let drive = std::fs::read_to_string(
        native_row_py()
            .parent()
            .expect("no rig dir")
            .join("drive.py"),
    )
    .expect("drive.py is not readable");
    let analyser = std::fs::read_to_string(native_row_py()).expect("analyser unreadable");

    // Claim -> the `SUBJECT_FIELDS` entry that checks it, or None with the
    // reason it is not a subject. A field is not allowed to be silently
    // neither.
    let claimed: &[(&str, Option<&str>, &str)] = &[
        (
            "browser_version",
            Some("browser_version"),
            "the build under test. Gated between rows naming the same browser; across firefox and \
             chromium a difference is the comparison",
        ),
        (
            "driver_version",
            Some("driver_version"),
            "reported on the pair, never gated: geckodriver versions independently of Firefox",
        ),
        (
            "binary",
            None,
            "a filesystem path. Two boxes install the same build at different paths and one box \
             installs different builds at the same path, so it identifies no build",
        ),
        (
            "version_match",
            None,
            "browser-against-driver WITHIN one leg, not one leg against another. It is `None` on \
             every Firefox leg by construction, so a pair check on it would refuse Firefox and \
             pass nothing",
        ),
    ];

    // Enumerated from drive.py rather than listed: the dict literals that
    // record what was driven are the ones that mention `browser_version`.
    let mut recorded: Vec<String> = Vec::new();
    for opener in ["info = {", "return {"] {
        let mut from = 0usize;
        while let Some(at) = drive[from..].find(opener) {
            let start = from + at + opener.len() - 1;
            let mut depth = 0i32;
            let mut end = start;
            for (i, c) in drive[start..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = start + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let body = &drive[start..end];
            if body.contains("\"browser_version\"") {
                let mut rest = body;
                while let Some(q) = rest.find('"') {
                    let after = &rest[q + 1..];
                    match after.find('"') {
                        Some(close) => {
                            let key = &after[..close];
                            if after[close + 1..].starts_with(':')
                                && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                                && !key.is_empty()
                                && !recorded.iter().any(|k| k == key)
                            {
                                recorded.push(key.to_string());
                            }
                            rest = &after[close + 1..];
                        }
                        None => break,
                    }
                }
            }
            from = start + 1;
        }
    }
    recorded.sort();

    // Non-vacuity FIRST. A parse that found nothing would pass every
    // assertion below while checking no field at all — the shape of a gate
    // that cannot fail, which this tree has been bitten by repeatedly.
    assert!(
        recorded.len() >= 4,
        "only {} build-identity fields were parsed out of drive.py ({recorded:?}); the parse broke \
         and this gate would otherwise pass vacuously",
        recorded.len(),
    );
    assert!(
        recorded.iter().any(|k| k == "browser_version"),
        "drive.py no longer records `browser_version` under that spelling; the browser build is \
         what moved under a live campaign on 2026-09-04 and this is the field that catches it: \
         {recorded:?}",
    );

    for field in &recorded {
        let claim = claimed
            .iter()
            .find(|(name, _, _)| name == field)
            .unwrap_or_else(|| {
                panic!(
                    "drive.py records `{field}` to describe the browser a leg ran, and this table \
                     does not say whether it is part of a measurement's SUBJECT. Claim it here — \
                     either name the `SUBJECT_FIELDS` entry that checks it, or say why it \
                     identifies no build. An unclaimed identity field is one nothing compares, \
                     which is how a browser update crosses a before/after pair in silence"
                )
            });
        if let Some(entry) = claim.1 {
            assert!(
                analyser.contains(&format!("\"{entry}\", _read_")),
                "`{field}` is claimed to be checked by native_row.py's `SUBJECT_FIELDS` entry \
                 `{entry}`, and no such entry exists",
            );
        }
    }
    assert_eq!(
        recorded.len(),
        claimed.len(),
        "this table claims fields drive.py no longer records; drive.py has {recorded:?}",
    );

    // Equality is not the whole gate. Each row pins the spelling its value is
    // recorded in, so a reshaped field stops carrying a verdict instead of
    // going on comparing equal.
    assert!(
        analyser.contains("def misshapen("),
        "native_row.py's subject table no longer pins the SHAPE of the values \
         it compares. Equality alone cannot see a field that changed shape: \
         two reshaped values compare equal while nothing reading the old \
         spelling is reading a build at all",
    );

    // The other half of a measurement's subject, and the commoner error: the
    // app commit. It is not in drive.py's `binary` block because a native leg
    // drives no browser — it is stamped per ARM by `run_measure_native.sh`.
    assert!(
        analyser.contains("\"commit\", _read_commit"),
        "native_row.py's `SUBJECT_FIELDS` no longer claims `commit`. A before/after pair on two \
         app commits is the error this campaign hits most, and three distinct bases were in play \
         on 2026-09-03 alone",
    );
    let runner = std::fs::read_to_string(
        native_row_py()
            .parent()
            .expect("no rig dir")
            .join("run_measure_native.sh"),
    )
    .expect("run_measure_native.sh is not readable");
    assert!(
        runner.contains("--commit \"$leg_commit\""),
        "run_measure_native.sh no longer stamps a per-arm commit on its rows, so the subject check \
         has nothing to read",
    );
}
