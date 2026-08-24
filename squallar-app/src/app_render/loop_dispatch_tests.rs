use super::*;
use crate::test_keys;
use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};
use squallar_device_profile::constants::MAX_LOOP_FRAMES;
use squallar_egui::pane::{LayerTimeState, LoopFrame, LoopPhase};
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_radar::sites::RadarSite;
use squallar_radar::types::RadarProduct;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minute as i64)
}

fn target(site: &str, elevation: f32) -> RenderTarget {
    test_keys::key(
        site,
        &squallar_radar::fields::known::REFLECTIVITY,
        elevation,
    )
}

pub(super) fn scan_with_sweeps(elevations: &[f32]) -> Arc<Scan> {
    let sweeps = elevations
        .iter()
        .enumerate()
        .map(|(i, &elevation)| {
            let radial = Radial::new(
                0,
                0,
                0.0,
                1.0,
                RadialStatus::ElevationStart,
                i as u8 + 1,
                elevation,
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
            Sweep::new(i as u8 + 1, vec![radial])
        })
        .collect();
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
        sweeps,
    ))
}

pub(super) fn volume_with_sweeps(
    elevations: &[f32],
) -> squallar_radar::loop_downloads::CachedVolume {
    (scan_with_sweeps(elevations), Arc::default())
}

