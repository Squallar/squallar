//! **A mosaic decoded into a retained block is the same mosaic, bit for bit.**
//!
//! The gate that stops the staging pool from being a data-corruption bug in a
//! performance fix's clothes. Everything else about the pool is a question of
//! how much it allocates; this is the one that decides whether it may exist at
//! all.
//!
//! Its own binary, and the ordering inside it is load-bearing: the two
//! reference mosaics are decoded **while the process-global slot has never been
//! given anything**, so they are the unpooled path by construction rather than
//! by a flag that could be wrong. Everything after them runs through a block
//! that already held something else.
//!
//! The two shipped products are used rather than one granule twice, because
//! they reserve **different** missing codes — the composite's -99, the rate's
//! -3 — so a decode that inherited the previous granule's mapping differs here
//! and nowhere else. That is the same difference `MrmsProduct::missing_codes`
//! exists for, and the same one that once left a third of the rate mosaic
//! reporting -3 mm/h as a measurement.

use squallar_overlays::mrms::{MrmsGrid, MrmsProduct, decode, staging};
use squallar_overlays::render::gridded::GridValues;

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");
const RATE_GZ: &[u8] = include_bytes!("../testdata/MRMS_PrecipRate_00.00_20260822-032400.grib2.gz");

fn gz_for(product: MrmsProduct) -> &'static [u8] {
    match product {
        MrmsProduct::ReflectivityComposite => COMPOSITE_GZ,
        MrmsProduct::PrecipRate => RATE_GZ,
    }
}

fn decode_granule(product: MrmsProduct) -> MrmsGrid {
    let grib = decode::gunzip(gz_for(product)).expect("the committed granule is a gzip member");
    decode::parse_grib2(&grib, product).expect("the committed granule decodes")
}

/// The code the poisoned block is filled with.
///
/// **`0xDEADBEEF` is gone because the slot is no longer `f32`.** The staged
/// buffer is a `Vec<u16>` of the source's own codes, and there is no such thing
/// as a `u16` no packing could publish — "impossible by bit pattern" is not
/// available at this width. What replaces it is a *checked* premise rather than
/// a weaker one: [`no_cold_mosaic_carries_the_poison_code`] runs inside the gate
/// below, over both shipped granules and all 24.5 M points, before any poison is
/// fed in. While it holds, "anything that reaches the grid without having been
/// written by the decode reads back as poison" is true at **every** point,
/// exactly as the `f32` spelling made it true by construction.
const POISON_CODE: u16 = 0xDEAD;

/// A code neither granule carries, in every slot of a mosaic-sized buffer.
///
/// `give` clears the buffer rather than zeroing it, so these codes stay in the
/// block the next decode is handed. Anything that reaches the grid without
/// having been written by the decode reads back as this.
fn poisoned_mosaic() -> Vec<u16> {
    vec![POISON_CODE; staging::STAGING_POINTS]
}

/// The stored codes of a granule that took the narrow arm.
///
/// Panics on [`GridValues::F32`], which is itself part of the premise: the pool
/// is a `Vec<u16>` slot and only the narrow arm's buffer can round-trip through
/// it, so a shipped product that stopped taking that arm must fail here loudly
/// rather than turn this whole gate into a test of the unpooled path.
fn codes(grid: &MrmsGrid) -> &[u16] {
    match &grid.grid.values {
        GridValues::Scaled(scaled) => &scaled.codes,
        GridValues::F32(_) | GridValues::Bytes(_) => panic!(
            "premise: a shipped MRMS granule decodes to 16-bit codes; this one \
             fell to another arm, so it can never be staged through the pool",
        ),
    }
}

/// What makes [`POISON_CODE`] stand in for `0xDEADBEEF`: it is in neither
/// mosaic, so a surviving poisoned slot differs from the cold decode wherever
/// it survives.
fn no_cold_mosaic_carries_the_poison_code(a: &MrmsGrid, b: &MrmsGrid) {
    for (name, grid) in [("composite", a), ("rate", b)] {
        assert!(
            !codes(grid).contains(&POISON_CODE),
            "premise: the {name} mosaic carries code {POISON_CODE:#06x} itself, \
             so poison surviving at those points would read as the real \
             granule; pick another code",
        );
    }
}

