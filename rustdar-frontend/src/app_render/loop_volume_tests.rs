//! The 3D loop's host half: what becomes resident, what bounds it, and what a
//! region change lets go of before it rebuilds.
//!
//! # What these are written against
//!
//! A 3D loop frame is not a picture. The other two loop kinds cache a raster
//! per frame and can drop and re-render one at will; a raymarch is a function
//! of the camera, so what this loop caches is the **input** — one live
//! `Rg16Float` 3D texture per frame, and the march swaps which one it samples.
//! That makes three things true at once that are not true of the other kinds,
//! and each of the tests below is one of them:
//!
//!  * the frame list **is** the resident set, because re-entering a window
//!    costs ~140 ms against a 200 ms playback interval;
//!  * the store is bounded by **bytes** rather than by a shed, because a set
//!    holder is exempt from every shed there is;
//!  * a change of key **releases before it builds**, because the seamless-swap
//!    rule that keeps the old grid through a rebuild is a peak of two full sets
//!    — 936 MiB against a 512 MiB budget on desktop.
//!
//! The pane-level identity rules live in `rustdar_egui::pane`; the store's own
//! rules live in `volume::bridge::tests`. These are the dispatcher's.

use super::*;
use crate::app::tests::{empty_scan, headless};
use crate::loop_downloads::LoopDownloadManager;
use crate::platform_double::TestBridge;
use crate::volume::bridge::VolumeEntry;
use rustdar_egui::pane::{
    GeoPoint, LoopFrame, LoopFrameImage, LoopPhase, LoopPlaybackState, PaneKind, VolumeRegion,
    VolumeStamp, VolumeTarget,
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

/// An app with one 3D pane running a volume loop over `minutes`, every one of
/// whose scans is already downloaded.
///
/// The scans are `empty_scan`s, which carry no moment — so every build is
/// *refused*, and a refusal is an entry in the store exactly as a grid is. That
/// is deliberate and is what keeps these tests about the dispatcher: a real
/// resample is 89 ms apiece and `build_voxels` is `rustdar_radar`'s to test.
/// Everything below asserts about which targets the store is holding for this
/// pane, which a refusal answers as well as a grid does.
fn app_with_volume_loop(minutes: &[u32]) -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    app.render.ensure_pane_count(1);
    app.loop_mgr = LoopDownloadManager::new();
    for &m in minutes {
        app.loop_mgr
            .cache_scan(SITE, ts(m), std::sync::Arc::new(empty_scan()));
    }

    let pane = app.gui.pane_mut(0).expect("pane 0 exists");
    pane.site = SITE.to_string();
    pane.selected_product = PRODUCT;
    pane.selected_elevation = TILT;
    pane.set_kind(PaneKind::Volume);

    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::Volume);
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
    pane.loop_state = ls;
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

/// A picked box, distinct from the default one about the site — 20 km rather
/// than the full surveillance range, which is the resolution trade the region
/// picker exists to make.
fn region() -> VolumeRegion {
    VolumeRegion::new(
        GeoPoint {
            lat: 35.33,
            lon: -97.27,
        },
        20.0,
    )
    .expect("a finite centre and an in-range half-width")
}

/// Run dispatch until every frame has been offered a build, which at
/// `MAX_LOOP_VOLUME_BUILDS_PER_FRAME` per pass takes one pass per frame.
///
/// The `+ 2` is slack for the pass that finds everything already resident; a
/// loop that needed more than that would be one whose pacing does not converge,
/// and the assertions at the call sites would catch it as a short set.
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

/// The volume times every test here loops over. Named so `resident_times` can
/// enumerate the same set the loop was built from rather than guessing.
const MINUTES: [u32; 4] = [0, 5, 10, 15];

/// **The resident set equals the frame list.** For this loop kind they are one
/// thing, and nothing else in the codebase makes them so.
///
/// A plan-view loop holds `MAX_LOOP_FRAMES` and textures
/// `MAX_LOOP_RENDER_BUDGET` of them, dropping and re-rendering as the playhead
/// walks. Re-entering a resident 3D window costs ~140 ms (89 ms resample +
/// 51 ms upload) against the 200 ms interval at `DEFAULT_LOOP_SPEED_FPS`, so
/// that treadmill does not close here — which is why `loop_frame_budget` and
/// `loop_frames_held` both answer `MAX_LOOP_VOLUME_FRAMES` for a volume loop.
///
/// Reverting any of that shows up here: a render set smaller than the frame
/// list leaves the far frames unbuilt, and a `retain_set` that did not state
/// the whole list would let the store shed them as the later ones landed.
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

    // And the frames name them, which is what makes the playhead able to march
    // one: a store entry nothing points at is memory, not a loop.
    let frames = &app.gui.pane(0).expect("pane 0").loop_state.frames;
    for (idx, frame) in frames.iter().enumerate() {
        assert!(
            frame.render_failed || frame.image.is_some(),
            "frame {idx} was left neither named nor retired, so readiness \
             waits on it for ever",
        );
    }
}

