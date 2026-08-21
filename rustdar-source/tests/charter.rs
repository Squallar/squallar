//! The crate's charter, held as tests: a dependency ceiling and the graph
//! shape it exists to create.
//!
//! Both read `cargo metadata --no-deps --format-version 1`, whose
//! `packages[].dependencies` are *declared* dependencies, so no feature
//! selection can mask what these assert. One name may appear once per kind,
//! so entries are judged per `(kind, name)`.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-source sits one level under the workspace root")
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

/// The substrate stays a substrate: its dependency set may not grow past the
/// charter. `[dependencies]` ⊆ {rustdar-geo, rustdar-units, chrono, serde,
/// serde_json, web-time, reqwest, rustls, log}; dev additionally {tokio}; no
/// build deps. `serde_json` is a normal dependency, the `SourceHandler`
/// config-persistence hooks being typed on `serde_json::Value`.
///
/// ⊆ and not equality on purpose. The floor assertion at the bottom is what
/// keeps this test falsifiable — an empty parse cannot pass it.
#[test]
fn the_dependency_ceiling_holds() {
    const NORMAL_CEILING: &[&str] = &[
        "rustdar-geo",
        "rustdar-units",
        "chrono",
        "serde",
        "serde_json",
        "web-time",
        "reqwest",
        "rustls",
        "log",
    ];
    const DEV_EXTRA: &[&str] = &["tokio"];

    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-source");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            "normal" => NORMAL_CEILING.contains(&name.as_str()),
            "dev" => NORMAL_CEILING.contains(&name.as_str()) || DEV_EXTRA.contains(&name.as_str()),
            other => panic!(
                "rustdar-source declares a `{other}` dependency on {name}; \
                 the charter permits none"
            ),
        };
        assert!(
            allowed,
            "rustdar-source declares {name} ({kind}), which is outside the \
             charter ceiling. The substrate is contract/vocabulary only — if a \
             step genuinely needs this, the charter in this test and the plan \
             must change first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares dependencies.
    assert!(
        deps.iter().any(|(k, n)| k == "normal" && n == "reqwest"),
        "rustdar-source no longer declares reqwest — either the crate changed \
         shape or this test is reading the wrong package: {deps:?}",
    );
}

/// **Neither layer crate declares a GUI dependency, of any kind.**
///
/// The rule is older than this test and was held only by reading the manifests:
/// `rustdar-overlays` and `rustdar-source` are contract, vocabulary and
/// behaviour, and nothing in them may name the toolkit. `rustdar-source`'s
/// charter ceiling above is an allowlist and already refuses it; overlays had
/// no ceiling at all, so this is where the rule becomes a test instead of a
/// grep somebody once ran. WO-E10.4.
///
/// The floor is the falsifiability half: both crates must declare *something*,
/// or "declares no egui" is true of an empty list.
#[test]
fn no_layer_crate_declares_a_gui_dependency() {
    const GUI_CRATES: &[&str] = &["egui", "eframe", "epaint", "emath", "egui-winit"];
    let meta = metadata();
    for package in ["rustdar-overlays", "rustdar-source"] {
        let deps = declared_deps(&meta, package);
        assert!(
            !deps.is_empty(),
            "{package} declares no dependencies at all, so every absence below \
             is the reader's and not the manifest's",
        );
        let gui: Vec<_> = deps
            .iter()
            .filter(|(_, name)| GUI_CRATES.contains(&name.as_str()))
            .collect();
        assert!(
            gui.is_empty(),
            "{package} declares {gui:?}. These crates are the layer contract \
             and the vocabulary under it; a handler that can name the toolkit \
             can draw, and then the seam is no longer the only way in. \
             Anything genuinely shared belongs on the trait or in \
             rustdar-source's own types.",
        );
    }
}

/// rustdar-overlays declares NO dependency on rustdar-radar, of any kind. The
/// second half is the presence control that keeps the first falsifiable.
#[test]
fn the_overlays_to_radar_edge_stays_cut() {
    let meta = metadata();

    let overlays = declared_deps(&meta, "rustdar-overlays");
    let radar = declared_deps(&meta, "rustdar-radar");

    assert!(
        !overlays.iter().any(|(_, name)| name == "rustdar-radar"),
        "rustdar-overlays declares rustdar-radar again ({overlays:?}). That \
         edge is what forced every overlay handler to compile against the \
         whole radar pipeline; anything both sides need belongs in \
         rustdar-source instead.",
    );
    assert!(
        overlays
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "rustdar-source"),
        "rustdar-overlays no longer stands on rustdar-source: {overlays:?}",
    );
    assert!(
        radar
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "rustdar-source"),
        "rustdar-radar no longer stands on rustdar-source: {radar:?}",
    );
}
