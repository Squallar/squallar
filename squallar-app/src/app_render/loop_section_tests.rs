//! The cross-section loop: what gets dispatched, what gets placed, and what the
//! frame thread is allowed to spend doing it.

use super::*;
use crate::app::tests::headless;
use crate::platform_double::TestBridge;
use crate::test_keys;
use squallar_egui::pane::{
    LoopFrame, LoopFrameImage, LoopPhase, PaneKind, SectionLine, SectionLoopKey,
};
use squallar_geo::GeoPoint;
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_radar::sites::RadarSite;
use squallar_radar::types::RenderView;
use squallar_source::id::known;

const SITE: &str = "KTLX";
/// The field every test here shares, named the way a pane and a render
/// key name it.
const PRODUCT_ID: squallar_source::product::FieldId = squallar_radar::fields::known::REFLECTIVITY;
const TILT: f32 = 0.5;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 10)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

fn site() -> RadarSite {
    // The fixture network is otherwise installed only as a side effect of
    // `app::tests::headless`, so a test in here that reads the table before it
    // builds an App reads whatever an earlier test left behind: green in the
    // package run, red when the suite is filtered alone.
    crate::test_sites::install();
    squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone()
}

pub(super) fn line() -> SectionLine {
    SectionLine::new(
        GeoPoint {
            lat: 35.0,
            lon: -98.0,
        },
        GeoPoint {
            lat: 36.0,
            lon: -97.0,
        },
    )
    .expect("two distinct points on Earth")
}

pub(super) fn key() -> SectionLoopKey {
    SectionLoopKey::new(line(), None, squallar_radar::srv::SrvFallback::default())
}

/// This suite's plan-view half of a section identity: [`test_keys::key`] at the site,
/// product and tilt every test here shares.
fn target() -> RenderTarget {
    test_keys::key(SITE, &PRODUCT_ID, TILT)
}