fn loop_on(ctx: &egui::Context, site: &'static str, textured: &[usize]) -> LayerTimeState {
    let mut ls = squallar_egui::radar_layer::begin_loop(
        3600,
        &RadarSite {
            name: site,
            network: squallar_radar::sites::RadarNetwork::of_id(site),
            lat: 35.0,
            lon: -97.0,
            heights: None,
        },
        squallar_radar::types::RenderView::PlanView,
    );
    ls.phase = LoopPhase::Rendering;
    ls.frames = (0..3)
        .map(|i| LoopFrame {
            timestamp: ts(i),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    ls.retarget_renders(&squallar_radar::fields::known::REFLECTIVITY, 0.5);
    for &i in textured {
        let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
        ls.frames[i].image = Some(squallar_egui::pane::LoopFrameImage::PlanView(
            squallar_egui::pane::RadarImageData {
                texture: ctx.load_texture("test", image, egui::TextureOptions::NEAREST),
                lat: 35.0,
                lon: -97.0,
                max_range_km: 100.0,
                placed: squallar_radar::types::ImageBounds::from_radar_site(35.0, -97.0, 100.0)
                    .into(),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
                hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
            },
        ));
    }
    ls
}

fn response(
    timestamp: chrono::NaiveDateTime,
    target: RenderTarget,
) -> crate::channels::LoopRenderResponse {
    crate::channels::LoopRenderResponse {
        pane_idx: 0,
        timestamp,
        target,
        snapped: 0.5,
        site_lat: 35.33,
        site_lon: -97.27,
        image: Some(egui::ColorImage::filled([1, 1], egui::Color32::WHITE)),
        max_range_km: 100.0,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        polar: Default::default(),
    }
}

fn dummy_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    ctx.load_texture("test", image, egui::TextureOptions::NEAREST)
}

fn queued(
    target: RenderTarget,
    timestamp: chrono::NaiveDateTime,
    snapped: f32,
) -> LoopRenderRequest {
    LoopRenderRequest {
        pane_idx: 0,
        frame_idx: 0,
        timestamp,
        target,
        snapped,
        site_lat: 35.0,
        site_lon: -97.0,
    }
}

#[test]
fn a_queued_render_for_the_same_target_suppresses_a_duplicate() {
    let q = [queued(target("KTLX", 0.5), ts(0), 0.48)];
    assert!(render_already_queued(
        q.iter(),
        ts(0),
        &target("KTLX", 0.5),
        0.48
    ));
    assert!(render_already_queued(
        q.iter(),
        ts(0),
        &target("KTLX", 0.505),
        0.48
    ));
}

#[test]
fn a_queued_render_for_another_site_suppresses_nothing() {
    let q = [queued(target("KTLX", 0.5), ts(0), 0.48)];
    assert!(
        !render_already_queued(q.iter(), ts(0), &target("KOUN", 0.5), 0.48),
        "a pane on another site must still render its own frame"
    );
}

#[test]
fn a_queued_render_at_another_timestamp_or_sweep_suppresses_nothing() {
    let q = [queued(target("KTLX", 0.5), ts(0), 0.48)];
    assert!(!render_already_queued(
        q.iter(),
        ts(1),
        &target("KTLX", 0.5),
        0.48
    ));
    assert!(!render_already_queued(
        q.iter(),
        ts(0),
        &target("KTLX", 0.5),
        1.5
    ));
    assert!(!render_already_queued(
        [].iter(),
        ts(0),
        &target("KTLX", 0.5),
        0.48
    ));
}

#[test]
fn suppression_and_acceptance_weigh_the_same_sweep() {
    let ctx = egui::Context::default();
    let receiver = loop_on(&ctx, "KTLX", &[]);
    let want = receiver.rendered_for.clone().expect("target adopted");
    let q = [queued(target("KTLX", 0.5), ts(0), 0.48)];

    for own in [0.48, 0.485, 1.4] {
        let suppressed = render_already_queued(q.iter(), ts(0), &want, own);
        let accepted = receiver
            .frame_accepting_broadcast(
                ts(0),
                &want,
                BroadcastSweep {
                    rendered: 0.48,
                    own: Some(own),
                },
            )
            .is_some();
        assert_eq!(
            suppressed, accepted,
            "own sweep {own}: suppressed={suppressed} but accepted={accepted}"
        );
    }

    assert!(render_already_queued(q.iter(), ts(0), &want, 0.48));
}

#[test]
fn a_queued_render_for_another_product_suppresses_nothing() {
    let q = [queued(target("KTLX", 0.5), ts(0), 0.48)];
    let velocity = test_keys::key("KTLX", &squallar_radar::fields::known::VELOCITY, 0.5);
    assert!(!render_already_queued(q.iter(), ts(0), &velocity, 0.48));
}

#[test]
fn a_donor_is_judged_against_the_receiving_panes_target() {
    let ctx = egui::Context::default();
    let ktlx = loop_on(&ctx, "KTLX", &[1]);
    let koun = loop_on(&ctx, "KOUN", &[]);
    let loops = [(0usize, &ktlx), (1usize, &koun)];

    assert_eq!(
        find_donor(loops, 1, ts(1), koun.rendered_for.as_ref().unwrap()),
        None,
        "a KTLX loop must not serve a KOUN loop"
    );
    assert_eq!(
        find_donor(loops, 1, ts(1), ktlx.rendered_for.as_ref().unwrap()),
        Some((0, 1)),
        "precondition: only the target argument distinguishes these"
    );
}

#[test]
fn a_listing_for_the_site_the_loop_left_is_refused() {
    let ctx = egui::Context::default();
    let mut koun = loop_on(&ctx, "KOUN", &[]);
    koun.frames.clear();
    let stale = vec![ts(0)];

    assert!(
        accept_scan_listing(
            test_loop_allocation(),
            &test_budgets(),
            &mut koun,
            "KTLX",
            stale,
            1,
        )
        .is_none(),
        "a KTLX listing is not this KOUN loop's frame list"
    );
    assert!(koun.frames.is_empty(), "and left no frames behind");

    let live = vec![ts(0)];
    let plan = accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut koun,
        "KOUN",
        live,
        1,
    )
    .expect("its own listing");
    assert_eq!(
        plan.site, "KOUN",
        "the plan carries the site it was listed for"
    );
    assert_eq!(plan.frames.len(), 1);
    assert_eq!(koun.frames.len(), 1);
}

#[test]
fn a_listing_for_an_inactive_loop_is_refused() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.phase = LoopPhase::Inactive;

    let scans = vec![ts(0)];
    assert!(
        accept_scan_listing(
            test_loop_allocation(),
            &test_budgets(),
            &mut ls,
            "KTLX",
            scans,
            1,
        )
        .is_none()
    );
}

#[test]
fn an_empty_listing_switches_the_loop_off() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.phase = LoopPhase::FetchingScanList;

    assert!(
        accept_scan_listing(
            test_loop_allocation(),
            &test_budgets(),
            &mut ls,
            "KTLX",
            Vec::new(),
            1,
        )
        .is_none(),
        "there is nothing to download"
    );
    assert!(
        !ls.is_active(),
        "the pane must fall back to its static image, not sit in Rendering"
    );
    assert!(ls.frames.is_empty());
}

#[test]
fn a_loop_no_frame_of_which_can_render_is_switched_off() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    for frame in &mut ls.frames {
        frame.render_failed = true;
    }
    let mgr = LoopDownloadManager::new();

    assert!(
        settle_radar_loop_phase(&mgr, 0, &mut ls, test_loop_allocation().plan_view_frames),
        "the caller has to release this pane's loop state"
    );
    assert!(!ls.is_active());
}

