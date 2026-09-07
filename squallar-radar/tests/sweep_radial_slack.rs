//! **That a decoded sweep does not park its `Vec` doubling slack for the life
//! of the volume**, watched at the allocator as well as at the capacity.
//!
//! `Sweep::from_radials` splits a decoded volume's radials into sweeps by
//! pushing into a `Vec` with no capacity known in advance, so every sweep ends
//! on a power-of-two rung. A real 0.5° surveillance cut is 720 radials and lands
//! in a vector of capacity **1024** — 304 unused `Radial` slots the allocator
//! holds for as long as the volume is resident, and a decoded volume lives in up
//! to four caches at once. Measured over 208 real archive volumes the spare runs
//! ~42 % of the length, which is why `squallar_radar::scan_size` charges
//! `capacity` and not `len`.
//!
//! Two instruments, because either alone can lie. `radials_capacity()` is what
//! the fix changes and could be satisfied by a vector that reports a smaller
//! capacity while holding the same block; `squallar_alloc::live_bytes` is bytes
//! granted less bytes returned, and cannot be.
//!
//! **One `#[test]`**: the counter is process-global and libtest runs a binary's
//! tests on several threads, so a second test here would be allocating inside
//! this one's window.

#![cfg(not(target_arch = "wasm32"))]

use nexrad_model::data::{MomentData, Radial, RadialStatus, Sweep};

#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

/// A real 0.5° surveillance cut's radial count — and the number whose next
/// power of two is 1024, which is the whole point.
const RADIALS: u16 = 720;

/// The slack that count lands on when a `Vec` doubles its way there.
const DOUBLED_CAPACITY: usize = 1024;

fn radial(elevation_number: u8, azimuth_number: u16) -> Radial {
    Radial::new(
        1_760_000_000_000 + i64::from(azimuth_number),
        azimuth_number,
        f32::from(azimuth_number) * 0.5,
        0.5,
        RadialStatus::ElevationStart,
        elevation_number,
        0.5,
        // One gate, so the figure below is the CONTAINERS and not the moments:
        // gate buffers are sized from a gate count the decoder already has and
        // carry no slack worth the name.
        Some(MomentData::from_fixed_point(
            1,
            0,
            250,
            8,
            2.0,
            66.0,
            vec![32],
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn one_cut() -> Vec<Radial> {
    (0..RADIALS).map(|a| radial(1, a)).collect()
}

fn live() -> u64 {
    squallar_alloc::live_bytes().expect("this binary installed the counter")
}

/// **A sweep off the decoder holds exactly its radials, and the split it
/// performs is unchanged.**
///
/// Floor — delete the two `sweep_radials.shrink_to_fit()` calls from
/// `Sweep::from_radials`: the capacity below reads 1024 against a length of 720,
/// and the byte figure rises by `304 * size_of::<Radial>()`.
#[test]
fn a_sweep_off_the_decoder_parks_no_doubling_slack_and_still_splits_the_same() {
    // The premise this suite is about: a `Vec` grown by `push` to 720 really
    // does land on 1024, so there is 304 slots' worth of slack to give back.
    let mut grown: Vec<Radial> = Vec::new();
    for a in 0..RADIALS {
        grown.push(radial(1, a));
    }
    assert_eq!(
        grown.capacity(),
        DOUBLED_CAPACITY,
        "premise: pushing {RADIALS} radials lands on {DOUBLED_CAPACITY} slots",
    );
    let slack_bytes = (DOUBLED_CAPACITY - usize::from(RADIALS)) * size_of::<Radial>();
    drop(grown);

    let before = live();
    let sweeps = Sweep::from_radials(one_cut());
    let held = live();

    assert_eq!(sweeps.len(), 1, "one elevation number, one sweep");
    let sweep = &sweeps[0];
    assert_eq!(sweep.radials().len(), usize::from(RADIALS));
    assert_eq!(
        sweep.radials_capacity(),
        sweep.radials().len(),
        "the sweep is holding {} radial slots for {} radials — \
         {} B of slack the allocator cannot reclaim while the volume is cached",
        sweep.radials_capacity(),
        sweep.radials().len(),
        (sweep.radials_capacity() - sweep.radials().len()) * size_of::<Radial>(),
    );

    // And the allocator agrees: what the sweep costs is its own radials and
    // their moments, with no room for another 304 slots on top.
    let cost = held - before;
    let ceiling = usize::from(RADIALS) * size_of::<Radial>() + slack_bytes;
    assert!(
        (cost as usize) < ceiling,
        "the sweep cost {cost} B, which leaves room for the {slack_bytes} B of \
         slack this suite exists to have removed (a doubled sweep's containers \
         alone would be {ceiling} B)",
    );

    // Non-triviality: the counter can see a block of the class being argued
    // about, so a `cost` that came back small cannot be a dead instrument.
    let quiet = live();
    let block: Vec<u8> = vec![0; slack_bytes];
    assert!(
        live() >= quiet + slack_bytes as u64,
        "the counter did not see a {slack_bytes} B grant",
    );
    drop(block);

    // ── The split itself is unchanged: same sweeps, order and radials ─────
    let mut radials = Vec::new();
    radials.extend((0..10).map(|a| radial(1, a)));
    radials.extend((0..10).map(|a| radial(2, a)));
    radials.extend((0..5).map(|a| radial(1, a)));

    let sweeps = Sweep::from_radials(radials);
    assert_eq!(
        sweeps
            .iter()
            .map(|s| (s.elevation_number(), s.radials().len()))
            .collect::<Vec<_>>(),
        vec![(1, 10), (2, 10), (1, 5)],
        "`from_radials` splits on every elevation change, in order, and does \
         not regroup — a re-run of the same walk it always did",
    );
    for sweep in &sweeps {
        assert_eq!(
            sweep.radials_capacity(),
            sweep.radials().len(),
            "every sweep it emits is shrunk, not just the last",
        );
    }
}
