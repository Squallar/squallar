//! The crate's charter, held as tests: an empty dependency ceiling and the
//! three-method fence on the trait itself.
//!
//! The ceiling test reads `cargo metadata --no-deps --format-version 1` from
//! the workspace root. `packages[].dependencies` there are *declared*
//! dependencies — feature-independent and resolution-independent — so no
//! feature selection (default, `--all-features`, CI's llvm-cov arm) can mask
//! or fake what it asserts. Dep-name mechanics, recorded at M0 and relied on
//! here: a workspace-internal dep appears with `"req": "*"` and a `path`;
//! `kind` is `null` for normal deps (normalised to "normal" below); one name
//! may legitimately appear once per kind, so entries are judged per
//! `(kind, name)`.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-kv sits one level under the workspace root")
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

/// The floor stays a floor: the *only* dependency this crate may declare, of
/// any kind, is the dev-only serde_json that this file itself needs to parse
/// `cargo metadata`. The ceiling is empty on purpose — a three-method blob
/// contract over `std` is the crate's whole identity.
///
/// The floor assertion at the bottom is what keeps this test falsifiable — a
/// broken parse or a renamed package cannot pass as an empty set.
#[test]
fn the_dependency_ceiling_holds() {
    let meta = metadata();
    let deps = declared_deps(&meta, "rustdar-kv");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            "dev" => name == "serde_json",
            "normal" => false,
            other => panic!(
                "rustdar-kv declares a `{other}` dependency on {name}; \
                 the charter permits none"
            ),
        };
        assert!(
            allowed,
            "rustdar-kv declares {name} ({kind}). rustdar-kv is the \
             persistence floor — a three-method blob contract over std; a \
             dependency here changes this test and the plan first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares its one dev dependency,
    // so a broken parse or a renamed package cannot pass as an empty set.
    assert!(
        deps.iter().any(|(k, n)| k == "dev" && n == "serde_json"),
        "rustdar-kv no longer declares serde_json (dev) — either the crate \
         changed shape or this test is reading the wrong package: {deps:?}",
    );
}

/// The fence on the trait itself: `KvStore` declares exactly `load`, `store`
/// and `store_now`, and nothing more.
///
/// A textual scrape rather than a trait-object probe, because the failure this
/// exists to catch is a *new* method — which no compiled assertion against the
/// current three could see. The scanner walks `src/lib.rs` from the trait's
/// opening line to its column-zero closing brace and collects every `fn`
/// declared inside.
#[test]
fn the_contract_is_three_methods_and_nothing_more() {
    let src = include_str!("../src/lib.rs");

    // The trait block: from its declaration to the first column-zero `}`.
    let start = src
        .find("pub trait KvStore {")
        .expect("src/lib.rs declares `pub trait KvStore` — the scanner is pinned to it");
    let block_rel_end = src[start..]
        .find("\n}")
        .expect("the trait block closes with a column-zero brace");
    let block = &src[start..start + block_rel_end];

    let methods: Vec<&str> = block
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            line.strip_prefix("fn ")
                .and_then(|rest| rest.split('(').next())
        })
        .collect();

    // Pin the scanner before trusting it: each of the three literal spellings
    // must have been found, or the scrape is reading the wrong text and every
    // later assertion is vacuous.
    for expected in ["load", "store", "store_now"] {
        assert!(
            methods.contains(&expected),
            "the scanner did not find `fn {expected}` inside the KvStore \
             trait block — it is reading the wrong text: {methods:?}",
        );
    }

    assert_eq!(
        methods.len(),
        3,
        "KvStore declares {methods:?}. Three on purpose: no enumeration, no \
         deletion, no transactions; absence == unreadable — if a consumer \
         needs a fourth verb it is asking for a database, and this crate is \
         deliberately not one.",
    );
}