#[test]
fn a_loop_still_waiting_on_its_scans_is_left_alone() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    let mut mgr = LoopDownloadManager::new();
    mgr.mark_in_flight("KTLX", ts(0));

    assert!(!settle_radar_loop_phase(
        &mgr,
        0,
        &mut ls,
        test_loop_allocation().plan_view_frames
    ));
    assert_eq!(ls.phase, LoopPhase::Rendering, "still working");

    let mut mgr = LoopDownloadManager::new();
    mgr.insert_pending(
        0,
        PendingDownloads {
            site: "KTLX".to_string(),
            queue: [ts(1)].into_iter().collect(),
        },
    );
    assert!(!settle_radar_loop_phase(
        &mgr,
        0,
        &mut ls,
        test_loop_allocation().plan_view_frames
    ));
    assert_eq!(ls.phase, LoopPhase::Rendering);
}

#[test]
fn a_loop_with_something_to_show_is_promoted_rather_than_abandoned() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[1]);
    ls.frames[0].render_failed = true;
    ls.frames[2].render_failed = true;
    let mgr = LoopDownloadManager::new();

    assert!(!settle_radar_loop_phase(
        &mgr,
        0,
        &mut ls,
        test_loop_allocation().plan_view_frames
    ));
    assert_eq!(ls.phase, LoopPhase::Ready);
}

// ── The settle mechanism is layer-agnostic (WI-2) ─────────────────────────
//
// A model layer's timeline is a `LayerTimeState` like radar's with the two
// radar-shaped halves absent: no `LoopGeometry` in `anchor`, so nothing can
// read a NEXRAD site out of it, and no `RenderTarget` in `rendered_for`, so
// nothing can read a `RadarProduct` out of it either.

/// `loop_on`'s timeline stripped of exactly those two halves.
///
/// The frames still carry radar's image variant. `LoopFrameImage::Overlay`
/// exists since the draw fork (WI-6), but nothing under test here looks inside
/// an image, so the cheaper fixture stands.
fn model_shaped_loop(ctx: &egui::Context, textured: &[usize]) -> LayerTimeState {
    let mut ls = loop_on(ctx, "KTLX", textured);
    ls.anchor = None;
    ls.rendered_for = None;
    ls
}

/// The acceptance of WI-2: a loop with no radar geometry reaches `Ready`.
///
/// Before this item the mechanism could not get here at all. `loop_batch_settled`
/// asks `loop_product` for a `RadarProduct` and answers "nothing has settled"
/// without one, so a model timeline was pinned in `Rendering` for good — and a
/// loop that never leaves `Rendering` is a loop whose Play button never enables.
#[test]
fn a_loop_with_no_radar_geometry_still_reaches_ready() {
    let ctx = egui::Context::default();
    let mut ls = model_shaped_loop(&ctx, &[1]);

    assert!(ls.anchor.is_none(), "precondition: no radar geometry");
    assert_eq!(
        squallar_egui::radar_layer::site(&ls),
        "",
        "precondition: and reading a site out of it answers the empty string \
         rather than refusing — which is what made the gate below dangerous"
    );

    assert!(
        !settle_loop_phase(0, &mut ls, |_| true, |_| false),
        "a loop with a frame to show is promoted, not switched off"
    );
    assert_eq!(ls.phase, LoopPhase::Ready);
}

/// **The correctness pin of WI-2.** A non-radar loop whose frames are still
/// loading must survive the readiness pass with its timeline intact.
///
/// This is the failure `radar_layer::site` in the generic path produced, and it
/// is worse than a stall: the site read answers `""`, nothing is ever in flight
/// for `""`, so `settle_loop_phase` fell through to `*ls = LayerTimeState::new()`
/// and **destroyed a working loop while its data was on the wire**. The
/// assertion is therefore about the surviving state, not about a call.
#[test]
fn a_model_loop_whose_frames_are_still_arriving_is_not_destroyed() {
    let ctx = egui::Context::default();
    let mut ls = model_shaped_loop(&ctx, &[]);
    let frames_before = ls.frames.len();
    assert!(
        frames_before > 0 && ls.frames.iter().all(|f| f.image.is_none()),
        "precondition: a timeline that has everything still to fetch"
    );

    // The layer says its data is still coming. What is asserted is the state
    // that survives the call, in that order deliberately: the damage this pins
    // is a wiped timeline, so the timeline is what gets read first.
    let switched_off = settle_loop_phase(0, &mut ls, |_| true, |_| true);

    assert_eq!(
        ls.frames.len(),
        frames_before,
        "the loop's frames must survive a pass made while they are still loading"
    );
    assert!(ls.is_active(), "and so must the loop itself");
    assert_eq!(ls.phase, LoopPhase::Rendering, "still working");
    assert!(!switched_off, "so the caller is never told to release it");
}

