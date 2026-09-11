//! **The browser rig's own executable pins are gated here, from the board.**
//!
//! This is `native_measure_logic.rs` for the other arm, and it exists because
//! the two arms were gated asymmetrically in the direction that hides
//! failures.
//!
//! `native_row.py` reads per-family telemetry lines by SHAPE, so a new family
//! is picked up unasked and the native arm keeps working. `drive.py` reads
//! them by NAMED pattern and refuses what it was not told about. So the arm
//! that silently adapts had a row on the workspace board
//! (`native_measure_logic.rs`) and the arm that fails loudly had none — and
//! Firefox governs over Chrome on the web target, so the ungated one was the
//! governing one.
//!
//! **What that cost, measured rather than imagined.** `0216ecaf0` added
//! `("frame_content_all", "content")` to `FrameLineWatcher.poll` so the new
//! cuts are ingested, and did not add `"content:"` to
//! `WINDOW_FAMILY_PREFIXES`, which is what decides whether an ingested family
//! is windowed. It landed with a green `cargo test --workspace`, a clean
//! `land-check.sh`, a worktree sweep and five tampers. `drive.py --selftest`
//! exited 1 on that tree, and both `run_measure.sh:311` and
//! `run_tier2.sh:323` run it before they do anything at all, so every web leg
//! on main — the Tier-2 behavioural gate included — refused to start. Nothing
//! said so until an unrelated lane ran the script by hand, and `ea372b597`
//! repaired it one token wide.
//!
//! **Why executing it rather than asserting over its text.** The board
//! already carries three static gates over `drive.py`'s source —
//! `rig_verdict_binding.rs` and `rig_android_engines.rs` here,
//! `app_render::rig_js_tests` in the crate — and all three were green on
//! `0216ecaf0`. They are the right shape for a BINDING between two files; they
//! cannot see a rule the driver decides at runtime without re-deriving that
//! rule in Rust, which is the duplication `native_measure_logic.rs` exists to
//! avoid. The rules live in the rig's own selftests; these rows are what make
//! those selftests RUN.
//!
//! **No new dependency.** `cargo test --workspace` already executes Python:
//! `native_measure_logic.rs` shells out to `native_row.py` and deliberately
//! PANICS rather than skipping when no interpreter is on PATH. These rows take
//! the same position for the same reason, so a machine that can run the board
//! today can run these.
//!
//! **Cost, on `ea372b597`, this box, three consecutive runs:** `drive.py
//! --selftest` 5.99 / 5.95 / 5.91 s wall over 78 `[self-test] ok` pins;
//! `run_tier2.sh --selftest` 6.06 s wall, of which its own eleven bash checks
//! are ~0.1 s — the rest is the `drive.py` selftest it runs first at line 323.
//! Both are pure CPU with no network, no browser and no display; the two rows
//! run concurrently inside this binary, so the board pays about six seconds of
//! wall, not twelve.
//!
//! **What these do not check.** That the rig drives a real browser correctly:
//! that needs a browser, and `run_tier2.sh` without `--selftest` is where it
//! is checked. What these check is that the rig can start at all, and that the
//! rules its legs are judged by still hold — the failure a green board cannot
//! otherwise see, because the web legs were never on it.

use std::path::PathBuf;
use std::process::Command;

/// The rig directory, resolved from this file rather than from a working
/// directory: `cargo test` runs with the workspace root as cwd today, and that
/// is a convention rather than a promise. Same resolution as
/// `native_measure_logic.rs`.
fn rig_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-app has no parent directory")
        .join(".github/browser-rig")
}

/// The interpreter, by the names a machine that can run the rig at all will
/// have. Deliberately **not** a skip, and the same position
/// `native_measure_logic.rs` takes: a gate that quietly stops checking when
/// its tool is absent is the vacuous form this tree has been bitten by
/// repeatedly, and the whole rig — `drive.py`, `serve.py`, `native_row.py` —
/// is python already.
fn python() -> &'static str {
    for name in ["python3", "python"] {
        if Command::new(name).arg("--version").output().is_ok() {
            return name;
        }
    }
    panic!(
        "neither `python3` nor `python` is on PATH. The whole browser rig is \
         python, so this is not a missing optional extra: without it the web \
         arm's driver cannot start, and the pins that decide whether a browser \
         leg's reading may be quoted at all are unchecked"
    );
}

