//! **Every Tier-2 verdict is bound to the run that produced it, and a rebase
//! cannot quietly undo that.**
//!
//! The launcher and the driver are two files, neither one's suite compiles the
//! other, and the binding between them is spelled in a flag. That is exactly
//! the shape that broke on 2026-08-31: a rebase dropped the rig commit that
//! added `--expect-frame-progress`, `drive.py` rejected the flag, twelve legs
//! died on the argument error in under a second each, and `run_tier2.sh` went
//! on to read whatever JSON was already on disk under each leg's name and
//! reported all twelve as PASS — twelve "independent" three-minute runs, all
//! carrying `total_s=179.18`, identical to the centisecond.
//!
//! The repair added a run id: the launcher mints one per attempt, the driver
//! copies it into its result JSON, and the summary refuses any artefact that
//! does not carry it back. That repair has the *same* two-file shape as the
//! defect, so it is checked the same way this tree already checks the other
//! cross-file rig seams — from the Rust side, at compile time, naming both
//! halves. See `the_rig_asks_for_the_basemap_reading_it_can_now_take` in
//! `squallar-app/src/app_render/raster_telemetry_line_tests.rs` for the
//! original of this pattern.
//!
//! **What these tests do not check.** That the mechanism works: that is the
//! rig's own RED/GREEN, run against a planted stale artefact. What they check
//! is that both halves are still present and still refer to each other, which
//! is the failure mode a rebase produces and a passing browser run cannot see.

/// The launcher and the driver, read at compile time so a deleted file is a
/// build failure rather than a test that silently stops checking.
const RUN_TIER2: &str = include_str!("../../.github/browser-rig/run_tier2.sh");
const DRIVE_PY: &str = include_str!("../../.github/browser-rig/drive.py");
/// The measurement arm. Not a gate — but it had the identical stale-read shape,
/// and a stale FIGURE is harder to catch than a stale verdict, because a
/// plausible number in a comparison table looks like nothing at all.
const RUN_MEASURE: &str = include_str!("../../.github/browser-rig/run_measure.sh");

/// **Both halves of the run-id binding exist.**
///
/// Either alone is useless. A driver that declares `--run-id` and a launcher
/// that never passes it leaves every artefact carrying `run_id: null`, which
/// the summary's equality check would then be comparing against `None` — and
/// two stale nulls match. A launcher that passes a flag the driver does not
/// declare is the 2026-08-31 incident itself, in reverse.
#[test]
fn the_launcher_and_the_driver_still_agree_on_the_run_id_flag() {
    assert!(
        DRIVE_PY.contains("\"--run-id\""),
        "drive.py no longer declares --run-id, so nothing it writes can be \
         tied to the invocation that wrote it; run_tier2.sh will pass the flag \
         anyway, argparse will refuse the whole command line, and every leg \
         will die before opening a browser",
    );
    assert!(
        RUN_TIER2.contains("--run-id"),
        "run_tier2.sh never passes --run-id, so every result JSON carries a \
         null id, the summary's binding check compares null against null, and \
         a leg that never started is reported with the previous run's verdict \
         -- which is the defect this seam exists to make impossible",
    );
}

/// **The driver actually records the id, rather than merely accepting it.**
///
/// An argument that parses and is then dropped is worse than no argument: the
/// launcher passes it, the flag test above goes green, and the artefact still
/// says nothing about which run made it.
#[test]
fn the_driver_writes_the_run_id_into_its_result() {
    assert!(
        DRIVE_PY.contains("\"run_id\": args.run_id"),
        "drive.py accepts --run-id but no longer copies it into the result \
         dict, so the token never reaches disk and the summary's check has \
         nothing to compare against",
    );
}

