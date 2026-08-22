//! The crate's charter, held as tests: a Band 0 position with no first-party
//! dependencies, and the consumer edge that says the crate is actually reached.
//!
//! Both read `cargo metadata --no-deps` from the workspace root, whose
//! `packages[].dependencies` are *declared* deps — feature-independent, so no
//! feature selection can mask them. Entries are judged per `(kind, name)`.
//!
//! Why the first-party rule is worth a test of its own: the charter in
//! `rustdar-source` walks the **transitive** closure to keep the
//! overlays→radar edge cut, so a first-party dependency added here would reach
//! `rustdar-overlays` and could re-open that edge from underneath. Keeping this
//! crate a leaf makes that impossible rather than merely unlikely.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-netcdf sits one level under the workspace root")
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

/// **This crate declares no first-party dependency, of any kind.**
///
/// That is what makes it a Band 0 leaf. The allow-list is exact rather than a
/// prefix rule: each name is here because a NetCDF4/CF reader genuinely needs
/// it, and a new one is a decision to write down, not to discover in a diff.
#[test]
fn the_crate_is_a_band_zero_leaf() {
    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-netcdf");

    for (kind, name) in &deps {
        assert!(
            !name.starts_with("rustdar-") && name != "nexrad-level3",
            "rustdar-netcdf declares the first-party crate {name} ({kind}). \
             It is a Band 0 leaf: it knows NetCDF4/HDF5 and the CF conventions \
             and nothing about this workspace's domain. A first-party edge here \
             also lands in `rustdar-overlays`, where the transitive charter in \
             rustdar-source keeps the overlays->radar edge cut.",
        );
        let allowed = match kind.as_str() {
            "normal" => matches!(name.as_str(), "chrono" | "hdf5-pure" | "log"),
            "dev" => name == "serde_json",
            other => panic!(
                "rustdar-netcdf declares a `{other}` dependency on {name}; \
                 the charter permits none"
            ),
        };
        assert!(
            allowed,
            "rustdar-netcdf declares {name} ({kind}), which its charter does \
             not list. Add it here with the reason, or do not add it.",
        );
    }

    // Falsifiability floor: an empty or misread parse cannot pass the loop
    // above, because the loop is vacuous over an empty set.
    for required in ["chrono", "hdf5-pure", "log"] {
        assert!(
            deps.iter().any(|(k, n)| k == "normal" && n == required),
            "rustdar-netcdf no longer declares {required} (normal) — either the \
             crate changed shape or this test is reading the wrong package: \
             {deps:?}",
        );
    }
}

/// The extracted crate is actually reached: `rustdar-overlays` stands on it.
///
/// Presence, not absence, so it doubles as a falsifiability floor — and it is
/// the edge that would rot silently if the two consumers were ever quietly
/// re-pointed at a copy of this code.
#[test]
fn the_format_layer_sits_under_the_data_crate() {
    let meta = metadata();
    let overlays = declared_deps(&meta, "rustdar-overlays");

    assert!(
        overlays
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "rustdar-netcdf"),
        "rustdar-overlays no longer stands on rustdar-netcdf: {overlays:?}",
    );
}
