//! **The Android path drives both engines, and a phone is not a desktop.**
//!
//! Until 2026-08-31 `drive.py --android` accepted `--browser chromium` and
//! hard-errored on Firefox with *"--android drives Chrome over chromedriver"*.
//! Nothing was wrong with the code that produced that message — it did exactly
//! what it said. The defect was one level up: every Android figure the campaign
//! held was Blink's, and it was quoted as "web" for a target where Firefox
//! governs and runs roughly twice Chromium's service time on the desktop. A
//! column that measures only the engine which tends to win is not a
//! measurement of the platform, and no browser run can notice that — a
//! Blink-only Android leg passes every assertion it has.
//!
//! So the check is here, in the same place and the same shape as
//! `rig_verdict_binding.rs`: read at compile time, naming both halves, catching
//! the failure mode a passing run cannot see.
//!
//! **What these tests do not check.** That Firefox on Android *works* — that is
//! `drive.py --selftest` (the capability shape and every arm of the argument
//! validator, offline) and, in the end, a phone. What they check is that the
//! capability to drive it is still present and still reachable from the
//! launcher, which is what a refactor or a bad rebase quietly removes.

/// The driver, read at compile time so a deleted file is a build failure rather
/// than a test that silently stops checking.
const DRIVE_PY: &str = include_str!("../../.github/browser-rig/drive.py");
/// The measurement arm. Not a gate — which is exactly why the Android knob
/// being reachable from it is worth pinning: nothing there ever goes red, so a
/// silently un-drivable phone leg is never contradicted by anything.
const RUN_MEASURE: &str = include_str!("../../.github/browser-rig/run_measure.sh");

/// **`--android` is not single-engine.**
///
/// The one assertion this file exists for. It is spelled against the shared
/// accept-list rather than against either engine's branch, because the defect
/// was never a missing Firefox branch — it was a validator that named one
/// engine, and a validator naming one engine is what a second branch would have
/// to route around.
#[test]
fn the_android_mode_accepts_both_engines() {
    assert!(
        DRIVE_PY.contains("ANDROID_BROWSERS = (\"chromium\", \"firefox\")"),
        "drive.py's --android accept-list is no longer both engines. If it \
         names only chromium again, every Android figure taken after this \
         point is Blink's and will be reported as `web` -- which is the exact \
         state this seam was added to end, and which no browser run can see \
         because a Blink-only leg passes everything it asserts",
    );
    assert!(
        !DRIVE_PY.contains("--android drives Chrome over chromedriver"),
        "drive.py has gone back to refusing Firefox with --android by name. \
         Firefox governs this target; an Android column without it measures \
         the engine that tends to win",
    );
}

/// **Both engines have somewhere to be driven to.**
///
/// An accept-list is a promise about what `launch()` can build. A browser that
/// is accepted and has no capability branch does not fail at the CLI — it fails
/// on the phone, minutes in, having measured nothing.
#[test]
fn each_accepted_engine_has_a_capability_branch_and_a_default_package() {
    assert!(
        DRIVE_PY.contains("\"chromium\": \"com.android.chrome\"")
            && DRIVE_PY.contains("\"firefox\": \"org.mozilla.firefox"),
        "an engine --android accepts has no default package, so drive.py \
         would KeyError in launch() rather than drive anything. One default \
         cannot be right for two engines: com.android.chrome handed to \
         geckodriver is a package-not-found on the device",
    );
    assert!(
        DRIVE_PY.contains("def chromium_android_capabilities"),
        "the Blink Android capability builder is gone; every Android figure \
         already taken came out of it",
    );
    assert!(
        DRIVE_PY.contains("android_package=android_package"),
        "the Gecko Android capabilities are no longer built from the resolved \
         package, so geckodriver would be asked for a desktop binary",
    );
}

/// **The one thing that fails on the phone rather than at the CLI.**
///
/// Read verbatim out of the pinned geckodriver 0.37.1 binary:
/// *"androidPackage and binary are mutual exclusive"*, and confirmed against
/// the running driver — a payload carrying both is refused with
/// `invalid argument`. The desktop Firefox path always sends `binary`, so the
/// single most likely way for this to regress is somebody "tidying up" the two
/// arms of `firefox_capabilities` back into one.
#[test]
fn the_gecko_android_capabilities_never_carry_a_binary() {
    assert!(
        DRIVE_PY.contains("androidPackage and binary are mutual exclusive"),
        "drive.py no longer records WHY the Android arm of \
         firefox_capabilities omits `binary`. That reason is not a style \
         preference: geckodriver refuses the session outright, and the leg \
         dies on the device with nothing measured",
    );
    assert!(
        DRIVE_PY.contains("\"binary\" not in opts"),
        "drive.py's selftest no longer asserts that the firefox Android \
         capabilities omit `binary`, so the one mistake that cannot be caught \
         from this box has stopped being caught from this box",
    );
}

