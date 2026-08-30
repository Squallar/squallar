//! The crate's charter, held as tests: what it may declare, and what the
//! resolved graph is allowed to reach.
//!
//! This crate exists so that the `building` layer's parse, its projection and
//! its tessellation can run **inside the offload worker**, which links neither
//! egui, wgpu nor winit. That sentence is worth nothing on its own, so it is
//! two tests: a declared ceiling read from `cargo metadata --no-deps`, and a
//! walk of the *resolved* graph, which is the one that says what actually gets
//! linked.
//!
//! Shaped after `squallar-elevation/tests/charter.rs`, which holds the same
//! property for the terrain half. The two are separate crates and not one
//! because their dependency sets have nothing in common past `squallar-geo`
//! and `squallar-source`: a Terrain-RGB decoder needs a PNG decoder and a
//! footprint extruder needs a tessellator, and neither wants the other linked
//! into every one of its test runs.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-buildings sits one level under the workspace root")
        .to_path_buf()
}

fn metadata(args: &[&str]) -> serde_json::Value {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.arg("metadata").args(args);
    let out = cmd
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
    meta["packages"]
        .as_array()
        .expect("metadata carries a packages array")
        .iter()
        .find(|p| p["name"].as_str() == Some(package))
        .unwrap_or_else(|| panic!("workspace member `{package}` is missing from cargo metadata"))
        ["dependencies"]
        .as_array()
        .expect("a package carries a dependencies array")
        .iter()
        .map(|d| {
            (
                d["kind"].as_str().unwrap_or("normal").to_string(),
                d["name"]
                    .as_str()
                    .expect("a dependency has a name")
                    .to_string(),
            )
        })
        .collect()
}

/// The exact declared set. Equality and not a subset of a list, because every
/// entry here is a decision.
///
/// **`lyon_tessellation`, `mvt-reader` and `geo-types` are the arrival this
/// crate was created to hold**, and the plan said so in writing before it
/// happened, on the precedent `squallar-elevation` set when its own job row
/// brought `squallar-source` in. The claim the plan carried — "no new
/// dependency" — was true only of `squallar-egui`, which reaches all three
/// behind `vendor/walkers`' `mvt` feature. A worker-side crate cannot reach
/// them that way, because walkers is an egui widget.
///
/// **What the arrival cost, measured on this tree 2026-08-30 — denominator:
/// distinct packages reachable *as dependencies*, roots themselves excluded:**
///
/// * `squallar-buildings`' own resolved closure: **172 packages, zero
///   forbidden**;
/// * `squallar-elevation`'s, for scale, the crate this one is modelled on:
///   **182, zero forbidden**;
/// * **new packages in `Cargo.lock`: zero.** All three arrivals resolve to
///   versions the workspace already locked — `mvt-reader` 2.4.0,
///   `lyon_tessellation` 1.0.20, `geo-types` 0.7.19 — because
///   `squallar-egui` enables walkers' `mvt` feature unconditionally and that
///   is where they already were. The whole cost of this charter is the three
///   lines below and the one in `squallar-worker/tests/charter.rs`.
///
/// `lyon_path` is deliberately **not** declared even though it is used:
/// `lyon_tessellation` re-exports the whole of it as
/// `lyon_tessellation::path`, so one entry buys both.
/// `squallar-device-profile` is deliberately absent for the reason
/// `squallar-elevation` records — nothing here reaches it, and a
/// declared-but-unused dependency on it would drag `squallar-radar` into every
/// `cargo test -p squallar-buildings` for nothing.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata(&["--no-deps", "--format-version", "1"]);
    let deps = declared_deps(&meta, "squallar-buildings");

    let expected: BTreeSet<(String, String)> = [
        ("normal", "geo-types"),
        ("normal", "log"),
        ("normal", "lyon_tessellation"),
        ("normal", "mvt-reader"),
        ("normal", "squallar-geo"),
        ("normal", "squallar-source"),
        ("dev", "serde_json"),
    ]
    .into_iter()
    .map(|(k, n)| (k.to_string(), n.to_string()))
    .collect();

    assert_eq!(
        deps, expected,
        "squallar-buildings' declared dependencies have moved. This crate is \
         the parse-and-extrude half of the buildings path and it has to link \
         inside the offload worker, which links neither egui, wgpu nor winit; \
         a dependency lands here only after this charter and the plan change \
         first, in writing.",
    );
}