/// An app with one aimed cross-section pane running a section loop over
/// `minutes`, and no volumes cached for any of them.
fn app_with_section_loop(minutes: &[u32]) -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    app.render.ensure_pane_count(1);
    app.loop_mgr = LoopDownloadManager::new();
    let pane = app.gui.pane_mut(0).expect("pane 0 exists");
    pane.set_site(SITE.to_string());
    pane.set_selected_product(PRODUCT_ID);
    pane.set_selected_elevation(TILT);
    pane.set_kind(PaneKind::CrossSection);
    pane.cross_section_mut().expect("a section pane").line = Some(line());

    let mut ls = squallar_egui::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = minutes
        .iter()
        .map(|&m| LoopFrame {
            timestamp: ts(m),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    ls.retarget_renders_for(&PRODUCT_ID, TILT, Some(key()));
    *pane.time_state_mut(&known::RADAR) = ls;
    app
}

/// A one-rung reflectivity volume **with an elevation cut table**.
fn volume() -> std::sync::Arc<nexrad_model::data::Scan> {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let radial = Radial::new(
        0,
        0,
        0.0,
        1.0,
        RadialStatus::ElevationStart,
        1,
        TILT,
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
    let cut = ElevationCut::new(
        TILT as f64,
        ChannelConfiguration::Unknown,
        WaveformType::Unknown,
        0.0,
        false,
        false,
        false,
        false,
        0,
        0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    );
    std::sync::Arc::new(Scan::new(
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
            vec![cut],
        ),
        vec![Sweep::new(1, vec![radial])],
    ))
}

/// A section frame whose volume has not downloaded is left alone: not cut, and
/// above all not retired.
#[test]
fn a_frame_whose_volume_has_not_arrived_is_neither_cut_nor_retired() {
    let mut app = app_with_section_loop(&[0, 1, 2]);

    app.dispatch_loop_renders();

    let ls = &app.gui.pane(0).unwrap().time_state(&known::RADAR);
    assert!(
        ls.frames.iter().all(|f| !f.render_failed),
        "a section frame was retired while its volume was still downloading"
    );
    assert!(
        ls.frames.iter().all(|f| !f.render_in_flight),
        "a cut was dispatched for a frame with no volume to cut"
    );
}

/// A volume that carries nothing to cut retires the frame, so readiness stops
/// waiting on it and the dispatcher stops retrying it.
#[test]
fn a_volume_with_no_ladder_retires_the_frame() {
    let mut app = app_with_section_loop(&[0]);
    app.loop_mgr.cache_scan(
        SITE,
        ts(0),
        (
            std::sync::Arc::new(crate::app::tests::empty_scan()),
            Default::default(),
        ),
    );

    app.dispatch_loop_renders();

    assert!(
        app.gui.pane(0).unwrap().time_state(&known::RADAR).frames[0].render_failed,
        "a frame whose volume carries nothing to cut was left waiting, so the \
         loop never settles and sits in Rendering for the session"
    );
}

/// **The frame-thread cap.** However many frames are ready to cut, one dispatch
/// pass starts at most [`MAX_LOOP_SECTION_CUTS_PER_FRAME`] of them.
#[test]
fn one_dispatch_pass_starts_at_most_the_capped_number_of_cuts() {
    let mut app = app_with_section_loop(&[0, 1, 2, 3, 4]);
    for m in 0..5 {
        app.loop_mgr
            .cache_scan(SITE, ts(m), (volume(), Default::default()));
    }
    const { assert!(MAX_LOOP_SECTION_CUTS_PER_FRAME < 5) };

    app.dispatch_loop_renders();

    let in_flight = app
        .gui
        .pane(0)
        .unwrap()
        .time_state(&known::RADAR)
        .frames
        .iter()
        .filter(|f| f.render_in_flight)
        .count();
    assert_eq!(
        in_flight, MAX_LOOP_SECTION_CUTS_PER_FRAME,
        "a dispatch pass started {in_flight} cuts against a cap of \
         {MAX_LOOP_SECTION_CUTS_PER_FRAME}, so the whole-volume extraction each \
         one costs lands on one frame instead of being spread over several"
    );
}

/// And the loop still makes progress: successive passes pick up the frames the
/// cap deferred rather than starting the same one again.
#[test]
fn successive_dispatch_passes_work_through_the_render_set() {
    let mut app = app_with_section_loop(&[0, 1, 2]);
    for m in 0..3 {
        app.loop_mgr
            .cache_scan(SITE, ts(m), (volume(), Default::default()));
    }

    let mut started = std::collections::HashSet::new();
    for _ in 0..3 {
        app.dispatch_loop_renders();
        let ls = &app.gui.pane(0).unwrap().time_state(&known::RADAR);
        for (idx, frame) in ls.frames.iter().enumerate() {
            if frame.render_in_flight {
                started.insert(idx);
            }
        }
        for frame in &mut app
            .gui
            .pane_mut(0)
            .unwrap()
            .time_state_mut(&known::RADAR)
            .frames
        {
            if frame.render_in_flight {
                frame.render_in_flight = false;
                frame.render_failed = true;
            }
        }
    }
    assert_eq!(
        started.len(),
        3,
        "the cap stalled the loop instead of pacing it: after three passes \
         only {} of three frames had been started",
        started.len()
    );
}

/// A finished cut is placed with its own axes and its own ladder, and the frame
/// stops being in flight.
#[test]
fn a_finished_cut_is_placed_with_its_own_axes_and_ladder() {
    let ctx = egui::Context::default();
    let mut ls = squallar_egui::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = vec![LoopFrame {
        timestamp: ts(0),
        image: None,
        render_in_flight: true,
        render_failed: false,
    }];
    ls.retarget_renders_for(&PRODUCT_ID, TILT, Some(key()));
    ls.frames[0].render_in_flight = true;

    let mut sr = section_response(&ctx, 4242);
    let placed = accept_section_result(&mut ls, &mut sr, |image| {
        ctx.load_texture("cut", image, egui::TextureOptions::NEAREST)
    })
    .expect("the loop is awaiting this cut");

    assert_eq!(placed.ladder, 4242);
    assert_eq!(placed.axes.tilt_count, 1);
    assert_eq!(placed.tilt_elevations_deg, vec![0.5]);
    assert!(!ls.frames[0].render_in_flight);
    let stored = ls.frames[0]
        .image
        .as_ref()
        .and_then(LoopFrameImage::section)
        .expect("the frame holds a section");
    assert_eq!(stored.ladder, 4242);
    assert_eq!(stored.axes, placed.axes);
}

/// A cut the loop has been retargeted away from is refused, and refusing it costs
/// nothing.
#[test]
fn a_cut_for_a_line_the_loop_has_left_is_refused_without_uploading() {
    let ctx = egui::Context::default();
    let mut ls = squallar_egui::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = vec![LoopFrame {
        timestamp: ts(0),
        image: None,
        render_in_flight: true,
        render_failed: false,
    }];
    let elsewhere = SectionLine::new(
        GeoPoint {
            lat: 30.0,
            lon: -99.0,
        },
        GeoPoint {
            lat: 31.0,
            lon: -98.0,
        },
    )
    .expect("two distinct points on Earth");
    ls.retarget_renders_for(
        &PRODUCT_ID,
        TILT,
        Some(SectionLoopKey::new(
            elsewhere,
            None,
            squallar_radar::srv::SrvFallback::default(),
        )),
    );
    ls.frames[0].render_in_flight = true;

    let mut sr = section_response(&ctx, 1);
    let uploaded = std::cell::Cell::new(false);
    let placed = accept_section_result(&mut ls, &mut sr, |image| {
        uploaded.set(true);
        ctx.load_texture("cut", image, egui::TextureOptions::NEAREST)
    });

    assert!(placed.is_none(), "a cut along the old line was placed");
    assert!(
        !uploaded.get(),
        "the raster was uploaded before being refused, so every superseded cut \
         costs a GPU texture"
    );
    assert!(ls.frames[0].image.is_none());
}

/// A reply carrying no raster retires the frame rather than leaving it in
/// flight for ever.
#[test]
fn a_cut_that_produced_nothing_retires_its_frame() {
    let ctx = egui::Context::default();
    let mut ls = squallar_egui::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = vec![LoopFrame {
        timestamp: ts(0),
        image: None,
        render_in_flight: true,
        render_failed: false,
    }];
    ls.retarget_renders_for(&PRODUCT_ID, TILT, Some(key()));
    ls.frames[0].render_in_flight = true;

    let mut sr = section_response(&ctx, 1);
    sr.image = None;
    sr.axes = None;
    assert!(accept_section_result(&mut ls, &mut sr, |_| unreachable!()).is_none());
    assert!(!ls.frames[0].render_in_flight);
    assert!(
        ls.frames[0].render_failed,
        "a cut that answered nothing left its frame in limbo, so the loop \
         never settles"
    );
}

/// A frame already cut from the ladder its volume resolves *now* is not cut
/// again; one cut from a different ladder is.
#[test]
fn a_frame_is_recut_when_its_volume_resolves_a_different_ladder() {
    let ctx = egui::Context::default();
    let mut app = app_with_section_loop(&[0]);
    app.loop_mgr
        .cache_scan(SITE, ts(0), (volume(), Default::default()));

    let FrameSection::At(current) = frame_section(&app.loop_mgr, &target(), ts(0)) else {
        panic!("the cached volume must resolve a ladder");
    };

    app.gui
        .pane_mut(0)
        .unwrap()
        .time_state_mut(&known::RADAR)
        .frames[0]
        .image = Some(section_picture(&ctx, current));
    app.dispatch_loop_renders();
    assert!(
        !app.gui.pane(0).unwrap().time_state(&known::RADAR).frames[0].render_in_flight,
        "a frame already cut from this volume's ladder was cut again, so every \
         dispatch pass re-cuts the whole loop"
    );

    app.gui
        .pane_mut(0)
        .unwrap()
        .time_state_mut(&known::RADAR)
        .frames[0]
        .image = Some(section_picture(&ctx, current.wrapping_add(1)));
    // The first pass filed the fresh cut in the shared store, and a pane
    // whose own cut is stale would take it from there. A volume moving on
    // stales the store's cut with the pane's — which this fixture, faking
    // the stale cut on the pane alone, spells by emptying the store.
    drop(app.loop_frames.clear());
    app.dispatch_loop_renders();
    assert!(
        app.gui.pane(0).unwrap().time_state(&known::RADAR).frames[0].render_in_flight,
        "a frame cut from a ladder its volume no longer resolves was left \
         alone, so a section of a partial volume stands for the whole loop"
    );
}

/// Suppression is a promise of acceptance, so what the dedupe weighs and what
/// acceptance weighs must be the same things.
#[test]
fn the_cut_dedupe_weighs_both_halves_of_the_key() {
    let queued = [LoopSectionRequest {
        pane_idx: 0,
        frame_idx: 0,
        timestamp: ts(0),
        target: target(),
        key: key(),
        ladder: 1,
        site_lat: 35.33,
        site_lon: -97.28,
    }];

    assert!(section_already_queued(
        queued.iter(),
        ts(0),
        &target(),
        &key()
    ));
    assert!(
        !section_already_queued(queued.iter(), ts(1), &target(), &key()),
        "another frame's cut was suppressed"
    );
    assert!(
        !section_already_queued(
            queued.iter(),
            ts(0),
            &test_keys::key("KOUN", &PRODUCT_ID, TILT),
            &key()
        ),
        "another site's cut was suppressed, so its frame is served by neither"
    );
    let elsewhere = SectionLoopKey::new(
        SectionLine::new(
            GeoPoint {
                lat: 30.0,
                lon: -99.0,
            },
            GeoPoint {
                lat: 31.0,
                lon: -98.0,
            },
        )
        .expect("two distinct points on Earth"),
        None,
        squallar_radar::srv::SrvFallback::default(),
    );
    assert!(
        !section_already_queued(queued.iter(), ts(0), &target(), &elsewhere),
        "a cut along another line was suppressed on the promise of a broadcast \
         that will refuse it"
    );
}

/// A reply the tests can hand to `accept_section_result` without a worker.
fn section_response(ctx: &egui::Context, ladder: u64) -> crate::channels::LoopSectionResponse {
    let _ = ctx;
    crate::channels::LoopSectionResponse {
        pane_idx: 0,
        timestamp: ts(0),
        target: target(),
        key: key(),
        ladder,
        image: Some(egui::ColorImage::from_rgba_unmultiplied(
            [1, 1],
            &[255, 255, 255, 255],
        )),
        axes: Some(axes()),
        tilt_elevations_deg: vec![0.5],
        tilt_collected_ms: vec![0],
    }
}

fn section_picture(ctx: &egui::Context, ladder: u64) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::Section(squallar_egui::pane::SectionImageData {
        texture: ctx.load_texture("section", image, egui::TextureOptions::NEAREST),
        axes: axes(),
        tilt_elevations_deg: vec![0.5],
        tilt_collected_ms: vec![0],
        ladder,
    })
}

