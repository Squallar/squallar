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
//!   6. an unreadable CPU-time reading is not a verdict.
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