/// **The summary still refuses an artefact that carries somebody else's id.**
///
/// This is the assertion the whole seam is for. Before it existed the summary
/// did one thing to decide a verdict — `os.path.isfile` — and printed that
/// file's `pass` field, with nothing tying the file to the run.
#[test]
fn the_summary_rejects_an_artefact_from_another_run() {
    assert!(
        RUN_TIER2.contains("got != want"),
        "run_tier2.sh's summary no longer compares the artefact's run_id \
         against the id the leg was launched with, so a JSON left behind by an \
         earlier run is read as this run's verdict again",
    );
    assert!(
        RUN_TIER2.contains("DID NOT RUN"),
        "run_tier2.sh's summary no longer has a did-not-run state. A leg that \
         produced no result is then either a silent pass or indistinguishable \
         from a leg that ran and failed, and neither is true",
    );
}

/// **An argument the driver refuses is fatal, and says so.**
///
/// argparse's own exit code is 2, which this rig also spends on "the leg ran
/// and an assertion was false". While those shared a code, no wrapper could
/// tell a dropped rig commit from a red leg — so the launcher retried the
/// usage error as though it were a live-network flake, and printed the same
/// twenty-five line usage dump once per attempt with the one useful sentence
/// buried in each.
#[test]
fn a_refused_argument_has_its_own_exit_code_on_both_sides() {
    assert!(
        DRIVE_PY.contains("EXIT_USAGE = 64"),
        "drive.py no longer spends a distinct exit code on a refused command \
         line, so a dropped rig commit is indistinguishable from a failed \
         assertion and the launcher cannot stop the run",
    );
    assert!(
        DRIVE_PY.contains("sys.exit(EXIT_USAGE)"),
        "drive.py declares EXIT_USAGE but no longer exits with it; argparse's \
         default 2 collides with this rig's own leg-failure code",
    );
    assert!(
        RUN_TIER2.contains("64) printf usage"),
        "run_tier2.sh no longer reads exit 64 as a refused command line, so a \
         rejected flag is filed as a leg failure, retried once as a flake, and \
         the remaining legs are handed the same doomed argument list",
    );
}

/// **A full disk is not reported as a failed leg.**
///
/// Observed 2026-08-31: `OSError: [Errno 122] Disk quota exceeded` raised
/// inside a `print()` during teardown, after every assertion had already
/// passed, escaping as an unhandled traceback that the launcher filed as a leg
/// failure. What the leg was asserting is unknown in that state — it neither
/// passed nor failed — and a reader sent to look for a rendering bug is being
/// sent to the wrong file entirely.
#[test]
fn a_full_disk_is_infrastructure_and_not_a_verdict() {
    assert!(
        DRIVE_PY.contains("EXIT_INFRA = 69"),
        "drive.py no longer separates the box running out of room from the app \
         being wrong, so a full disk is reported as a red leg",
    );
    assert!(
        DRIVE_PY.contains("INFRA_ERRNOS"),
        "drive.py no longer names the errnos that mean the filesystem rather \
         than the code",
    );
    assert!(
        RUN_TIER2.contains("INFRA"),
        "run_tier2.sh's summary no longer carries an infrastructure state, so \
         a full disk hides among the did-not-runs and the repair is looked for \
         in the wrong place",
    );
}

/// **The measurement arm binds its figures the same way.**
///
/// `run_measure.sh` is explicitly not a gate, which is exactly why this is
/// worth pinning: nothing there goes red, so a stale row is never contradicted
/// by anything. It is the arm whose numbers get quoted into comparison tables.
#[test]
fn the_measurement_arm_also_refuses_an_unbound_artefact() {
    assert!(
        RUN_MEASURE.contains("--run-id"),
        "run_measure.sh no longer passes --run-id, so a leg that died before \
         writing leaves the previous run's FIGURES under its name and they are \
         printed as this run's",
    );
    assert!(
        RUN_MEASURE.contains("STALE ARTEFACT"),
        "run_measure.sh's summary no longer refuses an artefact from another \
         run",
    );
}