/// Axes with one rung. The arithmetic inside them is `squallar_radar::xsect`'s
/// business; nothing here reaches a rasterizer.
fn axes() -> squallar_radar::xsect::SectionAxes {
    squallar_radar::xsect::SectionAxes {
        length_km: 100.0,
        base_km_msl: 0.0,
        top_km_msl: 20.0,
        near_ground_range_km: 0.0,
        far_ground_range_km: 100.0,
        coverage_ground_range_km: 100.0,
        cone_of_silence_km: 0.0,
        tilt_count: 1,
        widest_tilt_gap_deg: 0.0,
        top_tilt_deg: 0.5,
        top_declared_cut_deg: 0.5,
    }
}

// ---------------------------------------------------------------------------
// **Which timeline the section half of the funnel addresses** (WO-T3.8).
//
// A section cut is keyed by `SectionLoopKey` + `RenderTarget`, both cut out of
// a decoded NEXRAD volume, so every read in the section arms is radar's own
// timeline by construction. WO-T3.7 wrote that down and found nothing enforced
// it: retargeting the cut dispatch, the result acceptance, the broadcast or
// the donor search at `transport_state()` passed the whole tree.
//
// **The reachable state they fork on**: `PaneState::refresh_transport` returns
// early while the transport's own loop is active, so arming a GMGSI loop and
// *then* enabling radar leaves the controls on the satellite while radar
// animates underneath. Every pin below runs twice — radar driving, which is
// the floor and is asserted against a literal, then the satellite driving.

