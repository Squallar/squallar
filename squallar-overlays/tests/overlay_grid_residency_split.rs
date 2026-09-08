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
//! That is the anchor every unit price below is checked against.
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

/// **Three holders, one mosaic each, and the census family is their sum.**
///
/// Each step below adds exactly one mosaic and the assertion after it names
/// the holder that took it. The three are, in the order a playing loop fills
/// them:
///
/// 1. `MrmsHandler::cached_grids` — the live mosaic of the pane's selected
///    product. **Live**: `hover_value_at` reads values out of it and
///    `prepare_job` re-rasterizes from it on every pan, zoom and product
///    change.
/// 2. `MrmsHandler::frame_grids` — one loop frame's staged granule, bounded at
///    `FRAME_STAGING_BYTES` (one mosaic) however many frames the loop holds.
/// 3. `mrms::staging::global()` — the buffer the pool parks between decodes.
///    **Nothing reads it.** It exists to remove an allocation, and its only
///    release is `SourceHandler::release_data`, which fires when no pane draws
///    the layer.
///
/// Floor — delete any one of the three terms from
/// `MrmsHandler::resident_source_bytes`: that step's delta reads 0 and the
/// total lands a mosaic short.
#[test]
fn the_overlay_grid_family_is_three_named_mosaics() {
    let mosaic = squallar_overlays::mrms::CONUS_GRID_BYTES as u64;
    assert_eq!(
        mosaic, 49_000_000,
        "the denominator this whole suite is written in",
    );

    let mut registry = OverlayRegistry::default();
    let pane = PaneRef::bare(0);

    assert_eq!(
        registry.resident_source_bytes(),
        0,
        "premise: a fresh registry holds no decoded source at all. A non-zero \
         here is another handler answering, and every delta below would be \
         measured against it",
    );

    // ── 1. the live cache ────────────────────────────────────────────────
    registry
        .get_handler_mut(&known::MRMS)
        .expect("the shipped registry carries MRMS")
        .apply_fetch_result(Box::new(MrmsFetchResult(Ok(decode_granule()))), &pane);
    let after_live = registry.resident_source_bytes();
    assert_eq!(
        after_live, mosaic,
        "one live granule is one mosaic in the family: the pane's carry and \
         the cache entry are the SAME allocation, so a figure of two here \
         would be `resident_source_bytes` double-counting the carry",
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
        mosaic,
        "a staged loop granule is a SECOND mosaic beside the live one — the \
         frame store is not the live cache and a frame is never drawn from it",
    );

    // ── 3. the pool's parked buffer ──────────────────────────────────────
    // Through the pool's own door, which is what `MrmsGridCache::insert` and
    // `MrmsFrameCache::insert` call when they displace a granule.
    squallar_overlays::mrms::staging::global().recycle(decode_granule());
    assert_eq!(
        squallar_overlays::mrms::staging::global().retained_bytes() as u64,
        mosaic,
        "premise: the shipped slot took the offered buffer",
    );
    let after_park = registry.resident_source_bytes();
    assert_eq!(
        after_park - after_frame,
        mosaic,
        "the parked buffer is a THIRD mosaic, and it is in the census family \
         because it is on the heap — `resident_source_bytes` reads \
         `staging.retained_bytes()` for exactly this reason",
    );

    assert_eq!(
        after_park,
        3 * mosaic,
        "the steady state of one pane looping one MRMS product is three \
         mosaics — 147,000,000 B, against the 155.8 to 219.0 MB the wasm32 \
         page reads for this whole family on the Tier-2 legs. One layer's \
         three stores account for most of a family that four layers publish \
         into",
    );

    // ── What the pressure lever can give back, and what it cannot ────────
    // Nothing reads the parked buffer, so it is the one of the three a
    // memory governor may take without trading anything a pane is drawing.
    assert_eq!(
        squallar_overlays::staging::release_all_retained(),
        mosaic,
        "the crate-wide release must find the parked mosaic and price it — a \
         zero here is a lever that ran and gave nothing back, which reads \
         exactly like one nothing called",
    );
    assert_eq!(
        registry.resident_source_bytes(),
        2 * mosaic,
        "releasing the pools takes the family back to the two mosaics a pane \
         is actually using — one third of it, freed with nothing refetched \
         and nothing redrawn",
    );

    // Non-triviality: the release is not simply zeroing the whole family.
    assert_eq!(
        squallar_overlays::mrms::staging::global().retained_bytes(),
        0,
        "and the pool's own level agrees",
    );
}
