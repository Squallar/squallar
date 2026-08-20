//! The 3D loop's host half: what becomes resident, what bounds it, and what a region change
//! lets go of before it rebuilds.

use super::*;
use crate::app::tests::{empty_scan, headless};
use crate::platform_double::TestBridge;
use rustdar_device_profile::constants::MAX_LOOP_FRAMES;
use rustdar_egui::pane::{
    LayerTimeState, LoopFrame, LoopFrameImage, LoopPhase, VolumeRegion, VolumeStamp, VolumeTarget,
};
use rustdar_geo::GeoPoint;
use rustdar_radar::loop_downloads::LoopDownloadManager;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::{RadarProduct, RenderView};
use rustdar_volumetric::bridge::VolumeEntry;

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
    // The fixture network is otherwise installed only as a side effect of
    // `app::tests::headless`, so a test in here that reads the table before it
    // builds an App reads whatever an earlier test left behind: green in the
    // package run, red when the suite is filtered alone.
    crate::test_sites::install();
    rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone()
}

/// An app with one 3D pane running a volume loop over `minutes`, every one of whose scans
/// is already downloaded.
fn app_with_volume_loop(minutes: &[u32]) -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    app.render.ensure_pane_count(1);
    app.loop_mgr = LoopDownloadManager::new();
    for &m in minutes {
        app.loop_mgr.cache_scan(
            SITE,
            ts(m),
            (std::sync::Arc::new(empty_scan()), Default::default()),
        );
    }

    let pane = app.gui.pane_mut(0).expect("pane 0 exists");
    pane.set_site(SITE.to_string());
    pane.set_selected_product(PRODUCT);
    pane.set_selected_elevation(TILT);
    pane.set_view(rustdar_radar::types::RenderView::Volume);

    let mut ls = rustdar_egui::radar_layer::begin_loop(3600, &site(), RenderView::Volume);
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
    *pane.loop_state_mut() = ls;
    app
}

/// The target the loop's frame at `minute` is built from, over `region`.
fn frame_target(minute: u32, region: Option<VolumeRegion>) -> VolumeTarget {
    VolumeTarget {
        volume: VolumeStamp {
            site: SITE.to_owned(),
            collected: ts(minute),
        },
        product: PRODUCT,
        region,
    }
}

/// A picked box, distinct from the default one about the site — 20 km rather than the full
/// surveillance range, which is the resolution trade the region picker exists to make.
fn region() -> VolumeRegion {
    VolumeRegion::new(
        GeoPoint {
            lat: 35.33,
            lon: -97.27,
        },
        rustdar_radar::voxel::HalfExtentKm::square(20.0),
    )
    .expect("a finite centre and an in-range half-width")
}

/// Run dispatch until every frame has been offered a build, which at
/// `MAX_LOOP_VOLUME_BUILDS_PER_FRAME` per pass takes one pass per frame.
fn dispatch_until_settled(app: &mut crate::app::App, frames: usize) {
    for _ in 0..frames + 2 {
        app.dispatch_loop_renders();
    }
}

/// Every target the store is holding for pane 0, oldest volume time first.
fn resident_times(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    let mut times: Vec<chrono::NaiveDateTime> = MINUTES
        .iter()
        .flat_map(|&m| {
            [None, Some(region())]
                .into_iter()
                .filter(move |r| app.volume_store.lookup(&frame_target(m, *r)).is_some())
                .map(move |_| ts(m))
        })
        .collect();
    times.sort_unstable();
    times.dedup();
    times
}

/// The volume times every test here loops over.
const MINUTES: [u32; 4] = [0, 5, 10, 15];

/// thing, and nothing else in the codebase makes them so.
#[test]
fn the_resident_set_is_the_whole_frame_list() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());

    assert_eq!(
        resident_times(&app),
        MINUTES.iter().map(|&m| ts(m)).collect::<Vec<_>>(),
        "the store is not holding one entry per loop frame, so the playhead \
         will reach a frame with no resident grid and the march will have \
         nothing to sample",
    );

    // And the frames name them, which is what makes the playhead able to march one: a store
    // entry nothing points at is memory, not a loop.
    let frames = &app.gui.pane(0).expect("pane 0").loop_state().frames;
    for (idx, frame) in frames.iter().enumerate() {
        assert!(
            frame.render_failed || frame.image.is_some(),
            "frame {idx} was left neither named nor retired, so readiness \
             waits on it for ever",
        );
    }
}

