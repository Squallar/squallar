//! **What the OS would give this process reaches the capacity in force**, and
//! is re-read rather than remembered.
//!
//! Three claims, each with the control arm that resembles it: a bridge that
//! reads nothing leaves every figure exactly where it was before the reader
//! existed; a bridge that reads something puts a host figure on an arm that
//! had none; and the reading moves between ticks, because a value snapshotted
//! at construction is the high-water mark this reader was written to replace.
//!
//! `squallar-app`'s test binary declares no `#[global_allocator]`, so
//! `squallar_alloc::live_bytes()` is `None` here and the pool is the available
//! figure alone. That premise is asserted rather than assumed — the pool's own
//! arithmetic is gated in `squallar-device-profile`, where both terms can be
//! fed.

use super::tests::headless;
use crate::platform_double::TestBridge;
use squallar_device_profile::scene::CapacitySource;

const MIB: u64 = 1024 * 1024;

/// A machine that says it would give this process 6 GiB.
const AVAILABLE: u64 = 6 * 1024 * MIB;

/// **A bridge with no available reader changes nothing.** The control for
/// every assertion below, and the state every browser is in permanently: no
/// pool on the profile, no host figure on the capacity, and `fit`'s host arm
/// as inert as it was.
#[test]
fn a_bridge_that_reads_nothing_leaves_the_capacity_where_it_was() {
    let app = headless(TestBridge::desktop());
    assert_eq!(app.device_profile.host_pool_bytes, None);
    let cap = app.capacity();
    assert_eq!(cap.source, CapacitySource::Presumed);
    assert_eq!(
        cap.host_bytes, None,
        "a desktop bracket with no reading declares no host presumption",
    );
    assert_eq!(cap.host_allowance(), None);
}

/// **A bridge that reads gives the presumed arm a host figure it never had.**
///
/// This is the decoupling, end to end: no VRAM reader has answered — the
/// double consults no driver, so the capacity is `Presumed` — and the host
/// pool stands anyway. Before this landed, `DeviceProfile::capacity` set a
/// host figure only on the measured arm, so a native machine whose card had
/// no reader carried no host capacity at all.
#[test]
fn an_available_reading_reaches_the_capacity_without_waiting_for_the_gpu() {
    // The premise the equality below rests on: this binary counts nothing.
    assert_eq!(
        squallar_alloc::live_bytes(),
        None,
        "this test binary installed a counting allocator, so the pool is no \
         longer the available figure alone",
    );

    let app = headless(TestBridge::desktop().with_available_memory(AVAILABLE));
    assert_eq!(app.device_profile.host_pool_bytes, Some(AVAILABLE));

    let cap = app.capacity();
    assert_eq!(
        cap.source,
        CapacitySource::Presumed,
        "the double consults no driver, so the GPU pool is still presumed",
    );
    assert_eq!(cap.host_bytes, Some(AVAILABLE));
    assert_eq!(
        cap.host_allowance(),
        Some(AVAILABLE / 4 * 3),
        "three quarters of the pool, the same fraction as every other arm",
    );

    // Nothing about the GPU moved: the two pools are read independently and
    // the host reading must not touch the other one.
    let bare = headless(TestBridge::desktop());
    assert_eq!(cap.gpu_bytes, bare.capacity().gpu_bytes);
}

/// **The reading is re-taken on the telemetry tick, not remembered.**
///
/// What the OS will give moves with every other program on the box, so a
/// figure taken once at construction would be exactly the high-water mark
/// this reader exists to replace. The gauge is moved *down* between ticks —
/// the direction that matters, since a pool that could only be seeded at boot
/// would report the machine at its emptiest for the whole session.
///
/// The second arm is the one that could not be inferred from the first: a
/// reader that stops answering leaves the field unread rather than stale, the
/// way the GPU reading already does.
#[test]
fn the_pool_is_re_read_on_the_tick_and_a_reader_that_stops_leaves_it_unread() {
    let bridge = TestBridge::desktop().with_available_memory(AVAILABLE);
    let gauge = bridge.available_memory_gauge();
    let mut app = headless(bridge);
    assert_eq!(app.device_profile.host_pool_bytes, Some(AVAILABLE));

    // The tick's own 2 s gate, cleared rather than waited out — the same
    // thing `budget_readout_cadence_tests` does, and it is what makes the
    // three ticks below three ticks rather than one and two no-ops.
    let tick = |app: &mut super::App| {
        app.frame_telemetry_said = None;
        app.report_frame_telemetry();
    };

    // Another program takes 4 GiB of it.
    let squeezed = AVAILABLE - 4 * 1024 * MIB;
    gauge.set(Some(squeezed));
    assert_eq!(
        app.device_profile.host_pool_bytes,
        Some(AVAILABLE),
        "the profile moved without a tick",
    );
    tick(&mut app);
    assert_eq!(app.device_profile.host_pool_bytes, Some(squeezed));
    assert_eq!(app.capacity().host_bytes, Some(squeezed));

    // And back up: the figure is not a ratchet. A pool that could only fall
    // would be the defect the whole campaign is about, one field further on.
    gauge.set(Some(AVAILABLE));
    tick(&mut app);
    assert_eq!(app.device_profile.host_pool_bytes, Some(AVAILABLE));

    // A reader that stops answering: unread, not the last figure held stale.
    gauge.set(None);
    tick(&mut app);
    assert_eq!(app.device_profile.host_pool_bytes, None);
    assert_eq!(app.capacity().host_bytes, None);
}
