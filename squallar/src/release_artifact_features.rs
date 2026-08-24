//! No release-artifact row asks cargo for a test-only feature.
//!
//! Read the title literally. It is a claim about the **command line** a matrix
//! row runs, not about everything that ends up linked into the executable —
//! see "What this does not cover" below, which names a residual that is real,
//! measured, and outside this gate.
//!
//! Test-only. See the module mount in `lib.rs` for why it lives in this crate:
//! `squallar` is the package whose executable the desktop rows upload.
//!
//! # Why this gate exists
//!
//! `.github/workflows/build.yaml`'s desktop rows do two jobs with one command.
//! They are the workspace's cross-target compile coverage, *and* the binary
//! they produce is `matrix.artifact` — the file a person downloads and runs.
//! A feature flag added for the coverage half therefore lands in the shipped
//! executable, and cargo offers no way for a feature to opt out of
//! `--all-features`.
//!
//! That is not hypothetical. Until WO-FAKESHIP those four rows passed
//! `--all-features`, which turned on the overlays crate's synthetic proof
//! layer — the one that existed only to show a source needs nothing but the
//! seams. **Every desktop download registered it, drew it, and persisted its
//! id into the user's config.** Read out of the built executable's strings,
//! not inferred: the same binary built both ways in one tree carried the
//! layer's handler symbol 129 times under `--all-features` and zero times
//! under the list this row now passes. The web arm was never affected —
//! `wasm-pack … --release` takes default features.
//!
//! **The fake source itself was deleted on 2026-08-22**, so that particular
//! feature no longer exists to be enabled. The history is kept because the
//! *shape* of the defect is what this gate refuses, and the shape outlived
//! the feature: `test-support` below is the same class, live today.
//!
//! # What is asserted, and what is not
//!
//! The property, not the spelling. The question is *"can a release-artifact
//! row enable a test-only feature"*, so the assertion runs over the feature
//! **set** a row's cargo command requests: `--all-features` fails not because
//! of the literal but because it expands to every feature declared by every
//! workspace member, read here from the manifests themselves. Delete the
//! feature from the tree and this test goes green honestly; re-add
//! `--all-features` under any other spelling and it goes red again. Asserting
//! the literal `--all-features` would have done neither.
//!
//! # What this does not cover
//!
//! **A feature enabled by something other than the command line.** The desktop
//! rows also pass `--all-targets`, which selects the test targets, and one of
//! those — `squallar-app`'s — carries a **dev**-dependency
//! `squallar-radar = { features = ["test-support"] }`. Under resolver 2 that
//! feature is unified into the lib unit the binary links. Measured, by
//! counting `squallar_radar` rustc invocations in `cargo check -v`:
//!
//! ```text
//! --workspace --all-targets --features squallar/jni-typecheck  2 units, 2 with test-support
//! --workspace              --features squallar/jni-typecheck  1 unit,  0 with test-support
//! -p squallar --bins                                           1 unit,  0 with test-support
//! ```
//!
//! So `--all-targets`, not `--all-features`, puts `test-support` in the shipped
//! binary — it did so before WO-FAKESHIP and it still does. It was far milder
//! than the fake source (three state-forcing methods, no registered layer, no
//! catalogue row, nothing written to a config) but it is **the same class of
//! defect**, and the only fix is to stop building the artifact with the command
//! that provides the compile coverage. That is the fold this workflow's own
//! comment block records as a measured decision (1.8 GB of `target/` instead of
//! 12; a 27.5 MB upload instead of 602 MB on four rows every run), so unfolding
//! it is not a change a gate should force. **Recorded here so it is not
//! rediscovered as news, and deliberately not asserted.**
//!
//! **A feature enabled out of the scrape's sight.** This reads the cargo
//! invocations the workflow spells out. A row whose `cmd` shells into a
//! Makefile or Gradle could enable a feature this never sees. Measured at the
//! time of writing: `--all-features` occurs **zero** times under `packaging/`,
//! the iOS and macOS makefiles invoke `cargo … -p squallar` with no feature
//! flag, and the Android Gradle build's `cargo ndk` invocation names none — so
//! the hole is empty today, and it is a hole.

/// The workflow whose rows build and upload the release artifacts.
///
/// `include_str!`, so moving or renaming the file fails the **build**, not one
/// assertion inside it.
#[cfg(test)]
const BUILD_WORKFLOW: &str = include_str!("../../.github/workflows/build.yaml");

/// The workspace manifest, read for its `members` list.
#[cfg(test)]
const WORKSPACE_MANIFEST: &str = include_str!("../../Cargo.toml");

/// Workspace root: this crate's manifest dir is `squallar/`.
#[cfg(test)]
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

