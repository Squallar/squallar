//! What [`scan_bytes`] counts, over volumes built to make each part of the
//! sum visible on its own.
//!
//! **Nothing here pins a byte count for a shape**, and that is deliberate.
//! The figure is exact for the volume in hand, not for the bytes it was
//! decoded from: `Vec` capacity is not stable the way length is, two decodes
//! of the same archive object grow differently, and a 336-byte spread was
//! observed inside one shape group of the 208-volume corpus. A test that
//! hard-coded a volume's size would be pinning the growth policy of the
//! standard library. So every assertion here is a **relation** — against the
//! capacities the vectors actually ended up with, or between two shapes.

use super::*;
use nexrad_model::data::{
    CFPMomentData, MomentData, PulseWidth, RadialStatus, VolumeCoveragePattern,
};

/// One allocator block, spelled the way the module charges it.
const BLOCK: usize = ALLOCATOR_BLOCK_OVERHEAD;

/// What a scan is charged before any sweep is walked: the sweep vector's own
/// block plus the metadata blocks the walk cannot enumerate.
const SCAN_LEVEL_BLOCKS: usize = (1 + SCAN_METADATA_BLOCKS) * ALLOCATOR_BLOCK_OVERHEAD;

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
///
/// Seven is the ceiling and it is reachable: the seventh slot is clutter
/// filter power, and a third of the 1,498,164 radials censused across the
/// archive corpus carry it.
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
    let cfp = (moments > 6).then(|| {
        CFPMomentData::from_fixed_point(gates as u16, 2125, 250, 8, 2.0, 66.0, vec![7u8; gates])
    });
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
        cfp,
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

/// **What the scan in hand is actually holding**, derived from the shape the
/// caller asked for and the capacities the vectors ended up with — never from
/// [`scan_bytes`]'s own walk.
///
/// This is the oracle the tests below check against. It reads capacity through
/// the accessors rather than assuming `collect` sized anything exactly, so it
/// stays true for a vector that was grown by pushing.
fn held(scan: &Scan, gates: usize, moments: usize) -> usize {
    if scan.sweeps_capacity() == 0 {
        return 0;
    }
    let mut total = scan.sweeps_capacity() * size_of::<Sweep>() + SCAN_LEVEL_BLOCKS;
    for sweep in scan.sweeps() {
        if sweep.radials_capacity() == 0 {
            continue;
        }
        total += sweep.radials_capacity() * size_of::<Radial>() + BLOCK;
        // Every radial in these fixtures carries the same load, so the gate
        // term is arithmetic rather than another walk: one buffer per moment,
        // `gates` bytes in it, one allocator block holding it.
        let per_radial = if gates == 0 {
            0
        } else {
            moments * (gates + BLOCK)
        };
        total += sweep.radials().len() * per_radial;
    }
    total
}

/// The gate payload dominates and is counted exactly: every moment of every
/// radial of every sweep, once. The containers are the rest, and they are
/// `capacity * size_of::<T>()` at each level rather than an estimate, so the
/// whole figure is exact against the volume it is given.
#[test]
fn the_gate_bytes_of_every_moment_are_counted_once() {
    let (sweeps, radials, gates, moments) = (3, 40, 1000, 4);
    let scan = scan_of(sweeps, radials, gates, moments);
    assert_eq!(scan_bytes(&scan), held(&scan, gates, moments));

    // And the same total, decomposed, so a reader can see which term is which.
    let payload = sweeps * radials * gates * moments;
    let moment_blocks = sweeps * radials * moments * BLOCK;
    let containers = scan.sweeps_capacity() * size_of::<Sweep>()
        + scan
            .sweeps()
            .iter()
            .map(|s| s.radials_capacity() * size_of::<Radial>() + BLOCK)
            .sum::<usize>();
    assert_eq!(
        scan_bytes(&scan),
        payload + moment_blocks + containers + SCAN_LEVEL_BLOCKS
    );
}

