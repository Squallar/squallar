// The rule behind the `mobile` cfg, in a file both `build.rs` (which
// `include!`s it) and the test suite can see. Testing it through `cfg!(mobile)`
// would prove nothing: on a desktop host both the cfg and the targets it is
// derived from are false whatever the build script did.

/// Whether `target_os` names a handheld, for memory and bandwidth budgets.
/// A string because that is the form `CARGO_CFG_TARGET_OS` gives `build.rs`.
fn is_mobile_target(target_os: &str) -> bool {
    matches!(target_os, "android" | "ios")
}

#[cfg(test)]
mod mobile_cfg_tests {
    use super::is_mobile_target;

    #[test]
    fn handhelds_are_mobile() {
        assert!(is_mobile_target("android"));
        assert!(is_mobile_target("ios"));
    }

    #[test]
    fn desktops_are_not_mobile() {
        for os in ["linux", "windows", "macos", "freebsd"] {
            assert!(!is_mobile_target(os), "{os} must not be treated as mobile");
        }
    }

    /// wasm32's `target_os` is empty or "unknown"; its arm is selected by
    /// `target_arch`, not by this.
    #[test]
    fn wasm_is_not_mobile() {
        assert!(!is_mobile_target("unknown"));
        assert!(!is_mobile_target(""));
    }

    /// iOS's simulator and catalyst variants still report `target_os = "ios"`;
    /// a typo'd or renamed OS must fall to the desktop side.
    #[test]
    fn unknown_targets_fall_back_to_desktop() {
        assert!(!is_mobile_target("Android"), "matching is case-sensitive");
        assert!(!is_mobile_target("android-ndk"));
    }
}