/// Arm a running satellite loop on `pane_idx` and hand it the transport.
///
/// The satellite's own frame is stamped hours away from anything radar
/// carries, so a lookup made against it cannot land on radar's answer by
/// accident — and it is a real frame, so a mis-addressed *write* lands
/// somewhere rather than being swallowed by an empty list.
fn a_satellite_loop_takes_the_transport(app: &mut crate::app::App, pane_idx: usize) {
    let pane = app
        .gui
        .pane_mut(pane_idx)
        .expect("the fixture built a pane");
    let mut sat = squallar_egui::pane::LayerTimeState::new();
    sat.phase = LoopPhase::Playing;
    sat.span_secs = 43_200;
    sat.frames = vec![LoopFrame {
        timestamp: ts(0) - chrono::Duration::hours(10),
        image: None,
        render_in_flight: false,
        render_failed: false,
    }];
    *pane.time_state_mut(&known::GMGSI) = sat;
    pane.set_transport_layer(known::GMGSI);

    assert!(
        !std::ptr::eq(pane.transport_state(), pane.time_state(&known::RADAR)),
        "precondition: the transport really addresses another timeline, or the \
         two reads are one object and the case is vacuous",
    );
    assert!(
        pane.transport_state().is_active(),
        "precondition: the satellite loop is genuinely running",
    );
    assert!(
        pane.transport_state().section_key().is_none(),
        "precondition: the satellite timeline carries no section key — it is \
         not a radar timeline and cannot have one",
    );
}

