use super::super::App;
use super::super::tests::headless;
use super::selection_tests::show;
use crate::platform_double::TestBridge;
use crate::render_dispatch::CachedRenderOutput;
use rustdar_radar::chunks::{ClosedVolume, PollOutcome, VolumeIndex, VolumeProgress};
use rustdar_radar::types::RadarProduct;
use std::sync::Arc;

fn vol(index: u16) -> VolumeIndex {
    VolumeIndex::new(index).expect("a legal volume index")
}

/// A volume carrying `sweeps` complete cuts, elevation numbers 1..=sweeps.
fn volume(sweeps: u8) -> Arc<nexrad_model::data::Scan> {
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };
    let cut = |number: u8| {
        let radial = Radial::new(
            1_760_000_000_000,
            0,
            0.0,
            1.0,
            RadialStatus::ElevationStart,
            number,
            0.5 * number as f32,
            Some(MomentData::from_fixed_point(
                1,
                0,
                250,
                8,
                2.0,
                66.0,
                vec![0],
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        Sweep::new(number, vec![radial])
    };
    Arc::new(Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        ),
        (1..=sweeps).map(cut).collect(),
    ))
}

/// The progress a volume of `sweeps` cuts reports once every one of them has
/// sealed and the volume has ended.
fn complete(sweeps: u8, whole: bool) -> VolumeProgress {
    VolumeProgress {
        volume: vol(42),
        volume_time: Some(
            chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        ),
        sealed_elevations: (1..=sweeps).collect(),
        sealed_angles: (1..=sweeps).map(|n| 0.5 * n as f32).collect(),
        abandoned: Vec::new(),
        saw_scan_end: true,
        volume_complete: true,
        whole_volume_complete: whole,
        chunks_ingested: 55,
        late_radials_dropped: 0,
    }
}

/// A round that closed a whole `sweeps`-cut volume and rolled to the next,
/// exactly as `ChunkPoller::roll` reports one.
fn closing_round(sweeps: u8) -> PollOutcome {
    closing_round_of(sweeps, true)
}

fn closing_round_of(sweeps: u8, whole: bool) -> PollOutcome {
    PollOutcome {
        closed: Some(ClosedVolume {
            progress: complete(sweeps, whole),
            scan: Some(volume(sweeps)),
            declared_nyquist: Default::default(),
        }),
        rolled_to: Some(vol(43)),
        ..Default::default()
    }
}

/// An app with one live KTLX pane on `product` that has already drawn the previous
/// volume — `last_rendered` set and an image in the cache.
fn app_showing_a_drawn_volume(product: RadarProduct) -> App {
    let mut app = headless(TestBridge::desktop());
    show(&mut app, product, 0.5, &[0.5, 1.0, 1.5]);
    assert!(
        app.chunk_feeds.snapshot("KTLX").is_none(),
        "precondition: the site has no live snapshot to fall back on"
    );
    app.render.pane_render[0].last_rendered = Some((product, 0.5));
    app.render.cache_render(
        "KTLX",
        product,
        rustdar_radar::types::RenderView::PlanView,
        0.5,
        CachedRenderOutput {
            image: Arc::new(egui::ColorImage::default()),
            max_range_km: 100.0,
            hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        },
    );
    app
}

/// **The staleness bug.** A volume that completes on a healthy feed must
/// re-render the panes reading it.
#[test]
fn a_completed_volume_re_renders_every_whole_volume_pane() {
    let mut whole_volume = 0;
    for &product in RadarProduct::all() {
        if !product.reads_whole_volume() {
            continue;
        }
        whole_volume += 1;
        let mut app = app_showing_a_drawn_volume(product);

        app.apply_chunk_outcome("KTLX", &closing_round(5));

        assert!(
            app.render.pane_render[0].last_rendered.is_none(),
            "{product:?}: the volume completed and the pane was not \
                 invalidated, so it keeps showing the previous volume until the \
                 user changes product, changes tilt, presses Refresh, or the feed \
                 dies"
        );
        assert!(
            app.render
                .get_cached_render(
                    "KTLX",
                    product,
                    rustdar_radar::types::RenderView::PlanView,
                    0.5
                )
                .is_none(),
            "{product:?}: the previous volume's image survived the reset, so \
                 the pane re-renders straight back into it"
        );
        assert_eq!(
            app.scan_data
                .get("KTLX")
                .map(|(scan, _)| scan.sweeps().len())
                .unwrap_or(0),
            5,
            "{product:?}: the completed volume never reached the display"
        );
    }
    assert!(
        whole_volume >= 6,
        "the whole-volume set shrank to {whole_volume}; this test is about \
             the products only this branch refreshes"
    );
}

