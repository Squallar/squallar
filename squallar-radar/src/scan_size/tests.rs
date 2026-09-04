//! What [`scan_bytes`] counts, over volumes built to make each part of the
//! sum visible on its own.

use super::*;
use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, VolumeCoveragePattern};

/// The VCP the shape is filed under. Nothing in the price depends on it: a
/// coverage pattern carries no gates.
fn vcp() -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        212,
        0,
        0.5,
        PulseWidth::Short,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    )
}

/// One radial carrying `moments` moments of `gates` bytes each.
fn radial(gates: usize, moments: usize) -> Radial {
    let moment = || {
        Some(MomentData::from_fixed_point(
            gates as u16,
            2125,
            250,
            8,
            2.0,
            66.0,
            vec![7u8; gates],
        ))
    };
    let slot = |n: usize| if moments > n { moment() } else { None };
    Radial::new(
        1_700_000_000_000,
        0,
        0.5,
        0.5,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        slot(0),
        slot(1),
        slot(2),
        slot(3),
        slot(4),
        slot(5),
        None,
    )
}

fn scan_of(sweeps: usize, radials: usize, gates: usize, moments: usize) -> Scan {
    Scan::new(
        vcp(),
        (0..sweeps)
            .map(|s| {
                Sweep::new(
                    s as u8 + 1,
                    (0..radials).map(|_| radial(gates, moments)).collect(),
                )
            })
            .collect(),
    )
}

/// The gate payload dominates and is counted exactly: every moment of every
/// radial of every sweep, once. The container arithmetic is the rest, and it
/// is `len * size_of::<T>()` at each level rather than an estimate, so the
/// whole figure is exact against the shape it is given.
#[test]
fn the_gate_bytes_of_every_moment_are_counted_once() {
    let (sweeps, radials, gates, moments) = (3, 40, 1000, 4);
    let scan = scan_of(sweeps, radials, gates, moments);
    let payload = sweeps * radials * gates * moments;
    let containers = sweeps * size_of::<Sweep>() + sweeps * radials * size_of::<Radial>();
    assert_eq!(scan_bytes(&scan), payload + containers);
}

/// A moment the radial does not carry costs its gates nothing — an `Option`
/// that is `None` must not be charged, or an 8-moment price would be quoted
/// for every 3-moment volume in the archive.
#[test]
fn an_absent_moment_is_not_charged() {
    let three = scan_bytes(&scan_of(2, 10, 500, 3));
    let six = scan_bytes(&scan_of(2, 10, 500, 6));
    assert_eq!(six - three, 2 * 10 * 500 * 3);
}

/// An empty volume is its containers and nothing else, and a volume with no
/// sweeps at all is zero — not a panic and not a floor.
#[test]
fn an_empty_volume_prices_at_nothing() {
    let nothing = Scan::new(vcp(), Vec::new());
    assert_eq!(scan_bytes(&nothing), 0);
    assert_eq!(scan_bytes(&scan_of(2, 0, 0, 0)), 2 * size_of::<Sweep>());
}

/// **The figure lands in the range the app's own decode was measured at.**
///
/// `squallar_app::volume_inventory` records 46.1-46.8 MiB median and 58.3 MiB
/// max of live heap over 208 real archive volumes, counted by a global
/// allocator. Priced here at a VCP 212 shape — 16 sweeps of 720 radials, and
/// an average of three moments' worth of gates on each — this reads 46 MiB,
/// on the measured median.
///
/// **This is an order-of-magnitude sanity check, not an independent oracle.**
/// The moment load per radial is the one figure not fixed by the VCP, and it
/// was chosen to sit where the measured median sits; what the test pins is
/// that the arithmetic reaches tens of MiB at a real volume's shape rather
/// than the kilobytes a `size_of_val` would report. The bound is loose on
/// purpose: a radial carrying all six moments at 1200 gates prices at
/// 82.5 MiB, past the measured max, which says the archive's volumes do not
/// carry six full moments on every radial — not that this is counting wrong.
#[test]
fn a_realistic_volume_prices_near_the_measured_range() {
    let mib = scan_bytes(&scan_of(16, 720, 1400, 3)) as f64 / (1024.0 * 1024.0);
    assert!(
        (40.0..=60.0).contains(&mib),
        "a VCP-212-shaped volume priced at {mib:.1} MiB, nowhere near the \
         46-58 MiB the decode path was measured at"
    );
}

/// The price is linear in every dimension of the shape. The guard against an
/// accumulator that forgets a level of the walk and quotes one sweep's worth
/// for a whole volume.
///
/// Four identical sweeps cost exactly four times one, because each carries
/// its own `Sweep` slot. Twice the radials in ONE sweep does **not** cost
/// twice, and the difference is exactly that one sweep slot, charged once
/// however many radials hang off it — spelled out here so a reader does not
/// take the asymmetry for an off-by-one.
#[test]
fn the_price_scales_with_the_shape() {
    let one = scan_bytes(&scan_of(1, 100, 1000, 2));
    assert_eq!(
        scan_bytes(&scan_of(4, 100, 1000, 2)),
        4 * one,
        "sweeps past the first went uncounted"
    );
    assert_eq!(
        scan_bytes(&scan_of(1, 200, 1000, 2)),
        2 * one - size_of::<Sweep>(),
        "radials past the first hundred went uncounted"
    );
}