/// Two layer-linked, aimed cross-section panes running the same section loop
/// over `minutes`, and no volumes cached.
pub(super) fn two_section_panes(minutes: &[u32]) -> crate::app::App {
    let mut app = crate::app::tests::two_pane_app(SITE, SITE);
    app.render.ensure_pane_count(2);
    app.loop_mgr = LoopDownloadManager::new();
    for idx in 0..2 {
        let pane = app.gui.pane_mut(idx).expect("the fixture built two panes");
        pane.set_site(SITE.to_string());
        pane.set_selected_product(PRODUCT_ID);
        pane.set_selected_elevation(TILT);
        pane.set_kind(PaneKind::CrossSection);
        pane.cross_section_mut().expect("a section pane").line = Some(line());

        let mut ls =
            squallar_egui::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
        ls.phase = LoopPhase::Rendering;
        ls.frames = minutes
            .iter()
            .map(|&m| LoopFrame {
                timestamp: ts(m),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        ls.retarget_renders_for(&PRODUCT_ID, TILT, Some(key()));
        *pane.time_state_mut(&known::RADAR) = ls;
    }
    // Deliberately **unlinked**: a finished cut reaches the second pane on
    // the cut's identity, not the link, and a linked fixture could not tell
    // the two apart.
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .layer_link = false;
    app
}

/// Run one dispatch pass over a section loop whose frame has a cuttable
/// volume, and answer whether radar's own frame was marked in flight.
fn radars_section_frame_goes_in_flight(park_on_the_satellite: bool) -> bool {
    let mut app = app_with_section_loop(&[0]);
    app.loop_mgr
        .cache_scan(SITE, ts(0), (volume(), Default::default()));
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut app, 0);
    }

    app.dispatch_loop_renders();

    app.gui.pane(0).unwrap().time_state(&known::RADAR).frames[0].render_in_flight
}

/// **A dispatched cut marks the frame it is for in radar's own list.**
///
/// The mark is what stops the very next pass cutting the same frame again —
/// and a cut is a whole-volume extraction on the frame thread, the most
/// expensive thing the loop does. A transport-addressed write puts the mark on
/// a timeline that is not the one the dispatcher reads, so **every pass
/// re-cuts the same frame** and the section loop never advances past it.
#[test]
fn a_dispatched_cut_marks_radars_own_frame_not_the_transports() {
    assert!(
        radars_section_frame_goes_in_flight(false),
        "floor: with radar driving, a frame with a cuttable volume is \
         dispatched and marked — if this arm marked nothing the assertion \
         below would be satisfied by two unmarked frames",
    );
    assert!(
        radars_section_frame_goes_in_flight(true),
        "a cross-section cut was dispatched for a frame that was never marked \
         in flight, because the pane's transport sat on a satellite loop: the \
         next pass sees an unmarked frame with no picture and extracts the \
         whole volume all over again, on the frame thread, for ever",
    );
}

