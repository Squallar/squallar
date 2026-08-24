//! Emits the `mobile` cfg — the device *class* distinction, as against
//! `target_os = "android"`, which stays where the point really is Android.
//!
//! Cargo scopes a build script's cfgs to the crate that declares it, so every
//! `mobile` cascade lives in this crate.

// A build script is its own crate and cannot `use` the library it builds, so the
// rule is pulled in as text. The library compiles the same file under
// `cfg(test)`, which is what makes the rule assertable on a desktop host.
include!("src/mobile_cfg.rs");

fn main() {
    // Without this, `#[cfg(mobile)]` trips the `unexpected_cfgs` lint. It is
    // only a lint; the `compile_error!` in constants.rs is what actually stops
    // a handheld build taking desktop budgets.
    println!("cargo::rustc-check-cfg=cfg(mobile)");

    // Set by cargo for the *target* being compiled, so cross-compiles are right.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if is_mobile_target(&target_os) {
        println!("cargo::rustc-cfg=mobile");
    }

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/mobile_cfg.rs");
}
