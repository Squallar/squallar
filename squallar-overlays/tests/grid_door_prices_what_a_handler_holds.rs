//! **What the admission door charges for a gridded layer, against what that
//! layer's handler can actually hold.**
//!
//! `source_grid_budget_bytes` + `source_grid_staging_bytes` is what a pane
//! enabling MRMS, GMGSI or the model layer asks the heap to be able to hold,
//! and it is one of the terms `NeedTerms::overlay_grids_host` adds up. Nothing
//! held that figure against the handler it prices, and the two directions of
//! being wrong are not symmetric:
//!
//! * **Under-charging admits something that then fails to allocate.** A layer
//!   let through on a price below what it will really take fails mid-session,
//!   which is far worse than a refusal at the door.
//! * **Over-charging refuses layers the heap could have carried.** The user
//!   meets a layer that will not enable, and the cause is an accounting error
//!   rather than a shortage.
//!
//! So the door has to price the ceiling. **What it can no longer do is meet a
//! real granule to the byte**, and the reason is the point of the MRMS store:
//! a tiled mosaic's bytes are a property of the *granule*, not of its point
//! count, and over 28 granules of both shipped products they run 2,350,138 to
//! 8,943,164 B against a ceiling of 49,496,648 — the figure a granule with no
//! uniform tile would attain and which nothing about the format forbids. The
//! door prices that ceiling, because the alternative is admitting a layer on a
//! measured average and failing to allocate on the granule that exceeds it.
//!
//! This suite therefore holds three separate things: the door never charges
//! LESS than a handler at its populations holds; the gap between the two is
//! the elision and is named here in bytes; and the staging half is still
//! attained to the byte, because a decode plane is a point count and has not
//! moved.
//!
//! **Its own binary, and one test**, for the reason
//! `overlay_grid_residency_split.rs` gives: the staging slots are
//! process-global statics, so a second test in this binary could not tell its
//! own parked block from one another test left behind.
//!
//! **The fixture property that makes this able to fail**: all three of a
//! handler's populations — the key-space cache, the staged frame granule and
//! the parked decode block — must be **simultaneously non-empty** when the
//! comparison runs. Any one of them empty and the sum lands short of the door
//! for a reason that has nothing to do with the door being wrong, so each is
//! asserted at its own level first. That is also what stops the equality being
//! satisfied by a handler that holds nothing at all.

use squallar_overlays::mrms::{MrmsFetchResult, MrmsFrameFetch, MrmsGrid, MrmsProduct, decode};
use squallar_overlays::render::gridded::{
    ByteCodes, GridValues, MAX_ABSENT_BYTES, MAX_ABSENT_POINTS,
};
use squallar_overlays::render::handlers::{source_grid_budget_bytes, source_grid_staging_bytes};
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::handler::PaneRef;
use squallar_source::id::known;
use squallar_source::time::FrameStamp;

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");
const PRECIP_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_PrecipRate_00.00_20260822-032400.grib2.gz");
const GMGSI_NC: &[u8] = include_bytes!(
    "../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

fn granule(product: MrmsProduct, gz: &[u8]) -> MrmsGrid {
    decode::parse_grib2(&decode::gunzip(gz).expect("a gzip member"), product)
        .expect("the committed granule decodes")
}

fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
        .expect("a real date")
        .and_hms_opt(h, m, 0)
        .expect("a real time")
}