/// **Release before build.** A region change invalidates the whole set, and the
/// store's seamless-swap rule would otherwise keep every old grid while the new
/// ones were built.
///
/// That rule is right for one grid — it is what stops a live 3D pane flashing
/// "Building…" every sealed sweep — and wrong for thirteen: 13 × 36.001 MiB
/// twice over is 936 MiB against a 512 MiB budget. So a set holder releases
/// first and accepts the first-build message for the fraction of a second that
/// costs.
///
/// The observable is the store's contents *at the moment the new key's first
/// build is dispatched*: no target keyed to the old region may still be there.
/// Dispatch is run one pass at a time so the assertion lands inside the
/// transition rather than after it has resolved itself.
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

    // The region drag. It writes straight through to the pane, exactly as the
    // product combo box does.
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
///
/// `extract_volume_parts` runs on the frame thread — the job wire carries a
/// `RenderInput`, not a `Scan` — so at most `MAX_LOOP_VOLUME_BUILDS_PER_FRAME`
/// of them may be paid per pass. But a pass over a settled loop must be free to
/// name every frame it finds already resident, or a thirteen-frame loop would
/// take thirteen frames to notice grids it already had, every time the playhead
/// moved.
///
/// `App::volume_extractions` counts the walks, and it is a `#[cfg(test)]`
/// counter on the App rather than a timing measurement, so this cannot be flaky.
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
///
/// The teardown `PaneState::set_kind` starts is pane-local; the store is keyed
/// by pane index and a `PaneState` cannot reach it. Without the host-side half,
/// 468 MiB stays allocated for a pane that has gone back to showing one live
/// volume — the 3D counterpart of the download queue that outlived its loop.
#[test]
fn switching_the_loop_off_gives_the_resident_set_back() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());
    assert_eq!(
        resident_times(&app).len(),
        MINUTES.len(),
        "precondition: a full set must be resident to be given back",
    );

    app.gui.pane_mut(0).expect("pane 0").loop_state = LoopPlaybackState::new();
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
///
/// The quiet half of the teardown above, and the one with no visible symptom
/// until a user tries it: while a 3D loop runs the pane paints the playhead's
/// frame and stops emitting `PrepareVolume`, so `VolumePane::rendered_for`
/// freezes at whatever it named when the loop started. Release the set without
/// clearing it and that key names a grid the store no longer holds — the
/// level-triggered ask never fires again, and the pane reads "Building the REF
/// volume…" for the rest of the session.
#[test]
fn switching_the_loop_off_lets_the_pane_ask_for_a_live_volume_again() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());
    // The key the live pane was left holding when the loop took over. Planted
    // rather than driven through a GUI pass, because what is under test is the
    // teardown's obligation to clear it, not how it came to be set.
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .volume_mut()
        .expect("a 3D pane")
        .rendered_for = Some(frame_target(MINUTES[0], None));

    app.gui.pane_mut(0).expect("pane 0").loop_state = LoopPlaybackState::new();
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

    // And a live 3D pane — one that never held a set — keeps its key, which is
    // what stops this clearing becoming a rebuild every frame.
    let mut live = headless(TestBridge::desktop());
    live.render.ensure_pane_count(1);
    let pane = live.gui.pane_mut(0).expect("pane 0");
    pane.set_kind(PaneKind::Volume);
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

/// The refusal path is terminal, and it is what stops a loop over volumes with
/// nothing to resample sitting in `Rendering` for the session.
///
/// Every scan in these fixtures is empty, so every build is refused — which is
/// why the tests above can assert about store contents without a real resample.
/// This is the assertion that makes that legitimate rather than accidental: a
/// refusal must retire the frame, exactly as an unrenderable sweep does on the
/// plan-view path.
#[test]
fn a_volume_with_nothing_to_resample_retires_its_frame() {
    let mut app = app_with_volume_loop(&MINUTES);
    dispatch_until_settled(&mut app, MINUTES.len());

    let frames = &app.gui.pane(0).expect("pane 0").loop_state.frames;
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

/// A 3D loop is capped at its **resident** frame count when the scan listing
/// lands, not at `MAX_LOOP_FRAMES`.
///
/// Sixty frames sampled down to thirteen is what makes the frame list and the
/// resident set the same thing on desktop. Without this the list would be sixty
/// long, the render set a thirteen-wide window inside it, and the loop would be
/// back on the treadmill it cannot afford — 89 ms of resample per playback
/// step, at a 200 ms interval.
#[test]
fn the_scan_listing_is_sampled_to_the_resident_frame_count() {
    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::Volume);
    ls.phase = LoopPhase::Rendering;
    let listing: Vec<_> = (0..MAX_LOOP_FRAMES + 20)
        .map(|i| {
            (
                // Minutes apart, which past an hour has to roll into the hour
                // rather than saturate — `ts` above takes a minute-of-hour and
                // this listing is longer than one.
                ts(0) + chrono::Duration::minutes(i64::try_from(i).expect("a small index")),
                rustdar_radar::archive::Identifier::new(format!("v{i}")),
            )
        })
        .collect();
    assert!(
        listing.len() > MAX_LOOP_FRAMES,
        "precondition: the listing must exceed even the plan-view cap, or the \
         sampling below is not exercised",
    );

    accept_scan_listing(&mut ls, SITE, listing.clone());
    assert_eq!(
        ls.frames.len(),
        MAX_LOOP_VOLUME_FRAMES,
        "a 3D loop took the plan-view frame count, so its frame list is longer \
         than its resident set can be",
    );

    // The plan-view loop is unchanged, which is what makes the assertion above
    // about the view rather than about the cap having moved for everyone.
    let mut plan = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::PlanView);
    plan.phase = LoopPhase::Rendering;
    accept_scan_listing(&mut plan, SITE, listing);
    assert_eq!(plan.frames.len(), MAX_LOOP_FRAMES);
}