/// How many values differ as **bit patterns** — `f32::NAN != f32::NAN`, and a
/// mapped sentinel is exactly a `NaN`, so `==` would call two grids equal
/// wherever the difference actually is.
fn differing(a: &MrmsGrid, b: &MrmsGrid) -> usize {
    a.grid
        .values
        .iter()
        .zip(b.grid.values.iter())
        .filter(|(x, y)| x.to_bits() != y.to_bits())
        .count()
}

fn same_shape(a: &MrmsGrid, b: &MrmsGrid) -> bool {
    (
        a.grid.ni,
        a.grid.nj,
        a.grid.values.len(),
        a.valid,
        a.visible_points,
    ) == (
        b.grid.ni,
        b.grid.nj,
        b.grid.values.len(),
        b.valid,
        b.visible_points,
    ) && a.value_range.map(|(x, y)| (x.to_bits(), y.to_bits()))
        == b.value_range.map(|(x, y)| (x.to_bits(), y.to_bits()))
}

/// **Poison a block, decode into it, switch products, decode again — and get
/// the same two mosaics the cold path produced.**
///
/// One test rather than three, because the state each step leaves is the next
/// step's input: the buffer the rate is decoded into is the one the composite
/// filled a moment earlier, which is the product switch this is named for.
///
/// **Floor — `set_len_shortcut`** (measured 2026-08-31): have the decode
/// `set_len(points)` on the buffer it is handed and write only the first half
/// of the granule into it, which is the shape any future "the buffer is
/// already the right size" shortcut would take. Observed red at the shape
/// assertion — the poisoned tail moves `visible_points` and `value_range`
/// before the value walk is even reached.
///
/// Two lesser corruptions are caught earlier and never get here, which is why
/// they are recorded rather than asserted: a `give` that skipped its `clear`
/// trips `take`'s own clear and, failing that, the decode's
/// `values.len() != ni * nj` refusal; and a `take` relaxed from
/// `points == STAGING_POINTS` to `capacity >= points` is caught by
/// `staging::tests::a_grid_of_another_shape_is_never_given_the_mosaic_buffer`,
/// since both shipped products are the same shape and cannot show it here.
#[test]
fn a_product_switch_through_the_retained_block_is_bit_identical() {
    // Cold: nothing has been given back, so each of these owns a block nothing
    // else has touched. This is the unpooled path, by construction.
    let composite_cold = decode_granule(MrmsProduct::ReflectivityComposite);
    let rate_cold = decode_granule(MrmsProduct::PrecipRate);
    assert_eq!(
        composite_cold.grid.values.len(),
        rate_cold.grid.values.len(),
        "premise: the two shipped products share one CONUS grid shape, so a \
         difference between them below is a difference in the readings",
    );
    assert!(
        differing(&composite_cold, &rate_cold) > 0,
        "premise: the two mosaics differ, so an equality below is not two \
         readings of one granule agreeing with itself",
    );

    no_cold_mosaic_carries_the_poison_code(&composite_cold, &rate_cold);

    // ── A poisoned block, then the composite through it ───────────────────
    staging::global().give(poisoned_mosaic());
    let composite_pooled = decode_granule(MrmsProduct::ReflectivityComposite);
    assert!(
        same_shape(&composite_pooled, &composite_cold),
        "a mosaic decoded into a retained block keeps its own shape, stamp and \
         summary",
    );
    assert_eq!(
        differing(&composite_pooled, &composite_cold),
        0,
        "a mosaic decoded into a block full of the poison code must be the \
         mosaic, value for value and bit for bit",
    );

    // ── Then the OTHER product, through the block the composite just filled ─
    staging::global().recycle(composite_pooled);
    let rate_pooled = decode_granule(MrmsProduct::PrecipRate);
    assert!(
        same_shape(&rate_pooled, &rate_cold),
        "and so does the next product's",
    );
    assert_eq!(
        differing(&rate_pooled, &rate_cold),
        0,
        "the rate decoded into the block the COMPOSITE had just filled must be \
         the rate — the two reserve different missing codes, so an inherited \
         mapping or an inherited tail shows up right here",
    );
}
