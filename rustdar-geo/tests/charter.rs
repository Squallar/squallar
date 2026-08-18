//! The crate's charter, held as tests: an empty dependency ceiling and the
//! graph position the crate exists to hold.
//!
//! Both read `cargo metadata --no-deps --format-version 1` from the workspace
//! root. `packages[].dependencies` there are *declared* dependencies —
//! feature-independent and resolution-independent — so no feature selection
//! (default, `--all-features`, CI's llvm-cov arm) can mask or fake what these
//! assert. Dep-name mechanics, recorded at M0 and relied on here: a
//! workspace-internal dep appears with `"req": "*"` and a `path`; `kind` is
//! `null` for normal deps (normalised to "normal" below); one name may
//! legitimately appear once per kind, so entries are judged per
//! `(kind, name)`. Assertions key on dependency *names*, never on feature
//! emptiness — a later feature on either crate must not disturb them.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-geo sits one level under the workspace root")
        .to_path_buf()
}

/// The declared-dependency metadata of every workspace member, as parsed JSON.
fn metadata() -> serde_json::Value {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata should run");
    assert!(
        out.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata emits valid JSON")
}

/// `(kind, name)` for every dependency `package` declares. `kind: null` is a
/// normal dependency; target-gated entries carry their kind like any other and
/// are included — a gated dependency is still a dependency.
fn declared_deps(meta: &serde_json::Value, package: &str) -> BTreeSet<(String, String)> {
    let packages = meta["packages"]
        .as_array()
        .expect("metadata carries a packages array");
    let found = packages
        .iter()
        .find(|p| p["name"].as_str() == Some(package))
        .unwrap_or_else(|| panic!("workspace member `{package}` is missing from cargo metadata"));
    found["dependencies"]
        .as_array()
        .expect("a package carries a dependencies array")
        .iter()
        .map(|d| {
            let kind = d["kind"].as_str().unwrap_or("normal").to_string();
            let name = d["name"]
                .as_str()
                .expect("a dependency has a name")
                .to_string();
            (kind, name)
        })
        .collect()
}

/// The floor stays a floor: the *only* dependency this crate may declare, of
/// any kind, is the dev-only serde_json that this file itself needs to parse
/// `cargo metadata`. Equality and not ⊆ a list, because the ceiling is empty
/// on purpose — pure geometry over `std` is the crate's whole identity, and
/// the definitions it holds are exactly the ones every other crate must be
/// able to reach without dragging anything else in.
///
/// The floor assertion at the bottom is what keeps this test falsifiable — a
/// broken parse or a renamed package cannot pass as an empty set.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-geo");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            "dev" => name == "serde_json",
            "normal" => false,
            other => panic!(
                "rustdar-geo declares a `{other}` dependency on {name}; \
                 the charter permits none"
            ),
        };
        assert!(
            allowed,
            "rustdar-geo declares {name} ({kind}). rustdar-geo is the \
             workspace's arithmetic floor — pure geometry over std; if a step \
             genuinely needs a dependency, the charter in this test and the \
             plan must change first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its one dev dependency,
    // so a broken parse or a renamed package cannot pass as an empty set.
    assert!(
        deps.iter().any(|(k, n)| k == "dev" && n == "serde_json"),
        "rustdar-geo no longer declares serde_json (dev) — either the crate \
         changed shape or this test is reading the wrong package: {deps:?}",
    );
}

/// The graph shape WO-G1 created stays: rustdar-source stands on rustdar-geo,
/// so every crate above the substrate reaches the floor's definitions by
/// re-export rather than by restating them.
///
/// Presence, not absence, so it doubles as this file's second falsifiability
/// floor: a renamed package or a broken parse cannot pass it.
#[test]
fn the_floor_sits_under_the_substrate() {
    let meta = metadata();
    let source = declared_deps(&meta, "rustdar-source");

    assert!(
        source
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "rustdar-geo"),
        "rustdar-source no longer stands on rustdar-geo: {source:?}",
    );
}
