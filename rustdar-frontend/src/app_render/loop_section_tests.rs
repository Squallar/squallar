//! The cross-section loop: what gets dispatched, what gets placed, and what the
//! frame thread is allowed to spend doing it.
//!
//! The pane-level identity rules — which loop may take which picture, and why
//! `RenderTarget` alone cannot answer that — live in
//! `rustdar_egui::pane::section_loop_tests`. These are the host's half: the
//! planning inside `dispatch_loop_renders`, the placement inside
//! `poll_loop_section_results`, and the pacing that keeps the extraction off
//! the frame budget.

use super::*;
use crate::app::tests::headless;
use crate::loop_downloads::LoopDownloadManager;
use crate::platform_double::TestBridge;
use rustdar_egui::pane::{
    GeoPoint, LoopFrame, LoopFrameImage, LoopPhase, LoopPlaybackState, PaneKind, SectionLine,
    SectionLoopKey,
};
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::{RadarProduct, RenderView};

const SITE: &str = "KTLX";
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const TILT: f32 = 0.5;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 10)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

fn site() -> RadarSite {
    rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone()
}

fn line() -> SectionLine {
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

fn key() -> SectionLoopKey {
    SectionLoopKey::new(line(), None)
}

fn target() -> RenderTarget {
    RenderTarget::new(SITE, PRODUCT, TILT)
}

/// An app with one aimed cross-section pane running a section loop over
/// `minutes`, and no volumes cached for any of them.
fn app_with_section_loop(minutes: &[u32]) -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    app.render.ensure_pane_count(1);
    app.loop_mgr = LoopDownloadManager::new();
    let pane = app.gui.pane_mut(0).expect("pane 0 exists");
    pane.site = SITE.to_string();
    pane.selected_product = PRODUCT;
    pane.selected_elevation = TILT;
    pane.set_kind(PaneKind::CrossSection);
    pane.cross_section_mut().expect("a section pane").line = Some(line());

    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::CrossSection);
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
    ls.retarget_renders_for(PRODUCT, TILT, Some(key()));
    pane.loop_state = ls;
    app
}

