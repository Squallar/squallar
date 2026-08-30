//! **Every adapter-only suite in this directory is either run by CI or says
//! why not.**
//!
//! This directory is the hardware quarantine: every `#[ignore]`d test that
//! needs a wgpu adapter lives here, and the `gpu` job in
//! `.github/workflows/test.yaml` is the one thing that runs them. Until
//! 2026-08-30 that job named its targets by hand — `--test volume_gpu --test
//! volume_silhouette --test volume_shader_mutants` — beside a prose comment
//! listing "the other three" files it skipped. Both lists were maintained by
//! nobody. Four suites (`volume_drape`, `volume_ground_aspect`, `volume_light`,
//! `volume_occluder`) and one older one (`raster_upload_gpu`) had landed into a
//! job that never mentioned them, carrying 33 `#[ignore]`d tests that no push
//! executed; four of those files' own module docs said CI opted in, which was
//! false. The acceptance evidence for B1, B3, B4, C2 and D1 was hand-run and
//! gated by nothing.
//!
//! A hand-maintained list is what produced that, so the job no longer keeps
//! one. It **derives** its targets: every file here with a column-0 `#[ignore]`
//! attribute is run, unless the file's own module doc opts out with a
//! [`MARKER`] line. A suite added tomorrow is covered the day it lands without
//! anyone editing the workflow.
//!
//! What still needs a human is the *reason* a suite is skipped, and this test
//! is what keeps that honest. It holds four things together:
//!
//! **One**: every opt-out names a reason, and the file really is env-driven —
//! so [`MARKER`] cannot become a way to quiet a failing test. The check is that
//! the file reads `std::env::var`, which is what all three real exclusions do
//! (they read a Level II volume path out of `VOL` and panic without it).
//!
//! **Two**: the workflow still derives. A future edit that replaces the loop
//! with a literal `--test volume_gpu` list reopens exactly the hole this closes,
//! so a literal suite name in that job fails here.
//!
//! **Three**: the shell and this file agree on how a suite is spelled. Both
//! grep for the same two patterns, and they are asserted to be present in the
//! workflow verbatim — a marker renamed on one side only is a silent no-op
//! otherwise, with CI quietly running everything or nothing.
//!
//! **Four**: the same rule for the four `#[ignore]`d tests that live in a lib
//! rather than here (three in squallar-volumetric, one in squallar-gpu). That
//! step listed them with `--exact` behind a written-out `3 passed`, which
//! catches a rename and misses an addition; it now discovers the count. Those
//! lists were exactly complete on 2026-08-30 — this half of the fix is the
//! same defect caught before it bit, not another one found.
//!
//! It does **not** check that the suites pass, or that the `gpu` job is wired
//! into any branch protection. It checks that nothing in this directory is
//! silently unexecuted.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The module-doc line a suite uses to opt out of the `gpu` job, followed by
/// the reason. Spelled identically in the workflow's derivation.
const MARKER: &str = "//! ci-excluded:";

/// How a real `#[ignore]` attribute is told apart from the many doc comments in
/// this directory that merely discuss one. Every genuine attribute here sits at
/// column 0; every mention sits inside a `//!` or `///`. Asserted rather than
/// assumed by [`the_column_zero_rule_really_discriminates`].
const IGNORE_ATTR: &str = "#[ignore";

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-gpu sits one level under the workspace root")
        .join(".github/workflows/test.yaml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the gpu job's workflow should be readable at {path:?}: {e}"))
}

/// **The `gpu` job's shell, with every comment line removed.**
///
/// Every assertion below searches this rather than the file, and that is not
/// tidiness. Twice while this test was being written a check passed because the
/// *prose* above a step contained the string it was looking for while the
/// command underneath had been changed out from under it — once for [`MARKER`]
/// and once for `--exact`. A workflow comment is the one place in this
/// repository guaranteed to discuss the very flags the job runs, so searching
/// the file as a whole is close to guaranteed to be vacuous.
///
/// The `gpu` job is everything before the `test` job, which follows it.
fn gpu_job_shell() -> String {
    let yaml = workflow();
    let job = yaml
        .split("\n  test:")
        .next()
        .expect("test.yaml should carry a `test:` job after the `gpu:` one")
        .to_owned();
    assert!(
        job.contains("gpu:") && job.len() < yaml.len(),
        "could not isolate the gpu job from test.yaml; the split found no \
         `test:` job after it, so these assertions would be reading the whole \
         file including the coverage row",
    );
    job.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does this file carry a real `#[ignore]`d test — i.e. is it a suite the `gpu`
/// job is responsible for?
fn is_gpu_suite(source: &str) -> bool {
    source.lines().any(|l| l.starts_with(IGNORE_ATTR))
}

/// The reason a file opts out, if it does.
fn exclusion_reason(source: &str) -> Option<String> {
    source
        .lines()
        .find_map(|l| l.strip_prefix(MARKER))
        .map(|r| r.trim().to_owned())
}

/// Every `.rs` directly in this directory, as `(stem, source)`.
fn suites() -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let dir = tests_dir();
    for entry in std::fs::read_dir(&dir).expect("the tests directory should be readable") {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_some_and(|e| e == "rs") && path.is_file() {
            let stem = path
                .file_stem()
                .expect("a .rs file has a stem")
                .to_string_lossy()
                .into_owned();
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{path:?} should be readable: {e}"));
            found.insert(stem, source);
        }
    }
    assert!(
        found.len() > 10,
        "only {} .rs files found in {dir:?}; this scan is reading the wrong \
         directory and would pass vacuously",
        found.len(),
    );
    found
}

