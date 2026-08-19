//! The crate's charter, held as tests: the dependency ceiling of the 3D
//! volume stack and the graph position the crate exists to hold.
//!
//! Both read `cargo metadata --no-deps --format-version 1` from the workspace
//! root. `packages[].dependencies` there are *declared* dependencies —
//! feature-independent and resolution-independent — so no feature selection
//! (default, `--all-features`, CI's llvm-cov arm) can mask or fake what these
//! assert. Dep-name mechanics, recorded at M0 and relied on here: a
//! workspace-internal dep appears with `"req": "*"` and a `path`; `kind` is
//! `null` for normal deps (normalised to "normal" below); one name may
//! legitimately appear once per kind, so entries are judged per
//! `(kind, name)`. Target-gated entries carry their kind like any other and
//! are included — a gated dependency is still a dependency.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-volumetric sits one level under the workspace root")
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

/// The 3D stack's ceiling: the render substrate it marches through and the
/// vocabularies it draws, nothing more.
///
/// The fences, spelled out because each one is load-bearing:
/// * CPU rasterizers never move here — tiny-skia lives in rustdar-overlays and
///   the radar polar render in rustdar-radar, beside their sources, by
///   architecture.
/// * volumetric -> gpu is a NORMAL dependency; gpu -> volumetric exists only
///   as a dev-dep (WO-RV land 3, the hardware quarantine) — legal because
///   dev-deps never enter the normal graph.
/// * There is NO direct `wgpu` entry: every `wgpu::` value rides
///   `egui_wgpu::wgpu`, rustdar-gpu is the one feature-chooser, and the
///   single-copy guard at the boundary depends on this crate never resolving
///   `::wgpu` itself.
/// * `cargo test -p rustdar-volumetric` needs no adapter — adapter-needing
///   tests are `#[ignore]`d and CI runs them from the gpu job.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-volumetric");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            // serde_json parses `cargo metadata` in this very file; pollster
            // blocks on the adapter/device requests in the `#[ignore]`d unit
            // tests; nexrad-model + chrono build and stamp the synthetic scan
            // the bridge tests feed the voxel builder; rustdar-geo feeds the
            // uniform/bridge differential fixtures.
            "dev" => matches!(
                name.as_str(),
                "serde_json" | "pollster" | "nexrad-model" | "chrono" | "rustdar-geo"
            ),
            "normal" => matches!(
                name.as_str(),
                "rustdar-gpu"
                    | "rustdar-device-profile"
                    | "rustdar-radar"
                    | "rustdar-egui"
                    | "egui"
                    | "egui-wgpu"
                    | "half"
                    | "log"
            ),
            other => panic!(
                "rustdar-volumetric declares a `{other}` dependency on {name}; \
                 the charter permits none of that kind"
            ),
        };
        assert!(
            allowed,
            "rustdar-volumetric declares {name} ({kind}). The 3D stack sits on \
             the wgpu boundary (rustdar-gpu), the policy floor \
             (rustdar-device-profile) and the voxel/pane vocabularies \
             (rustdar-radar, rustdar-egui) — never on wgpu directly (the \
             boundary is the one feature-chooser and the single-copy guard \
             depends on it), never on rustdar-app, and CPU rasterizers \
             never move here. Anything else lands here only after this \
             charter and the plan change first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its dev dependencies and
    // the rustdar crates it imports, so a broken parse or a renamed package
    // cannot pass as an empty set.
    for (kind, name) in [
        ("dev", "serde_json"),
        ("normal", "rustdar-gpu"),
        ("normal", "rustdar-device-profile"),
        ("normal", "rustdar-radar"),
        ("normal", "rustdar-egui"),
    ] {
        assert!(
            deps.iter().any(|(k, n)| k == kind && n == name),
            "rustdar-volumetric no longer declares {name} ({kind}) — either the \
             crate changed shape or this test is reading the wrong package: \
             {deps:?}",
        );
    }
}

/// The graph shape WO-RV created stays: rustdar-app stands on
/// rustdar-volumetric, so the app side reaches the probe, the raymarch, the
/// staging path and the bridge through this crate rather than by owning them.
///
/// Presence, not absence, so it doubles as this file's second falsifiability
/// floor: a renamed package or a broken parse cannot pass it.
#[test]
fn the_stack_sits_under_the_app() {
    let meta = metadata();
    let frontend = declared_deps(&meta, "rustdar-app");

    assert!(
        frontend
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "rustdar-volumetric"),
        "rustdar-app no longer stands on rustdar-volumetric: {frontend:?}",
    );
}