/// Features that exist to serve a test and must never reach a shipped binary.
///
/// * `test-support` — `squallar-radar`'s state-forcing hooks
///   (`force_retire_at`, `force_serving`, `backdate_handshake`). Its own doc
///   says "never enabled by a normal dependency"; `squallar-app`'s *dev*
///   dependency is what turns it on. **It is a needle here anyway**, even
///   though no command line names it today: the module docs record, measured,
///   that `--all-targets` already unifies it into the shipped lib unit, so a
///   command line that named it too would be adding a second route to a place
///   it should not be.
///
/// Matched on the segment after the last `/`, so a namespaced `package/name`
/// entry and a bare `name` are the same needle.
#[cfg(test)]
const TEST_ONLY_FEATURES: &[&str] = &["test-support"];

/// The desktop rows whose artifact is a runnable executable. Named, so a
/// renamed key or a re-shaped matrix fails loudly instead of passing on an
/// empty haystack.
#[cfg(test)]
const DESKTOP_ARTIFACT_ROWS: &[&str] = &[
    "linux-x86_64",
    "macos-aarch64",
    "macos-x86_64",
    "windows-x86_64",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    /// One `matrix.include` entry, as a flat map of its scalar keys. Block
    /// scalars (`cmd: |`) arrive joined with newlines.
    type Row = BTreeMap<String, String>;

    fn indent(line: &str) -> usize {
        line.len() - line.trim_start().len()
    }

    fn is_skippable(line: &str) -> bool {
        let t = line.trim();
        t.is_empty() || t.starts_with('#')
    }

    /// The rows of the build matrix's `include:` list.
    ///
    /// Deliberately not a YAML library: the workspace has none, and the house
    /// pattern for scraping a non-Rust asset is a small purpose-built reader
    /// (`squallar-web/tests/pwa_assets.rs` over `sw.js`,
    /// `network_security_config.rs` over the Android XML). Comment lines are
    /// dropped first, so the prose above the rows — which names
    /// `--all-features` — is never read as a row.
    fn matrix_rows(yaml: &str) -> Vec<Row> {
        let lines: Vec<&str> = yaml.lines().collect();
        let include_at = lines
            .iter()
            .position(|l| l.trim() == "include:")
            .expect("build.yaml no longer contains a matrix `include:` list");

        let item_indent = indent(lines[include_at]) + 2;
        let mut rows: Vec<Row> = Vec::new();
        let mut open_key: Option<String> = None;

        for line in &lines[include_at + 1..] {
            if is_skippable(line) {
                continue;
            }
            let col = indent(line);
            if col < item_indent {
                break; // dedented out of the include list
            }
            let body = line.trim_end();
            let trimmed = body.trim_start();

            if col == item_indent && trimmed.starts_with("- ") {
                rows.push(Row::new());
                open_key = take_pair(rows.last_mut().unwrap(), &trimmed[2..]);
            } else if col == item_indent + 2 && !trimmed.starts_with('-') {
                let row = rows
                    .last_mut()
                    .expect("a matrix key appeared before any `- ` row");
                open_key = take_pair(row, trimmed);
            } else if let (Some(key), Some(row)) = (open_key.as_ref(), rows.last_mut()) {
                // Continuation of a block scalar.
                row.get_mut(key).unwrap().push('\n');
                row.get_mut(key).unwrap().push_str(trimmed);
            }
        }
        rows
    }

    /// Store `key: value` into `row`. Returns the key if the value was a block
    /// scalar introducer (`|` / `>`), so following lines append to it.
    fn take_pair(row: &mut Row, text: &str) -> Option<String> {
        let (key, value) = text.split_once(':')?;
        let key = key.trim().to_string();
        let value = value.trim().to_string();
        let is_block = value == "|" || value == ">" || value == "|-" || value == ">-";
        row.insert(key.clone(), if is_block { String::new() } else { value });
        is_block.then_some(key)
    }

    /// What a shell command asks cargo to enable.
    #[derive(Debug, PartialEq, Eq)]
    enum Request {
        /// `--all-features`: every feature of every selected package.
        All,
        /// The union of every `--features` / `-F` list in the command.
        Named(BTreeSet<String>),
    }

    fn feature_request(cmd: &str) -> Request {
        let tokens: Vec<&str> = cmd.split_whitespace().collect();
        let mut named = BTreeSet::new();
        for (i, tok) in tokens.iter().enumerate() {
            if *tok == "--all-features" {
                return Request::All;
            }
            let list = if let Some(rest) = tok.strip_prefix("--features=") {
                Some(rest)
            } else if *tok == "--features" || *tok == "-F" {
                tokens.get(i + 1).copied()
            } else {
                None
            };
            if let Some(list) = list {
                named.extend(list.split(',').map(|f| f.trim().to_string()));
            }
        }
        Request::Named(named)
    }

    /// The `members = [...]` paths of the workspace manifest.
    ///
    /// Comment lines are dropped **before** the closing `]` is looked for: the
    /// members list carries long comments and one of them contains the literal
    /// `[patch.crates-io]`, so a naive `find(']')` truncates the list after
    /// two entries. That mistake was made here first and caught by
    /// [`every_test_only_needle_names_a_declared_feature`]'s floor.
    fn workspace_members() -> Vec<PathBuf> {
        let start = WORKSPACE_MANIFEST
            .find("members = [")
            .expect("the workspace manifest no longer declares `members = [`");
        let mut out = Vec::new();
        for line in WORKSPACE_MANIFEST[start..].lines() {
            let code = match line.find('#') {
                Some(i) => &line[..i],
                None => line,
            };
            out.extend(
                code.split('"')
                    .skip(1)
                    .step_by(2)
                    .map(|m| PathBuf::from(ROOT).join(m)),
            );
            if code.contains(']') {
                return out;
            }
        }
        panic!("unterminated `members` list in the workspace manifest");
    }

    /// The `[features]` keys a manifest declares, plus the entries of its
    /// `default` list (so a test-only feature cannot be smuggled in by making
    /// it default rather than by naming it on a command line).
    fn declared_features(manifest: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut in_features = false;
        for line in manifest.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_features = t == "[features]";
                continue;
            }
            if !in_features || t.starts_with('#') || t.is_empty() {
                continue;
            }
            if let Some((key, _)) = t.split_once('=') {
                out.insert(key.trim().to_string());
            }
        }
        out
    }

    /// Every feature name `--all-features` would turn on across the workspace.
    fn all_workspace_features() -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for member in workspace_members() {
            let path = member.join("Cargo.toml");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("workspace member manifest {path:?}: {e}"));
            out.extend(declared_features(&text));
        }
        out
    }

    fn test_only_hits<'a>(features: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        features
            .into_iter()
            .filter(|f| {
                let name = f.rsplit('/').next().unwrap_or(f);
                TEST_ONLY_FEATURES.contains(&name)
            })
            .cloned()
            .collect()
    }

    /// Presence control. Everything below counts over what this finds, so if
    /// the matrix is re-shaped, a key is renamed or the desktop rows move, the
    /// suite fails here rather than passing over an empty haystack.
    #[test]
    fn the_scrape_finds_the_release_artifact_rows() {
        let rows = matrix_rows(BUILD_WORKFLOW);
        assert!(
            rows.len() >= 8,
            "control: the build matrix parsed as {} rows, which is too few to \
             be the real matrix -- the `include:` reader has rotted",
            rows.len(),
        );

        let uploaded: Vec<&String> = rows
            .iter()
            .filter(|r| r.contains_key("artifact"))
            .filter_map(|r| r.get("name"))
            .collect();
        for want in DESKTOP_ARTIFACT_ROWS {
            assert!(
                uploaded.iter().any(|n| *n == want),
                "control: no matrix row named `{want}` carries an `artifact:` \
                 key. Either the row was renamed, the key was renamed, or the \
                 artifact stopped being uploaded -- in every case the gate \
                 below is reading the wrong thing. Found: {uploaded:?}",
            );
        }

        for row in rows.iter().filter(|r| r.contains_key("artifact")) {
            let name = row.get("name").map(String::as_str).unwrap_or("<unnamed>");
            assert!(
                row.contains_key("cmd"),
                "control: artifact row `{name}` has no `cmd:` -- the feature \
                 scrape has nothing to read for it",
            );
        }
    }

    /// Second presence control: the needles name features that exist.
    ///
    /// Without this the gate below could pass because `TEST_ONLY_FEATURES`
    /// went stale, rather than because the workflow is clean.
    #[test]
    fn every_test_only_needle_names_a_declared_feature() {
        let members = workspace_members();
        assert!(
            members.len() >= 15,
            "control: the workspace manifest parsed as {} members, which is \
             too few to be the real list -- the `members` reader has rotted: \
             {members:?}",
            members.len(),
        );
        let declared = all_workspace_features();
        assert!(
            declared.len() >= 10,
            "control: only {} features found across {} workspace members -- \
             the manifest reader has rotted",
            declared.len(),
            members.len(),
        );
        for needle in TEST_ONLY_FEATURES {
            assert!(
                declared.contains(*needle),
                "`{needle}` is in TEST_ONLY_FEATURES but no workspace member \
                 declares it. Either it was deleted (drop the needle) or it \
                 was renamed (re-point it) -- a needle that matches nothing \
                 makes the gate below vacuous.",
            );
        }
    }

    /// The gate. Ruling (47)'s defect, encoded so it cannot come back.
    #[test]
    fn no_release_artifact_row_enables_a_test_only_feature() {
        let all = all_workspace_features();
        for row in matrix_rows(BUILD_WORKFLOW)
            .iter()
            .filter(|r| r.contains_key("artifact"))
        {
            let name = row.get("name").map(String::as_str).unwrap_or("<unnamed>");
            let cmd = &row["cmd"];
            let (asked, hits) = match feature_request(cmd) {
                Request::All => ("--all-features", test_only_hits(&all)),
                Request::Named(named) => ("its explicit --features list", test_only_hits(&named)),
            };
            assert!(
                hits.is_empty(),
                "matrix row `{name}` uploads `{artifact}`, and {asked} enables \
                 {hits:?} -- features that exist to serve a test. Whatever that \
                 row builds is what a person downloads and runs. Name the \
                 features the desktop build actually wants instead; cargo has \
                 no way for a feature to opt out of `--all-features`.",
                artifact = row["artifact"],
            );
        }
    }
}
