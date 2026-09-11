//! **One instant, two stores, one fetch and one decode** — the live cache is
//! the second place a named frame may be drawn from, so an instant it already
//! holds is not fetched again.
//!
//! A gridded layer fills its two stores from two paths that never meet:
//! `apply_fetch_result` (the auto-poll) into the live cache, `apply_frame` (a
//! loop frame) into the frame staging store. `fetch_frame` declined a stamp the
//! *staging* store held and never asked the live cache, so an instant both
//! wanted was **fetched twice and decoded twice**, and `prepare_job` could not
//! use the copy it already had.
//!
//! `ModelDataHandler` has read both stores all along
//! (`frame_grids.get(key).or_else(|| cached_grids.get(key))`) and its own note
//! states why that is not a fallback to another instant's picture. This is that
//! arm for MRMS and GMGSI, plus the fetch-side guard the model layer's arm
//! implies.
//!
//! **The byte delta is null and that is expected**: the frame staging budget
//! runs at 97.4 % of `FRAME_STAGING_BYTES` on the six-pane scene, so a slot this
//! frees is claimed by one of the frames the loop wants and cannot hold. What
//! this removes is a GET and a decode, not residency.
//!
//! Its own binary, for the reason `overlay_grid_residency_split.rs` gives: the
//! shipped staging slot is process-global, so a second test in the same binary
//! could not tell its own parked mosaic from one another test left behind.

use squallar_overlays::mrms::{MrmsFetchResult, MrmsGrid, MrmsListing, MrmsProduct, decode};
use squallar_overlays::render::overlay_state::{OverlayRegistry, RasterizeContext};
use squallar_source::handler::{FetchConfig, PaneRef};
use squallar_source::id::known;
use squallar_source::time::{FrameListing, FrameStamp};

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");

fn decode_granule() -> MrmsGrid {
    let grib = decode::gunzip(COMPOSITE_GZ).expect("the committed granule is a gzip member");
    decode::parse_grib2(&grib, MrmsProduct::ReflectivityComposite)
        .expect("the committed granule decodes")
}

/// `tls::client` and not `reqwest::Client::new()`, which panics for want of a
/// crypto provider and only *happens* to work when another test installed one.
fn fetch_config() -> FetchConfig {
    FetchConfig {
        client: squallar_source::tls::client(
            squallar_source::tls::USER_AGENT,
            std::time::Duration::from_secs(1),
        )
        .build()
        .expect("a client with a crypto provider installed"),
        zone_cache_dir: None,
        sources: squallar_source::origins::DataSources::default(),
        viewport: None,
        as_of: chrono::Utc::now().naive_utc(),
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    }
}

fn ctx_at(frame: Option<FrameStamp>, now: chrono::NaiveDateTime) -> RasterizeContext {
    RasterizeContext {
        is_dark: false,
        zoom: 5.0,
        device_scale: 1.0,
        now,
        as_of: now,
        frame,
    }
}

/// A registry holding ONE live MRMS granule and nothing staged, plus the
/// listing that names its instant — the state an auto-poll leaves behind just
/// before a loop's window reaches it.
fn live_only() -> (OverlayRegistry, chrono::NaiveDateTime) {
    let mut registry = OverlayRegistry::default();
    let pane = PaneRef::bare(0);
    squallar_overlays::staging::release_all_retained();

    let grid = decode_granule();
    let valid = grid.valid;

    // The listing first: `fetch_frame` answers `None` for a stamp no listing of
    // its own named, so without this the fetch assertion below would pass on
    // EVERY build for a reason that has nothing to do with the live cache.
    registry.apply_frames(
        &known::MRMS,
        FrameListing {
            range: (valid, valid),
            frames: vec![FrameStamp { valid, run: None }],
            complete: true,
        },
        Box::new(MrmsListing {
            product: MrmsProduct::ReflectivityComposite,
            range: (valid, valid),
            keys: vec![(valid, "a/committed/object.grib2.gz".to_string())],
            complete: true,
        }),
        &pane,
    );

    registry
        .get_handler_mut(&known::MRMS)
        .expect("the shipped registry carries MRMS")
        .apply_fetch_result(Box::new(MrmsFetchResult(Ok(grid))), &pane);

    let split = registry.resident_source_split();
    assert!(
        split.live > 0 && split.staged == 0,
        "premise: one granule LIVE and nothing staged, or neither assertion \
         below is about the live cache: {split:?}",
    );
    (registry, valid)
}

#[test]
fn the_frame_fetch_declines_an_instant_the_live_cache_holds() {
    let (registry, valid) = live_only();
    let pane = PaneRef::bare(0);

    // The listing named it and the staging store does not hold it, so every
    // guard `fetch_frame` had before this one passes. What declines it is the
    // live cache.
    let task = registry.fetch_frame(
        &known::MRMS,
        &fetch_config(),
        &pane,
        &FrameStamp { valid, run: None },
    );
    assert!(
        task.is_none(),
        "the granule for this instant is already decoded and resident in the \
         live cache; fetching it again is a second GET and a second decode of \
         bytes the process is already holding",
    );
}

#[test]
fn a_named_frame_the_live_cache_holds_still_describes_a_job() {
    let (registry, valid) = live_only();
    let pane = PaneRef::bare(0);

    // The other half, and it must land in the same commit: declining the fetch
    // without this arm would leave the frame with no picture at all.
    let job = registry.prepare_job(
        &known::MRMS,
        &ctx_at(Some(FrameStamp { valid, run: None }), valid),
        &pane,
    );
    assert!(
        job.is_some(),
        "a named frame whose granule is in the LIVE cache under exactly that \
         instant must be drawn from it",
    );
}

#[test]
fn a_named_frame_at_another_instant_still_describes_nothing() {
    let (registry, valid) = live_only();
    let pane = PaneRef::bare(0);

    // **The guard that makes the arm above legitimate.** The live cache holds
    // one granule per product, so an UNFILTERED fall-back to it would hand
    // every frame of a loop the same picture — one instant's mosaic presented,
    // unlabelled, as another's. The filter is `valid == stamp.valid`, and this
    // is the case that proves it is really there.
    let elsewhere = valid + chrono::Duration::minutes(4);
    assert_ne!(elsewhere, valid, "premise: a different instant");
    let job = registry.prepare_job(
        &known::MRMS,
        &ctx_at(
            Some(FrameStamp {
                valid: elsewhere,
                run: None,
            }),
            valid,
        ),
        &pane,
    );
    assert!(
        job.is_none(),
        "the live cache holds ONE granule per product and it depicts {valid}, \
         not {elsewhere}; describing a job from it here would paint one \
         instant's mosaic as another's",
    );
}