/// The playing frame is what the pane paints, and it is a *grid* rather than a
/// raster — so `active_image` and `active_section_image` must both refuse it.
///
/// The loop path's view axis, applied to the third kind: a consumer that asked
/// for "the image" and was handed a volume frame would draw a plan view into a
/// 3D pane's box, which is the collision `LoopPlaybackState::view` exists to
/// stop.
#[test]
fn the_playing_frame_is_a_grid_and_no_raster_consumer_takes_it() {
    let mut app = app_with_volume_loop(&MINUTES);
    // A resident grid named on the playhead's frame, planted directly: what is
    // under test is which accessor answers, not how the frame was filled.
    let pane = app.gui.pane_mut(0).expect("pane 0");
    pane.loop_state.phase = LoopPhase::Playing;
    pane.loop_state.current_frame = 1;
    pane.loop_state.frames[1].image = Some(LoopFrameImage::Volume(
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
///
/// Every other fixture in this file is an `empty_scan`, so every build is
/// *refused* — and a refused frame is never named: `LoopFrameImage` stays
/// `None` and `render_failed` goes up instead. That is a state in which the
/// resident-set statement cannot go wrong, and it is why the defect
/// [`the_resident_set_survives_its_own_frames_landing`] pins survived a suite
/// that already claimed to cover it. A grid is the other half, and only a scan
/// carrying a moment over real elevation cuts produces one.
///
/// Two sweeps of eight radials, which is the smallest shape
/// `rustdar_radar::voxel::build_voxels` returns a grid for.
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

/// [`app_with_volume_loop`], over volumes that resample into real grids rather
/// than into refusals.
fn app_with_built_volume_loop(minutes: &[u32]) -> crate::app::App {
    let mut app = app_with_volume_loop(minutes);
    app.loop_mgr = LoopDownloadManager::new();
    for &m in minutes {
        app.loop_mgr
            .cache_scan(SITE, ts(m), std::sync::Arc::new(resamplable_scan(m)));
    }
    app
}

/// One dispatch pass, with the worker's replies taken delivery of exactly as
/// `App::poll_voxel_results` does.
///
/// The builds are real and run on the real job wire — `offload` spawns a thread
/// natively — so the pass waits for precisely the replies it dispatched and no
/// more. How many that is, is read off the store rather than guessed: a
/// `Building` entry is opened at dispatch and by nothing else.
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

/// **A frame landing must not take it out of its own loop's resident set.**
///
/// The dispatcher plans the set, hands it to `make_volume_frames_resident`, and
/// that pass states the whole thing through `VolumeStore::retain_set` — which
/// detaches the holder from *everything it did not name*. So the planned list
/// has to be the whole frame list, every pass, whatever state the frames are
/// in. Skipping a frame because it is already resident and already named drops
/// it out of the statement, and the next pass hands its grid back: the set is
/// eaten one frame at a time, from the front, as it is built.
///
/// What the user sees is the report this test was written from — a loop that
/// "sort of" plays and then shows the newest volume for every frame. The last
/// grid built is the only survivor (nothing is stated once every frame is
/// named, so the last statement stands), and `lookup_for_pane`'s same-scope
/// fallback quietly paints it under every other frame's caption.
///
/// This cannot be written against the refusal fixtures the rest of this file
/// uses: a refusal never names a frame, so the skip is never taken and the set
/// is always stated whole.
#[test]
fn the_resident_set_survives_its_own_frames_landing() {
    let mut app = app_with_built_volume_loop(&MINUTES);
    for _ in 0..MINUTES.len() + 3 {
        pass(&mut app);
    }

    let live = app.volume_store.live_ids();
    let resident = resident_times(&app);
    let frames = &app.gui.pane(0).expect("pane 0").loop_state.frames;
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
