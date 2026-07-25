// The rule behind the `mobile` cfg, in a file both `build.rs` and the test
// suite can see.
//
// `build.rs` `include!`s this text (a build script is its own crate and cannot
// `use` anything from the library), and the library compiles it under `cfg(test)`
// so the rule can be asserted on any host. Testing it through `cfg!(mobile)`
// instead would prove nothing: on a desktop host both the cfg and the targets it
// is derived from are false no matter what the build script did.

/// Whether `target_os` names a handheld, for the purposes of memory and
/// bandwidth budgets.
///
/// Takes the OS as a string because that is the form `build.rs` gets it in —
/// `CARGO_CFG_TARGET_OS` describes the *target* being compiled, which is the
/// only thing a cross-compile can be keyed on.
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

    /// The desktop targets must not pick up handheld budgets.
    #[test]
    fn desktops_are_not_mobile() {
        for os in ["linux", "windows", "macos", "freebsd"] {
            assert!(!is_mobile_target(os), "{os} must not be treated as mobile");
        }
    }

    /// wasm32's `target_os` is empty or "unknown", and it is emphatically not
    /// mobile: `constants.rs` gives it its own arm with tighter budgets still,
    /// and that arm is selected by `target_arch`, not by this.
    #[test]
    fn wasm_is_not_mobile() {
        assert!(!is_mobile_target("unknown"));
        assert!(!is_mobile_target(""));
    }

    /// iOS's simulator and catalyst variants still report `target_os = "ios"`,
    /// so nothing else needs listing — but a typo'd or renamed OS must fall to
    /// the desktop side rather than silently matching.
    #[test]
    fn unknown_targets_fall_back_to_desktop() {
        assert!(!is_mobile_target("Android"), "matching is case-sensitive");
        assert!(!is_mobile_target("android-ndk"));
    }
}
