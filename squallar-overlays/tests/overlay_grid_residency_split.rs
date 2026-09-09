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

/// **Three holders, and only two of them are a mosaic.**
///
/// The three, in the order a playing loop fills them:
///
/// 1. `MrmsHandler::cached_grids` — the live mosaic of the pane's selected
///    product, **tiled**. **Live**: `hover_value_at` reads values out of it and
///    `prepare_job` re-rasterizes from it on every pan, zoom and product
///    change.
/// 2. `MrmsHandler::frame_grids` — one loop frame's staged granule, tiled the
///    same way, one at a time however many frames the loop holds.
/// 3. `mrms::staging::global()` — the **band buffer** the pool parks between
///    decodes: 16 rows of the mosaic, 224,000 B. **Nothing else reads it**, and
///    its only release is `SourceHandler::release_data`, which fires when no
///    pane draws the layer.
///
/// So the family is `2 x granule + 1 band`, and the term that moved twice is
/// the third. It was a whole 49,000,000 B **plane** — the decode built one and
/// the tiler read it once — which the tiled store left untouched and which
/// therefore became **81 % of what one looping pane held**. It also charged the
/// FIRST granule more than the flat store ever had: the plane came back to the
/// slot at the first decode rather than at the first eviction, so one live
/// granule read 5,554,748 + 49,000,000 = **54,554,748 B against 49,000,000
/// before**, a +5,554,748 B regression that only flipped negative from the
/// second granule on. `decode::tile_png_codes` builds the store out of the PNG
/// row walk instead and no plane exists, which closes both: the first granule
/// is **5,778,748 B**, under the flat store's 49,000,000 from the first
/// granule rather than from the second.
///
/// Floor — delete any one of the three terms from
/// `MrmsHandler::resident_source_bytes`: that step's delta reads 0 and the
/// total lands short by that term.
#[test]
fn the_overlay_grid_family_is_two_granules_and_a_band() {
    let band = squallar_overlays::mrms::CONUS_BAND_BYTES as u64;
    assert_eq!(
        band, 224_000,
        "the parked decode buffer: 16 rows, which is all a tiler that decides \
         a tile from 16 rows ever has to hold",
    );
    /// What the same three holders cost while the decode still built a plane
    /// for the tiler to read. Held here so the win is a *difference* this
    /// suite states rather than one a reader has to compute.
    const PLANE: u64 = 49_000_000;
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
        band,
        "the decode hands its band buffer back the instant the last row is \
         tiled, so the pool is warm from the FIRST granule rather than from \
         the first eviction",
    );
    assert_eq!(
        after_live,
        GRANULE + band,
        "one live granule is one TILED mosaic plus the 16-row buffer it was \
         streamed through: the pane's carry and the cache entry are the same \
         allocation, so a figure of two granules here would be \
         `resident_source_bytes` double-counting the carry",
    );
    // **The first-granule regression, closed.** While the tiler read a plane
    // this line was 54,554,748 — above the 49,000,000 the flat store held, and
    // only below it from the second granule on.
    assert_eq!(after_live, 5_778_748);
    assert!(
        after_live < PLANE,
        "one live granule must be under the flat store from the FIRST granule, \
         not from the second: {after_live} against {PLANE}",
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
        band,
        "and it is NOT a second band buffer: the slot held one and the second \
         decode was handed it back",
    );

    assert_eq!(
        after_frame,
        2 * GRANULE + band,
        "the steady state of one pane looping one MRMS product is two tiled \
         granules and one 16-row band — 11,333,496 B, against 60,109,496 with \
         the plane, 147,000,000 with the flat store, and the 155.8 to 219.0 MB \
         the wasm32 page reads for this whole family on the Tier-2 legs",
    );
    assert_eq!(after_frame, 11_333_496);

    // ── What the pressure lever can give back, and what it cannot ────────
    // Nothing reads the parked band, so it is still the one of the three a
    // memory governor may take without trading anything a pane is drawing.
    // **What it is worth has collapsed**, and that is the point: it was 81 % of
    // this family and it is 1.98 % of it. The lever is no longer where the
    // bytes are, because the bytes are no longer there to take.
    assert_eq!(
        squallar_overlays::staging::release_all_retained(),
        band,
        "the crate-wide release must find the parked band and price it — a \
         zero here is a lever that ran and gave nothing back, which reads \
         exactly like one nothing called",
    );
    assert_eq!(
        registry.resident_source_bytes(),
        2 * GRANULE,
        "releasing the pools takes the family to the two granules a pane is \
         actually using: 11,109,496 B, and the whole of what a governor can \
         still reclaim here is 224,000 B",
    );

    // Non-triviality: the release is not simply zeroing the whole family.
    assert_eq!(
        squallar_overlays::mrms::staging::global().retained_bytes(),
        0,
        "and the pool's own level agrees",
    );
}
