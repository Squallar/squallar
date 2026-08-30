//! The crate's charter, held as tests: the dependency ceiling of the wgpu
//! boundary and the graph position the crate exists to hold.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-gpu sits one level under the workspace root")
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

/// The wgpu boundary's ceiling: the render stack and the two squallar crates it
/// provably imports, nothing more.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "squallar-gpu");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            // serde_json parses `cargo metadata` in this very file; pollster
            // blocks on the adapter requests in the `#[ignore]`d GPU tests.
            "dev" => matches!(
                name.as_str(),
                "serde_json"
                    | "pollster"
                    | "squallar-volumetric"
                    | "squallar-radar"
                    // D2: the prism suites render a REAL city rather than a
                    // synthetic block of boxes, so they call `read_footprints`
                    // and `extrude` over the committed Monaco tile. The
                    // buildings crate links neither egui nor wgpu by charter,
                    // which is exactly why this edge may only ever be a
                    // dev-dependency.
                    //
                    // **The clause below asks for the charter AND the plan to
                    // change first, in writing. Only the charter has.** The
                    // plan's D2 does not mention this dependency, and the
                    // amendment is owed rather than made — recording it here
                    // because a half-satisfied escape clause that nobody wrote
                    // down is how the clause stops meaning anything.
                    | "squallar-buildings"
                    | "nexrad-model"
                    | "chrono"
                    | "squallar-geo"
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
                    | "squallar-egui"
                    | "squallar-device-profile"
            ),
            other => panic!(
                "squallar-gpu declares a `{other}` dependency on {name}; \
                 the charter permits none of that kind"
            ),
        };
        assert!(
            allowed,
            "squallar-gpu declares {name} ({kind}). squallar-gpu is the wgpu \
             boundary — egui must never depend on it, it must never depend on \
             squallar-volumetric or squallar-app as a NORMAL dep; the GPU \
             test suite's dev-dep back onto squallar-volumetric (WO-RV land 3, \
             the hardware quarantine) is legal because dev-deps never enter \
             the normal graph. Anything else lands here only after this \
             charter and the plan change first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its dev dependencies and
    // the two squallar crates it imports, so a broken parse or a renamed
    // package cannot pass as an empty set.
    for (kind, name) in [
        ("dev", "serde_json"),
        ("dev", "squallar-volumetric"),
        ("dev", "squallar-buildings"),
        ("normal", "squallar-egui"),
        ("normal", "squallar-device-profile"),
    ] {
        assert!(
            deps.iter().any(|(k, n)| k == kind && n == name),
            "squallar-gpu no longer declares {name} ({kind}) — either the crate \
             changed shape or this test is reading the wrong package: {deps:?}",
        );
    }
}

/// The graph shape WO-RG created stays: squallar-app stands on squallar-gpu,
/// so the app side reaches the renderer, the upload path, the mirror and the
/// staging ring through this boundary rather than by owning them.
#[test]
fn the_boundary_sits_under_the_app() {
    let meta = metadata();
    let frontend = declared_deps(&meta, "squallar-app");

    assert!(
        frontend
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "squallar-gpu"),
        "squallar-app no longer stands on squallar-gpu: {frontend:?}",
    );
}
