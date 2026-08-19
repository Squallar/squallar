//! Emits the `mobile` cfg.
//!
//! Some decisions in this crate are about the *class of device* — how much RAM
//! and bandwidth to assume — not about which OS API exists. Those were spelled
//! `target_os = "android"` because Android was the only handheld target. iOS is
//! the next one, and it would need the same budgets while matching none of
//! those cfgs.
//!
//! `mobile` says what is actually meant. `target_os = "android"` stays where the
//! point really is Android specifically (the JNI wiring in the `rustdar`
//! shell's `src/android/` modules and rustdar-location's android arm).
//!
//! Cargo scopes a build script's cfgs to the crate that declares it, so this
//! file has to be duplicated by every crate that wants `mobile` — today that is
//! rustdar-device-profile alone, which is why every `mobile` cascade lives
//! here. rustdar-egui still makes the same device-class distinction with
//! `target_os` cfgs (its tile budgets) and would need its own copy to adopt
//! `mobile`; it is deliberately left alone. Its pane caps stopped being a drift
//! hazard when they moved into this crate's `budget` module — the UI reads them
//! from the floor rather than mirroring them.

// A build script is its own crate and cannot `use` the library it builds, so the
// rule is pulled in as text. The library compiles the same file under
// `cfg(test)`, which is what makes the rule assertable on a desktop host.
include!("src/mobile_cfg.rs");

fn main() {
    // Without this, `#[cfg(mobile)]` trips the `unexpected_cfgs` lint.
    //
    // Note this is only a lint, and nothing in CI turns warnings into failures,
    // so it is not what stops a handheld build silently taking desktop budgets
    // if this script stops running. The `compile_error!` in
    // rustdar-device-profile's own constants.rs is.
    println!("cargo::rustc-check-cfg=cfg(mobile)");

    // Set by cargo for the *target* being compiled, so this stays correct when
    // cross-compiling — which is the only way Android and iOS are ever built.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if is_mobile_target(&target_os) {
        println!("cargo::rustc-cfg=mobile");
    }

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/mobile_cfg.rs");
}