/// The rest of the branch, which is what the site reset exists to serve.
#[test]
fn a_completed_volume_reaches_the_scan_info_and_the_loop_cache() {
    let mut app = app_showing_a_drawn_volume(RadarProduct::EchoTopsInterpolated);
    app.gui.pane_mut(0).unwrap().loop_state.frames.clear();

    app.apply_chunk_outcome("KTLX", &closing_round(5));

    let shown = app
        .gui
        .pane(0)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .expect("the pane must still have scan info");
    let cached = app.loop_mgr.get_cached("KTLX", &shown);
    assert_eq!(
        cached.map(|(scan, _)| scan.sweeps().len()),
        Some(5),
        "the completed volume never reached the loop cache, so an active \
             loop's newest frame stays a volume behind"
    );
}

/// A completed volume **replaces** the site's scan info; it does not merge
/// into it.
#[test]
fn a_completed_volume_replaces_the_scan_info_rather_than_merging_into_it() {
    let product = RadarProduct::EchoTopsInterpolated;
    let mut app = app_showing_a_drawn_volume(product);
    app.gui
        .pane_mut(0)
        .unwrap()
        .scan_info
        .as_mut()
        .unwrap()
        .product_elevations
        .entry(product)
        .or_default()
        .push(9.9);
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::Fetching(true));

    app.apply_chunk_outcome("KTLX", &closing_round(5));

    let angles: Vec<f32> = app
        .gui
        .pane(0)
        .and_then(|p| p.scan_info.as_ref())
        .and_then(|i| i.product_elevations.get(&product).cloned())
        .unwrap_or_default();
    assert!(
        !angles.iter().any(|a| (a - 9.9).abs() < 0.05),
        "a tilt the completed volume does not carry survived, so the scan \
             info was merged rather than replaced and the tilt list only ever \
             grows: {angles:?}"
    );
    assert!(
        !app.gui.fetching(),
        "the volume completed and the spinner stayed up, so a Refresh \
             waiting on it never ends and the archive poll stays wedged behind it"
    );
}

/// Freshness is stamped from the volume that was applied, cut for cut.
#[test]
fn a_completed_volume_stamps_freshness_for_its_own_cuts() {
    let mut app = app_showing_a_drawn_volume(RadarProduct::EchoTopsInterpolated);
    let mut outcome = closing_round(5);
    outcome.sealed_elevations = vec![1];
    outcome.sealed_angles = vec![0.5];

    app.apply_chunk_outcome("KTLX", &outcome);

    for n in 1..=5u8 {
        assert!(
            app.chunk_feeds.freshness("KTLX", 0.5 * n as f32).is_some(),
            "cut {n} of the completed volume was never stamped, so the status \
                 bar has nothing to say about the tilt on screen"
        );
    }
}

/// **A volume that is complete but not whole must not enter the loop cache.**
#[test]
fn a_volume_complete_only_for_its_selection_stays_out_of_the_loop_cache() {
    let mut app = app_showing_a_drawn_volume(RadarProduct::Reflectivity);
    let outcome = closing_round_of(1, false);

    app.apply_chunk_outcome("KTLX", &outcome);

    let shown = app
        .gui
        .pane(0)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .expect("the live display still gets the volume");
    assert!(
        app.loop_mgr.get_cached("KTLX", &shown).is_none(),
        "a volume holding only the cuts one pane's tilt asked for was cached \
             for the loops, where `frame_data` hands it to whatever product the \
             pane is showing later — echo tops would clamp every column to 0.5°"
    );
    assert!(
        app.render.pane_render[0].last_rendered.is_none(),
        "the live display was not refreshed either: those cuts are all it \
             asked for, so skipping the whole branch trades one bug for the other"
    );
}

/// **A whole closed volume becomes the site's merge base**.
#[test]
fn a_whole_closed_volume_becomes_the_merge_base() {
    let mut app = app_showing_a_drawn_volume(RadarProduct::Reflectivity);
    assert!(
        !app.base_scans.contains_key("KTLX"),
        "precondition: no base yet, so the write below is this round's"
    );

    app.apply_chunk_outcome("KTLX", &closing_round_of(5, true));
    let based = app
        .base_scans
        .get("KTLX")
        .map(|(scan, _, _)| scan.sweeps().len());
    assert_eq!(
        based,
        Some(5),
        "the whole closed volume did not become the merge base, so the \
             base ages from the first archive fetch for as long as the feed runs"
    );

    let mut short = app_showing_a_drawn_volume(RadarProduct::Reflectivity);
    short.apply_chunk_outcome("KTLX", &closing_round_of(1, false));
    assert!(
        !short.base_scans.contains_key("KTLX"),
        "a volume that closed short of whole was installed as the merge \
             base; every consumer now stands on a ladder missing cuts"
    );
}

