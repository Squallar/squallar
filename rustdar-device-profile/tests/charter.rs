//! The crate's charter, held as tests: a closed dependency ceiling and the
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
        .expect("rustdar-device-profile sits one level under the workspace root")
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

/// The ceiling: declared `(kind, name)` pairs are a subset of the three the
/// charter names, and the two the crate's identity rests on are really there.
///
/// rustdar-radar (+ log) is forced, not convenient: `VoxelShape` sits in this
/// crate's public signatures (`grid_shape`, `volume_grid_shape`,
/// `VOLUME_GRID_FLOOR_SHAPE`) and the wire/raster ceilings the brackets are
/// denominated in are radar's vocabulary — parameterising those return types
/// away is not possible without moving radar's voxel vocabulary, which is
/// fenced.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-device-profile");

    let allowed: BTreeSet<(String, String)> = [
        ("normal", "rustdar-radar"),
        ("normal", "log"),
        ("dev", "serde_json"),
    ]
    .into_iter()
    .map(|(k, n)| (k.to_string(), n.to_string()))
    .collect();

    for entry in &deps {
        assert!(
            allowed.contains(entry),
            "rustdar-device-profile declares {} ({}). rustdar-device-profile \
             is the device-class policy floor — pure data/policy denominated \
             in rustdar-radar's size vocabulary; a dependency lands here only \
             after this charter and the plan change first, in writing. In \
             particular: NEVER wgpu (the 3D floor is a pinned literal, held \
             to wgpu by rustdar-app's agreement test), NEVER rustdar-egui \
             (the pane caps moved HERE so the UI could sit above the floor), \
             NEVER rustdar-kv until the kv lane lands the memo re-home in \
             writing.",
            entry.1,
            entry.0,
        );
    }

    // Falsifiability floor: the crate really declares its two load-bearing
    // dependencies, so a broken parse or a renamed package cannot pass as an
    // empty set.
    for (kind, name) in [("normal", "rustdar-radar"), ("dev", "serde_json")] {
        assert!(
            deps.iter().any(|(k, n)| k == kind && n == name),
            "rustdar-device-profile no longer declares {name} ({kind}) — \
             either the crate changed shape or this test is reading the wrong \
             package: {deps:?}",
        );
    }
}

/// The graph shape WO-RD created stays: the app side stands on the policy
/// floor — rustdar-app reads its budgets from here, and rustdar-egui
/// reads the pane caps from here rather than the other way round.
///
/// Presence, not absence, so it doubles as this file's second falsifiability
/// floor: a renamed package or a broken parse cannot pass it.
#[test]
fn the_floor_sits_under_the_app_side() {
    let meta = metadata();
    for consumer in ["rustdar-app", "rustdar-egui"] {
        let deps = declared_deps(&meta, consumer);
        assert!(
            deps.iter()
                .any(|(kind, name)| kind == "normal" && name == "rustdar-device-profile"),
            "{consumer} no longer stands on rustdar-device-profile: {deps:?}",
        );
    }
}