#[test]
fn the_door_charges_exactly_what_a_handler_at_its_ceiling_holds() {
    let mosaic = squallar_overlays::mrms::CONUS_GRID_BYTES as u64;
    let pool = squallar_overlays::mrms::staging::global();
    let mut registry = OverlayRegistry::default();
    let pane_a = PaneRef::bare(0);
    let pane_b = PaneRef::bare(1);

    assert_eq!(
        registry
            .get_handler_mut(&known::MRMS)
            .expect("the shipped registry carries MRMS")
            .resident_source_bytes(),
        0,
        "premise: a fresh handler holds no decoded source, so every level \
         below is one this test put there",
    );

    // ── 1. the key-space cache: one grid per product a pane can select ──
    for (product, gz) in [
        (MrmsProduct::ReflectivityComposite, COMPOSITE_GZ),
        (MrmsProduct::PrecipRate, PRECIP_GZ),
    ] {
        registry
            .get_handler_mut(&known::MRMS)
            .expect("MRMS")
            .apply_fetch_result(
                Box::new(MrmsFetchResult(Ok(granule(product, gz)))),
                if product == MrmsProduct::PrecipRate {
                    &pane_b
                } else {
                    &pane_a
                },
            );
    }
    let cache_only = registry
        .get_handler_mut(&known::MRMS)
        .expect("MRMS")
        .resident_source_bytes();
    // **The key space is full — both products resident — and it is far under
    // the ceiling the door prices.** The pool is already holding the plane of
    // the second decode, so the cache's own half is the difference.
    let parked = pool.retained_bytes() as u64;
    let cache_grids = cache_only - parked;
    assert_eq!(
        cache_grids, 8_836_214,
        "the two committed granules, tiled: 5,554,748 for the composite and \
         3,281,466 for the rate. They were 98,000,000 flat",
    );
    assert!(
        cache_grids < source_grid_budget_bytes(&known::MRMS),
        "premise: the key space is under the ceiling the door prices, which is \
         what the gap below is a measurement of",
    );

    // ── 2. the staged frame granule ──
    registry.apply_frame(
        &known::MRMS,
        FrameStamp {
            valid: at(0, 0),
            run: None,
        },
        Box::new(MrmsFrameFetch {
            product: MrmsProduct::ReflectivityComposite,
            valid: at(0, 0),
            grid: Some(granule(MrmsProduct::ReflectivityComposite, COMPOSITE_GZ)),
        }),
        &pane_a,
    );
    let with_frame = registry
        .get_handler_mut(&known::MRMS)
        .expect("MRMS")
        .resident_source_bytes();
    assert_eq!(
        with_frame - cache_only,
        5_554_748,
        "premise: a loop frame's granule is staged beside the cache, not \
         inside it — and it is the same committed composite the cache holds, \
         at the same tiled size",
    );

    // ── 3. the parked decode block ──
    if pool.retained_bytes() == 0 {
        pool.give(
            pool.take(squallar_overlays::mrms::staging::STAGING_POINTS)
                .expect("a mosaic buffer fits on a test host"),
        );
    }
    assert_eq!(
        pool.retained_bytes() as u64,
        mosaic,
        "premise: the decode pool is holding its block, so all three \
         populations are non-empty at once",
    );

    // ── the door, against the ceiling all three make ──
    let ceiling = registry
        .get_handler_mut(&known::MRMS)
        .expect("MRMS")
        .resident_source_bytes();
    let door = source_grid_budget_bytes(&known::MRMS) + source_grid_staging_bytes(&known::MRMS);
    assert!(
        ceiling <= door,
        "the door must never charge LESS than the handler holds: {ceiling} \
         against {door}. Lowering it — dropping the parked block's half on the \
         argument that a still session no longer holds one — under-charges by \
         exactly that block the moment a loop of the layer runs, and an \
         admitted layer that then cannot allocate is the failure a door exists \
         to prevent",
    );
    assert_eq!(
        ceiling, 63_390_962,
        "what THIS handler holds with all three populations non-empty: two \
         tiled granules in the cache, one staged loop frame, and the 49 MB \
         decode plane the pool parks. It was 196,000,000 with a flat store",
    );
    assert_eq!(
        door, 196_993_296,
        "and what the door charges: two ceiling-sized grids plus two planes. \
         The gap to the line above is the elision — 133,602,334 B a granule \
         with no uniform tile would take up and the shipped ones do not. It is \
         the direction the door has to err in; a price set at what granules \
         actually cost is one the next granule can exceed",
    );

    // Left as this binary found it.
    assert_eq!(
        squallar_overlays::staging::release_all_retained(),
        mosaic,
        "the pressure lever finds the block this test parked and prices it",
    );

    // ── the absent set, which the GMGSI budget used not to carry ──
    //
    // A byte-arm grid holds a second allocation beside its codes and
    // `GridValues::resident_bytes` counts it, so a budget spelled as points ×
    // width under-states every real granule. Pinned against a grid built AT
    // the bound rather than against `size_of::<u32>()` restated, which would
    // be the same defect one level down.
    let codes = vec![0u8; 4096];
    let absent: Vec<u32> = (0..MAX_ABSENT_POINTS as u32).collect();
    let at_the_bound = GridValues::Bytes(
        ByteCodes::new(codes, absent).expect("a set at the bound is one the byte arm carries"),
    );
    assert_eq!(
        at_the_bound.resident_bytes(),
        4096 + MAX_ABSENT_BYTES,
        "a byte-arm grid costs its codes PLUS its absent set, and \
         `MAX_ABSENT_BYTES` is what that set costs at its bound — a width that \
         moved under this constant fails here rather than in a budget",
    );

    // And the committed GMGSI granule, which is the reason the constant moved:
    // it reads more than its point count and must fit the budget the layer
    // states for one granule.
    let real = squallar_overlays::gmgsi::decode::decode(
        GMGSI_NC.to_vec(),
        squallar_overlays::gmgsi::GmgsiChannel::LongwaveIr,
    )
    .expect("the committed granule decodes");
    let real_bytes = real.grid.values.resident_bytes();
    assert!(
        real_bytes > squallar_overlays::gmgsi::GRID_POINTS,
        "premise: a real granule is LARGER than its point count ({real_bytes} \
         against {}), which is the whole reason a budget spelled as points \
         alone under-stated it. A fixture with an empty absent set would make \
         the assertion below pass over the defect",
        squallar_overlays::gmgsi::GRID_POINTS,
    );
    assert!(
        real_bytes as u64 <= source_grid_staging_bytes(&known::GMGSI) - GRID_POOL_BYTES,
        "and one granule fits the budget the layer states for one granule: \
         {real_bytes} B against a frame-staging budget that must cover it",
    );
}

/// The pool half of `source_grid_staging_bytes`, so the frame half can be read
/// out of it — spelled from the pool's own figures rather than as a literal.
const GRID_POOL_BYTES: u64 = (squallar_overlays::gmgsi::staging::STAGING_POINTS
    * squallar_overlays::gmgsi::staging::StagingPool::ELEMENT_BYTES)
    as u64;
