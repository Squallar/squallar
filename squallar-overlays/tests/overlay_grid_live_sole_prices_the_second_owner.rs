//! **`overlay grid live sole`: a resident live granule has TWO owners, so
//! releasing the cache would free NOTHING** — and the row says so instead of
//! letting a cut be scored on the `live` term.
//!
//! This is not incidental to a scene. `MrmsHandler::apply_fetch_result` holds
//! the mosaic twice by construction, and its own note says why the ORDER
//! matters: "the mosaic being replaced is held twice — by this cache and by
//! `state.data`". `set_data` first, then the cache insert. So every resident
//! granule has the cache and the layer's `OverlayState` pointing at it, and a
//! release that drops only the cache's reference frees nothing — which is why
//! `release_data` clears both.
//!
//! A cut scored on `live` claims those bytes. A cut scored on `sole` does not.
//! On the six-pane scene this row read `sole 0 B over 0 entries, shared
//! 29265072 B over 3 entries` across 9279 walks, which is this fact measured
//! rather than argued.

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
fn a_resident_live_granule_reads_shared_and_never_sole() {
    let mut registry = OverlayRegistry::default();
    let pane = PaneRef::bare(0);
    squallar_overlays::staging::release_all_retained();

    // **The empty floor first.** All-zero is what this row reads on a fresh
    // registry AND what it would read if the walk were wired to nothing, so
    // the reading below is only evidence because this one came first.
    let empty = registry.live_sole();
    assert_eq!(
        (
            empty.sole_bytes,
            empty.shared_bytes,
            empty.sole_entries,
            empty.shared_entries
        ),
        (0, 0, 0, 0),
        "premise: nothing resident, nothing owned",
    );

    let grid = decode_granule();
    let one = grid.resident_bytes() as u64;
    registry
        .get_handler_mut(&known::MRMS)
        .expect("the shipped registry carries MRMS")
        .apply_fetch_result(Box::new(MrmsFetchResult(Ok(grid))), &pane);

    let sole = registry.live_sole();
    assert_eq!(
        sole.shared_entries, 1,
        "the one resident granule is held by the cache AND by `state.data`",
    );
    assert_eq!(
        sole.shared_bytes, one,
        "and it is priced whole on the side a release cannot take",
    );
    assert_eq!(
        (sole.sole_bytes, sole.sole_entries),
        (0, 0),
        "NOTHING here is free to give back: dropping the cache's reference \
         leaves the layer's own carry holding all {one} bytes",
    );

    // **The decomposition invariant**, which is what makes the row quotable
    // beside `overlay grid states` rather than a second opinion about it.
    let split = registry.resident_source_split();
    assert_eq!(
        sole.sole_bytes + sole.shared_bytes,
        split.live,
        "sole + shared must be exactly the `live` term the states row publishes",
    );
}