/// **The price is the capacity the vectors hold, not their length.**
///
/// The guard on the whole point of the change. A decoder that grows a radial
/// vector radial by radial leaves ~42 % spare past the length in real archive
/// volumes, the allocator holds every byte of it, and a `size_of_val` on the
/// `&[Radial]` the model hands out cannot see any of it — which is what put
/// this function 1.35–2.01 % below live heap on all 208 corpus volumes.
#[test]
fn spare_capacity_in_a_radial_vector_is_charged() {
    let exact: Vec<Radial> = (0..100).map(|_| radial(500, 3)).collect();
    let mut grown: Vec<Radial> = Vec::new();
    for _ in 0..100 {
        grown.push(radial(500, 3));
    }
    let (exact_cap, grown_cap) = (exact.capacity(), grown.capacity());
    assert!(
        grown_cap > exact_cap,
        "the fixture did not produce a vector with spare capacity \
         ({grown_cap} vs {exact_cap}), so this test proves nothing"
    );

    let priced = |radials: Vec<Radial>| scan_bytes(&Scan::new(vcp(), vec![Sweep::new(1, radials)]));
    assert_eq!(
        priced(grown) - priced(exact),
        (grown_cap - exact_cap) * size_of::<Radial>(),
        "the spare slots the allocator is holding went uncharged"
    );
}

/// Every allocation the walk can see is charged one block, and an empty
/// buffer is charged none — a `Vec` of zero length never asked the allocator
/// for anything, so charging it a header would invent bytes.
#[test]
fn one_allocator_block_is_charged_per_allocation() {
    let scan = scan_of(2, 10, 400, 5);
    let blocks = 2 * 10 * 5   // one gate buffer per present moment
        + 2                   // one radial vector per sweep
        + 1                   // the sweep vector
        + SCAN_METADATA_BLOCKS;
    let without_overhead = {
        let payload = 2 * 10 * 400 * 5;
        let containers = scan.sweeps_capacity() * size_of::<Sweep>()
            + scan
                .sweeps()
                .iter()
                .map(|s| s.radials_capacity() * size_of::<Radial>())
                .sum::<usize>();
        payload + containers
    };
    assert_eq!(scan_bytes(&scan), without_overhead + blocks * BLOCK);

    // A zero-gate moment is a zero-length buffer: no block, no bytes.
    let empty_buffers = scan_of(1, 4, 0, 6);
    assert_eq!(scan_bytes(&empty_buffers), held(&empty_buffers, 0, 6));
}

/// A moment the radial does not carry costs its gates nothing — an `Option`
/// that is `None` must not be charged, or a seven-moment price would be quoted
/// for every three-moment volume in the archive.
#[test]
fn an_absent_moment_is_not_charged() {
    let three = scan_bytes(&scan_of(2, 10, 500, 3));
    let five = scan_bytes(&scan_of(2, 10, 500, 5));
    let seven = scan_bytes(&scan_of(2, 10, 500, 7));
    // The three loads the corpus census actually found. Each added moment is
    // its gates plus the block holding them, and nothing else moves.
    assert_eq!(five - three, 2 * 10 * 2 * (500 + BLOCK));
    assert_eq!(seven - five, 2 * 10 * 2 * (500 + BLOCK));
}

/// An empty volume is its containers and nothing else, and a volume with no
/// sweeps at all is zero — not a panic and not a floor.
///
/// Zero rather than the metadata blocks: a `Scan` whose sweep vector never
/// allocated is not a decoded volume, and the residual blocks were counted
/// against volumes that were.
#[test]
fn an_empty_volume_prices_at_nothing() {
    let nothing = Scan::new(vcp(), Vec::new());
    assert_eq!(scan_bytes(&nothing), 0);
    let sweeps_only = scan_of(2, 0, 0, 0);
    assert_eq!(
        scan_bytes(&sweeps_only),
        sweeps_only.sweeps_capacity() * size_of::<Sweep>() + SCAN_LEVEL_BLOCKS
    );
}