/// A one-rung reflectivity volume **with an elevation cut table**, which is
/// what `sampler::ladder_fingerprint` needs and the plan-view fixtures
/// deliberately do without.
///
/// The cut table is the difference and it is load-bearing: `ladder_fingerprint`
/// refuses a pattern with no cuts, which is `chunks.rs`' real mid-flight state
/// and the reason `SectionUnavailable::AwaitingCoveragePattern` exists. A
/// fixture without one would make every test below pass by never cutting
/// anything.
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
///
/// Retiring it would be the quiet failure. `render_failed` is terminal for the
/// loop's current target, so a frame retired while its download was still in
/// flight would stay blank for the life of the loop and playback would step
/// over a volume that arrived perfectly well seconds later.
#[test]
fn a_frame_whose_volume_has_not_arrived_is_neither_cut_nor_retired() {
    let mut app = app_with_section_loop(&[0, 1, 2]);

    app.dispatch_loop_renders();

    let ls = &app.gui.pane(0).unwrap().loop_state;
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
///
/// The distinction from the test above is the whole point: "not here yet" and
/// "here and empty" look identical from the frame and are opposite instructions.
#[test]
fn a_volume_with_no_ladder_retires_the_frame() {
    let mut app = app_with_section_loop(&[0]);
    // Present, so the frame is not `Pending`, and carrying no sweeps, so
    // `ladder_fingerprint` refuses it.
    app.loop_mgr.cache_scan(
        SITE,
        ts(0),
        std::sync::Arc::new(crate::app::tests::empty_scan()),
    );

    app.dispatch_loop_renders();

    assert!(
        app.gui.pane(0).unwrap().loop_state.frames[0].render_failed,
        "a frame whose volume carries nothing to cut was left waiting, so the \
         loop never settles and sits in Rendering for the session"
    );
}

/// **The frame-thread cap.** However many frames are ready to cut, one dispatch
/// pass starts at most [`MAX_LOOP_SECTION_CUTS_PER_FRAME`] of them.
///
/// Each cut costs a whole-volume extraction on the frame thread — measured at
/// ~1.0 ms on a real VCP-212 reflectivity volume — because the job wire carries
/// a `RenderInput` rather than a `Scan` and on wasm the volume is only reachable
/// from the main thread at all. Without the cap a desktop pass would run
/// `MAX_CONCURRENT_RENDERS` of them back to back on the frame that starts the
/// loop, and the pane would drop frames at exactly the moment the user asked
/// for an animation.
#[test]
fn one_dispatch_pass_starts_at_most_the_capped_number_of_cuts() {
    let mut app = app_with_section_loop(&[0, 1, 2, 3, 4]);
    for m in 0..5 {
        app.loop_mgr.cache_scan(SITE, ts(m), volume());
    }
    // Precondition: more frames are ready than the cap allows, or this test
    // cannot tell a cap from its absence.
    const { assert!(MAX_LOOP_SECTION_CUTS_PER_FRAME < 5) };

    app.dispatch_loop_renders();

    let in_flight = app
        .gui
        .pane(0)
        .unwrap()
        .loop_state
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
        app.loop_mgr.cache_scan(SITE, ts(m), volume());
    }

    let mut started = std::collections::HashSet::new();
    for _ in 0..3 {
        app.dispatch_loop_renders();
        let ls = &app.gui.pane(0).unwrap().loop_state;
        for (idx, frame) in ls.frames.iter().enumerate() {
            if frame.render_in_flight {
                started.insert(idx);
            }
        }
        // Clear the marks the way a reply would, so the next pass sees the
        // frames as available again — the cut itself needs a worker this test
        // deliberately does not have.
        for frame in &mut app.gui.pane_mut(0).unwrap().loop_state.frames {
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
///
/// The axes travel with the raster because they are labels *on* it. A loop that
/// placed the raster and kept the previous frame's scales would animate each
/// volume's slice under the last one's height and distance axes, which is a
/// wrong reading of a correct picture.
#[test]
fn a_finished_cut_is_placed_with_its_own_axes_and_ladder() {
    let ctx = egui::Context::default();
    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = vec![LoopFrame {
        timestamp: ts(0),
        image: None,
        render_in_flight: true,
        render_failed: false,
    }];
    ls.retarget_renders_for(PRODUCT, TILT, Some(key()));
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

/// A cut the loop has been retargeted away from is refused, and refusing it
/// costs nothing — the upload is the expensive half and must not run for a
/// raster that is about to be dropped.
#[test]
fn a_cut_for_a_line_the_loop_has_left_is_refused_without_uploading() {
    let ctx = egui::Context::default();
    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = vec![LoopFrame {
        timestamp: ts(0),
        image: None,
        render_in_flight: true,
        render_failed: false,
    }];
    // Keyed to a line the reply below does not name.
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
    ls.retarget_renders_for(PRODUCT, TILT, Some(SectionLoopKey::new(elsewhere, None)));
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
    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = vec![LoopFrame {
        timestamp: ts(0),
        image: None,
        render_in_flight: true,
        render_failed: false,
    }];
    ls.retarget_renders_for(PRODUCT, TILT, Some(key()));
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
///
/// The newest frame's volume is re-cached under the same `(site, timestamp)`
/// key as more of it seals, so a section cut from a two-rung ladder can
/// otherwise stand at the head of a loop while the real volume grows to
/// fourteen. This reuses `sampler::ladder_fingerprint` — the same fingerprint
/// the live pane's `SectionTarget::ladder` carries — rather than inventing a
/// second notion of section staleness.
#[test]
fn a_frame_is_recut_when_its_volume_resolves_a_different_ladder() {
    let ctx = egui::Context::default();
    let mut app = app_with_section_loop(&[0]);
    app.loop_mgr.cache_scan(SITE, ts(0), volume());

    // The ladder the cached volume actually resolves, asked the way the
    // dispatcher asks it, so the "matching" case below really matches.
    let FrameSection::At(current) = frame_section(&app.loop_mgr, &target(), ts(0)) else {
        panic!("the cached volume must resolve a ladder");
    };

    app.gui.pane_mut(0).unwrap().loop_state.frames[0].image = Some(section_picture(&ctx, current));
    app.dispatch_loop_renders();
    assert!(
        !app.gui.pane(0).unwrap().loop_state.frames[0].render_in_flight,
        "a frame already cut from this volume's ladder was cut again, so every \
         dispatch pass re-cuts the whole loop"
    );

    app.gui.pane_mut(0).unwrap().loop_state.frames[0].image =
        Some(section_picture(&ctx, current.wrapping_add(1)));
    app.dispatch_loop_renders();
    assert!(
        app.gui.pane(0).unwrap().loop_state.frames[0].render_in_flight,
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
            &RenderTarget::new("KOUN", PRODUCT, TILT),
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
    }
}

fn section_picture(ctx: &egui::Context, ladder: u64) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::Section(rustdar_egui::pane::SectionImageData {
        texture: ctx.load_texture("section", image, egui::TextureOptions::NEAREST),
        axes: axes(),
        tilt_elevations_deg: vec![0.5],
        ladder,
    })
}

/// Axes with one rung. The arithmetic inside them is `rustdar_radar::xsect`'s
/// business; nothing here reaches a rasterizer.
fn axes() -> rustdar_radar::xsect::SectionAxes {
    rustdar_radar::xsect::SectionAxes {
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
