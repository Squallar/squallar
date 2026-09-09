//! **What the `overlay grids` census family is made of, priced holder by
//! holder on the real committed granule.**
//!
//! `squallar_egui::heap_census`'s `OVERLAY_GRID_BYTES` is one number —
//! `OverlayRegistry::resident_source_bytes`, the sum of every handler's own
//! `resident_source_bytes` — and a sum is not attributable. A reading of it
//! can be one mosaic or four, and the difference is the difference between a
//! cache that is doing its job and a block nothing reads. This suite names the
//! terms and prices each one, through the shipped registry's own doors, so a
//! census reading can be decomposed rather than guessed at.
//!
//! **The measured readings this was written against, and why arithmetic over
//! them is not an answer.** All four are the **wasm32 page instance** — a
//! native reading dies of different things and is not comparable:
//!
//! | leg | tip | `overlay grids` |
//! |---|---|---|
//! | Tier-2 `huge`, Firefox, canvas 2878x1651 | `651d289d1` | 155,776,456 B |
//! | Tier-2 `long`, Firefox | post-loop-fix | 218,991,296 B |
//! | Tier-2 `long`, Firefox (second leg) | post-loop-fix | 203,994,296 B |
//! | Tier-2 `long`, Chromium | post-loop-fix | 218,664,080 B |
//!
//! Every one of them is **flat across every census sample of its leg** — the
//! `long` legs from their first sample, the `huge` leg from t+2.2 s — so this
//! is a working set that arrives and sits, not a cache converging.
//!
//! The one figure that decomposes uniquely is the `huge` leg's FIRST sample,
//! at t+0.001 s: **56,620,748 = 49,000,000 + 7,620,748**, one MRMS mosaic plus
//! one HRRR grid (its values plus the 184 B of `HrrrGridData`), to the byte.
//! That is the anchor every unit price below is checked against — and it is a
//! reading of the FLAT store. The mosaic term is 5,554,748 B on this tree; the
//! decomposition above is kept as the measurement it was, not re-derived.
//!
//! **The settled figures do not.** A search over `a` mosaics, `b` HRRR grids,
//! `c` GMGSI granules and a GLM remainder finds several exact fits for each of
//! them — 155,776,456 is `3 MRMS + 1 HRRR + 1,155,708` and also
//! `1 MRMS + 1 GMGSI + 12 HRRR + 327,228`, and both are arithmetically
//! perfect. **A sum of four holders cannot be attributed by solving it**, and
//! that is a gap in the instrument rather than a fact about the heap: the
//! census publishes `OverlayRegistry::resident_source_bytes`, one `u64` over
//! every handler, and nothing on the page says which layer or which of a
//! layer's own stores it came from.
//!
//! So this suite prices the holders **at the allocator, in process, on the
//! real committed granule** instead, which is what the campaign's standing
//! ruling asks for: an exact in-process figure beats a noisy end-to-end one,
//! and here it beats an ambiguous one.
//!
//! Its own binary, and one test, for the reason `mrms_staging_blocks.rs` gives:
//! the shipped staging slot is process-global, so a second test in the same
//! binary could not tell its own parked mosaic from one another test left
//! behind.

use squallar_overlays::mrms::{MrmsFetchResult, MrmsFrameFetch, MrmsGrid, MrmsProduct, decode};
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::handler::PaneRef;
use squallar_source::id::known;
use squallar_source::time::FrameStamp;

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");

/// One granule through the whole shipped path, exactly as
/// `mrms::fetch::decode_body` runs it.
fn decode_granule() -> MrmsGrid {
    let grib = decode::gunzip(COMPOSITE_GZ).expect("the committed granule is a gzip member");
    decode::parse_grib2(&grib, MrmsProduct::ReflectivityComposite)
        .expect("the committed granule decodes")
}

fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
        .expect("a real date")
        .and_hms_opt(h, m, 0)
        .expect("a real time")
}