/// **The price rises with every radial added and never falls.**
///
/// Monotonicity is the property a cache depends on and the one a shape-pinned
/// number cannot express: a volume with more in it must never price lower
/// than a volume with less, whatever the allocator did with the capacities.
#[test]
fn the_price_is_monotone_in_radial_count() {
    let mut previous = 0;
    for radials in [0usize, 1, 2, 7, 40, 41, 300, 720] {
        let scan = scan_of(4, radials, 800, 5);
        let priced = scan_bytes(&scan);
        assert_eq!(priced, held(&scan, 800, 5));
        assert!(
            priced > previous || radials == 0,
            "{radials} radials priced at {priced}, not above the {previous} \
             the shorter volume priced at"
        );
        previous = priced;
    }
}

/// **The figure lands in the range the app's own decode was measured at.**
///
/// The corpus is 208 real archive volumes decoded under a counting global
/// allocator: **48.88 MiB median live heap, 74.63 MiB maximum**. Priced here
/// at a VCP 212 shape — 16 sweeps of 720 radials, three moments' worth of
/// gates on each — this reads a little under the corpus median, which is what
/// the radial census predicts: a uniform three-moment model is the light end
/// of a corpus where a third of radials carry all seven.
///
/// **This is an order-of-magnitude sanity check, not an independent oracle.**
/// What it pins is that the arithmetic reaches tens of MiB at a real volume's
/// shape rather than the kilobytes a `size_of_val` would report.
///
/// **What the moment load per radial actually is, measured.** Over 1,498,164
/// radials of the same corpus, a radial carries **three, five or seven
/// moments — never four and never six**; the splits are the cut boundaries,
/// and a third of all radials carry seven. So the load is not a free
/// parameter to be tuned until the arithmetic agrees, and the old note here
/// that inferred "the archive's volumes do not carry six full moments on
/// every radial" from a six-moment shape pricing past the measured maximum
/// was reasoning from two false premises: six is not a load any radial in the
/// corpus carries, and the maximum it was measured against (58.3 MiB) was a
/// discounted figure — the real one, 74.63 MiB, sits 9.5 % *under* that
/// six-moment shape rather than comfortably below it, so the headroom the
/// inference rested on was never there.
#[test]
fn a_realistic_volume_prices_near_the_measured_range() {
    let mib = scan_bytes(&scan_of(16, 720, 1400, 3)) as f64 / (1024.0 * 1024.0);
    assert!(
        (40.0..=74.63).contains(&mib),
        "a VCP-212-shaped volume priced at {mib:.1} MiB, outside the \
         48.88 MiB median / 74.63 MiB max the decode path was measured at"
    );
}

/// The price is linear in every dimension of the shape. The guard against an
/// accumulator that forgets a level of the walk and quotes one sweep's worth
/// for a whole volume.
///
/// The relations are not bare multiples, because two terms are charged **once
/// per scan** however big the scan is: the sweep vector's own block and the
/// metadata blocks beside it. Spelled out rather than absorbed, so a reader
/// does not take the residue for an off-by-one.
#[test]
fn the_price_scales_with_the_shape() {
    let one = scan_bytes(&scan_of(1, 100, 1000, 2));
    assert_eq!(
        scan_bytes(&scan_of(4, 100, 1000, 2)),
        4 * one - 3 * SCAN_LEVEL_BLOCKS,
        "sweeps past the first went uncounted"
    );
    // Twice the radials in ONE sweep costs twice the radials, but the sweep
    // slot, the radial vector's block and the scan-level blocks are each
    // still charged exactly once.
    assert_eq!(
        scan_bytes(&scan_of(1, 200, 1000, 2)),
        2 * one - size_of::<Sweep>() - BLOCK - SCAN_LEVEL_BLOCKS,
        "radials past the first hundred went uncounted"
    );
}

/// A scan's price is the sum of its sweeps' prices plus the scan-level terms.
///
/// The eviction path prices each sweep on its own as it hands it to the
/// deferred-drop queue, so what the queue is told it holds and what the cache
/// was told it released have to be the same bytes.
#[test]
fn the_sweep_prices_sum_to_the_scan_price() {
    let scan = scan_of(5, 60, 900, 7);
    let sweeps: usize = scan.sweeps().iter().map(sweep_bytes).sum();
    assert_eq!(
        scan_bytes(&scan),
        sweeps + scan.sweeps_capacity() * size_of::<Sweep>() + SCAN_LEVEL_BLOCKS
    );
}
