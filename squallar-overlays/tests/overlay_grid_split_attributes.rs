//! **The `overlay grids` census family, attributed to the handler holding
//! it** — the property `OverlayRegistry::resident_source_bytes` cannot have,
//! because it is one `u64` folded over fifteen handlers.
//!
//! `tests/overlay_grid_residency_split.rs` states the problem this suite
//! closes: a sum of four holders "cannot be attributed by solving it", and it
//! shows two arithmetically perfect and mutually contradictory fits for the
//! same reading. That is a gap in the instrument, and
//! `OverlayRegistry::resident_source_split` is the instrument that closes it.
//!
//! Its own binary and one test, for the reason
//! `overlay_grid_residency_split.rs` gives: the shipped staging slot is
//! process-global, so a second test in the same binary could not tell its own
//! parked mosaic from one another test left behind.

use squallar_overlays::mrms::{MrmsFetchResult, MrmsGrid, MrmsProduct, decode};
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::handler::PaneRef;
use squallar_source::id::known;

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");

fn decode_granule() -> MrmsGrid {
    let grib = decode::gunzip(COMPOSITE_GZ).expect("the committed granule is a gzip member");
    decode::parse_grib2(&grib, MrmsProduct::ReflectivityComposite)
        .expect("the committed granule decodes")
}

#[test]
fn the_split_attributes_the_family_to_one_named_handler() {
    let mut registry = OverlayRegistry::default();
    let pane = PaneRef::bare(0);

    // The shipped slot is process-global; this binary's own decodes fill it,
    // so the level every reading below is taken from is one this test set.
    squallar_overlays::staging::release_all_retained();

    let empty = registry.resident_source_split();
    assert_eq!(
        (empty.mrms, empty.gmgsi, empty.model, empty.glm, empty.other),
        (0, 0, 0, 0, 0),
        "premise: a fresh registry with the pools emptied attributes nothing. \
         A non-zero term here is another handler answering, and every reading \
         below would be measured against it",
    );
    assert_eq!(empty.total(), registry.resident_source_bytes());

    registry
        .get_handler_mut(&known::MRMS)
        .expect("the shipped registry carries MRMS")
        .apply_fetch_result(Box::new(MrmsFetchResult(Ok(decode_granule()))), &pane);

    let split = registry.resident_source_split();
    let sum = registry.resident_source_bytes();

    // **The sum and the split are the same walk**, so they may never disagree.
    // This is the property that makes the split quotable as a decomposition of
    // the published family rather than as a second opinion about it.
    assert_eq!(
        split.total(),
        sum,
        "the split's terms must add to exactly the figure `overlay grids` \
         publishes: {split:?} against {sum}",
    );

    // **The attribution itself.** One MRMS granule is one tiled mosaic plus
    // the 16-row band it streamed through, and it belongs to MRMS alone. The
    // pinned figure is `overlay_grid_residency_split.rs`'s, restated here so a
    // granule that stopped being elided fails in both suites.
    assert_eq!(
        split.mrms, 5_778_748,
        "one live MRMS granule, tiled, plus its retained band",
    );
    assert_eq!(
        (split.gmgsi, split.model, split.glm, split.other),
        (0, 0, 0, 0),
        "and nothing else may claim it — a family that reads the MRMS mosaic \
         into another term is exactly the mis-attribution this split exists \
         to prevent",
    );

    // ── the other axis: the same bytes by the STATE they sit in ──────────
    //
    // **Two decompositions of ONE walk**, so they must reach the same total.
    // A build where they differ has a handler whose four states do not add to
    // its own `resident_source_bytes`, which would make both rows unquotable.
    assert_eq!(
        split.state_total(),
        sum,
        "the state axis must reach the same figure the handler axis does: \
         live {} + staged {} + parked {} + carried {} against {sum}",
        split.live,
        split.staged,
        split.parked,
        split.carried,
    );

    // **And the states are not a single bucket.** One live granule is a live
    // mosaic plus the 16-row band the decode parked, and those are the two
    // terms with the most different prices in the whole family: the mosaic
    // answers hover and every re-raster, the band is read by nothing at all.
    // A split that folded them together would say this layer holds 5,778,748 B
    // of equally-priced bytes, which is exactly the confusion the state axis
    // exists to prevent.
    assert_eq!(
        split.parked,
        squallar_overlays::mrms::CONUS_BAND_BYTES as u64,
        "the parked term must find the retained decode band and price it — a \
         zero here reads exactly like a pool that parked nothing",
    );
    assert_eq!(split.parked, 224_000);
    assert_eq!(
        split.live, 5_554_748,
        "and the live term is the tiled mosaic alone, without the band",
    );
    assert_eq!(
        (split.staged, split.carried),
        (0, 0),
        "nothing is staged for a loop frame here and the carry is the same \
         allocation as the cache entry, so pricing either would be a \
         double-count of the mosaic above",
    );

    // **`other` is the escape hatch that must stay shut.** A gridded layer
    // that lands without a term in the split would otherwise vanish from the
    // decomposition while still being inside the sum, which is the failure
    // that makes a decomposition worse than no decomposition.
    assert_eq!(
        split.other, 0,
        "every handler answering non-zero today is named; a non-zero here is \
         a new gridded layer the split does not name",
    );
}
