//! The crate's charter, held as tests: the facade's dependency discipline —
//! a lean DEFAULT set for the vocabulary every consumer pays for, and named,
//! feature-fenced allowances for the provider arms (WO-RL-3/RL-4, seam ruling
//! 6: "everything between 'where am I' and the operating system"; amended in
//! writing at RL-4 when the android/web arms came home and the parser dep
//! became unconditional).
//!
//! The helpers read `cargo metadata --no-deps --format-version 1` from the
//! workspace root. `packages[].dependencies` there are *declared*
//! dependencies — feature-independent and resolution-independent — so no
//! feature selection (default, `--all-features`, CI's llvm-cov arm) can mask
//! or fake what these assert. Dep-name mechanics, recorded at M0 and relied
//! on here: a workspace-internal dep appears with `"req": "*"` and a `path`;
//! `kind` is `null` for normal deps (normalised to "normal" below); one name
//! may legitimately appear once per kind, so entries are judged per
//! `(kind, name)` — plus, since WO-RL-3, the `optional` flag, which is what
//! separates the lean default set from the fenced arms.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-location sits one level under the workspace root")
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

/// `(kind, name, optional)` for every dependency `package` declares.
/// `kind: null` is a normal dependency; target-gated entries carry their kind
/// like any other and are included — a gated dependency is still a dependency.
/// `optional: true` marks the feature-fenced arms: they are declared, judged
/// here, and pulled only where a feature names them.
fn declared_deps(meta: &serde_json::Value, package: &str) -> BTreeSet<(String, String, bool)> {
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
            let optional = d["optional"].as_bool().unwrap_or(false);
            (kind, name, optional)
        })
        .collect()
}