/// An opt-out names a reason, and the file it is on really is env-driven.
///
/// The second half is the non-triviality floor. Without it [`MARKER`] is a way
/// to delete a suite from CI by writing a sentence, which is a strictly worse
/// version of the hand-maintained list it replaced.
#[test]
fn every_excluded_suite_gives_a_reason_and_really_needs_the_environment() {
    for (stem, source) in suites() {
        let Some(reason) = exclusion_reason(&source) else {
            continue;
        };
        assert!(
            is_gpu_suite(&source),
            "{stem}.rs opts out of the gpu job with `{MARKER}` but has no \
             `#[ignore]`d test for the job to have run. The marker means \
             something only on an adapter-only suite; delete it.",
        );
        assert!(
            reason.len() >= 20,
            "{stem}.rs opts out of the gpu job with a {}-character reason \
             ({reason:?}). Say what the suite needs that CI has not got — the \
             next person to read the workflow gets only this sentence.",
            reason.len(),
        );
        assert!(
            source.contains("env::var"),
            "{stem}.rs opts out of the gpu job claiming {reason:?}, but it \
             never reads `env::var`. Every legitimate exclusion here is a \
             measurement instrument that takes a Level II volume path out of \
             the environment and panics without it. A suite that needs no \
             environment needs no exclusion: if it is failing on lavapipe, \
             that is a finding, not a line of prose.",
        );
    }
}

/// The suites the derivation will actually hand to `cargo test`.
fn derived() -> Vec<String> {
    suites()
        .into_iter()
        .filter(|(_, s)| is_gpu_suite(s) && exclusion_reason(s).is_none())
        .map(|(stem, _)| stem)
        .collect()
}

/// The workflow derives its target list instead of naming one.
///
/// A literal suite name in the `gpu` job is the defect this file exists to
/// prevent: it is how three suites came to be named and five to be forgotten.
#[test]
fn the_gpu_job_names_no_suite_by_hand() {
    let shell = gpu_job_shell();
    for stem in suites().keys() {
        let literal = format!("--test {stem}");
        assert!(
            !shell.contains(&literal),
            "`.github/workflows/test.yaml` names `{literal}` outright. The gpu \
             job derives its targets from this directory precisely so that a \
             list cannot go stale; a hand-written name reopens that. Pass the \
             stem through the loop's `$stem` instead.",
        );
    }
}

/// The workflow's shell and this file agree on what they are looking for.
///
/// Both greps are asserted verbatim. Renaming [`MARKER`] here and not there
/// would leave CI running the env-driven suites (which fail loudly, so that
/// direction is survivable) or, the other way round, silently running nothing
/// while every assertion in this file still passed.
#[test]
fn the_workflow_greps_for_the_patterns_this_file_defines() {
    let shell = gpu_job_shell();

    // The literal grep, not merely the pattern. Checking `contains(MARKER)`
    // is what this test did first, and it was vacuous: the comment above the
    // step explains the marker, so the prose satisfied the search while the
    // grep beneath it had been changed to something else entirely. Found by
    // mutation (M4) on 2026-08-30. Assert the command.
    let marker_grep = format!("grep -q '^{MARKER}'");
    // `[` is a bracket expression to grep, so the workflow escapes it. Built
    // here rather than written out, so the two spellings cannot drift.
    let ignore_grep = format!("grep -q '^{}'", IGNORE_ATTR.replace('[', "\\["));

    for needle in [&marker_grep, &ignore_grep] {
        assert!(
            shell.contains(needle.as_str()),
            "the gpu job does not run `{needle}`. Its derivation and this \
             ratchet have drifted apart: whatever the job greps for now, this \
             file's constants no longer describe it, and every other assertion \
             here is checking a rule CI has stopped applying.",
        );
    }

    assert!(
        shell.contains("squallar-gpu/tests/*.rs"),
        "the gpu job no longer globs `squallar-gpu/tests/*.rs`, so it is not \
         deriving its targets from this directory at all",
    );
}

