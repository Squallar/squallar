//! Runs the JavaScript behaviour suites under `cargo test`.
//!
//! The caching policy lives in `sw.js` and index.html's bootstrap, which no Rust
//! code executes. This shim puts the suites that run them under `cargo test`.
//!
//! Missing Node is a failure, not a skip — a skip is green on a machine that
//! never checked.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::process::Command;

/// Oldest Node whose `node:test` runner supports `describe`/`it` as used here.
const MINIMUM_NODE_MAJOR: u32 = 20;

fn web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn node_major() -> u32 {
    let output = Command::new("node")
        .arg("--version")
        .output()
        .unwrap_or_else(|e| {
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
             This is a behavioural gate on what squallar caches, not a lint. Read \
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

/// Adding a suite and forgetting to invoke it is the failure mode this whole
/// file exists to prevent.
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

    assert!(
        !found.is_empty(),
        "no *.test.mjs suites found in squallar-web/tests/"
    );

    for suite in &found {
        assert!(
            driver.contains(&format!("tests/{suite}")),
            "squallar-web/tests/{suite} exists but no test in sw_behaviour.rs runs it, so \
             `cargo test` does not gate it. Add a `run_suite(\"tests/{suite}\")` test."
        );
    }
}
