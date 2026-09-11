//! **`overlay grid dupes`, and that it is not vacuous** — one instant held by
//! both a layer's stores costs its bytes TWICE, and the counter says so.
//!
//! The row exists because `overlay grid states` cannot: it prices `live` and
//! `staged` as two honest terms and cannot say whether one is the same data as
//! the other. A zero on that question is a real and useful reading, which is
//! exactly why the counter needs a test that its non-zero branch works — a
//! counter wired to nothing also reads zero, on every scene, for ever.
//!
//! `apply_frame` is called directly here rather than through a fetch, because
//! the shipped fetch path now DECLINES an instant the live cache holds
//! (`live_cache_serves_a_named_frame.rs`). That guard is what makes the
//! duplicate rare; it is not what makes it impossible, so the counter stays.
//!
//! Its own binary: the shipped staging slot is process-global, so a second test
//! here could not tell its own parked mosaic from one another test left behind.

use squallar_overlays::mrms::{MrmsFetchResult, MrmsFrameFetch, MrmsGrid, MrmsProduct, decode};
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::handler::PaneRef;
use squallar_source::id::known;
use squallar_source::time::FrameStamp;

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");

fn decode_granule() -> MrmsGrid {
    let grib = decode::gunzip(COMPOSITE_GZ).expect("the committed granule is a gzip member");
    decode::parse_grib2(&grib, MrmsProduct::ReflectivityComposite)
        .expect("the committed granule decodes")
}

#[test]
fn one_instant_in_both_stores_is_counted_once_and_priced_whole() {
    let mut registry = OverlayRegistry::default();
    let pane = PaneRef::bare(0);
    squallar_overlays::staging::release_all_retained();

    let live = decode_granule();
    let valid = live.valid;
    let one = live.resident_bytes() as u64;
    assert!(one > 0, "premise: the committed granule costs something");

    registry
        .get_handler_mut(&known::MRMS)
        .expect("the shipped registry carries MRMS")
        .apply_fetch_result(Box::new(MrmsFetchResult(Ok(live))), &pane);

    // **Nothing is duplicated yet**, and this reading is the floor the one
    // below is measured against: without it a counter that answered "one
    // granule" unconditionally would pass the assertion at the end.
    let before = registry.staged_duplicating_live();
    assert_eq!(
        (before.granules, before.bytes),
        (0, 0),
        "one granule in ONE store is not a duplicate",
    );

    // A SECOND, independent decode of the same instant — which is what the two
    // stores did on every path before the fetch guard landed.
    let framed = decode_granule();
    assert_eq!(framed.valid, valid, "premise: the same instant");
    registry.apply_frame(
        &known::MRMS,
        FrameStamp { valid, run: None },
        Box::new(MrmsFrameFetch {
            product: MrmsProduct::ReflectivityComposite,
            valid,
            grid: Some(framed),
        }),
        &pane,
    );

    let after = registry.staged_duplicating_live();
    assert_eq!(
        (after.granules, after.bytes),
        (1, one),
        "one instant in both stores is ONE duplicate priced at ONE granule",
    );

    // **And the family really did grow by that granule.** The counter is a
    // claim about `overlay grids`, so it is checked against `overlay grids`
    // rather than only against itself.
    let split = registry.resident_source_split();
    assert_eq!(
        split.staged, one,
        "the staged term is the second copy, whole",
    );
    assert_eq!(split.live, one, "and the live term still holds the first",);
}
