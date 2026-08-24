//! The crate's charter, held as tests: a closed dependency ceiling and the
//! graph position the crate exists to hold.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-device-profile sits one level under the workspace root")
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

/// The ceiling: declared `(kind, name)` pairs are a subset of the three the
/// charter names, and the two the crate's identity rests on are really there.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "squallar-device-profile");

    let allowed: BTreeSet<(String, String)> = [
        ("normal", "squallar-radar"),
        ("normal", "log"),
        ("dev", "serde_json"),
    ]
    .into_iter()
    .map(|(k, n)| (k.to_string(), n.to_string()))
    .collect();

    for entry in &deps {
        assert!(
            allowed.contains(entry),
            "squallar-device-profile declares {} ({}). squallar-device-profile \
             is the device-class policy floor — pure data/policy denominated \
             in squallar-radar's size vocabulary; a dependency lands here only \
             after this charter and the plan change first, in writing. In \
             particular: NEVER wgpu (the 3D floor is a pinned literal, held \
             to wgpu by squallar-app's agreement test), NEVER squallar-egui \
             (the pane caps moved HERE so the UI could sit above the floor), \
             NEVER squallar-kv until the kv lane lands the memo re-home in \
             writing.",
            entry.1,
            entry.0,
        );
    }

    // Falsifiability floor: a broken parse cannot pass as an empty set.
    for (kind, name) in [("normal", "squallar-radar"), ("dev", "serde_json")] {
        assert!(
            deps.iter().any(|(k, n)| k == kind && n == name),
            "squallar-device-profile no longer declares {name} ({kind}) — \
             either the crate changed shape or this test is reading the wrong \
             package: {deps:?}",
        );
    }
}

/// The app side stands on the policy floor: squallar-app reads its budgets from
/// here, and squallar-egui reads the pane caps from here.
#[test]
fn the_floor_sits_under_the_app_side() {
    let meta = metadata();
    for consumer in ["squallar-app", "squallar-egui"] {
        let deps = declared_deps(&meta, consumer);
        assert!(
            deps.iter()
                .any(|(kind, name)| kind == "normal" && name == "squallar-device-profile"),
            "{consumer} no longer stands on squallar-device-profile: {deps:?}",
        );
    }
}
