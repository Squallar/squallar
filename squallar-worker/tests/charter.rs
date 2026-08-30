//! The crate's charter, held as tests: a dependency ceiling and the graph
//! position it exists to hold.
//!
//! Both read `cargo metadata --no-deps --format-version 1`, whose
//! `packages[].dependencies` are *declared* deps — feature- and
//! resolution-independent, so no feature selection can mask them. A
//! workspace-internal dep appears with `"req": "*"` and a `path`; `kind` is
//! `null` for normal deps (normalised to "normal" below); one name may
//! appear once per kind, so entries are judged per `(kind, name)`.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-worker sits one level under the workspace root")
        .to_path_buf()
}

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
/// normal dependency; target-gated entries carry their kind and are included.
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

/// The exact declared set, both kinds — an equality, not a ceiling.
#[test]
fn the_dependency_ceiling_holds() {
    let expected: BTreeSet<(String, String)> = [
        ("normal", "squallar-source"),
        ("normal", "squallar-radar"),
        ("normal", "squallar-overlays"),
        ("normal", "squallar-elevation"),
        ("normal", "squallar-device-profile"),
        ("normal", "squallar-geo"),
        ("normal", "egui"),
        ("normal", "log"),
        ("normal", "web-time"),
        ("dev", "chrono"),
        ("dev", "nexrad-model"),
        ("dev", "serde_json"),
    ]
    .into_iter()
    .map(|(k, n)| (k.to_string(), n.to_string()))
    .collect();

    let meta = metadata();
    let deps = declared_deps(&meta, "squallar-worker");

    assert_eq!(
        deps, expected,
        "squallar-worker's declared dependencies moved off the charter: the \
         engine composes the source crates and reads the device floor; egui \
         is the premultiply exception, recorded; squallar-elevation is the \
         third codec registry, chained last so no wire code is renumbered; a \
         new dependency changes this test and the plan first, in writing.",
    );
}

/// The engine sits above the vocabulary and pipeline crates and below the
/// app side; neither squallar-radar nor squallar-overlays may declare it back.
#[test]
fn the_engine_sits_above_the_vocabulary() {
    let meta = metadata();

    for consumer in ["squallar-app", "squallar-web"] {
        let deps = declared_deps(&meta, consumer);
        assert!(
            deps.iter()
                .any(|(kind, name)| kind == "normal" && name == "squallar-worker"),
            "{consumer} no longer stands on squallar-worker — either the \
             engine moved again or this test is reading the wrong package: \
             {deps:?}",
        );
    }

    for below in ["squallar-radar", "squallar-overlays"] {
        let deps = declared_deps(&meta, below);
        assert!(
            !deps.iter().any(|(_, name)| name == "squallar-worker"),
            "{below} declares squallar-worker ({deps:?}). That direction is \
             the charter: the engine composes the pipeline crates' codec \
             rows; a pipeline crate that could see the engine could name its \
             own runner, and the composition point would stop being one \
             module.",
        );
    }
}