/// Non-triviality: radar's `still_arriving` closure is answered by the download
/// manager and keyed to the loop's own site, so a closure that ignored its
/// argument could not stand in for it.
#[test]
fn the_radar_arm_reads_its_arrivals_from_the_download_manager() {
    let ctx = egui::Context::default();
    let ktlx = loop_on(&ctx, "KTLX", &[]);
    let koun = loop_on(&ctx, "KOUN", &[]);
    let mut mgr = LoopDownloadManager::new();

    assert!(
        !radar_still_arriving(&mgr, &ktlx),
        "nothing is on the wire yet"
    );
    mgr.mark_in_flight("KTLX", ts(0));
    assert!(
        radar_still_arriving(&mgr, &ktlx),
        "the manager is what says a volume is in flight"
    );
    assert!(
        !radar_still_arriving(&mgr, &koun),
        "and another site's download is not this loop's"
    );
}

#[test]
fn the_frame_list_and_the_frame_plan_describe_the_same_scans() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.phase = LoopPhase::FetchingScanList;
    assert!(
        ls.is_fetching(),
        "precondition: a loop awaiting its listing"
    );

    let scans: Vec<_> = (0..(MAX_LOOP_FRAMES as u32 + 40)).map(ts).collect();

    let plan = accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut ls,
        "KTLX",
        scans,
        1,
    )
    .expect("accepted");

    assert_eq!(plan.frames.len(), MAX_LOOP_FRAMES, "capped");
    assert_eq!(
        ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
        plan.frames.clone(),
        "the sampled set is the frame list, frame for frame"
    );
    assert_eq!(
        ls.current_frame(),
        ls.frames.len() - 1,
        "playback starts at the newest"
    );
    assert_eq!(ls.phase, LoopPhase::Rendering);
    assert!(
        !ls.is_fetching(),
        "and the loop has stopped reading as fetching"
    );
}

#[test]
fn a_long_listing_is_sampled_evenly_across_its_whole_span() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    let total = MAX_LOOP_FRAMES * 3 + 7;
    let scans: Vec<_> = (0..total as u32).map(ts).collect();

    accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut ls,
        "KTLX",
        scans,
        1,
    )
    .expect("accepted");

    let picked: Vec<i64> = ls
        .frames
        .iter()
        .map(|f| (f.timestamp - ts(0)).num_minutes())
        .collect();

    assert_eq!(picked.len(), MAX_LOOP_FRAMES);
    assert_eq!(picked[0], 0, "the oldest scan in the window is kept");
    assert_eq!(
        picked[MAX_LOOP_FRAMES - 1],
        total as i64 - 1,
        "and the newest, or the loop stops short of the scan the pane is showing"
    );

    let strides: Vec<i64> = picked.windows(2).map(|w| w[1] - w[0]).collect();
    let min = *strides.iter().min().expect("more than one frame");
    let max = *strides.iter().max().unwrap();
    assert!(min > 0, "strictly increasing, so no scan is sampled twice");
    assert!(
        max - min <= 1,
        "strides ran {min}..={max}; the sample must be evenly spaced, or the \
             loop covers only part of its own lookback window"
    );
}

#[test]
fn a_listing_one_scan_over_the_cap_is_recorded_as_sampled() {
    let ctx = egui::Context::default();
    let cap = test_budgets().loop_frames_held;
    assert_eq!(
        cap, MAX_LOOP_FRAMES,
        "the resolver and the constant have parted company; this test follows \
         the resolver, and `ui_timeline`'s literals need re-reading against it",
    );
    for (listed, expected) in [(cap, false), (cap + 1, true)] {
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        let scans: Vec<_> = (0..listed as u32).map(ts).collect();

        accept_scan_listing(
            test_loop_allocation(),
            &test_budgets(),
            &mut ls,
            "KTLX",
            scans,
            1,
        )
        .expect("accepted");

        assert_eq!(
            ls.sampled,
            Some(expected),
            "a listing of {listed} against a cap of {cap} kept {} frames and \
             recorded {:?}",
            ls.frames.len(),
            ls.sampled,
        );
    }
}

