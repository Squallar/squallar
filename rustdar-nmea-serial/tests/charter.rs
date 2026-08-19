//! The crate's charter, held as tests: the provider's dependency ceiling and
//! the graph position it exists to hold — one provider of
//! `rustdar_location::Fix`, standing ON the domain crate.
//!
//! Both helpers read `cargo metadata --no-deps --format-version 1` from the
//! workspace root. `packages[].dependencies` there are *declared*
//! dependencies — feature-independent and resolution-independent — so no
//! feature selection (default, `--all-features`, CI's llvm-cov arm) can mask
//! or fake what these assert. Dep-name mechanics, recorded at M0 and relied
//! on here: a workspace-internal dep appears with `"req": "*"` and a `path`;
//! `kind` is `null` for normal deps (normalised to "normal" below); one name
//! may legitimately appear once per kind, so entries are judged per
//! `(kind, name)`. Assertions key on dependency *names*, never on feature
//! emptiness — a later feature on either crate must not disturb them.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-nmea-serial sits one level under the workspace root")
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
/// are included — a gated dependency is still a dependency (`serialport` is
/// optional behind the `serial` feature and is judged here all the same).
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

/// The provider's allowance: the domain it provides for, the NMEA grammar, the
/// serial transport (optional, behind `serial`), time, logging and serde.
/// Nothing UI-side, nothing radar-side — a transport crate that could name
/// either would stop being a leaf providers can share.
#[test]
fn the_dependency_ceiling_holds() {
    const NORMAL_CEILING: &[&str] = &[
        "rustdar-location",
        "chrono",
        "log",
        "nmea",
        "serde",
        "serialport",
    ];

    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-nmea-serial");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            "dev" => name == "serde_json",
            "normal" => NORMAL_CEILING.contains(&name.as_str()),
            other => panic!(
                "rustdar-nmea-serial declares a `{other}` dependency on \
                 {name}; the charter permits none of that kind"
            ),
        };
        assert!(
            allowed,
            "rustdar-nmea-serial declares {name} ({kind}). rustdar-nmea-serial \
             is NMEA parsing and the serial-port transport — one provider of \
             rustdar_location::Fix; if a step genuinely needs a new \
             dependency, the charter in this test and the plan must change \
             first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its named deps, so a
    // broken parse or a renamed package cannot pass as an empty set.
    assert!(
        deps.iter().any(|(k, n)| k == "normal" && n == "nmea"),
        "rustdar-nmea-serial no longer declares nmea (normal) — either the \
         crate changed shape or this test is reading the wrong package: \
         {deps:?}",
    );
}

/// The graph shape WO-RL-1 created stays: the provider stands on the domain
/// crate, so every consumer of a fix reaches the one vocabulary rather than a
/// transport's private restatement of it.
///
/// Presence, not absence, so it doubles as this file's second falsifiability
/// floor: a renamed package or a broken parse cannot pass it.
#[test]
fn the_provider_sits_on_the_domain() {
    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-nmea-serial");

    assert!(
        deps.iter()
            .any(|(kind, name)| kind == "normal" && name == "rustdar-location"),
        "rustdar-nmea-serial no longer stands on rustdar-location: {deps:?}",
    );
}