/// store's seamless-swap rule would otherwise keep every old grid while the new ones were
/// built.
#[test]
fn a_region_change_releases_the_old_set_before_building_the_new_one() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());
    assert_eq!(
        resident_times(&app).len(),
        MINUTES.len(),
        "precondition: a full set must be resident, or the release below has \
         nothing to release and the test passes vacuously",
    );
    for &m in &MINUTES {
        assert!(
            app.volume_store.lookup(&frame_target(m, None)).is_some(),
            "precondition: the first set is keyed to the default box",
        );
    }

    // The pane's box changes.
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .volume_mut()
        .expect("a 3D pane")
        .region = Some(region());

    // One pass: the retarget is noticed, the set is released, and at most
    // `MAX_LOOP_VOLUME_BUILDS_PER_FRAME` of the new key's builds start.
    app.dispatch_loop_renders();

    for &m in &MINUTES {
        assert!(
            app.volume_store.lookup(&frame_target(m, None)).is_none(),
            "a grid resampled over the old box survived into the rebuild, so \
             the peak is two full sets rather than one",
        );
    }
    let new_key_resident = MINUTES
        .iter()
        .filter(|&&m| {
            app.volume_store
                .lookup(&frame_target(m, Some(region())))
                .is_some()
        })
        .count();
    assert!(
        new_key_resident <= MAX_LOOP_VOLUME_BUILDS_PER_FRAME,
        "{new_key_resident} of the new key's grids were built in one pass, so \
         the per-frame pacing is not capping the frame-thread extraction",
    );

    // And it does converge: the new set arrives, one build per frame.
    dispatch_until_settled(&mut app, MINUTES.len());
    for &m in &MINUTES {
        assert!(
            app.volume_store
                .lookup(&frame_target(m, Some(region())))
                .is_some(),
            "the loop never rebuilt its set over the new box",
        );
    }
}

/// The pacing is a cap on the *extraction*, not on the naming.
#[test]
fn the_pacing_caps_the_extraction_and_not_the_naming() {
    let mut app = app_with_volume_loop(&MINUTES);

    let before = app.volume_extractions.get();
    app.dispatch_loop_renders();
    let first_pass = app.volume_extractions.get() - before;
    assert_eq!(
        first_pass as usize, MAX_LOOP_VOLUME_BUILDS_PER_FRAME,
        "one dispatch pass ran {first_pass} whole-volume extractions on the \
         frame thread, against a cap of {MAX_LOOP_VOLUME_BUILDS_PER_FRAME}",
    );

    dispatch_until_settled(&mut app, MINUTES.len());
    let settled = app.volume_extractions.get();
    app.dispatch_loop_renders();
    assert_eq!(
        app.volume_extractions.get(),
        settled,
        "a pass over a settled loop paid for an extraction, so a loop rebuilds \
         grids it is already holding",
    );
    assert_eq!(
        resident_times(&app).len(),
        MINUTES.len(),
        "the settled pass lost a grid",
    );
}

/// A 3D loop's grids do not outlive the loop.
#[test]
fn switching_the_loop_off_gives_the_resident_set_back() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());
    assert_eq!(
        resident_times(&app).len(),
        MINUTES.len(),
        "precondition: a full set must be resident to be given back",
    );

    *app.gui.pane_mut(0).expect("pane 0").loop_state_mut() = LayerTimeState::new();
    app.dispatch_loop_renders();

    assert_eq!(
        app.volume_store.texture_bytes(),
        0,
        "the resident set outlived the loop that asked for it",
    );
    assert!(
        resident_times(&app).is_empty(),
        "grids from the retired loop are still in the store",
    );
}

