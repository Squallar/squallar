//! The crate's charter, held as tests: the parser's dependency ceiling and the
//! graph position it exists to hold — a parser/transport that does NOT know
//! the app's fix model, and names no squallar-* package at all.
//!
//! Both helpers read `cargo metadata --no-deps --format-version 1`, whose
//! `packages[].dependencies` are *declared* dependencies, so no feature
//! selection can mask them. Entries are judged per `(kind, name)`.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-nmea-serial sits one level under the workspace root")
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
/// normal dependency; target-gated and optional entries are included.
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

/// The parser's allowance: the NMEA grammar, the serial transport (optional,
/// behind `serial`), time, logging and serde. No squallar-* name of any kind.
#[test]
fn the_dependency_ceiling_holds() {
    const NORMAL_CEILING: &[&str] = &["chrono", "log", "nmea", "serde", "serialport"];

    let meta = metadata();
    let deps = declared_deps(&meta, "squallar-nmea-serial");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            "dev" => name == "serde_json",
            "normal" => NORMAL_CEILING.contains(&name.as_str()),
            other => panic!(
                "squallar-nmea-serial declares a `{other}` dependency on \
                 {name}; the charter permits none of that kind"
            ),
        };
        assert!(
            allowed,
            "squallar-nmea-serial declares {name} ({kind}). squallar-nmea-serial \
             is NMEA parsing and the serial-port transport in its own parsed \
             vocabulary — it does not know the app's fix model (WO-RL-3 \
             flipped that edge; squallar_location::serial owns the \
             translation); if a step genuinely needs a new dependency, the \
             charter in this test and the plan must change first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its named deps.
    assert!(
        deps.iter().any(|(k, n)| k == "normal" && n == "nmea"),
        "squallar-nmea-serial no longer declares nmea (normal) — either the \
         crate changed shape or this test is reading the wrong package: \
         {deps:?}",
    );
}
