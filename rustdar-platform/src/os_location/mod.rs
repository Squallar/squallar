//! The seam between `DesktopPlatform` and whatever location service the OS
//! offers.
//!
//! Windows has `Geolocator` and `AppCapability`, macOS has `CLLocationManager`,
//! Linux has GeoClue2 — first-class services rustdar has never asked. Each gets
//! a private module here exposing one type, `OsLocationReader`, with the same
//! shape as [`SerialGpsReader::start`]: a constructor that returns `None` when
//! there is nothing to read, and a value that stops when it is dropped. That
//! symmetry is the point — the consumer side already knows how to hold one of
//! those, and `DesktopPlatform` ends up with two identical-looking readers
//! feeding one channel drain.
//!
//! **This module is the entire `cfg` surface.** Nothing outside it names a
//! target, and no provider file carries a `cfg` of its own. A per-OS `cfg`
//! spread across call sites is how a build ends up compiling two providers, or
//! none, on a target nobody tested.
//!
//! Only `unsupported` exists today; the real providers are separate pieces of
//! work with their own dependencies, their own permission mechanics and, in two
//! cases, their own packaging. The arms are written out anyway so that landing
//! one is a one-line change here rather than a redesign.
//!
//! [`SerialGpsReader::start`]: rustdar_gps::SerialGpsReader::start

#[cfg(target_os = "linux")]
mod linux;
mod unsupported;

/// CoreLocation, for macOS and iOS.
///
/// Declared unconditionally, unlike the arm below that selects it: the half of
/// that file which decides what a `CLAuthorizationStatus` means and which of a
/// `CLLocation`'s components are sentinels names no Objective-C type at all,
/// and the `cargo test` that has to run those decisions is a Linux one. The
/// CoreLocation half carries the same `cfg` this module's Apple arm does.
mod apple;

// The arm table. Written on `target_os` and never on `unix` or `target_family`,
// because both of those get it wrong in a way that compiles: Android *is*
// `unix` and would take the Linux arm despite having a completely different
// location API reached over JNI, and iOS is `unix` but not `macos` while
// sharing macOS's CoreLocation. `target_os` is the only axis on which these
// four are actually distinct.
//
// The `not(...)` arm is not a leftover. wasm32 lands there — it is neither
// Android nor iOS, so it selects `DesktopPlatform` and therefore compiles this
// module, and `clippy.yaml` runs `cargo check --workspace --target
// wasm32-unknown-unknown` over it on every push. A browser tab genuinely has no
// OS location service (the page's own Geolocation API is the *web bridge's*
// business, not this one's), so `unsupported` is the right answer there and not
// a stub.

/// GeoClue2 over `zbus`, on the peer-scoped connection its `Location` objects
/// require.
#[cfg(target_os = "linux")]
use linux as provider;

/// Phase 4: `AppCapability` for the state, `Geolocator` to prompt, an MTA
/// worker to keep `RequestAccessAsync` off the frame thread.
///
/// Compiled under `test` as well as on Windows, and that is not a convenience.
/// The half of that file worth testing is pure — which `AppCapabilityAccessStatus`
/// means "ask", which `PositionSource` is a satellite, how a null `IReference`
/// decodes — and every one of those failures is silent on a device: a wrong arm
/// renders the wrong control or the wrong fix quality and throws nothing. CI has
/// no Windows runner to run tests on, so the mapping is written against plain
/// `i32`s, pinned to the real bindings by `const` assertions that only a Windows
/// build evaluates, and exercised on the Linux host that `cargo test` runs on.
#[cfg(any(target_os = "windows", test))]
mod windows;

/// `self::`, because a bare `windows` here is ambiguous with the crate of that
/// name — uniform paths would find both and refuse to choose.
#[cfg(target_os = "windows")]
use self::windows as provider;

/// The permission half of the Windows arm, which `OsLocationReader` is not.
///
/// Deliberately outside the shared provider surface below. The two have
/// different lifetimes: a reader is a subscription that comes and goes with the
/// user's "Turn off" button, whereas the capability watcher has to be running
/// *before* anything starts — it is what decides whether starting is even
/// allowed — and has to keep running afterwards so a revocation made in Settings
/// is still noticed. Every arm answers `location_permission` from its own
/// mechanism, and there is nothing common to name until more than one exists.
#[cfg(target_os = "windows")]
pub use self::windows::LocationService;