#[test]
fn a_loop_that_has_taken_no_listing_records_no_fidelity() {
    let ctx = egui::Context::default();
    assert_eq!(loop_on(&ctx, "KTLX", &[]).sampled, None);
}

#[test]
fn a_rendered_frame_is_placed_where_the_render_actually_drew_it() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.frames[1].render_in_flight = true;
    let mut rr = response(ts(1), ls.rendered_for.clone().expect("target adopted"));

    assert_ne!(
        rr.site_lat,
        squallar_egui::radar_layer::coords(&ls).0,
        "precondition: the two sources differ"
    );
    assert_ne!(rr.site_lon, squallar_egui::radar_layer::coords(&ls).1);

    let texture = accept_render_result(&mut ls, &mut rr, None, |_| dummy_texture(&ctx))
        .expect("the loop is awaiting this result");

    let image = ls.frames[1]
        .image
        .as_ref()
        .and_then(squallar_egui::pane::LoopFrameImage::plan_view)
        .expect("the frame was filled with a plan view");
    assert_eq!(
        image.lat, rr.site_lat,
        "the latitude the image was projected around"
    );
    assert_eq!(image.lon, rr.site_lon);
    assert_eq!(image.max_range_km, rr.max_range_km);
    assert!(
        !ls.frames[1].render_in_flight,
        "and the frame is no longer in flight"
    );

    let broadcast = rendered_image(&rr, &texture, None);
    assert_eq!((broadcast.lat, broadcast.lon), (image.lat, image.lon));
}

#[test]
fn a_refused_result_is_never_uploaded() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.frames[1].render_in_flight = true;
    let mut stale = response(ts(1), target("KTLX", 2.4));

    let mut uploads = 0;
    let placed = accept_render_result(&mut ls, &mut stale, None, |_| {
        uploads += 1;
        dummy_texture(&ctx)
    });

    assert!(
        placed.is_none(),
        "a result for another elevation is not this loop's"
    );
    assert_eq!(uploads, 0, "and nothing was uploaded for it");
    assert!(ls.frames[1].image.is_none());
    assert!(
        stale.image.is_some(),
        "and its pixels were not taken off the response"
    );
}

#[test]
fn a_failed_render_retires_its_frame_without_a_texture() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.frames[1].render_in_flight = true;
    let mut failed = crate::channels::LoopRenderResponse {
        image: None,
        ..response(ts(1), ls.rendered_for.clone().expect("target adopted"))
    };

    let mut uploads = 0;
    let placed = accept_render_result(&mut ls, &mut failed, None, |_| {
        uploads += 1;
        dummy_texture(&ctx)
    });

    assert!(placed.is_none());
    assert_eq!(uploads, 0, "a failed render uploads nothing");
    assert!(ls.frames[1].render_failed, "the frame is retired");
    assert!(!ls.frames[1].render_in_flight, "and released");
    assert!(ls.frames[1].image.is_none());
}

#[test]
fn a_download_is_cached_under_the_site_it_came_from() {
    let mut mgr = LoopDownloadManager::new();
    let volume = volume_with_sweeps(&[0.5]);
    mgr.mark_in_flight("KTLX", ts(0));

    apply_completed_download(
        &mut mgr,
        crate::channels::LoopScanDownloadResponse {
            site: "KTLX".to_string(),
            timestamp: ts(0),
            scan: Some(volume.clone()),
        },
    );

    assert!(Arc::ptr_eq(
        &mgr.get_cached("KTLX", &ts(0)).expect("cached").0,
        &volume.0
    ));
    assert!(mgr.get_cached("KOUN", &ts(0)).is_none());
    assert!(!mgr.is_in_flight("KTLX", &ts(0)), "and its mark is cleared");
}

#[test]
fn a_failed_download_clears_its_mark_and_caches_nothing() {
    let mut mgr = LoopDownloadManager::new();
    mgr.mark_in_flight("KTLX", ts(0));

    apply_completed_download(
        &mut mgr,
        crate::channels::LoopScanDownloadResponse {
            site: "KTLX".to_string(),
            timestamp: ts(0),
            scan: None,
        },
    );

    assert!(!mgr.is_in_flight("KTLX", &ts(0)));
    assert!(!mgr.is_cached("KTLX", &ts(0)));
}