/// Switching the loop off leaves the pane able to ask for a live volume again.
#[test]
fn switching_the_loop_off_lets_the_pane_ask_for_a_live_volume_again() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());
    // The key the live pane was left holding when the loop took over.
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .volume_mut()
        .expect("a 3D pane")
        .rendered_for = Some(frame_target(MINUTES[0], None));

    *app.gui.pane_mut(0).expect("pane 0").loop_state_mut() = LayerTimeState::new();
    app.dispatch_loop_renders();

    assert!(
        app.gui
            .pane(0)
            .expect("pane 0")
            .volume()
            .expect("a 3D pane")
            .rendered_for
            .is_none(),
        "the pane still names a grid the teardown released, so its \
         level-triggered ask will never fire and it will read \"Building…\" \
         for the rest of the session",
    );

    // And a live 3D pane — one that never held a set — keeps its key, which is what stops
    // this clearing becoming a rebuild every frame.
    let mut live = headless(TestBridge::desktop());
    live.render.ensure_pane_count(1);
    let pane = live.gui.pane_mut(0).expect("pane 0");
    pane.set_view(rustdar_radar::types::RenderView::Volume);
    pane.volume_mut().expect("a 3D pane").rendered_for = Some(frame_target(MINUTES[0], None));
    live.dispatch_loop_renders();
    assert!(
        live.gui
            .pane(0)
            .expect("pane 0")
            .volume()
            .expect("a 3D pane")
            .rendered_for
            .is_some(),
        "a 3D pane with no loop had its key cleared, so it rebuilds an 8 MiB \
         grid every frame with a hot CPU as the only symptom",
    );
}

/// The refusal path is terminal, and it is what stops a loop over volumes with nothing to
/// resample sitting in `Rendering` for the session.
#[test]
fn a_volume_with_nothing_to_resample_retires_its_frame() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());

    let frames = &app.gui.pane(0).expect("pane 0").loop_state().frames;
    assert!(
        frames.iter().all(|f| f.render_failed),
        "an empty volume left its frame un-retired, so readiness waits on a \
         build that will never produce anything",
    );
    assert!(
        frames.iter().all(|f| !f.render_in_flight),
        "a retired frame is still marked in flight, which is a build nothing \
         will ever answer",
    );
    for &m in &MINUTES {
        assert!(
            matches!(
                app.volume_store
                    .lookup(&frame_target(m, None))
                    .map(|f| f.entry),
                Some(VolumeEntry::Refused(_)),
            ),
            "the store holds something other than a refusal for a volume that \
             carries no moment",
        );
    }
}

/// A 3D loop is capped at its **resident** frame count when the scan listing lands, not at
/// `MAX_LOOP_FRAMES`.
#[test]
fn the_scan_listing_is_sampled_to_the_resident_frame_count() {
    let mut ls = rustdar_egui::radar_layer::begin_loop(3600, &site(), RenderView::Volume);
    ls.phase = LoopPhase::Rendering;
    let listing: Vec<_> = (0..MAX_LOOP_FRAMES + 20)
        .map(|i| {
            // Minutes apart, which past an hour has to roll into the hour rather than
            // saturate — `ts` above takes a minute-of-hour and this listing is longer
            // than one.
            ts(0) + chrono::Duration::minutes(i64::try_from(i).expect("a small index"))
        })
        .collect();
    assert!(
        listing.len() > MAX_LOOP_FRAMES,
        "precondition: the listing must exceed even the plan-view cap, or the \
         sampling below is not exercised",
    );

    accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut ls,
        SITE,
        listing.clone(),
        1,
    );
    assert_eq!(
        ls.frames.len(),
        test_loop_allocation().volume_frames,
        "a 3D loop took the plan-view frame count, so its frame list is longer \
         than its resident set can be",
    );

    // The plan-view loop is unchanged, which is what makes the assertion above about the
    // view rather than about the cap having moved for everyone.
    let mut plan = rustdar_egui::radar_layer::begin_loop(3600, &site(), RenderView::PlanView);
    plan.phase = LoopPhase::Rendering;
    accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut plan,
        SITE,
        listing,
        1,
    );
    assert_eq!(plan.frames.len(), MAX_LOOP_FRAMES);
}