/// **Three holders, and they are no longer one mosaic each.**
///
/// The three, in the order a playing loop fills them:
///
/// 1. `MrmsHandler::cached_grids` — the live mosaic of the pane's selected
///    product, **tiled**. **Live**: `hover_value_at` reads values out of it and
///    `prepare_job` re-rasterizes from it on every pan, zoom and product
///    change.
/// 2. `MrmsHandler::frame_grids` — one loop frame's staged granule, tiled the
///    same way, one at a time however many frames the loop holds.
/// 3. `mrms::staging::global()` — the **plane** the pool parks between decodes,
///    which is still 24.5 M `u16` and still 49,000,000 B: the decoder reads a
///    granule into one and the tiler reads it back out. **Nothing else reads
///    it**, and its only release is `SourceHandler::release_data`, which fires
///    when no pane draws the layer.
///
/// So the family is now `2 x granule + 1 plane` where it was `3 x mosaic`, and
/// the granule is the term that moved: 5,554,748 B for the committed composite
/// against the 49,000,000 it used to be. The plane did not move and is now
/// **81 % of what one looping pane holds** — the largest single term left, and
/// the one a governor can take without trading anything a pane is drawing.
///
/// Floor — delete any one of the three terms from
/// `MrmsHandler::resident_source_bytes`: that step's delta reads 0 and the
/// total lands short by that term.
#[test]
fn the_overlay_grid_family_is_two_granules_and_a_plane() {
    let plane = squallar_overlays::mrms::CONUS_GRID_BYTES as u64;
    assert_eq!(
        plane, 49_000_000,
        "the decode plane, which the tiled store did not move",
    );
    /// The committed 2026-08-21 composite, tiled. Pinned here as well as at
    /// `mrms::decode::tests` so a granule that stopped being elided fails in
    /// the census suite that would then be reporting the wrong family.
    const GRANULE: u64 = 5_554_748;

    let mut registry = OverlayRegistry::default();
    let pane = PaneRef::bare(0);

    // **The shipped slot is process-global**, and this binary's own decodes
    // fill it, so the level every delta below is measured from is one this
    // test set rather than one it inherited.
    squallar_overlays::staging::release_all_retained();
    assert_eq!(
        registry.resident_source_bytes(),
        0,
        "premise: a fresh registry with the pools emptied holds no decoded \
         source at all. A non-zero here is another handler answering, and \
         every delta below would be measured against it",
    );

    // ── 1. the live cache, and the plane the decode parks ────────────────
    registry
        .get_handler_mut(&known::MRMS)
        .expect("the shipped registry carries MRMS")
        .apply_fetch_result(Box::new(MrmsFetchResult(Ok(decode_granule()))), &pane);
    let after_live = registry.resident_source_bytes();
    assert_eq!(
        squallar_overlays::mrms::staging::global().retained_bytes() as u64,
        plane,
        "the decode hands its plane back the instant the tiler has read it, \
         so the pool is warm from the FIRST granule rather than from the \
         first eviction",
    );
    assert_eq!(
        after_live,
        GRANULE + plane,
        "one live granule is one TILED mosaic plus the plane it was read out \
         of: the pane's carry and the cache entry are the same allocation, so \
         a figure of two granules here would be `resident_source_bytes` \
         double-counting the carry",
    );

    // ── 2. the loop frame's staged granule ───────────────────────────────
    registry.apply_frame(
        &known::MRMS,
        FrameStamp {
            valid: at(0, 0),
            run: None,
        },
        Box::new(MrmsFrameFetch {
            product: MrmsProduct::ReflectivityComposite,
            valid: at(0, 0),
            grid: Some(decode_granule()),
        }),
        &pane,
    );
    let after_frame = registry.resident_source_bytes();
    assert_eq!(
        after_frame - after_live,
        GRANULE,
        "a staged loop granule is a SECOND tiled mosaic beside the live one — \
         the frame store is not the live cache and a frame is never drawn \
         from it",
    );
    assert_eq!(
        squallar_overlays::mrms::staging::global().retained_bytes() as u64,
        plane,
        "and it is NOT a second plane: the slot held one and the second decode \
         was handed it back",
    );

    assert_eq!(
        after_frame,
        2 * GRANULE + plane,
        "the steady state of one pane looping one MRMS product is two tiled \
         granules and one plane — 60,109,496 B, against 147,000,000 with the \
         flat store and against the 155.8 to 219.0 MB the wasm32 page reads \
         for this whole family on the Tier-2 legs",
    );
    assert_eq!(after_frame, 60_109_496);

    // ── What the pressure lever can give back, and what it cannot ────────
    // Nothing reads the parked plane, so it is the one of the three a memory
    // governor may take without trading anything a pane is drawing — and it is
    // now four fifths of the total rather than one third of it.
    assert_eq!(
        squallar_overlays::staging::release_all_retained(),
        plane,
        "the crate-wide release must find the parked plane and price it — a \
         zero here is a lever that ran and gave nothing back, which reads \
         exactly like one nothing called",
    );
    assert_eq!(
        registry.resident_source_bytes(),
        2 * GRANULE,
        "releasing the pools takes the family to the two granules a pane is \
         actually using: 11,109,496 B, 81 % of it freed with nothing \
         refetched and nothing redrawn",
    );

    // Non-triviality: the release is not simply zeroing the whole family.
    assert_eq!(
        squallar_overlays::mrms::staging::global().retained_bytes(),
        0,
        "and the pool's own level agrees",
    );
}
