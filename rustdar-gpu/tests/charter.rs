//! The crate's charter, held as tests: the dependency ceiling of the wgpu
//! boundary and the graph position the crate exists to hold.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-gpu sits one level under the workspace root")
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

/// The wgpu boundary's ceiling: the render stack and the two rustdar crates it
/// provably imports, nothing more.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-gpu");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            // serde_json parses `cargo metadata` in this very file; pollster
            // blocks on the adapter requests in the `#[ignore]`d GPU tests.
            "dev" => matches!(
                name.as_str(),
                "serde_json"
                    | "pollster"
                    | "rustdar-volumetric"
                    | "rustdar-radar"
                    | "nexrad-model"
                    | "chrono"
                    | "rustdar-geo"
                    | "naga"
                    | "syn"
            ),
            "normal" => matches!(
                name.as_str(),
                "egui"
                    | "egui-wgpu"
                    | "egui-winit"
                    | "winit"
                    | "wgpu"
                    | "rustdar-egui"
                    | "rustdar-device-profile"
            ),
            other => panic!(
                "rustdar-gpu declares a `{other}` dependency on {name}; \
                 the charter permits none of that kind"
            ),
        };
        assert!(
            allowed,
            "rustdar-gpu declares {name} ({kind}). rustdar-gpu is the wgpu \
             boundary — egui must never depend on it, it must never depend on \
             rustdar-volumetric or rustdar-app as a NORMAL dep; the GPU \
             test suite's dev-dep back onto rustdar-volumetric (WO-RV land 3, \
             the hardware quarantine) is legal because dev-deps never enter \
             the normal graph. Anything else lands here only after this \
             charter and the plan change first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its dev dependencies and
    // the two rustdar crates it imports, so a broken parse or a renamed
    // package cannot pass as an empty set.
    for (kind, name) in [
        ("dev", "serde_json"),
        ("dev", "rustdar-volumetric"),
        ("normal", "rustdar-egui"),
        ("normal", "rustdar-device-profile"),
    ] {
        assert!(
            deps.iter().any(|(k, n)| k == kind && n == name),
            "rustdar-gpu no longer declares {name} ({kind}) — either the crate \
             changed shape or this test is reading the wrong package: {deps:?}",
        );
    }
}

/// The graph shape WO-RG created stays: rustdar-app stands on rustdar-gpu,
/// so the app side reaches the renderer, the upload path, the mirror and the
/// staging ring through this boundary rather than by owning them.
#[test]
fn the_boundary_sits_under_the_app() {
    let meta = metadata();
    let frontend = declared_deps(&meta, "rustdar-app");

    assert!(
        frontend
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "rustdar-gpu"),
        "rustdar-app no longer stands on rustdar-gpu: {frontend:?}",
    );
}
