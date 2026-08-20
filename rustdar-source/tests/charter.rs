//! The crate's charter, held as tests: a dependency ceiling and the graph
//! shape it exists to create.
//!
//! Both read `cargo metadata --no-deps --format-version 1` from the workspace
//! root. `packages[].dependencies` there are *declared* dependencies —
//! feature-independent and resolution-independent — so no feature selection
//! (default, `--all-features`, CI's llvm-cov arm) can mask or fake what these
//! assert. Dep-name mechanics, recorded at M0 and relied on here: a
//! workspace-internal dep appears with `"req": "*"` and a `path`; `kind` is
//! `null` for normal deps (normalised to "normal" below); one name may
//! legitimately appear once per kind (tokio is normal *and* dev for some
//! members), so entries are judged per `(kind, name)`. Assertions key on
//! dependency *names*, never on feature emptiness — a later feature on either
//! crate must not disturb them.
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

/// The substrate stays a substrate: its dependency set may not grow past the
/// charter. `[dependencies]` ⊆ {rustdar-geo, rustdar-units, chrono, serde,
/// serde_json, web-time, reqwest, rustls, log}; dev additionally {tokio}; no
/// build deps.
///
/// `rustdar-geo` entered the ceiling at WO-G1 (the Phase 2G rustdar-geo
/// insertion, campaign plan amendment of 2026-08-18) — the written amendment
/// the failure message below demands: the horizontal geodesy this crate's
/// `geo` module used to define moved down to the workspace's geometry floor,
/// and the module re-exports it wholesale so every path above keeps
/// resolving.
///
/// **`serde_json` crossed from `dev` to `normal` at WO-M9**, and this
/// paragraph is that step's written amendment. The `SourceHandler` trait moved
/// into this crate with its four config-persistence hooks
/// (`serialize_state`/`deserialize_state`/`serialize_pane_state`/
/// `deserialize_pane_state`), every one of them typed on `serde_json::Value`
/// — the shape a layer's saved state has taken in every user's config file
/// since long before this crate existed. It is vocabulary the contract is
/// *made of*, not machinery the contract reaches for, which is the line this
/// ceiling draws; and the package was already declared here, as the dev
/// dependency `cargo metadata` parsing above uses. The three other names the
/// move needed — `rustdar-units`, `web-time`, `serde` — were already on the
/// ceiling and required no amendment. **No third-party package new to this
/// crate entered the graph at WO-M9.**
///
/// `serde_json` stays out of `DEV_EXTRA` on purpose: it is now a normal
/// dependency, so `NORMAL_CEILING` already permits it for both kinds and a
/// second entry would be a second place to keep in step.
///
/// A ⊆ ceiling and not equality on purpose: the charter *allows* the unlisted
/// members (serde arrives with later steps) without requiring them. The floor
/// assertion at the bottom is what keeps this test falsifiable — an empty
/// parse cannot pass it.
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

    // Falsifiability floor: the crate really declares dependencies, so a
    // broken parse or a renamed package cannot pass as an empty set.
    assert!(
        deps.iter().any(|(k, n)| k == "normal" && n == "reqwest"),
        "rustdar-source no longer declares reqwest — either the crate changed \
         shape or this test is reading the wrong package: {deps:?}",
    );
}

/// The edge WO-M3 cut stays cut: rustdar-overlays declares NO dependency on
/// rustdar-radar, of any kind, while both crates stand on rustdar-source.
///
/// The second half is the presence control that keeps the first falsifiable:
/// a test that only asserted absence would pass just as green against a
/// renamed package or a broken parse.
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
