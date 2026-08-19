//! The crate's charter, held as tests: a dependency ceiling and the graph
//! position it exists to hold.
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
        .expect("rustdar-worker sits one level under the workspace root")
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

/// The exact declared set, both kinds — an equality, not a ceiling, so a
/// dropped dependency is as loud as a new one and the census below never
/// rots silently.
#[test]
fn the_dependency_ceiling_holds() {
    let expected: BTreeSet<(String, String)> = [
        ("normal", "rustdar-source"),
        ("normal", "rustdar-radar"),
        ("normal", "rustdar-overlays"),
        ("normal", "rustdar-device-profile"),
        ("normal", "rustdar-geo"),
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
    let deps = declared_deps(&meta, "rustdar-worker");

    assert_eq!(
        deps, expected,
        "rustdar-worker's declared dependencies moved off the charter: the \
         engine composes the source crates and reads the device floor; egui \
         is the premultiply exception, recorded; a new dependency changes \
         this test and the plan first, in writing.",
    );
}

/// The direction pin: the engine sits ABOVE the vocabulary and pipeline
/// crates and BELOW the app side. rustdar-app and rustdar-web each stand
/// on rustdar-worker (the presence half that keeps the absence half
/// falsifiable), and neither rustdar-radar nor rustdar-overlays may ever
/// declare it back — codec rows live beside their pipelines and are composed
/// HERE, never the other way round.
#[test]
fn the_engine_sits_above_the_vocabulary() {
    let meta = metadata();

    for consumer in ["rustdar-app", "rustdar-web"] {
        let deps = declared_deps(&meta, consumer);
        assert!(
            deps.iter()
                .any(|(kind, name)| kind == "normal" && name == "rustdar-worker"),
            "{consumer} no longer stands on rustdar-worker — either the \
             engine moved again or this test is reading the wrong package: \
             {deps:?}",
        );
    }

    for below in ["rustdar-radar", "rustdar-overlays"] {
        let deps = declared_deps(&meta, below);
        assert!(
            !deps.iter().any(|(_, name)| name == "rustdar-worker"),
            "{below} declares rustdar-worker ({deps:?}). That direction is \
             the charter: the engine composes the pipeline crates' codec \
             rows; a pipeline crate that could see the engine could name its \
             own runner, and the composition point would stop being one \
             module.",
        );
    }
}
