//! **A mosaic decoded into a retained block is the same mosaic, bit for bit.**
//!
//! The gate that stops the GMGSI staging pool from being a data-corruption bug
//! in a performance fix's clothes — `mrms_staging_identity.rs` for this
//! layer, and the reasoning is the same. Everything else about the pool is a
//! question of how much it allocates; this is the one that decides whether it
//! may exist at all.
//!
//! Its own binary, and the ordering inside it is load-bearing: the reference
//! mosaic is decoded **while the process-global slot has never been given
//! anything**, so it is the unpooled path by construction rather than by a
//! flag that could be wrong. Everything after it runs through a block that
//! already held something else.
//!
//! Only the raster lands in the slot, so the raster is where poison would
//! surface; the axes are compared as well because they are the other half of
//! what a granule decodes to, and the last arm reads them off the granule
//! rather than taking them from the axis cache.

use squallar_overlays::gmgsi::{GmgsiChannel, decode, staging};
use squallar_overlays::hrrr::GridCoords;

const GRANULE: &[u8] = include_bytes!(
    "../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

fn decode_granule() -> decode::GmgsiGrid {
    decode::decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr)
        .expect("the committed granule decodes")
}

/// A byte no real reading can be, in every slot of a mosaic-sized buffer.
///
/// `give` clears the buffer rather than zeroing it, so these bytes stay in the
/// block the next decode is handed. Anything that reaches the grid without
/// having been written by the decode reads back as this.
fn poisoned_mosaic() -> Vec<f32> {
    let mut v: Vec<f32> = Vec::new();
    v.try_reserve_exact(staging::STAGING_POINTS)
        .expect("a mosaic buffer fits on a test host");
    v.resize(staging::STAGING_POINTS, f32::from_bits(0xDEAD_BEEF));
    v
}

fn axes(g: &decode::GmgsiGrid) -> (&[f64], &[f64]) {
    match &g.grid.coords {
        GridCoords::Separable { lat_axis, lon_axis } => (lat_axis, lon_axis),
        other => panic!("GMGSI must decode onto Separable, got {other:?}"),
    }
}

/// How many values differ as **bit patterns** — `f32::NAN != f32::NAN`, and
/// the planted fill is exactly a `NaN`, so `==` would call two grids equal
/// wherever the difference actually is.
fn differing_values(a: &decode::GmgsiGrid, b: &decode::GmgsiGrid) -> usize {
    a.grid
        .values
        .iter()
        .zip(b.grid.values.iter())
        .filter(|(x, y)| x.to_bits() != y.to_bits())
        .count()
}

fn differing_axes(a: &decode::GmgsiGrid, b: &decode::GmgsiGrid) -> usize {
    let (lat_a, lon_a) = axes(a);
    let (lat_b, lon_b) = axes(b);
    lat_a
        .iter()
        .chain(lon_a)
        .zip(lat_b.iter().chain(lon_b))
        .filter(|(x, y)| x.to_bits() != y.to_bits())
        .count()
}

/// **Poison a block, decode into it, recycle, decode again — and get the
/// mosaic the cold path produced, twice.**
///
/// The second pooled decode goes through the block the *first* pooled decode
/// filled, which is the steady state a playing loop lives in.
///
/// **Floor — `keep_the_tail`** (tamper): remove every `clear()` a buffer
/// passes through — the pool's on `give` and `take`, the decode's in
/// `read_whole` — so the raster lands after the poison. Observed red at the
/// length refusal inside the decode, before any comparison here is reached;
/// any one of the three clears alone is enough to keep it green, which is
/// why all three are there.
#[test]
fn a_loop_of_decodes_through_the_retained_block_is_bit_identical() {
    // Cold: nothing has been given back, so this owns a block nothing else
    // has touched. The unpooled path, by construction.
    let cold = decode_granule();
    assert_eq!(
        staging::global().totals().reused,
        0,
        "premise: the cold decode was not handed a retained buffer",
    );
    assert!(
        cold.grid.values.iter().any(|v| v.is_nan()),
        "premise: the planted fill is in the raster, so a `to_bits` comparison \
         is doing work `==` could not",
    );

    // A poisoned block, then the granule through it.
    staging::global().give(poisoned_mosaic());
    let pooled = decode_granule();
    assert_eq!(
        staging::global().totals().reused,
        1,
        "premise: the pooled decode WAS handed the poisoned block",
    );
    assert_eq!(
        (
            pooled.grid.nj,
            pooled.grid.ni,
            pooled.grid.values.len(),
            pooled.bounds,
            pooled.valid_time
        ),
        (
            cold.grid.nj,
            cold.grid.ni,
            cold.grid.values.len(),
            cold.bounds,
            cold.valid_time
        ),
        "a mosaic decoded into a retained block keeps its own shape, envelope \
         and stamp",
    );
    assert_eq!(
        differing_axes(&pooled, &cold),
        0,
        "both axes must be the axes: they were collapsed out of the same \
         poisoned block the raster then landed in",
    );
    assert_eq!(
        differing_values(&pooled, &cold),
        0,
        "a mosaic decoded into a block full of 0xDEADBEEF must be the mosaic, \
         value for value and bit for bit",
    );

    // Then again, through the block the pooled decode just filled — and
    // with the axes READ rather than remembered, so the axis comparison
    // below is against a real walk over the coordinate arrays and not
    // against a clone of what the cold decode remembered.
    staging::recycle(staging::global(), pooled.grid);
    let forgetful = decode::AxisCache::new();
    let again = decode::decode_in(
        GRANULE.to_vec(),
        GmgsiChannel::LongwaveIr,
        staging::global(),
        &forgetful,
    )
    .expect("the committed granule decodes");
    assert_eq!(staging::global().totals().reused, 2);
    assert_eq!(
        forgetful.totals(),
        decode::AxisCacheTotals { hits: 0, misses: 2 },
        "premise: both axes were read off this granule",
    );
    assert_eq!(differing_axes(&again, &cold), 0);
    assert_eq!(
        differing_values(&again, &cold),
        0,
        "and the decode after that, into the block the last raster occupied",
    );
}
