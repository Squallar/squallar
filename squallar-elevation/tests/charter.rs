//! The crate's charter, held as tests: what it may declare, and what the
//! resolved graph is allowed to reach.
//!
//! This crate exists so that Terrain-RGB decode and the box resample can run
//! **inside the offload worker**, which links neither egui, wgpu nor winit.
//! That sentence is worth nothing on its own, so it is two tests: a declared
//! ceiling read from `cargo metadata --no-deps`, and a walk of the *resolved*
//! graph, which is the one that says what actually gets linked.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-elevation sits one level under the workspace root")
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
                d["name"].as_str().expect("a dependency has a name").to_string(),
            )
        })
        .collect()
}

/// The exact declared set. Equality and not ⊆ a list, because every entry here
/// is a decision.
///
/// **`squallar-source` and `squallar-device-profile` are deliberately absent
/// for now, and `squallar-source` is a scheduled arrival rather than an open
/// question.** `JobCodec` — the vtable a job registry row is built from,
/// a `struct` and not a trait — is defined in `squallar-source/src/job.rs`, so
/// the work unit that adds the height job *will* declare `squallar-source`, and
/// this list gains a line then. Nothing in this
/// crate reaches either crate today, and a declared-but-unused dependency on
/// `squallar-device-profile` would drag `squallar-radar` into every
/// `cargo test -p squallar-elevation` for nothing.
///
/// **That arrival is a one-line ceiling edit and not a charter fight**, which is
/// worth writing down here so it is not rediscovered as a surprise. Resolved
/// closures over `cargo metadata`'s `resolve.nodes`, measured 2026-08-30 —
/// **denominator: distinct packages reachable *as dependencies*, roots
/// themselves excluded**:
///
/// * from `squallar-elevation` alone: **41 packages, zero forbidden, no
///   `reqwest`**;
/// * from elevation ∪ source ∪ device-profile: **236, still zero forbidden**.
///
/// (A review of this work unit quoted 237 for the second figure. The one-package
/// gap is the denominator, not a disagreement: `squallar-elevation` is a root
/// nothing depends on, so counting the roots too gives 237 there and 42 for the
/// first. Same graph, two conventions.)
///
/// What the larger closure *does* add is `reqwest`, which is why
/// [`the_offload_worker_can_link_this_crate`] does not name it — see that test's
/// own docs. Adding a dependency still changes this test and the plan first, in
/// writing; this paragraph is that writing for `squallar-source`.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata(&["--no-deps", "--format-version", "1"]);
    let deps = declared_deps(&meta, "squallar-elevation");

    let expected: BTreeSet<(String, String)> = [
        ("normal", "image"),
        ("normal", "squallar-geo"),
        ("dev", "serde_json"),
    ]
    .into_iter()
    .map(|(k, n)| (k.to_string(), n.to_string()))
    .collect();

    assert_eq!(
        deps, expected,
        "squallar-elevation's declared dependencies have moved. This crate is \
         the decode-and-resample half of the terrain path and it has to link \
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
        .find(|(_, n)| n.as_str() == "squallar-elevation")
        .map(|(id, _)| id.clone())
        .expect("squallar-elevation is in the resolve");

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
/// **`reqwest` is deliberately not on this list**, and the omission is the
/// interesting part. The plan reserves `squallar-source` as a future dependency
/// here, and `squallar-source` declares `reqwest` unconditionally (it is where
/// the TLS provider is installed). So "this crate does not link reqwest" is
/// true of the *declared* set today and will stop being true of the resolved
/// graph the moment the job codec lands. Asserting it here would turn that work
/// unit into a charter fight over a property the plan never actually needed:
/// reqwest compiles in a worker, and egui, wgpu and winit do not.
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
            "squallar-elevation's resolved graph reaches `{forbidden}`. Decode \
             and resample run inside the offload worker, which links none of \
             the renderer or windowing stack.",
        );
    }

    // Falsifiability floor. An empty or one-element closure would satisfy every
    // absence above, so the walk has to be shown to have found the graph.
    assert!(
        reached.contains("image") && reached.contains("png") && reached.contains("squallar-geo"),
        "the resolve walk found {reached:?}, which is not this crate's graph",
    );
    // Measured 2026-08-30: 41, roots excluded. Held as a floor rather than an
    // equality — a patch release adding a transitive package is not an
    // architecture change, and pinning the count would make it read as one.
    assert!(
        reached.len() >= 20,
        "the resolve walk reached only {} packages, against the 41 measured on \
         2026-08-30; it is not seeing this crate's graph: {reached:?}",
        reached.len(),
    );
}