#[test]
fn a_frames_data_is_looked_up_under_its_targets_site() {
    let mut mgr = LoopDownloadManager::new();
    let ktlx = volume_with_sweeps(&[0.5]);
    mgr.cache_scan("KTLX", ts(0), ktlx.clone());

    let found = frame_data(&mgr, &target("KTLX", 0.5), ts(0)).expect("KTLX's own scan");
    match found {
        LoopFrameData::Volume(scan, declared) => {
            assert!(Arc::ptr_eq(&scan, &ktlx.0));
            assert!(
                Arc::ptr_eq(&declared, &ktlx.1),
                "the frame's declarations must be its own volume's, not another \
                 volume's or a fresh empty table",
            );
        }
        LoopFrameData::Products(_) => panic!("reflectivity is a Level II product"),
    }
    assert!(
        frame_data(&mgr, &target("KOUN", 0.5), ts(0)).is_none(),
        "a KOUN loop must not render KTLX's scan"
    );
}

#[test]
fn the_receivers_sweep_comes_from_the_receivers_own_scan() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    mgr.cache_scan("KTLX", ts(0), volume_with_sweeps(&[0.5, 1.5]));
    mgr.cache_scan("KOUN", ts(0), volume_with_sweeps(&[1.4]));

    let ktlx = loop_on(&ctx, "KTLX", &[]);
    let koun = loop_on(&ctx, "KOUN", &[]);

    assert_eq!(
        own_sweep(
            &mgr,
            &ktlx,
            ts(0),
            squallar_radar::fields::known::REFLECTIVITY,
            0.5
        ),
        Some(0.5),
        "KTLX's scan carries the selected sweep"
    );
    assert_eq!(
        own_sweep(
            &mgr,
            &koun,
            ts(0),
            squallar_radar::fields::known::REFLECTIVITY,
            0.5
        ),
        Some(1.4),
        "KOUN's own scan snaps the same selection somewhere else"
    );
}

#[test]
fn a_broadcast_sweep_pairs_the_senders_image_with_the_receivers_own_scan() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    mgr.cache_scan("KOUN", ts(0), volume_with_sweeps(&[0.5, 1.4]));
    mgr.cache_scan("KTLX", ts(0), volume_with_sweeps(&[1.4]));
    let koun = loop_on(&ctx, "KOUN", &[]);

    let rr = crate::channels::LoopRenderResponse {
        snapped: 1.4,
        ..response(ts(0), target("KOUN", 0.5))
    };

    let sweep = broadcast_sweep(&mgr, &koun, &rr);

    assert_eq!(
        sweep.rendered, 1.4,
        "the tilt the image depicts — not the 0.5 selection"
    );
    assert_eq!(
        sweep.own,
        Some(0.5),
        "what this loop's own scan resolves that selection to"
    );
    assert!(!sweep.agrees(), "so the image must not be handed over");

    let ktlx = loop_on(&ctx, "KTLX", &[]);
    let sweep = broadcast_sweep(&mgr, &ktlx, &rr);
    assert_eq!(sweep.own, Some(1.4));
    assert!(sweep.agrees(), "and this one takes it");
}

#[test]
fn a_receiver_with_nothing_to_compare_reports_no_sweep() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    let ktlx = loop_on(&ctx, "KTLX", &[]);

    assert_eq!(
        own_sweep(
            &mgr,
            &ktlx,
            ts(0),
            squallar_radar::fields::known::REFLECTIVITY,
            0.5
        ),
        None,
        "nothing downloaded for this frame yet"
    );

    mgr.cache_scan("KTLX", ts(0), volume_with_sweeps(&[0.5]));
    assert_eq!(
        own_sweep(
            &mgr,
            &ktlx,
            ts(0),
            squallar_radar::fields::known::VELOCITY,
            0.5
        ),
        None,
        "the scan carries no sweep for this product"
    );
}

#[test]
fn readiness_counts_only_this_loops_own_downloads() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    let koun = loop_on(&ctx, "KOUN", &[]);
    for i in 0..3 {
        mgr.cache_scan("KTLX", ts(i), volume_with_sweeps(&[0.5]));
    }

    assert!(
        loop_batch_settled(&mgr, &koun, test_loop_allocation().plan_view_frames),
        "precondition: with no scan of its own, a blank frame is not waiting on a render"
    );

    for i in 0..3 {
        mgr.cache_scan("KOUN", ts(i), volume_with_sweeps(&[0.5]));
    }
    assert!(
        !loop_batch_settled(&mgr, &koun, test_loop_allocation().plan_view_frames),
        "downloaded but unrendered frames must hold the loop out of Ready"
    );
}