/// The job names no `#[ignore]`d **unit** test by hand either.
///
/// Four of them live in libs rather than in this directory (three in
/// squallar-volumetric, one in squallar-gpu), and until 2026-08-30 the job
/// listed all four with `--exact` behind an asserted `3 passed` / `1 passed`.
/// That catches a rename and misses an addition: a fifth would be filtered out
/// and the count would still hold. The step now discovers the count with
/// `--list --ignored` and runs the whole ignored set, so `--exact` in this job
/// means the hand list has come back.
#[test]
fn the_gpu_job_discovers_its_ignored_unit_tests_rather_than_listing_them() {
    let gpu_job = gpu_job_shell();
    assert!(
        gpu_job.contains("--list --ignored"),
        "the gpu job no longer discovers its ignored lib tests with \
         `--list --ignored`, so the count it asserts is a number somebody \
         typed. An added `#[ignore]`d lib test would then never run.",
    );
    assert!(
        !gpu_job.contains("--exact"),
        "the gpu job filters its ignored lib tests with `--exact` again. That \
         spelling asserts a count against a list of names, which cannot notice \
         a test being ADDED — the omission this file exists to prevent. Run \
         the whole ignored set against a discovered count instead.",
    );
}

/// Falsifiability floor: the classifier really splits this directory, and the
/// two sides are what they are documented to be.
///
/// Without this, a scan that classified nothing as a suite — a renamed
/// attribute, a moved directory — would satisfy every assertion above by
/// having nothing to check.
#[test]
fn the_classifier_puts_the_known_files_where_they_belong() {
    let all = suites();
    let derived = derived();

    // Run by the job: the three it always ran, plus the five that were landing
    // into silence until 2026-08-30.
    for stem in [
        "volume_gpu",
        "volume_silhouette",
        "volume_shader_mutants",
        "volume_drape",
        "volume_ground_aspect",
        "volume_light",
        "volume_occluder",
        "raster_upload_gpu",
    ] {
        assert!(
            derived.iter().any(|d| d == stem),
            "{stem} is not in the derived set {derived:?}. It carries \
             `#[ignore]`d tests and needs no environment, so the gpu job must \
             run it.",
        );
    }

    // Excluded, and each really does read a volume off the disk.
    for stem in ["volume_real_mask", "volume_march_cost", "floor_alignment"] {
        let source = all
            .get(stem)
            .unwrap_or_else(|| panic!("{stem}.rs should be in this directory"));
        assert!(
            exclusion_reason(source).is_some(),
            "{stem}.rs reads a Level II volume out of the environment and \
             cannot run on CI, but it no longer carries `{MARKER}` — so the \
             derivation will hand it to the job and the job will fail on an \
             unset VOL.",
        );
    }

    // Not adapter-only at all: these run in the ordinary `test` job. They
    // mention `#[ignore]` in prose, which is exactly what the column-0 rule is
    // for.
    for stem in ["charter", "wgpu_guard", "volume_stand_in"] {
        let source = all
            .get(stem)
            .unwrap_or_else(|| panic!("{stem}.rs should be in this directory"));
        assert!(
            !is_gpu_suite(source),
            "{stem}.rs now carries a real `#[ignore]` attribute. If it needs an \
             adapter the gpu job will pick it up automatically, which is \
             right — but this floor was written on it being a plain test, so \
             move it to the list above deliberately rather than by deleting \
             this line.",
        );
    }
}

/// The column-0 rule is a discriminator, not a coincidence.
///
/// It is load-bearing for the whole derivation, and this directory is full of
/// prose about `#[ignore]` — `volume_shader.rs` and `charter.rs` discuss it
/// without carrying one. If a real attribute were ever indented (inside a
/// `mod`, say), the shell would skip that suite silently.
#[test]
fn the_column_zero_rule_really_discriminates() {
    let mut discussed_without_carrying = 0usize;
    for (stem, source) in suites() {
        for (n, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with(IGNORE_ATTR) && !line.starts_with(IGNORE_ATTR) {
                panic!(
                    "{stem}.rs:{} indents a real `#[ignore]` attribute. The gpu \
                     job's derivation greps for it at column 0 and would skip \
                     this suite without a word. Put the attribute at column 0.",
                    n + 1,
                );
            }
        }
        if !is_gpu_suite(&source) && source.contains(IGNORE_ATTR) {
            discussed_without_carrying += 1;
        }
    }
    assert!(
        discussed_without_carrying >= 2,
        "only {discussed_without_carrying} file(s) in this directory mention \
         `#[ignore]` in prose without carrying one. That was 2 when this rule \
         was written (charter.rs, volume_shader.rs) and is the reason the \
         column-0 test exists rather than a plain substring search. If the \
         prose is genuinely gone, this floor can go with it — but check first \
         that the classifier has not simply stopped reading the files.",
    );
}