/// Deliver one finished cut for pane 0 into two layer-linked section panes and
/// answer whether each pane's radar frame ended up holding a section picture.
fn frames_holding_a_cut(park_on_the_satellite: bool) -> (bool, bool) {
    let ctx = egui::Context::default();
    let mut app = two_section_panes(&[0]);
    app.loop_mgr
        .cache_scan(SITE, ts(0), (volume(), Default::default()));
    let FrameSection::At(ladder) = frame_section(&app.loop_mgr, &target(), ts(0)) else {
        panic!(
            "precondition: the cached volume must resolve a ladder, or the \
                broadcast has no ladder to agree with"
        );
    };
    for idx in 0..2 {
        app.gui
            .pane_mut(idx)
            .unwrap()
            .time_state_mut(&known::RADAR)
            .frames[0]
            .render_in_flight = true;
    }
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut app, 0);
        a_satellite_loop_takes_the_transport(&mut app, 1);
    }

    app.channels
        .loop_section_sender
        .send(section_response(&ctx, ladder))
        .expect("the receiver lives on the App");
    app.poll_loop_section_results(&ctx);

    let holds = |app: &crate::app::App, idx: usize| {
        app.gui.pane(idx).unwrap().time_state(&known::RADAR).frames[0]
            .image
            .is_some()
    };
    (holds(&app, 0), holds(&app, 1))
}

/// **A finished cut lands in radar's own frame list on the pane that asked for
/// it, and in radar's own frame list on every sibling it is broadcast to.**
///
/// A `SectionImageData` is a slice through a decoded NEXRAD volume; no other
/// layer's timeline has a frame that could hold one. A transport-addressed
/// read at either site drops the cut on the floor: the frame stays blank,
/// `render_in_flight` is never cleared, and the pane waits for a
/// cross-section that has already been computed and thrown away.
#[test]
fn a_finished_cut_lands_in_radars_own_frames_not_the_transports() {
    let radar_driving = frames_holding_a_cut(false);
    assert_eq!(
        radar_driving,
        (true, true),
        "floor: with radar driving, the cut lands on its own pane and is \
         broadcast to the linked sibling — if neither took it the comparison \
         below would be satisfied by two blank panes",
    );

    let satellite_driving = frames_holding_a_cut(true);
    assert_eq!(
        satellite_driving, radar_driving,
        "a finished cross-section was thrown away because the panes' \
         transports sat on satellite loops: it matched no frame on a timeline \
         that holds no section key, so both panes go on showing \"Cutting the \
         cross-section…\" over a cut that is already done",
    );
}

/// Run one dispatch pass over two linked section panes where pane 0 already
/// holds a cut of the ladder its volume resolves now, and answer whether pane
/// 1 took the donor's picture instead of extracting the volume again.
fn a_section_donor_is_cloned(park_on_the_satellite: bool) -> bool {
    let ctx = egui::Context::default();
    let mut app = two_section_panes(&[0]);
    app.loop_mgr
        .cache_scan(SITE, ts(0), (volume(), Default::default()));
    let FrameSection::At(ladder) = frame_section(&app.loop_mgr, &target(), ts(0)) else {
        panic!(
            "precondition: the cached volume must resolve a ladder, or no \
                donor can ever match it"
        );
    };
    app.gui
        .pane_mut(0)
        .unwrap()
        .time_state_mut(&known::RADAR)
        .frames[0]
        .image = Some(section_picture(&ctx, ladder));
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut app, 0);
        a_satellite_loop_takes_the_transport(&mut app, 1);
    }

    app.dispatch_loop_renders();

    app.gui.pane(1).unwrap().time_state(&known::RADAR).frames[0]
        .image
        .is_some()
}

/// **The section donor search reads every sibling's radar frame list, never
/// its transport.**
///
/// A satellite timeline holds no `RenderTarget` and no `SectionLoopKey`, so a
/// transport-addressed search finds nobody: the receiving pane **extracts the
/// whole volume again** for a cut that is finished and on screen beside it —
/// the single most expensive thing this loop does, paid twice, on the frame
/// thread.
#[test]
fn a_section_donor_is_found_in_radars_own_frame_lists() {
    assert!(
        a_section_donor_is_cloned(false),
        "floor: with radar driving, a linked sibling's finished cut is cloned \
         into the pane that has none — if this arm cloned nothing the \
         assertion below would be satisfied by two blank frames",
    );
    assert!(
        a_section_donor_is_cloned(true),
        "the linked-pane cut clone stopped happening because the transports \
         sat on satellite loops: a second pane cutting the identical line, \
         product and tilt re-extracts the whole volume on the frame thread \
         instead of taking the raster already finished beside it",
    );
}