/// **A window is a desktop concept.**
///
/// `set_window_rect` on Android is the rig asking for something that cannot
/// mean anything: the browser owns the whole display. geckodriver refuses it
/// out loud (HTTP 500, *"Only supported in desktop applications"*) while
/// chromedriver accepts and ignores it — which is why five Blink Android legs
/// never surfaced this and the first Gecko one did. The guard belongs on the
/// Android branch, not on the engine, because the rig should not be asking
/// either driver.
#[test]
fn the_android_path_never_asks_the_device_to_resize_itself() {
    assert!(
        DRIVE_PY.contains("stage(\"window-not-set\""),
        "drive.py no longer skips the window-size call on Android. On Gecko \
         that is an HTTP 500 recorded as a gotcha on every phone leg; on Blink \
         it is silently ignored, which is worse, because the row then implies \
         a size the rig asked for and the device never honoured",
    );
    assert!(
        DRIVE_PY.contains("viewport_source"),
        "drive.py no longer records that an Android viewport was REPORTED \
         rather than set. Without it a phone row looks like a desktop row that \
         happened to land on an odd size, and the campaign's \
         matching-not-marking rule -- correct --canvas until the buffer is \
         exact -- appears to have been applied when it cannot be",
    );
}

/// **The rig refuses to drive somebody's daily browser.**
///
/// Both Android drivers clear the app's data before every session. MEASURED
/// 2026-08-31 from geckodriver 0.37.1's own `--log trace`, against a real
/// phone, on an invocation that asked for nothing of the kind:
///
/// ```text
/// mozdevice TRACE execute_host_command: >> "shell:pm clear org.mozilla.firefox"
/// mozdevice TRACE execute_host_command: << "Success\n"
/// ```
///
/// Nothing in the rig can prevent that — geckodriver's capability parser
/// accepts `androidActivity`, `androidDeviceSerial`, `androidPackage`,
/// `profile`, `androidIntentArguments`, `binary`, `env`, `log` and `prefs`,
/// and not one of them gates the clear. So the only available defence is
/// refusing the *package*, which makes this a gate rather than a comment
/// asking people to be careful.
///
/// The DEFAULT is the case that matters. A guard that only protects people who
/// were already thinking about it is not a guard, and "nobody chose" is the
/// state both of this project's Android browser incidents happened in.
#[test]
fn the_rig_refuses_to_drive_a_daily_browser_by_default() {
    assert!(
        DRIVE_PY.contains("ANDROID_DAILY_DRIVER_PACKAGES"),
        "drive.py no longer refuses to drive release browser packages. The \
         Android drivers run `pm clear <package>` before every session, which \
         deletes that browser's tabs, logins, bookmarks and history -- and \
         the rig has no way to stop them, so refusing the package is the only \
         defence there is",
    );
    assert!(
        DRIVE_PY.contains("\"org.mozilla.firefox\":")
            && DRIVE_PY.contains("\"com.android.chrome\":"),
        "a release browser package dropped off the refusal list. Both engines' \
         daily-driver packages belong on it: this project has already lost one \
         user's Chrome to the Blink path and had the Gecko path run `pm clear` \
         on their Firefox",
    );
    assert!(
        DRIVE_PY.contains("effective = package or ANDROID_DEFAULT_PACKAGE[browser]"),
        "the daily-driver guard no longer covers the RESOLVED DEFAULT, only an \
         explicitly typed package. The default is exactly the case where \
         nobody thought about it, which is the case the guard is for",
    );
    assert!(
        DRIVE_PY.contains("\"firefox\": \"org.mozilla.firefox_beta\""),
        "the firefox Android default is back to release Firefox. A default is \
         what runs when nobody chose, and the driver wipes whatever it drives; \
         Beta and Nightly are separate installs with separate storage and the \
         same engine",
    );
    assert!(
        DRIVE_PY.contains("--android-allow-daily-driver"),
        "the escape hatch is gone. A refusal with no way past it gets deleted \
         by the next person who needs a throwaway device, and then there is no \
         guard at all",
    );
}

/// **A phone throttles, so both ends of the leg travel with the figures.**
///
/// One temperature is not a reading. The Blink lane refuted its own thermal
/// hypothesis by reproducing an early result at its highest temperature, and it
/// could only do that because it had both ends of every leg.
#[test]
fn every_android_leg_records_the_device_at_both_ends() {
    assert!(
        DRIVE_PY.contains("\"device_before\"") && DRIVE_PY.contains("\"device_after\""),
        "drive.py no longer reads the device at both ends of an Android leg, \
         so a slow row and a hot row are indistinguishable and every thermal \
         hypothesis becomes unfalsifiable",
    );
}

/// **The measurement arm can actually reach the Android mode.**
///
/// `RIG_DRIVE_EXTRA="--android"` alone is not sufficient and this is the half
/// that proves somebody checked: `run_measure.sh`'s display probe is a hard
/// FATAL for any X11 browser, and a phone leg needs no X display — so on a
/// headless box the leg dies before `drive.py` is exec'd at all, and on a box
/// with a display it passes that check for a reason unconnected to the leg.
#[test]
fn the_measurement_arm_can_drive_a_phone() {
    assert!(
        RUN_MEASURE.contains("RIG_ANDROID"),
        "run_measure.sh has no Android knob again, so the only route to a \
         phone leg is RIG_DRIVE_EXTRA -- which does not get past the display \
         check that fires before drive.py is ever started",
    );
    assert!(
        RUN_MEASURE.contains("NEEDS_X11=0"),
        "run_measure.sh still demands an X display for an Android leg, which \
         renders on the phone's own compositor. On a headless box that is a \
         FATAL for a leg that needs nothing from X",
    );
    assert!(
        RUN_MEASURE.contains(".android"),
        "run_measure.sh no longer distinguishes an Android row's artefacts \
         from the desktop row for the same scene and browser. Sharing a \
         filename is how one silently overwrites the other and is then quoted \
         as it -- and these two rows are never comparable",
    );
}
