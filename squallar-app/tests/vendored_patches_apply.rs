//! Every `[patch.crates-io]` entry is actually applied to the graph.
//!
//! Raising a pin in `[workspace.dependencies]` past the vendored copy's
//! `package.version` does not fail. A patch is only accepted for a version that
//! satisfies the requirement, so cargo quietly stops applying it, resolves the
//! registry copy instead, and reports nothing. The vendored crate is a
//! workspace member, so it still compiles and its own tests still pass — it is
//! simply reachable from nothing. The board is green and the defect the
//! vendoring exists to carry is out of the build.
//!
//! Measured on renovate PR #21 (`pmtiles =0.23.0` -> `=0.24.0`): `cargo
//! metadata` exited 0 with no warning, `squallar-egui` resolved `pmtiles
//! 0.24.0` from crates.io, and the vendored `0.23.0` was in nobody's
//! dependency list. `walkers =0.58.0` was measured to do the same as soon as
//! `geo-types` moves to `0.7.20`, and to re-add 78 crates to the graph; today
//! it errors only because the stale `geo-types` pin happens to block it, which
//! is an accident and not a gate.
//!
//! `Cargo.lock` is the oracle. `cargo test` refreshes it before this test runs,
//! so it always describes the resolution the suite is about to build, and the
//! distinction is legible in it: a path-substituted crate appears exactly once
//! and carries no `source` key, while the registry copy that displaces it
//! carries one.
//!
//! The positive half of the check is [`patch_section_is_present_and_intact`].
//! Without it a renamed section header or a reworded entry would leave nothing
//! to iterate and this file would pass by finding no work to do.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Workspace root: this integration test's manifest dir is `squallar-app/`.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

/// Value of `key = "..."` on a single manifest line, ignoring anything else on
/// it. Enough for `Cargo.lock`, which is machine-written one field per line.
fn field(line: &str, key: &str) -> Option<String> {
    let rest = line
        .trim()
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix('=')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

/// `name` -> declared `path`, from the root manifest's `[patch.crates-io]`.
fn patch_entries() -> BTreeMap<String, String> {
    let manifest = fs::read_to_string(PathBuf::from(ROOT).join("Cargo.toml"))
        .expect("workspace root Cargo.toml");
    let mut out = BTreeMap::new();
    let mut inside = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t == "[patch.crates-io]";
            continue;
        }
        if !inside || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let (name, rhs) = t.split_once('=').expect("patch entry is `name = ...`");
        let at = rhs.find("path").expect("patch entry declares a path");
        let path = field(&rhs[at..], "path").expect("patch entry declares a path");
        out.insert(name.trim().to_owned(), path);
    }
    out
}

/// One `[[package]]` block of `Cargo.lock`, reduced to what decides this.
struct Locked {
    version: String,
    /// `None` for a path or workspace member, `Some` for a registry copy.
    source: Option<String>,
}

/// Every `[[package]]` in `Cargo.lock`, grouped by crate name. A name maps to
/// more than one entry exactly when the graph holds more than one copy of it.
fn locked_packages() -> BTreeMap<String, Vec<Locked>> {
    let lock =
        fs::read_to_string(PathBuf::from(ROOT).join("Cargo.lock")).expect("workspace Cargo.lock");
    let mut out: BTreeMap<String, Vec<Locked>> = BTreeMap::new();
    for block in lock.split("[[package]]").skip(1) {
        // Stop at the next top-level table so a trailing `[metadata]` or the
        // `[[patch.unused]]` list is never read as package fields.
        let block = block.split("\n[").next().unwrap_or(block);
        let (mut name, mut version, mut source) = (None, None, None);
        for line in block.lines() {
            if let Some(v) = field(line, "name") {
                name = Some(v);
            } else if let Some(v) = field(line, "version") {
                version = Some(v);
            } else if let Some(v) = field(line, "source") {
                source = Some(v);
            }
        }
        let (name, version) = (
            name.expect("locked package name"),
            version.expect("version"),
        );
        out.entry(name)
            .or_default()
            .push(Locked { version, source });
    }
    out
}

/// The positive anchor. A gate that iterates a list it failed to find passes by
/// doing nothing, so prove the list is there and points at real crates before
/// trusting the check below it.
#[test]
fn patch_section_is_present_and_intact() {
    let patches = patch_entries();
    assert!(
        !patches.is_empty(),
        "no `[patch.crates-io]` entries parsed out of the root Cargo.toml. Either \
         every vendored crate was un-vendored -- in which case delete this test \
         deliberately -- or the section moved and the check below is now vacuous."
    );

    for (name, path) in &patches {
        let manifest = PathBuf::from(ROOT).join(path).join("Cargo.toml");
        let text = fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("`{name}` is patched to `{path}`, unreadable: {e}"));
        assert!(
            text.lines()
                .any(|l| field(l, "name").as_deref() == Some(name.as_str())),
            "`{name}` is patched to `{path}`, whose manifest declares a different \
             crate. A patch is keyed by the crate name it replaces, so this \
             substitutes nothing."
        );
    }
}

/// The gate. Each patched name resolves to its vendored copy and to nothing
/// else.
#[test]
fn every_patched_crate_resolves_to_its_vendored_copy() {
    let patches = patch_entries();
    let locked = locked_packages();

    for (name, path) in &patches {
        let entries = locked
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` is patched but absent from Cargo.lock"));

        let from_registry: Vec<_> = entries
            .iter()
            .filter_map(|e| e.source.as_ref().map(|s| format!("{} ({s})", e.version)))
            .collect();

        assert!(
            from_registry.is_empty(),
            "`{name}` is patched to `{path}` but Cargo.lock still resolves it from \
             a registry: {}. The pin in [workspace.dependencies] no longer \
             matches the vendored `package.version`, so cargo dropped the patch \
             without saying so and the vendored fix is out of the graph. Move the \
             pin back, or port the upstream release into `{path}` and set its \
             `package.version` to match.",
            from_registry.join(", ")
        );

        assert_eq!(
            entries.len(),
            1,
            "`{name}` is patched to `{path}` but appears {} times in Cargo.lock. A \
             substituted crate is one copy; more than one means something in the \
             graph is holding a second.",
            entries.len()
        );
    }
}
