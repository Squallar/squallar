//! The crate's charter, held as tests: the dependency ceiling of the 3D volume
//! stack and the graph position the crate exists to hold.
//!
//! Both read `cargo metadata --no-deps --format-version 1`, whose
//! `packages[].dependencies` are *declared* deps — feature- and
//! resolution-independent. `kind` is `null` for normal deps (normalised to
//! "normal" below); entries are judged per `(kind, name)`.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-volumetric sits one level under the workspace root")
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

/// `(kind, name)` for every dependency `package` declares; target-gated
/// entries are included.
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

/// The 3D stack's ceiling: the render substrate it marches through and the
/// vocabularies it draws. No direct `wgpu` entry: every `wgpu::` value rides
/// `egui_wgpu::wgpu`, and the single-copy guard depends on that.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "squallar-volumetric");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            // pollster blocks on the adapter/device requests in the
            // `#[ignore]`d unit tests; nexrad-model + chrono stamp the
            // synthetic scan the bridge tests feed the voxel builder.
            "dev" => matches!(
                name.as_str(),
                "serde_json" | "pollster" | "nexrad-model" | "chrono" | "squallar-geo"
            ),
            "normal" => matches!(
                name.as_str(),
                "squallar-gpu"
                    | "squallar-device-profile"
                    | "squallar-radar"
                    | "squallar-egui"
                    | "egui"
                    | "egui-wgpu"
                    | "half"
                    | "log"
            ),
            other => panic!(
                "squallar-volumetric declares a `{other}` dependency on {name}; \
                 the charter permits none of that kind"
            ),
        };
        assert!(
            allowed,
            "squallar-volumetric declares {name} ({kind}). The 3D stack sits on \
             the wgpu boundary (squallar-gpu), the policy floor \
             (squallar-device-profile) and the voxel/pane vocabularies \
             (squallar-radar, squallar-egui) — never on wgpu directly (the \
             boundary is the one feature-chooser and the single-copy guard \
             depends on it), never on squallar-app, and CPU rasterizers \
             never move here. Anything else lands here only after this \
             charter and the plan change first, in writing.",
        );
    }

    // Falsifiability floor: an empty set cannot pass.
    for (kind, name) in [
        ("dev", "serde_json"),
        ("normal", "squallar-gpu"),
        ("normal", "squallar-device-profile"),
        ("normal", "squallar-radar"),
        ("normal", "squallar-egui"),
    ] {
        assert!(
            deps.iter().any(|(k, n)| k == kind && n == name),
            "squallar-volumetric no longer declares {name} ({kind}) — either the \
             crate changed shape or this test is reading the wrong package: \
             {deps:?}",
        );
    }
}

/// squallar-app stands on squallar-volumetric. Presence, not absence, so it
/// doubles as a falsifiability floor.
#[test]
fn the_stack_sits_under_the_app() {
    let meta = metadata();
    let frontend = declared_deps(&meta, "squallar-app");

    assert!(
        frontend
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "squallar-volumetric"),
        "squallar-app no longer stands on squallar-volumetric: {frontend:?}",
    );
}