/// Every package the resolved graph reaches from this crate, over all targets.
fn resolved_closure() -> BTreeSet<String> {
    let meta = metadata(&["--format-version", "1"]);
    let name_of: std::collections::BTreeMap<String, String> = meta["packages"]
        .as_array()
        .expect("metadata carries a packages array")
        .iter()
        .map(|p| {
            (
                p["id"].as_str().expect("a package has an id").to_string(),
                p["name"]
                    .as_str()
                    .expect("a package has a name")
                    .to_string(),
            )
        })
        .collect();
    let deps_of: std::collections::BTreeMap<String, Vec<String>> = meta["resolve"]["nodes"]
        .as_array()
        .expect("a resolve carries nodes")
        .iter()
        .map(|n| {
            (
                n["id"].as_str().expect("a node has an id").to_string(),
                n["dependencies"]
                    .as_array()
                    .expect("a node carries dependencies")
                    .iter()
                    .map(|d| d.as_str().expect("a dependency id is a string").to_string())
                    .collect(),
            )
        })
        .collect();

    let root = name_of
        .iter()
        .find(|(_, n)| n.as_str() == "squallar-buildings")
        .map(|(id, _)| id.clone())
        .expect("squallar-buildings is in the resolve");

    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(id) = queue.pop_front() {
        for next in deps_of.get(&id).into_iter().flatten() {
            if seen.insert(name_of[next].clone()) {
                queue.push_back(next.clone());
            }
        }
    }
    seen
}

/// The property the crate exists for: nothing a browser Worker or a native
/// offload thread cannot link.
///
/// The **resolved** graph, not the declared one — a renderer three crates down
/// still gets linked, and it is the linking that this crate's whole reason for
/// being is about.
///
/// **`walkers` is on the forbidden list and that is the interesting entry.**
/// It is where `mvt-reader` and `lyon_tessellation` reach this workspace from,
/// so reaching them *through* it would look like the cheaper route and would
/// silently put egui in this graph. The two are declared directly instead, and
/// this assertion is what keeps that from being undone by a later hand.
///
/// **`reqwest` is deliberately not on the list**, for the reason
/// `squallar-elevation` records: `squallar-source` declares it
/// unconditionally, it compiles in a worker, and asserting its absence would
/// be a charter fight over a property nothing needs.
#[test]
fn the_offload_worker_can_link_this_crate() {
    let reached = resolved_closure();

    for forbidden in [
        "egui",
        "egui-wgpu",
        "egui-winit",
        "eframe",
        "wgpu",
        "wgpu-core",
        "wgpu-hal",
        "winit",
        "walkers",
    ] {
        assert!(
            !reached.contains(forbidden),
            "squallar-buildings' resolved graph reaches `{forbidden}`. The \
             parse and the tessellation run inside the offload worker, which \
             links none of the renderer or windowing stack.",
        );
    }

    // Falsifiability floor. An empty or one-element closure would satisfy
    // every absence above, so the walk has to be shown to have found the
    // graph.
    assert!(
        reached.contains("mvt-reader")
            && reached.contains("lyon_tessellation")
            && reached.contains("lyon_path")
            && reached.contains("geo-types")
            && reached.contains("squallar-geo"),
        "the resolve walk found {reached:?}, which is not this crate's graph",
    );
    // Measured on this tree: **172**, roots excluded. Held as a floor rather
    // than an equality -- a patch release adding a transitive package is not
    // an architecture change, and pinning the count would make it read as one
    // -- but a floor an order of magnitude below actual is not a floor, it is
    // a formality, so it moves with the measurement it is taken from.
    assert!(
        reached.len() >= 140,
        "the resolve walk reached only {} packages, against the 172 measured \
         on 2026-08-30; it is not seeing this crate's graph: {reached:?}",
        reached.len(),
    );
    // `lyon_path` really is free: it arrives through `lyon_tessellation`'s own
    // re-export whether or not this crate declares it, which is what made the
    // ceiling above seven lines rather than eight. If this ever fails,
    // declaring it separately became necessary and the charter's record of
    // why it is absent is describing a decision that no longer holds.
    assert!(
        reached.contains("lyon_path"),
        "`lyon_path` is not in the resolved graph, so `lyon_tessellation`'s \
         re-export is no longer what serves this crate's paths: {reached:?}",
    );
}
