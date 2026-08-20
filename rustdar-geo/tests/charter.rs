//! The crate's charter, held as tests: an empty dependency ceiling and the
//! graph position the crate exists to hold.
//!
//! Both read `cargo metadata --no-deps` from the workspace root, whose
//! `packages[].dependencies` are *declared* deps — feature-independent, so no
//! feature selection can mask them. Entries are judged per `(kind, name)`.
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
/// normal dependency; target-gated entries are included.
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

/// The *only* dependency this crate may declare, of any kind, is the dev-only
/// serde_json this file itself needs. Equality and not ⊆ a list, because the
/// ceiling is empty on purpose.
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

    // Falsifiability floor: the crate really declares its one dev dependency.
    assert!(
        deps.iter().any(|(k, n)| k == "dev" && n == "serde_json"),
        "rustdar-geo no longer declares serde_json (dev) — either the crate \
         changed shape or this test is reading the wrong package: {deps:?}",
    );
}

/// rustdar-source stands on rustdar-geo, so every crate above the substrate
/// reaches the floor's definitions by re-export rather than by restating them.
/// Presence, not absence, so it doubles as a falsifiability floor.
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
