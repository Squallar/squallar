//! Runs the JavaScript behaviour suites under `cargo test`.
//!
//! # Why this file exists
//!
//! `sw.js` and the bootstrap script in `index.html` are the two places where
//! rustdar decides what may be cached, and both are JavaScript that no Rust code
//! ever executes. [`pwa_assets`] reads them as text, which catches a stale
//! declaration and nothing else — it cannot tell whether the worker actually
//! refuses to cache a radar sweep, because it never runs it.
//!
//! `tests/sw_routing.test.mjs` and `tests/index_bootstrap.test.mjs` do run them,
//! against a scope that models the browser's. But a test suite that no gate
//! invokes is worth about as much as no suite: it passes silently on the machine
//! of whoever last remembered it existed. This shim puts both under
//! `cargo test --workspace`, which is what CI runs, so a change that breaks the
//! caching policy fails the same build every other test here fails.
//!
//! # Missing Node is a failure, not a skip
//!
//! A skip would put this straight back where it started — green on a machine
//! that never checked. `node --test` has been stable since Node 20 and needs no
//! `package.json`, no lockfile, no `node_modules` and no network: the suites
//! import nothing outside this directory. If Node is genuinely unavailable, the
//! failure below says exactly that rather than pretending the policy was
//! verified.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::process::Command;

/// Oldest Node whose `node:test` runner supports `describe`/`it` as used here.
const MINIMUM_NODE_MAJOR: u32 = 20;

fn web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn node_major() -> u32 {
    let output = Command::new("node").arg("--version").output().unwrap_or_else(|e| {
        panic!(
            "could not run `node`: {e}\n\n\
             The service worker's caching policy is JavaScript, and these suites \
             are the only thing that checks it does what it claims. They need \
             Node {MINIMUM_NODE_MAJOR} or newer — no packages, no network, just \
             the built-in `node --test` runner."
        )
    });

    let version = String::from_utf8_lossy(&output.stdout);
    let major = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or_else(|| panic!("could not parse `node --version` output: {version:?}"));

    assert!(
        major >= MINIMUM_NODE_MAJOR,
        "node {major} is too old; `node --test` needs {MINIMUM_NODE_MAJOR} or newer"
    );
    major
}

/// Run one `node --test` suite and fail with its full output if it does not pass.
fn run_suite(file: &str) {
    node_major();

    let output = Command::new("node")
        .arg("--test")
        .arg(file)
        .current_dir(web_dir())
        .output()
        .unwrap_or_else(|e| panic!("running `node --test {file}`: {e}"));

    if !output.status.success() {
        panic!(
            "{file} failed.\n\n\
             This is a behavioural gate on what rustdar caches, not a lint. Read \
             the assertion message: it names the property that broke.\n\n\
             --- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn the_service_worker_enforces_its_caching_policy() {
    run_suite("tests/sw_routing.test.mjs");
}

#[test]
fn the_page_bootstrap_reports_connectivity_and_updates() {
    run_suite("tests/index_bootstrap.test.mjs");
}

/// Every `*.test.mjs` in this directory is run by one of the tests above.
///
/// Adding a suite and forgetting to invoke it is the failure mode this whole
/// file exists to prevent, so it is worth one assertion rather than trusting
/// that nobody will.
#[test]
fn every_javascript_suite_is_actually_invoked() {
    let driver = include_str!("sw_behaviour.rs");

    let mut found = Vec::new();
    for entry in std::fs::read_dir(web_dir().join("tests")).expect("reading tests/") {
        let name = entry.expect("reading a tests/ entry").file_name();
        let name = name.to_string_lossy().into_owned();
        if name.ends_with(".test.mjs") {
            found.push(name);
        }
    }
    found.sort();

    assert!(!found.is_empty(), "no *.test.mjs suites found in rustdar-web/tests/");

    for suite in &found {
        assert!(
            driver.contains(&format!("tests/{suite}")),
            "rustdar-web/tests/{suite} exists but no test in sw_behaviour.rs runs it, so \
             `cargo test` does not gate it. Add a `run_suite(\"tests/{suite}\")` test."
        );
    }
}