/// **A leg that names a canvas size has to have reached it.**
///
/// `fit_canvas` has always returned `met`, and until 2026-08-31 nothing read
/// it: a leg could ask for 2878x1651, be handed the browser default because
/// the Xvfb screen was smaller than the target, and pass — with every figure on
/// its row silently describing a size it never rendered at. That is the same
/// shape as the stale artefact: a verdict the rig has not earned.
///
/// This pins the CAPABILITY, which is what landed. Whether the large-canvas
/// leg is in the default roster is a separate question with a separate answer
/// — see `the_default_roster_is_the_one_the_gate_means` — because the leg
/// reproduces a live defect owned by another lane and is held out until that
/// scene can go green.
#[test]
fn the_large_canvas_leg_asserts_the_size_it_claims() {
    assert!(
        DRIVE_PY.contains("\"--expect-canvas\""),
        "drive.py no longer declares --expect-canvas, so `met` goes back to \
         being recorded and never read, and a leg that could not be made large \
         passes while claiming it was",
    );
    assert!(
        RUN_TIER2.contains("[ \"$leg\" = huge ]"),
        "run_tier2.sh no longer has a large-canvas leg to run at all, so no \
         roster can opt into one and the rig is structurally blind again to \
         any defect needing a canvas bigger than the browser default",
    );
    assert!(
        RUN_TIER2.contains("--expect-canvas"),
        "run_tier2.sh's large-canvas leg no longer asserts the size it asked \
         for, which makes it a leg that tests 1280 while its name says 2878",
    );
}

/// **The roster the gate means is the one it is quoted by.**
///
/// `RIG_LEGS` can shrink the roster as well as grow it, and a one-leg run
/// still prints a tally — "1/1 PASS" is a quotable sentence. A tally describes
/// whatever roster produced it, so the roster travels beside the verdict; a
/// missing denominator is the same defect as an unbound artefact, one level up.
///
/// The literal default is pinned so that adding or removing a leg is a
/// deliberate edit here as well as there, rather than something a roster
/// string can be quietly widened or narrowed into.
#[test]
fn the_default_roster_is_the_one_the_gate_means() {
    assert!(
        RUN_TIER2.contains("DEFAULT_LEGS=\"live doctored gesture long wide\""),
        "run_tier2.sh's default roster moved. That is allowed, but it is not \
         allowed to move quietly: every `N/N PASS` ever quoted from this gate \
         is a statement about this exact list",
    );
    assert!(
        RUN_TIER2.contains("roster: [$LEGS] x [$BROWSERS]"),
        "run_tier2.sh no longer prints the roster it drove, so a run with a \
         reduced RIG_LEGS produces a tally whose denominator is invisible",
    );
}

/// **The summary says what size each row ran at.**
///
/// Two legs that both passed are not the same evidence if one rendered at
/// 1280x815 and the other at 2878x1651, and before this the summary printed
/// only the CSS box — never the drawing buffer, which is what was actually
/// rasterized and what every byte figure on the row is a figure for.
#[test]
fn every_summary_row_carries_the_canvas_it_rendered_at() {
    assert!(
        DRIVE_PY.contains("\"canvas_buffer\""),
        "drive.py no longer records the drawing buffer in its verdict, so the \
         size a row's figures belong to is recoverable only by digging",
    );
    assert!(
        RUN_TIER2.contains("canvas_buffer"),
        "run_tier2.sh's summary no longer prints the drawing buffer, so a leg \
         that passed at 1280 and one that passed at 2878 are indistinguishable \
         in the one place people read",
    );
}

/// **Artefacts are removed before a leg starts.**
///
/// The run id makes a stale read *detectable*; wiping first makes it *rare*.
/// Without the wipe, a leg that fails on its quarantine retry leaves the first
/// attempt's artefact on disk under the same name — which is a stale artefact
/// carrying a real id from a real run, and precisely the case that is hardest
/// to reason about after the fact.
#[test]
fn each_attempt_clears_the_artefacts_it_is_about_to_write() {
    assert!(
        RUN_TIER2.contains("rm -f \"$OUT_DIR/$tag.json\""),
        "run_tier2.sh no longer clears a leg's artefacts before the attempt \
         that writes them, so `the file is missing` stops meaning `this leg \
         wrote nothing` and starts meaning `nobody has run this leg since the \
         last time it worked`",
    );
}