/// The counterweight: a *whole* volume does reach the cache, so the assertion
/// above is about the flag rather than about an append that never happens.
#[test]
fn a_whole_volume_does_reach_the_loop_cache() {
    let mut app = app_showing_a_drawn_volume(RadarProduct::Reflectivity);

    app.apply_chunk_outcome("KTLX", &closing_round_of(5, true));

    let shown = app
        .gui
        .pane(0)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .expect("the pane must still have scan info");
    assert_eq!(
        app.loop_mgr
            .get_cached("KTLX", &shown)
            .map(|(scan, _)| scan.sweeps().len()),
        Some(5),
        "a whole volume was withheld from the loop cache, so every loop frame \
             waits on an archive download the feed already had"
    );
}

/// The Level III refetch, which is the one of the three named consequences no
/// behavioural test here can see.
#[test]
fn the_completed_branch_refetches_level_three_and_owns_the_loop_append() {
    let source = include_str!("../app_chunks.rs");
    let start = source
        .find("fn apply_chunk_outcome(")
        .expect("apply_chunk_outcome is gone");
    let body = &source[start..];
    let split = body
        .find("if let Some((closed, _)) = completed {")
        .expect("the completed branch is gone");
    let (complete, rest) = {
        let after = &body[split..];
        let els = after
            .find("\n        } else {")
            .expect("the two branches are no longer an if/else");
        (&after[..els], &after[els..])
    };

    for call in [
        "self.render.reset_panes_for_site(",
        "self.spawn_level3_fetches(",
        "self.append_scan_to_active_loops(",
    ] {
        assert!(
            complete.contains(call),
            "{call} left the completed-volume branch, so nothing does it when a \
                 volume finishes"
        );
    }
    assert!(
        !rest[..rest
            .find("\n        // Deliberately absent")
            .unwrap_or(rest.len())]
            .contains("self.append_scan_to_active_loops("),
        "the mid-volume branch appends a loop frame, which freezes that frame \
             on however many cuts the volume had at the time"
    );
}

/// The round the coverage pattern arrives on invalidates the site's panes,
/// even though it seals nothing.
#[test]
fn a_learned_coverage_pattern_resets_the_sites_panes() {
    let source = include_str!("../app_chunks.rs");
    let start = source
        .find("fn apply_chunk_outcome(")
        .expect("apply_chunk_outcome is gone");
    let body = &source[start..];
    let guard = body
        .find("if outcome.sealed_elevations.is_empty() {")
        .expect("the seal-less early return is gone");
    let ret = guard
        + body[guard..]
            .find("return;")
            .expect("the early return no longer returns");
    assert!(
        body[guard..ret].contains("if outcome.learned_coverage_pattern {"),
        "a round that only learned the coverage pattern takes the seal-less \
             early return, so the panes drawn while the pattern was the \
             placeholder are never invalidated"
    );
    assert!(
        body[guard..ret].contains("self.render.reset_panes_for_site("),
        "the learned-pattern arm does not drop the site's cached images, which \
             is the only thing that makes the corrected volume reach the screen"
    );
}

/// A volume that ended *without* completing is not applied as one.
#[test]
fn an_incomplete_closed_volume_is_not_applied() {
    let product = RadarProduct::EchoTopsInterpolated;
    let mut app = app_showing_a_drawn_volume(product);
    let mut outcome = closing_round(5);
    let closed = outcome.closed.as_mut().unwrap();
    closed.progress.volume_complete = false;
    closed.progress.whole_volume_complete = false;
    closed.progress.abandoned = vec![rustdar_radar::chunks::AbandonedCut {
        elevation: 6,
        have: 12,
        expected: 720,
    }];
    assert!(
        closed.scan.is_some(),
        "precondition: the refusal must come from the flag, not from an \
             absent scan"
    );

    app.apply_chunk_outcome("KTLX", &outcome);

    assert!(
        !app.scan_data.contains_key("KTLX"),
        "a volume that closed short was installed anyway"
    );
    assert_eq!(
        app.render.pane_render[0].last_rendered,
        Some((product, 0.5)),
        "a volume that closed short ran the site reset, so the pane \
             re-rendered from it"
    );
}