#[test]
fn a_donor_on_the_same_target_is_found_and_never_the_receiver_itself() {
    let ctx = egui::Context::default();
    let a = loop_on(&ctx, "KTLX", &[2]);
    let b = loop_on(&ctx, "KTLX", &[]);
    let loops = [(0usize, &a), (1usize, &b)];
    let want = b.rendered_for.as_ref().unwrap();

    assert_eq!(find_donor(loops, 1, ts(2), want), Some((0, 2)));
    assert_eq!(find_donor(loops, 0, ts(2), want), None);
    assert_eq!(find_donor(loops, 1, ts(0), want), None);
}

#[test]
fn the_renderer_is_given_the_snapped_sweep_not_the_selection() {
    let req = queued(target("KTLX", 0.5), ts(0), 1.4);
    let params = req.render_params();

    assert_eq!(params.elevation, 1.4, "the sweep the scan carries");
    assert_ne!(params.elevation, req.target.elevation);
    assert_eq!(params.product, RadarProduct::Reflectivity);
    assert_eq!(params.lat, 35.0);
    assert_eq!(params.lon, -97.0);
}

#[test]
fn a_loops_render_set_is_its_span_budget_at_its_own_sites_cadence() {
    let ctx = egui::Context::default();
    let budgets = test_budgets();
    let allocation = test_loop_allocation();
    let span = budgets.loop_span_secs;

    for (radar, cadence) in [
        ("TDWR VCP 80/90", 360u32),
        ("WSR-88D precip", 259),
        ("WSR-88D clear air", 517),
    ] {
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        ls.cadence_secs = Some(cadence);
        let frames = loop_render_budget(allocation, &ls, &budgets);
        assert!(
            frames >= squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE,
            "{radar}: {frames} frames is not a loop"
        );
        if frames < budgets.loop_render_budget {
            let covered = (frames - 1) * cadence as usize;
            assert!(
                covered <= span && covered + cadence as usize > span,
                "{radar}: {frames} frames span {covered} s of a {span} s \
                 budget, which is either over the cap or short of it by a \
                 whole volume"
            );
        }
    }
}

#[test]
fn a_loop_that_has_not_learned_a_cadence_keeps_the_whole_budget() {
    let ctx = egui::Context::default();
    let budgets = test_budgets();
    let ls = loop_on(&ctx, "KTLX", &[]);
    assert_eq!(
        ls.cadence_secs, None,
        "precondition: a freshly built loop knows nothing about its site's cadence"
    );
    assert_eq!(
        loop_render_budget(test_loop_allocation(), &ls, &budgets),
        test_loop_allocation().frames_for(ls.view),
        "a loop with no cadence is held only by the pool's share"
    );
}

#[test]
fn a_listing_teaches_the_cadence_before_the_frame_count_is_spent() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.phase = LoopPhase::FetchingScanList;
    let scans: Vec<_> = (0..13u32).filter(|i| *i != 4).map(|i| ts(i * 6)).collect();

    accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut ls,
        "KTLX",
        scans,
        1,
    )
    .expect("accepted");

    assert_eq!(
        ls.cadence_secs,
        Some(360),
        "the median rides over the missing scan rather than being pulled by it"
    );
    let budgets = test_budgets();
    assert_eq!(
        budgets.frames_for_span(ls.cadence_secs),
        (1 + budgets.loop_span_secs / 360).min(budgets.loop_render_budget),
        "and the count spends that figure, not the arm's ceiling"
    );
}

#[test]
fn the_caption_fixtures_name_caps_this_workspace_ships() {
    // The `held` values `ui_timeline::tests`'s decimation fixture is built on,
    // in `a_loop_that_dropped_a_third_of_the_scans_never_claims_every_scan`.
    let caps = [14usize, 60];

    let shipped: Vec<usize> = squallar_device_profile::budget::BudgetLimits::SHIPPED
        .iter()
        .map(|limits| limits.loop_frames_held.floor)
        .collect();
    for cap in caps {
        assert!(
            shipped.contains(&cap),
            "`ui_timeline`'s decimation fixture is built on a {cap}-frame raster \
             cap, and the arms this workspace ships are {shipped:?}. The fixture \
             is a claim about a measured defect on a real target, so a cap \
             belonging to none of them makes it a claim about nothing — \
             re-derive it against the arm that moved.",
        );
    }
}