/// **The web driver's own pins still hold, so a web leg can start.**
///
/// This is the exact invocation `run_measure.sh:311` and `run_tier2.sh:323`
/// make before they do anything else. A red here is a red board for every web
/// leg, which is the state main was in for the hour after `0216ecaf0`.
#[test]
fn the_web_drivers_own_pins_still_hold() {
    let script = rig_dir().join("drive.py");
    assert!(
        script.is_file(),
        "the web rig's driver is not at {}. Every browser leg's frame \
         windowing, clock guard and family ingestion comes from it",
        script.display(),
    );

    let out = Command::new(python())
        .arg(&script)
        .arg("--selftest")
        .output()
        .expect("could not run drive.py --selftest");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Non-triviality floor FIRST, and it is not the verdict. `drive.py`'s
    // selftest reports each pin on its own line and its outcome in the exit
    // code, so a suite that stopped loading whole sub-suites would still exit
    // 0 and satisfy the success check below while checking almost nothing.
    // The floor is well under the roster (78 on ea372b597) so ordinary growth
    // does not touch it; what it catches is collapse.
    let pins = log
        .lines()
        .filter(|l| l.starts_with("[self-test] ok "))
        .count();
    assert!(
        pins >= 60,
        "drive.py's selftest reported only {pins} passing pins. The roster was \
         78 when this floor was set; a collapse to a handful means sub-suites \
         stopped loading, and a suite that runs nothing still exits 0:\n{log}"
    );

    // The pin that caught the defect this row exists for is still IN the
    // roster. Present, not passing: on a broken tree it appears as a
    // `[self-test] FAIL` line and the verdict below is what goes red. Deleting
    // the pin would otherwise leave every assertion here green over a driver
    // that no longer checks the thing.
    assert!(
        log.contains("every ingested family is windowed by WINDOW_FAMILY_PREFIXES"),
        "drive.py's selftest no longer carries the pin that every family \
         `FrameLineWatcher.poll` ingests is windowed by \
         `WINDOW_FAMILY_PREFIXES`. That is the pin `0216ecaf0` tripped, and \
         with it gone a family can be ingested and never windowed again — \
         every reading of it reaches the artifact as an absence:\n{log}"
    );

    assert!(
        out.status.success(),
        "drive.py --selftest failed. `run_measure.sh:311` and \
         `run_tier2.sh:323` both run this before they do anything, so while it \
         is red EVERY web leg on this tree refuses to start — the Tier-2 \
         behavioural gate included — and nothing else on the board says \
         so:\n{log}"
    );

    // Last, because it is only reachable on a SUCCESSFUL exit and that is the
    // case it guards: a selftest that returned early, or one whose reporting
    // broke, exits 0 with no verdict beside it. Ahead of the status check it
    // would fire first on an ordinary red and describe a broken instrument
    // instead of a broken pin.
    assert!(
        log.contains("SELFTEST PASS"),
        "drive.py --selftest exited 0 without printing its own verdict. An \
         exit code with no verdict beside it is not evidence the pins \
         ran:\n{log}"
    );
}

/// **The Tier-2 launcher's offline pins still hold.**
///
/// A second, smaller gap found by the same sweep: `run_tier2.sh` has its own
/// `--selftest`, and nothing ran it either. CI's web workflow runs
/// `run_tier2.sh --skip-build` — without `--selftest`, which skips this block
/// entirely — so the branches it covers are checked by no board, ever. They
/// are the branches a passing Tier-2 never executes: the retry path's
/// attempt-1 preservation (only reached after a failure), the `wideloop` seed
/// splice (a silent no-op would run the `long` scene under the `wideloop`
/// name), and the sampler's per-leg canvas header.
///
/// It is a superset of the row above — it runs `drive.py --selftest` first, at
/// line 323 — and is kept as a separate row anyway, because the eleven bash
/// checks that follow are ~0.1 s of the 6.06 s and a red in one half should
/// not be reported as the other.
///
/// The script hardcodes `PY=python3` and does not read an override, so this
/// row requires `python3` specifically where the row above accepts `python`.
/// That is the runner's own requirement being asserted, not a new one.
#[test]
fn the_tier2_launchers_offline_pins_still_hold() {
    let script = rig_dir().join("run_tier2.sh");
    assert!(
        script.is_file(),
        "the Tier-2 launcher is not at {}. It is the web target's behavioural \
         gate",
        script.display(),
    );

    let out = Command::new("bash")
        .arg(&script)
        .arg("--selftest")
        .output()
        .expect("could not run run_tier2.sh --selftest");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Non-triviality floor FIRST, over this script's OWN checks. `st_chk`
    // prints `  ok   <name>` per check; the `[self-test]` lines above them
    // belong to drive.py and are the other row's subject. Ten on ea372b597.
    let checks = log.lines().filter(|l| l.starts_with("  ok   ")).count();
    assert!(
        checks >= 8,
        "run_tier2.sh --selftest reported only {checks} of its own passing \
         checks. There were 10 when this floor was set, and they cover the \
         branches no passing Tier-2 run executes — the retry path's \
         preservation and the per-leg canvas header. A count of ZERO usually \
         means the run never reached its own block at all: this script runs \
         `drive.py --selftest` at line 323 and exits 1 on its red, which the \
         companion row in this file reports directly:\n{log}"
    );

    assert!(
        out.status.success(),
        "run_tier2.sh --selftest failed. Every branch it covers is one a green \
         Tier-2 run never reaches, so its regression is otherwise silent — \
         which is how the attempt-1 overwrite survived as long as it \
         did:\n{log}"
    );

    // Last, and only reachable on a successful exit: the script's own summary
    // names its own denominators, so an exit 0 with no summary beside it is a
    // run that returned before counting anything.
    assert!(
        log.contains("run_tier2 SELFTEST PASS"),
        "run_tier2.sh --selftest exited 0 without printing its own verdict \
         line, which is where its check count is reported:\n{log}"
    );
}