/// One `apple.rs` with two `#[cfg]` islands — `CLLocationManager` is the same
/// on both, but macOS constructs its bridge after `NSApplication` exists and
/// may not be running from a bundle at all, while iOS constructs it before
/// `UIApplicationMain` has run.
#[cfg(any(target_os = "macos", target_os = "ios"))]
use apple as provider;

/// Everything else, wasm32 included.
#[cfg(not(any(
    target_os = "linux",
    target_os = "windows",
    target_os = "macos",
    target_os = "ios"
)))]
use unsupported as provider;

pub use provider::OsLocationReader;

/// Choose between a fix from the serial reader and one from the OS.
///
/// **"Serial with a positional quality wins", not "serial wins".** The plain
/// rule looks obviously right — a dongle with a sky view beats an IP lookup by
/// three orders of magnitude — and it has a failure mode that is silent and
/// permanent: a receiver with *no* sky view goes on emitting GGA at quality 0,
/// with the last coordinates it had and a cleared fix flag, at 1 Hz forever. A
/// user with a USB GPS in a drawer and a working platform location service
/// would have the good fix discarded on every single frame in favour of a fix
/// the receiver itself is saying not to trust.
///
/// So the serial reader wins only while it is actually reporting a fix. When it
/// is not, and the OS is, the OS's fix is what there is. When neither is
/// positional the serial one is still preferred: it carries satellite counts
/// and HDOP the OS never reports, and preserving it is what keeps today's
/// behaviour unchanged on a machine with no OS provider at all.
pub fn prefer_fix(
    serial: Option<rustdar_gps::GpsFix>,
    os: Option<rustdar_gps::GpsFix>,
) -> Option<rustdar_gps::GpsFix> {
    match (serial, os) {
        (Some(serial), Some(os)) => Some(if serial.fix_quality == rustdar_gps::FixQuality::None {
            os
        } else {
            serial
        }),
        (serial, os) => serial.or(os),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_gps::{FixQuality, GpsFix};

    fn serial_fix(quality: FixQuality) -> GpsFix {
        GpsFix {
            fix_quality: quality,
            ..GpsFix::from_lat_lon(35.25, -97.5)
        }
    }

    fn os_fix() -> GpsFix {
        GpsFix {
            accuracy_m: Some(25_000.0),
            ..GpsFix::from_device_position(39.74, -104.99)
        }
    }

    /// A receiver that has a fix is the better source by three orders of
    /// magnitude, and it stays the better source.
    #[test]
    fn a_serial_fix_outranks_the_operating_systems() {
        let chosen = prefer_fix(Some(serial_fix(FixQuality::Gps)), Some(os_fix()))
            .expect("both were present");
        assert_eq!(chosen.fix_quality, FixQuality::Gps);
    }

    /// The regression the qualifier exists for: a dongle indoors emits quality
    /// 0 with real-looking coordinates at 1 Hz forever, and plain "serial wins"
    /// discards a good OS fix on every frame in favour of it.
    #[test]
    fn a_dongle_with_no_sky_view_does_not_shadow_a_real_fix() {
        let chosen = prefer_fix(Some(serial_fix(FixQuality::None)), Some(os_fix()))
            .expect("both were present");
        assert_eq!(
            chosen.fix_quality,
            FixQuality::Device,
            "a serial reader reporting no fix suppressed the one source that \
             had one"
        );
    }

    /// With nothing else on offer the serial reading still stands, quality and
    /// all. This is today's behaviour on a machine with no OS provider, and it
    /// must not change.
    #[test]
    fn a_lone_source_is_used_whatever_it_says() {
        assert_eq!(
            prefer_fix(Some(serial_fix(FixQuality::None)), None)
                .expect("the serial reading")
                .fix_quality,
            FixQuality::None,
        );
        assert_eq!(
            prefer_fix(None, Some(os_fix()))
                .expect("the OS reading")
                .fix_quality,
            FixQuality::Device,
        );
        assert!(prefer_fix(None, None).is_none());
    }
}