/// The facade sits under every consumer of "where am I", so the DEFAULT cost
/// of naming it must stay the vocabulary's lean set: the geo floor, the kv
/// blob floor (the gate persists its memo through it — the whole reason WO-RK
/// preceded this crate), time, logging and serde. Amended in writing at
/// WO-RL-2 (the gate moved in) and WO-RL-3 (the crate became the facade; the
/// provider arms below are OPTIONAL, so they never enter a default build).
///
/// The fenced set is the providers' own dependencies and nothing else: the
/// OS arms' target-gated bindings behind `os-providers`, and the NMEA
/// parser/transport behind `serial`. Every fenced entry must be `optional` —
/// an arm dependency that stopped being optional would silently join the
/// default cost of every vocabulary consumer.
#[test]
fn the_dependency_ceiling_holds() {
    const DEFAULT_NORMAL: &[&str] = &[
        "rustdar-geo",
        "rustdar-kv",
        // The parser and SerialConfig only (its default is off workspace-wide;
        // the transport and serialport stay behind the `serial` fence). Joined
        // the default set at WO-RL-4: the facade's serial verbs name the
        // config type.
        "rustdar-nmea-serial",
        "chrono",
        "log",
        "serde",
        "serde_json",
        "web-time",
    ];
    const FENCED_NORMAL: &[&str] = &[
        // os-providers, linux
        "ashpd",
        "futures-lite",
        "futures-channel",
        // os-providers, windows
        "windows",
        // os-providers, apple
        "objc2",
        "objc2-foundation",
        "objc2-core-location",
        // android-provider / jni-typecheck
        "jni",
        // web-provider, wasm32
        "web-sys",
        "js-sys",
        "wasm-bindgen",
        "wasm-bindgen-futures",
    ];

    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-location");

    for (kind, name, optional) in &deps {
        let allowed: bool = match (kind.as_str(), optional) {
            ("dev", _) => name == "serde_json",
            ("normal", false) => DEFAULT_NORMAL.contains(&name.as_str()),
            ("normal", true) => FENCED_NORMAL.contains(&name.as_str()),
            (other, _) => panic!(
                "rustdar-location declares a `{other}` dependency on {name}; \
                 the charter permits none of that kind"
            ),
        };
        assert!(
            allowed,
            "rustdar-location declares {name} ({kind}, optional={optional}). \
             rustdar-location is the location facade — everything between \
             'where am I' and the operating system: the vocabulary's DEFAULT \
             set stays lean, and a provider arm's dependency must be optional \
             behind its feature fence; if a step genuinely needs a new \
             dependency, the charter in this test and the plan must change \
             first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its named deps, so a
    // broken parse or a renamed package cannot pass as an empty set.
    assert!(
        deps.iter()
            .any(|(k, n, o)| k == "normal" && n == "rustdar-geo" && !o),
        "rustdar-location no longer declares rustdar-geo (normal, \
         non-optional) — either the crate changed shape or this test is \
         reading the wrong package: {deps:?}",
    );
}

/// The two fences themselves: the `[features]` table maps each arm to exactly
/// the optional dependencies the ceiling above allows, so a fenced dependency
/// cannot quietly migrate out from behind its feature (or a feature grow an
/// arm the charter never granted).
#[test]
fn the_feature_fences_map_the_arms() {
    let meta = metadata();
    let packages = meta["packages"].as_array().expect("packages array");
    let pkg = packages
        .iter()
        .find(|p| p["name"].as_str() == Some("rustdar-location"))
        .expect("rustdar-location in metadata");
    let features = &pkg["features"];

    let entries = |feature: &str| -> BTreeSet<String> {
        features[feature]
            .as_array()
            .unwrap_or_else(|| panic!("rustdar-location no longer declares feature `{feature}`"))
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect()
    };

    let os_providers = entries("os-providers");
    let expected: BTreeSet<String> = [
        "dep:ashpd",
        "dep:futures-lite",
        "dep:futures-channel",
        "dep:windows",
        "dep:objc2",
        "dep:objc2-foundation",
        "dep:objc2-core-location",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        os_providers, expected,
        "the `os-providers` fence no longer matches the charter's arm table",
    );

    let serial = entries("serial");
    let expected: BTreeSet<String> = ["rustdar-nmea-serial/serial"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        serial, expected,
        "the `serial` fence no longer forwards rustdar-nmea-serial's serial \
         gate (the parser dep itself is unconditional since WO-RL-4)",
    );

    let android = entries("android-provider");
    let expected: BTreeSet<String> = ["dep:jni"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        android, expected,
        "the `android-provider` fence no longer maps to exactly the JNI \
         surface",
    );

    let typecheck = entries("jni-typecheck");
    let expected: BTreeSet<String> = ["dep:jni"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        typecheck, expected,
        "the `jni-typecheck` fence stopped being the host-typecheck copy of \
         the android arm's one dependency",
    );

    let web = entries("web-provider");
    let expected: BTreeSet<String> = [
        "dep:web-sys",
        "dep:js-sys",
        "dep:wasm-bindgen",
        "dep:wasm-bindgen-futures",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        web, expected,
        "the `web-provider` fence no longer matches the browser arm's binding \
         set",
    );

    // No default feature: naming the facade must cost the lean set and
    // nothing else, without consumers having to remember to opt out.
    assert!(
        features["default"].as_array().is_none_or(|d| d.is_empty()),
        "rustdar-location grew a default feature; the vocabulary's default \
         cost is the charter's lean set, with every provider arm opt-in: \
         {features:?}",
    );
}

/// The flipped edge (WO-RL-3): the facade stands on its serial provider, and
/// the provider does not know the facade. Both halves asserted — presence
/// here, absence there — so this doubles as a falsifiability floor for the
/// parse (a renamed package cannot pass the presence half).
#[test]
fn the_facade_stands_on_the_provider_and_not_the_reverse() {
    let meta = metadata();

    let location = declared_deps(&meta, "rustdar-location");
    assert!(
        location
            .iter()
            .any(|(k, n, o)| k == "normal" && n == "rustdar-nmea-serial" && !o),
        "rustdar-location no longer declares rustdar-nmea-serial (normal, \
         unconditional since WO-RL-4) — the facade lost its parser: \
         {location:?}",
    );

    let nmea = declared_deps(&meta, "rustdar-nmea-serial");
    assert!(
        !nmea.iter().any(|(_, n, _)| n == "rustdar-location"),
        "rustdar-nmea-serial names rustdar-location again — the RL-1 edge \
         grew back; the parser must not know the app's fix model (its real \
         dep set: {nmea:?})",
    );
}