/// The playing frame is what the pane paints, and it is a *grid* rather than a raster — so
/// `active_image` and `active_section_image` must both refuse it.
#[test]
fn the_playing_frame_is_a_grid_and_no_raster_consumer_takes_it() {
    let mut app = app_with_volume_loop(&MINUTES);
    // A resident grid named on the playhead's frame, planted directly: what is under test
    // is which accessor answers, not how the frame was filled.
    let pane = app.gui.pane_mut(0).expect("pane 0");
    pane.loop_state_mut().phase = LoopPhase::Playing;
    pane.park_on_loop_frame(1);
    pane.loop_state_mut().frames[1].image = Some(LoopFrameImage::Volume(
        rustdar_egui::pane::VolumeFrameGrid {
            id: 42,
            target: frame_target(MINUTES[1], None),
        },
    ));

    let pane = app.gui.pane(0).expect("pane 0");
    assert_eq!(
        pane.active_volume_frame().map(|g| g.id),
        Some(42),
        "the pane cannot find the grid the playhead is on, so the march would \
         go on sampling the live volume while the transport claimed otherwise",
    );
    assert_eq!(
        pane.active_volume_frame()
            .map(|g| g.target.volume.collected),
        Some(ts(MINUTES[1])),
        "the frame names a different volume from the one the playhead is on",
    );
    assert!(
        pane.active_image().is_none(),
        "a plan-view consumer took a 3D loop frame, which it would stretch \
         across the pane's geographic bounds",
    );
    assert!(
        pane.active_section_image().is_none(),
        "a section consumer took a 3D loop frame, which it would draw into a \
         height scale and a tilt ladder that are not there",
    );
}