fn scan_with_echo() -> Arc<Scan> {
    let radials = (0..360)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32,
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                Some(MomentData::from_fixed_point(
                    300,
                    0,
                    250,
                    8,
                    2.0,
                    66.0,
                    (0..300)
                        .map(|g| ((i * 5 + g * 3) % 200 + 20) as u8)
                        .collect(),
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
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
        vec![Sweep::new(1, radials)],
    ))
}

#[test]
fn hovering_a_looping_pane_reads_a_value_out_of_the_frames_own_volume() {
    let (lat, lon) = (35.3333, -97.2778);
    let scan = scan_with_echo();

    let mut polar = squallar_radar::render::render_radar_to_image(
        &scan,
        0.5,
        RadarProduct::Reflectivity,
        lat,
        lon,
    )
    .expect("the fixture carries reflectivity at 0.5")
    .polar;
    let resident = polar.clone();
    polar.strip_values();

    let mut mgr = LoopDownloadManager::new();
    mgr.cache_scan("KTLX", ts(0), (Arc::clone(&scan), Arc::default()));

    let mut rr = response(
        ts(0),
        test_keys::key("KTLX", &squallar_radar::fields::known::REFLECTIVITY, 0.5),
    );
    rr.site_lat = lat;
    rr.site_lon = lon;
    rr.polar = polar;

    let gates = frame_gates(&mgr, &rr);
    assert!(
        gates.is_some(),
        "the frame's volume is in the loop's own cache",
    );

    let ctx = egui::Context::default();
    let texture = dummy_texture(&ctx);
    let img = rendered_image(&rr, &texture, gates);

    let mut read = 0u32;
    let mut az = 0.5f64;
    while az < 360.0 {
        let mut km = 1.0f64;
        while km < 70.0 {
            let looping = img.hover.read(az, km);
            assert_eq!(
                looping,
                resident.geometry().pick(az, km).map_or(
                    squallar_radar::hover::Reading::Unpainted,
                    |at| resident.at(at).map_or(
                        squallar_radar::hover::Reading::Unpainted,
                        squallar_radar::hover::Reading::Value,
                    )
                ),
                "({az}°, {km} km)",
            );
            if matches!(looping, squallar_radar::hover::Reading::Value(_)) {
                read += 1;
            }
            km *= 1.3;
        }
        az += 7.0;
    }
    assert!(
        read > 300,
        "only {read} points on the loop frame had a value"
    );

    let orphan = rendered_image(&rr, &texture, None);
    assert_eq!(
        orphan.hover.read(90.0, 20.0),
        squallar_radar::hover::Reading::NotResident,
    );
}

// ── The readiness walk itself (WI-2) ──────────────────────────────────────

/// **The claim WI-2 exists to make**, asserted through the real walk rather
/// than through a direct `settle_loop_phase` call: `update_loop_readiness`
/// settles *every* animating layer on a pane, not radar's slot by name.
///
/// Both halves matter and neither may be dropped. A walk narrowed to
/// `known::RADAR` leaves the model layer stuck in `Rendering` forever — which
/// is the hard-wire this item removes — and a walk narrowed to everything
/// *but* radar is the same defect mirrored, so radar's own slot is asserted on
/// the same pane in the same pass.
#[test]
fn the_readiness_walk_settles_every_animating_layer_not_only_radar() {
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());

    let pane = app.gui.pane_mut(0).expect("a pane exists");
    *pane.time_state_mut(&squallar_source::id::known::RADAR) = loop_on(&ctx, "KTLX", &[1]);
    *pane.time_state_mut(&squallar_source::id::known::MODEL_DATA) = model_shaped_loop(&ctx, &[1]);

    let pane = app.gui.pane(0).unwrap();
    assert_eq!(
        pane.time_state(&squallar_source::id::known::MODEL_DATA)
            .phase,
        LoopPhase::Rendering,
        "precondition: the model layer is animating and has not settled"
    );
    assert_eq!(
        pane.animating_layers().count(),
        2,
        "precondition: the pane really is running two loops, so the walk has \
         something to choose wrongly between"
    );

    app.update_loop_readiness();

    let pane = app.gui.pane(0).unwrap();
    assert_eq!(
        pane.time_state(&squallar_source::id::known::MODEL_DATA)
            .phase,
        LoopPhase::Ready,
        "the model layer has a frame to show, so the walk must settle it — a \
         loop left in Rendering is a loop whose Play button never enables"
    );
    assert!(
        pane.time_state(&squallar_source::id::known::RADAR)
            .is_render_ready(),
        "and radar's own slot is still settled by the same pass (it goes on to \
         Playing, because the transport addresses it and playback then starts)"
    );
}