/// A volume that really resamples, dated at `minute`.
fn resamplable_scan(minute: u32) -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let stamp_ms = ts(minute).and_utc().timestamp_millis();
    let sweep = |number: u8, elevation: f32| {
        let radials = (0..8u16)
            .map(|i| {
                Radial::new(
                    stamp_ms + i64::from(i),
                    i + 1,
                    f32::from(i) * 45.0,
                    45.0,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(MomentData::from_fixed_point(
                        4,
                        2125,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![120, 140, 160, 180],
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
        Sweep::new(number, radials)
    };
    let cut = |angle: f64| {
        ElevationCut::new(
            angle,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
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
        )
    };
    Scan::new(
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
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5), sweep(2, 1.5)],
    )
}

/// [`app_with_volume_loop`], over volumes that resample into real grids rather than into
/// refusals.
fn app_with_built_volume_loop(minutes: &[u32]) -> crate::app::App {
    let mut app = app_with_volume_loop(minutes);
    app.loop_mgr = LoopDownloadManager::new();
    for &m in minutes {
        app.loop_mgr.cache_scan(
            SITE,
            ts(m),
            (std::sync::Arc::new(resamplable_scan(m)), Default::default()),
        );
    }
    app
}

/// One dispatch pass, with the worker's replies taken delivery of exactly as
/// `App::poll_voxel_results` does.
fn pass(app: &mut crate::app::App) {
    app.dispatch_loop_renders();
    let in_flight = MINUTES
        .iter()
        .filter(|&&m| {
            matches!(
                app.volume_store
                    .lookup(&frame_target(m, None))
                    .map(|f| f.entry),
                Some(VolumeEntry::Building),
            )
        })
        .count();
    for _ in 0..in_flight {
        let reply = app
            .channels
            .voxel_receiver
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("every dispatched build answers, or the store's placeholder is a lie");
        let grid = reply
            .grid
            .expect("the fixture volume resamples into a grid");
        assert!(
            app.volume_store.complete(
                &reply.target,
                VolumeEntry::Ready(std::sync::Arc::new(*grid))
            ),
            "the store had nothing waiting for a build it opened",
        );
    }
}

/// The dispatcher plans the set, hands it to `make_volume_frames_resident`, and that pass
/// states the whole thing through `VolumeStore::retain_set` — which detaches the holder
/// from *everything it did not name*.
#[test]
fn the_resident_set_survives_its_own_frames_landing() {
    let mut app = app_with_built_volume_loop(&MINUTES);
    for _ in 0..MINUTES.len() + 3 {
        pass(&mut app);
    }

    let live = app.volume_store.live_ids();
    let resident = resident_times(&app);
    let frames = &app.gui.pane(0).expect("pane 0").loop_state().frames;
    assert_eq!(
        frames.len(),
        MINUTES.len(),
        "precondition: one frame per volume",
    );
    for (idx, frame) in frames.iter().enumerate() {
        assert!(
            !frame.render_failed,
            "precondition: frame {idx} was retired, so this fixture is back to \
             asserting about refusals",
        );
    }

    for (idx, frame) in frames.iter().enumerate() {
        let grid = frame
            .image
            .as_ref()
            .and_then(rustdar_egui::pane::LoopFrameImage::volume)
            .unwrap_or_else(|| panic!("frame {idx} was never named"));
        assert!(
            live.contains(&grid.id),
            "frame {idx} ({}) names grid {} and the store has let it go — the \
             playhead will march whatever grid is left instead, which is the \
             newest volume under every frame's caption",
            grid.target.volume.collected,
            grid.id,
        );
    }
    assert_eq!(
        resident,
        MINUTES.iter().map(|&m| ts(m)).collect::<Vec<_>>(),
        "the store is not holding one grid per loop frame",
    );
}

/// The 3D loop is the one kind whose frame list *is* its resident set, so the span budget
/// reaches its list rather than only its render set.
#[test]
fn a_slow_site_shortens_a_3d_loops_list_without_shortening_its_span() {
    let budgets = test_budgets();
    let step_mins = 15i64;
    let mut ls = rustdar_egui::radar_layer::begin_loop(10 * 3600, &site(), RenderView::Volume);
    ls.phase = LoopPhase::Rendering;
    let listing: Vec<_> = (0..40)
        .map(|i| ts(0) + chrono::Duration::minutes(i * step_mins))
        .collect();
    let oldest = *listing.first().expect("a listing");
    let newest = *listing.last().expect("a listing");

    let wanted = budgets.frames_for_span(Some((step_mins * 60) as u32));
    assert!(
        wanted < test_loop_allocation().volume_frames,
        "precondition: a {step_mins} min cadence must bind before the pool's \
         share of {} grids, or this proves nothing",
        test_loop_allocation().volume_frames,
    );

    accept_scan_listing(
        test_loop_allocation(),
        &budgets,
        &mut ls,
        SITE,
        listing.clone(),
        1,
    );

    assert_eq!(
        ls.frames.len(),
        wanted,
        "the span budget did not reach the 3D loop's frame list, which is the \
         one list that is also a resident set",
    );
    assert_eq!(
        ls.frames.first().map(|f| f.timestamp),
        Some(oldest),
        "the oldest scan went, so the caption's span is short of the lookback",
    );
    assert_eq!(
        ls.frames.last().map(|f| f.timestamp),
        Some(newest),
        "the newest scan went, so the loop stops short of what the pane shows",
    );
    assert_eq!(
        ls.sampled,
        Some(true),
        "the span budget cut the list without the sampler recording it, so the \
         caption would claim every scan over a list that dropped 31 of them",
    );

    // And the plan-view loop beside it is untouched: a raster frame's history costs no
    // texture until it is in the render set, so holding fewer would throw away resolution
    // the span budget never paid for.
    let mut plan = rustdar_egui::radar_layer::begin_loop(10 * 3600, &site(), RenderView::PlanView);
    plan.phase = LoopPhase::Rendering;
    accept_scan_listing(
        test_loop_allocation(),
        &budgets,
        &mut plan,
        SITE,
        listing,
        1,
    );
    assert_eq!(plan.frames.len(), MAX_LOOP_FRAMES.min(40));
}
