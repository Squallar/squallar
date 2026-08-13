use super::*;
use crate::constants::MAX_LOOP_FRAMES;
use crate::loop_downloads::LoopDownloadManager;
use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};
use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState};
use rustdar_radar::archive::Identifier;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::RadarProduct;

/// `minute` minutes past midnight, and so still ordered past the hour — long
/// listings run to hundreds of scans.
fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minute as i64)
}

fn target(site: &str, elevation: f32) -> RenderTarget {
    RenderTarget::new(site, RadarProduct::Reflectivity, elevation)
}

fn identifier(name: &str) -> Identifier {
    Identifier::new(name.to_string())
}

/// A scan whose only sweeps sit at `elevations`, each carrying reflectivity.
///
/// Real data, not a stand-in: `find_closest_elevation` walks the sweeps and asks
/// each radial for the product's moment, so a scan without one answers `None` for
/// every selection and the sweep tests would pass vacuously.
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

/// [`scan_with_sweeps`] as the loop cache holds one. Nothing here reads a fold
/// limit, so the fixture volume declares none — which is what a Message 1
/// volume gives the cache in production too.
pub(super) fn volume_with_sweeps(elevations: &[f32]) -> crate::loop_downloads::CachedVolume {
    (scan_with_sweeps(elevations), Arc::default())
}

/// A loop on `site` with three frames, retargeted to Reflectivity at 0.5, and
/// with `textured` already rendered.
fn loop_on(ctx: &egui::Context, site: &'static str, textured: &[usize]) -> LoopPlaybackState {
    let mut ls = LoopPlaybackState::new_for_loop(
        3600,
        &RadarSite {
            name: site,
            lat: 35.0,
            lon: -97.0,
            heights: None,
        },
        rustdar_radar::types::RenderView::PlanView,
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
    ls.retarget_renders(RadarProduct::Reflectivity, 0.5);
    for &i in textured {
        let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
        ls.frames[i].image = Some(rustdar_egui::pane::LoopFrameImage::PlanView(
            rustdar_egui::pane::RadarImageData {
                texture: ctx.load_texture("test", image, egui::TextureOptions::NEAREST),
                lat: 35.0,
                lon: -97.0,
                max_range_km: 100.0,
                nyquist_ms: None,
                melting_layer_source: None,
                hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
            },
        ));
    }
    ls
}

/// A successful render result for `timestamp` at `target`.
///
/// The coordinates are KTLX's real ones, which `loop_on` deliberately does *not*
/// use — its loops are built at a round 35.0/-97.0. Anything that placed an
/// image from a loop's own geometry rather than from the response would produce
/// those round numbers instead.
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
        // `Some`, not `None`: a response carrying no image is retired as
        // `render_failed`, so `None` is the *failure* fixture and has to be
        // asked for deliberately. The pixels never matter here — every seam
        // under test reads the metadata — so a 1x1 image stands in for a frame.
        image: Some(egui::ColorImage::filled([1, 1], egui::Color32::WHITE)),
        max_range_km: 100.0,
        nyquist_ms: None,
        melting_layer_source: None,
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

/// The behaviour the dedup exists for: one render serves both panes.
#[test]
fn a_queued_render_for_the_same_target_suppresses_a_duplicate() {
    let q = [queued(target("KTLX", 0.5), ts(0), 0.48)];
    assert!(render_already_queued(
        q.iter(),
        ts(0),
        &target("KTLX", 0.5),
        0.48
    ));
    // Selection jitter within tolerance is the same target.
    assert!(render_already_queued(
        q.iter(),
        ts(0),
        &target("KTLX", 0.505),
        0.48
    ));
}

/// The defect: suppressing here promises a broadcast that
/// `frame_accepting_broadcast` refuses across sites, leaving the frame served by
/// neither path — and pushing the pane into the site-blind clone path instead.
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
    // Same target, but the two scans resolved the selection to different sweeps,
    // so the images differ.
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

/// The coupling this file's `render_already_queued` docs describe, tested where
/// both halves are in scope. Suppressing a pane's render is a promise that the
/// queued render's result will be handed to it, so the two must agree for every
/// sweep — including when the receiver's own scan snaps the selection somewhere
/// else. A sweep-blind acceptance breaks it in the dangerous direction: not
/// suppressed (so the pane renders its own) yet accepted (so that render is
/// dropped as redundant and an image of the wrong tilt stays put).
#[test]
fn suppression_and_acceptance_weigh_the_same_sweep() {
    let ctx = egui::Context::default();
    let receiver = loop_on(&ctx, "KTLX", &[]);
    let want = receiver.rendered_for.clone().expect("target adopted");
    // A sibling's render of the 0.48° sweep, queued this pass.
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

    // Not the trivial agreement of "both always refuse".
    assert!(render_already_queued(q.iter(), ts(0), &want, 0.48));
}

#[test]
fn a_queued_render_for_another_product_suppresses_nothing() {
    let q = [queued(target("KTLX", 0.5), ts(0), 0.48)];
    let velocity = RenderTarget::new("KTLX", RadarProduct::Velocity, 0.5);
    assert!(!render_already_queued(q.iter(), ts(0), &velocity, 0.48));
}

/// The wiring the donor search exists to get right: every candidate is judged
/// against the *receiver's* target. Judging each against its own would compare it
/// to itself, always agree, and put a KTLX image into a KOUN loop.
#[test]
fn a_donor_is_judged_against_the_receiving_panes_target() {
    let ctx = egui::Context::default();
    let ktlx = loop_on(&ctx, "KTLX", &[1]);
    let koun = loop_on(&ctx, "KOUN", &[]);
    let loops = [(0usize, &ktlx), (1usize, &koun)];

    // Pane 1 (KOUN) asks. Pane 0 has the frame textured, but on another site.
    assert_eq!(
        find_donor(loops, 1, ts(1), koun.rendered_for.as_ref().unwrap()),
        None,
        "a KTLX loop must not serve a KOUN loop"
    );
    // The same candidate judged against its own target would have agreed.
    assert_eq!(
        find_donor(loops, 1, ts(1), ktlx.rendered_for.as_ref().unwrap()),
        Some((0, 1)),
        "precondition: only the target argument distinguishes these"
    );
}

/// The blocking defect. A scan listing cannot be cancelled, so one requested
/// before a site switch lands after the loop has been rebuilt for the new site.
/// Taking it puts the old radar's timestamps in the frame list and the old
/// radar's identifiers in the download queue — which are then labelled with the
/// *new* site, cached under it, and rendered with its geometry. Nothing
/// downstream can see it, and because the download filter treats that key as
/// satisfied, the real scans that would correct it are discarded on arrival.
#[test]
fn a_listing_for_the_site_the_loop_left_is_refused() {
    let ctx = egui::Context::default();
    let mut koun = loop_on(&ctx, "KOUN", &[]);
    koun.frames.clear();
    let stale = vec![(ts(0), identifier("KTLX20240101_000000_V06"))];

    assert!(
        accept_scan_listing(
            test_loop_allocation(),
            &test_budgets(),
            &mut koun,
            "KTLX",
            stale
        )
        .is_none(),
        "a KTLX listing is not this KOUN loop's frame list"
    );
    assert!(koun.frames.is_empty(), "and left no frames behind");

    // The loop's own listing is taken.
    let live = vec![(ts(0), identifier("KOUN20240101_000000_V06"))];
    let plan = accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut koun,
        "KOUN",
        live,
    )
    .expect("its own listing");
    assert_eq!(
        plan.site, "KOUN",
        "the plan carries the site it was listed for"
    );
    assert_eq!(plan.frames.len(), 1);
    assert_eq!(koun.frames.len(), 1);
}

/// A listing that arrives after the loop was switched off has nothing to fill.
#[test]
fn a_listing_for_an_inactive_loop_is_refused() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.phase = LoopPhase::Inactive;

    let scans = vec![(ts(0), identifier("KTLX20240101_000000_V06"))];
    assert!(
        accept_scan_listing(
            test_loop_allocation(),
            &test_budgets(),
            &mut ls,
            "KTLX",
            scans
        )
        .is_none()
    );
}

/// The wedge. A failed listing is delivered as an empty list, and so is a
/// window the site served nothing for. Advancing to `Rendering` with no frames
/// is a state nothing leaves: readiness skips loops with no frames,
/// `any_loop_active` reads false so the app stops repainting, nothing retries,
/// and the pane draws its (nonexistent) loop frames instead of its static
/// image for the rest of the session.
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
            Vec::new()
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

/// A loop in `Rendering` whose frames have all been ruled out — every scan
/// carries no sweep for the selected product — is the same dead end reached
/// from the other side: readiness needs a rendered frame to promote it, and
/// there will never be one.
#[test]
fn a_loop_no_frame_of_which_can_render_is_switched_off() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    for frame in &mut ls.frames {
        frame.render_failed = true;
    }
    let mgr = LoopDownloadManager::new();

    assert!(
        settle_loop_phase(&mgr, 0, &mut ls, test_loop_allocation().plan_view_frames),
        "the caller has to release this pane's loop state"
    );
    assert!(!ls.is_active());
}

/// …but not while its scans are still arriving. A frame with no scan yet is
/// "settled" as far as rendering goes — nothing is in flight for it *yet* — so
/// a check that only asked the render side would abandon every loop on the
/// pass right after its last download batch was dispatched.
#[test]
fn a_loop_still_waiting_on_its_scans_is_left_alone() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    let mut mgr = LoopDownloadManager::new();
    mgr.mark_in_flight("KTLX", ts(0));

    assert!(!settle_loop_phase(
        &mgr,
        0,
        &mut ls,
        test_loop_allocation().plan_view_frames
    ));
    assert_eq!(ls.phase, LoopPhase::Rendering, "still working");

    // Undispatched downloads hold it open too.
    let mut mgr = LoopDownloadManager::new();
    mgr.insert_pending(
        0,
        PendingDownloads {
            site: "KTLX".to_string(),
            queue: [(ts(1), identifier("KTLX20240101_000100_V06"))]
                .into_iter()
                .collect(),
        },
    );
    assert!(!settle_loop_phase(
        &mgr,
        0,
        &mut ls,
        test_loop_allocation().plan_view_frames
    ));
    assert_eq!(ls.phase, LoopPhase::Rendering);
}

/// One rendered frame is still enough to play, whatever became of the rest.
#[test]
fn a_loop_with_something_to_show_is_promoted_rather_than_abandoned() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[1]);
    ls.frames[0].render_failed = true;
    ls.frames[2].render_failed = true;
    let mgr = LoopDownloadManager::new();

    assert!(!settle_loop_phase(
        &mgr,
        0,
        &mut ls,
        test_loop_allocation().plan_view_frames
    ));
    assert_eq!(ls.phase, LoopPhase::Ready);
}

/// The frame list and the frame *plan* are the two halves of one decision:
/// every frame must be in the plan, or nothing ever fetches its data, it never
/// settles and the loop hangs in `Rendering`. That has to survive the sampling
/// that caps long listings.
///
/// The plan, not a download queue: which bytes a frame needs depends on the
/// pane's product, and switching between a Level II and a Level III product
/// re-derives the queue from this same plan rather than re-listing. So the
/// agreement being pinned is frames-to-plan; `plan_downloads_for` is what turns
/// the plan into one queue or the other, and
/// `a_level3_loop_queues_a_pairing_per_frame_and_no_volume_downloads` pins that
/// half.
///
/// Taking the listing also has to *advance* the phase. This is the one fixture
/// that starts where a real loop starts — `FetchingScanList`, set by
/// `new_for_loop` and left there until its listing lands — so a missing advance
/// reads as a loop still fetching rather than as a value already in place.
/// Left in `FetchingScanList`, `is_fetching()` never goes false: the pane keeps
/// its "fetching" label and keeps asking for continuous repaints forever.
#[test]
fn the_frame_list_and_the_frame_plan_describe_the_same_scans() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.phase = LoopPhase::FetchingScanList;
    assert!(
        ls.is_fetching(),
        "precondition: a loop awaiting its listing"
    );

    let scans: Vec<_> = (0..(MAX_LOOP_FRAMES as u32 + 40))
        .map(|i| (ts(i), identifier(&format!("KTLX2024010{}_V06", i))))
        .collect();

    let plan = accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut ls,
        "KTLX",
        scans,
    )
    .expect("accepted");

    assert_eq!(plan.frames.len(), MAX_LOOP_FRAMES, "capped");
    assert_eq!(
        ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
        plan.frames.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        "the sampled set is the frame list, frame for frame"
    );
    assert_eq!(
        ls.current_frame,
        ls.frames.len() - 1,
        "playback starts at the newest"
    );
    assert_eq!(ls.phase, LoopPhase::Rendering);
    assert!(
        !ls.is_fetching(),
        "and the loop has stopped reading as fetching"
    );
}

/// The cap has to *sample* the window, not truncate it. Taking the first
/// `MAX_LOOP_FRAMES` or the last `MAX_LOOP_FRAMES` satisfies the cap and the
/// frames-vs-queue agreement above equally well, and gives a loop that animates
/// only the oldest or the newest slice of the lookback the user asked for —
/// which plays smoothly and looks entirely correct.
#[test]
fn a_long_listing_is_sampled_evenly_across_its_whole_span() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    // Several times the cap, and not a multiple of it, so no exact stride
    // exists and the endpoints still have to be deliberate.
    let total = MAX_LOOP_FRAMES * 3 + 7;
    let scans: Vec<_> = (0..total as u32)
        .map(|i| (ts(i), identifier(&format!("KTLX2024010{}_V06", i))))
        .collect();

    accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut ls,
        "KTLX",
        scans,
    )
    .expect("accepted");

    // `ts` is one minute per listing position, so a frame's minute *is* the
    // position it was sampled from, and the gaps below are index strides.
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

/// **One scan over the cap is already a sample, and the loop records it as
/// one.** The whole fidelity claim in the timeline caption rests on this flag,
/// and it is decided here and nowhere else.
///
/// The boundary is the point. The rule the caption used before compared the
/// frame list's median gap against the listing's, which cannot see a sample at
/// all until two-step gaps are the majority — so everything from one scan over
/// the cap up to about 1.5x it read as "every scan". This walks the boundary:
/// exactly the cap is not a sample, and one more is.
///
/// The cap is taken from `Budgets::loop_frames_held`, which is what
/// `loop_frames_held` resolves a raster loop's cap from — not from
/// [`MAX_LOOP_FRAMES`], which is the same figure today and is no longer the one
/// the code reads. The unit half in `rustdar_egui::ui_timeline` has to write the
/// caps out as literals because neither `crate::budget` nor `crate::constants`
/// is visible from that crate; this is what would catch it drifting from either.
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
        let scans: Vec<_> = (0..listed as u32)
            .map(|i| (ts(i), identifier(&format!("KTLX2024010{}_V06", i))))
            .collect();

        accept_scan_listing(
            test_loop_allocation(),
            &test_budgets(),
            &mut ls,
            "KTLX",
            scans,
        )
        .expect("accepted");

        assert_eq!(
            ls.listing_sampled,
            Some(expected),
            "a listing of {listed} against a cap of {cap} kept {} frames and \
             recorded {:?}",
            ls.frames.len(),
            ls.listing_sampled,
        );
    }
}

/// A loop that has taken no listing yet claims nothing about fidelity.
///
/// `None` is a third state and not a synonym for `Some(false)`: a loop still in
/// `FetchingScanList` holds frames from no listing at all, and a caption reading
/// "every scan" off that would be making the claim before there was anything to
/// claim about.
#[test]
fn a_loop_that_has_taken_no_listing_records_no_fidelity() {
    let ctx = egui::Context::default();
    assert_eq!(loop_on(&ctx, "KTLX", &[]).listing_sampled, None);
}

/// The coordinates an image is placed at come off the response — the ones the
/// renderer was actually handed — never off the loop receiving it.
///
/// In production the two agree, but only via a coupling that lives in another
/// type: `site_lat`/`site_lon` move only in `new_for_loop`, which also clears
/// `rendered_for`, so a site change makes the target check reject the result
/// before any coordinate is read. That is an argument, not a guarantee, it is
/// invisible at the point of use, and it has to be re-made for every sibling
/// pane the broadcast hands the same texture to. Carrying the values retires the
/// argument; this test retires the way back to it.
#[test]
fn a_rendered_frame_is_placed_where_the_render_actually_drew_it() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.frames[1].render_in_flight = true;
    let mut rr = response(ts(1), ls.rendered_for.clone().expect("target adopted"));

    assert_ne!(
        rr.site_lat, ls.site_lat,
        "precondition: the two sources differ"
    );
    assert_ne!(rr.site_lon, ls.site_lon);

    let texture = accept_render_result(&mut ls, &mut rr, None, |_| dummy_texture(&ctx))
        .expect("the loop is awaiting this result");

    let image = ls.frames[1]
        .image
        .as_ref()
        .and_then(rustdar_egui::pane::LoopFrameImage::plan_view)
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

    // The same image, described identically, is what the broadcast hands on — so
    // a sibling taking it is told where it was drawn rather than assuming.
    let broadcast = rendered_image(&rr, &texture, None);
    assert_eq!((broadcast.lat, broadcast.lon), (image.lat, image.lon));
}

/// A result the loop has retargeted away from is refused, and refusing it must
/// cost nothing: the upload is the expensive half and must not run for an image
/// that is about to be dropped.
#[test]
fn a_refused_result_is_never_uploaded() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    // In flight, so only the target can be what refuses this.
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

/// No image means the render found no matching sweep. The frame is retired
/// rather than left in flight, or the dispatcher retries it forever and readiness
/// never stops waiting on it.
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

/// A finished download is filed under the site it was fetched from, which the
/// response carries. The requesting pane is not consulted — its loop may have
/// been rebuilt for another site while the download ran, and filing under that
/// site is exactly the corruption this key exists to prevent.
#[test]
fn a_download_is_cached_under_the_site_it_came_from() {
    let mut mgr = LoopDownloadManager::new();
    let volume = volume_with_sweeps(&[0.5]);
    mgr.mark_in_flight("KTLX", ts(0));

    apply_completed_download(
        &mut mgr,
        crate::channels::LoopScanDownloadResponse {
            pane_idx: 0,
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

/// A failed download still clears the mark, or the timestamp is never retried.
#[test]
fn a_failed_download_clears_its_mark_and_caches_nothing() {
    let mut mgr = LoopDownloadManager::new();
    mgr.mark_in_flight("KTLX", ts(0));

    apply_completed_download(
        &mut mgr,
        crate::channels::LoopScanDownloadResponse {
            pane_idx: 0,
            site: "KTLX".to_string(),
            timestamp: ts(0),
            scan: None,
        },
    );

    assert!(!mgr.is_in_flight("KTLX", &ts(0)));
    assert!(!mgr.is_cached("KTLX", &ts(0)));
}

/// The data a frame renders is named by the target it is rendered for, because
/// that is where the geometry came from.
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

/// The sharpest half of the broadcast check: the receiver's sweep has to be
/// resolved from the receiver's *own* scan. Answered with the sender's snapped
/// angle it would compare a value to itself, agree unconditionally, and the
/// sweep term would be decorative.
#[test]
fn the_receivers_sweep_comes_from_the_receivers_own_scan() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    // One timestamp, two sites, two different sweep sets — which is the whole
    // reason two loops can disagree about what a selection resolves to.
    mgr.cache_scan("KTLX", ts(0), volume_with_sweeps(&[0.5, 1.5]));
    mgr.cache_scan("KOUN", ts(0), volume_with_sweeps(&[1.4]));

    let ktlx = loop_on(&ctx, "KTLX", &[]);
    let koun = loop_on(&ctx, "KOUN", &[]);

    assert_eq!(
        own_sweep(&mgr, &ktlx, ts(0), RadarProduct::Reflectivity, 0.5),
        Some(0.5),
        "KTLX's scan carries the selected sweep"
    );
    assert_eq!(
        own_sweep(&mgr, &koun, ts(0), RadarProduct::Reflectivity, 0.5),
        Some(1.4),
        "KOUN's own scan snaps the same selection somewhere else"
    );
}

/// And the pair the response path actually builds. Both halves are pinned to
/// values nothing else in the call could supply:
///
/// - `rendered` is the *snapped* sweep off the response, never the selection the
///   target carries. Here the two are 1.4 and 0.5, so a `rendered` filled in from
///   `target.elevation` reads as the wrong tilt rather than as the same number.
/// - `own` is resolved from the receiver's own scan against the *selection*. Fed
///   the sender's snapped angle instead it would agree with itself, and the sweep
///   test would pass for every image regardless of the tilt it depicts. KOUN's
///   scan carries both angles precisely so that substitution changes the answer.
#[test]
fn a_broadcast_sweep_pairs_the_senders_image_with_the_receivers_own_scan() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    // One timestamp, two sites. KOUN's volume carries the selected 0.5° sweep and
    // a 1.4°; KTLX's is a partial volume whose only reflectivity sweep is the
    // 1.4°, so the same 0.5° selection snaps to a different tilt on each.
    mgr.cache_scan("KOUN", ts(0), volume_with_sweeps(&[0.5, 1.4]));
    mgr.cache_scan("KTLX", ts(0), volume_with_sweeps(&[1.4]));
    let koun = loop_on(&ctx, "KOUN", &[]);

    // A finished render of the 1.4° sweep, for a 0.5° selection. The target's
    // site is not read here on purpose — `own_sweep` looks the scan up under the
    // *receiving* loop's site, which is the whole point — so one response can be
    // offered to both loops below.
    //
    // It carries an image (`response`'s default, and see the note there): a
    // response with none is retired as `render_failed` before the broadcast loop
    // is reached, so a `None` fixture would put `broadcast_sweep` in a state the
    // response path never hands it.
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

    // Same call, a receiver whose scan does snap where the image was rendered.
    let ktlx = loop_on(&ctx, "KTLX", &[]);
    let sweep = broadcast_sweep(&mgr, &ktlx, &rr);
    assert_eq!(sweep.own, Some(1.4));
    assert!(sweep.agrees(), "and this one takes it");
}

/// No scan, or no sweep for the product, means the receiver cannot check the
/// image — which refuses the broadcast rather than accepting on faith.
#[test]
fn a_receiver_with_nothing_to_compare_reports_no_sweep() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    let ktlx = loop_on(&ctx, "KTLX", &[]);

    assert_eq!(
        own_sweep(&mgr, &ktlx, ts(0), RadarProduct::Reflectivity, 0.5),
        None,
        "nothing downloaded for this frame yet"
    );

    mgr.cache_scan("KTLX", ts(0), volume_with_sweeps(&[0.5]));
    assert_eq!(
        own_sweep(&mgr, &ktlx, ts(0), RadarProduct::Velocity, 0.5),
        None,
        "the scan carries no sweep for this product"
    );
}

/// Readiness asks "has this frame's scan downloaded" about the loop's own site.
/// Site-blind, another radar's scan at the same timestamp answers yes, and the
/// loop is promoted over frames that will never render.
#[test]
fn readiness_counts_only_this_loops_own_downloads() {
    let ctx = egui::Context::default();
    let mut mgr = LoopDownloadManager::new();
    let koun = loop_on(&ctx, "KOUN", &[]);
    // Every frame blank, and only *KTLX* scans downloaded.
    for i in 0..3 {
        mgr.cache_scan("KTLX", ts(i), volume_with_sweeps(&[0.5]));
    }

    assert!(
        loop_batch_settled(&mgr, &koun, test_loop_allocation().plan_view_frames),
        "precondition: with no scan of its own, a blank frame is not waiting on a render"
    );

    // Now KOUN's own scans arrive: the same blank frames become renders that
    // are owed, and readiness must wait for them.
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
    // Pane 0 asking for the same frame is not offered its own texture.
    assert_eq!(find_donor(loops, 0, ts(2), want), None);
    // Nobody has a frame at this timestamp textured.
    assert_eq!(find_donor(loops, 1, ts(0), want), None);
}

/// `target.elevation` is the pane's selection; `snapped` is the sweep this frame's
/// scan actually carries. `find_sweep` only matches within 0.05°, so handing the
/// renderer the selection retires every frame whose nearest sweep is further away.
#[test]
fn the_renderer_is_given_the_snapped_sweep_not_the_selection() {
    // A selection of 0.5 that snapped to a 1.4° sweep — well outside find_sweep's
    // 0.05° window, so the two are not interchangeable.
    let req = queued(target("KTLX", 0.5), ts(0), 1.4);
    let params = req.render_params();

    assert_eq!(params.elevation, 1.4, "the sweep the scan carries");
    assert_ne!(params.elevation, req.target.elevation);
    assert_eq!(params.product, RadarProduct::Reflectivity);
    assert_eq!(params.lat, 35.0);
    assert_eq!(params.lon, -97.0);
}

/// **The render set is the span budget at the site's own cadence, and the
/// same budget is the same wall clock on every radar.**
///
/// The three cadences the campaign of 2026-08-11 measured, against the frames
/// each buys: a TDWR volume is 360 s on both VCP 80 and VCP 90, a WSR-88D
/// precip volume 259 s, a WSR-88D clear-air volume 517 s. The frame counts
/// differ by more than 2x and the wall clock does not, which is the whole
/// point — before this the counts were equal and it was the wall clock that
/// differed by 2x, with nothing on screen saying so.
///
/// Driven through [`loop_render_budget`] rather than through
/// `Budgets::frames_for_span` directly, because the minimum with the pool's
/// share is the part that could be dropped without any budget test noticing.
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
        ls.scan_step_secs = Some(cadence);
        let frames = loop_render_budget(allocation, &ls, &budgets);
        assert!(
            frames >= crate::constants::MIN_LOOP_FRAMES_PER_PANE,
            "{radar}: {frames} frames is not a loop"
        );
        // The pool's share can bind first on a crowded screen; this is the
        // idle allocation, so the span is what binds unless the arm ran out of
        // frames before it ran out of budget.
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

/// A loop that has not learned a cadence yet keeps the whole render budget.
///
/// `scan_step_secs` is `None` from `new_for_loop` until a listing is accepted,
/// and again after a pane really changes radar, which replaces the whole state.
/// There is no honest conversion without it, so the loop behaves exactly as it
/// did before the span budget existed — erring the other way, by assuming the
/// fastest radar, would make a loop visibly shed frames a second after opening.
///
/// Re-picking the site a pane is already on keeps the loop *and* its cadence,
/// which is a different case and the right answer for it: the figure describes
/// a radar the pane has not left. See `SwitchRadarSite`'s `left_a_radar` gate.
#[test]
fn a_loop_that_has_not_learned_a_cadence_keeps_the_whole_budget() {
    let ctx = egui::Context::default();
    let budgets = test_budgets();
    let ls = loop_on(&ctx, "KTLX", &[]);
    assert_eq!(
        ls.scan_step_secs, None,
        "precondition: a freshly built loop knows nothing about its site's cadence"
    );
    assert_eq!(
        loop_render_budget(test_loop_allocation(), &ls, &budgets),
        test_loop_allocation().frames_for(ls.view),
        "a loop with no cadence is held only by the pool's share"
    );
}

/// **A listing teaches the loop its cadence before anything spends against it.**
///
/// `accept_scan_listing` writes `scan_step_secs` from the median gap of the
/// *unsampled* listing and then computes the frame count — so the very first
/// dispatch already has the site's own figure, and a 3D loop's frame list, which
/// **is** its resident set, is sized by it in the same call rather than a poll
/// later. A gap in the listing is what the median is a median for: one missing
/// scan doubles one gap and moves nothing.
#[test]
fn a_listing_teaches_the_cadence_before_the_frame_count_is_spent() {
    let ctx = egui::Context::default();
    let mut ls = loop_on(&ctx, "KTLX", &[]);
    ls.phase = LoopPhase::FetchingScanList;
    // Six-minute volumes, TDWR's own cadence, with the fourth scan missing —
    // one gap of twelve minutes among eleven of six.
    let scans: Vec<_> = (0..13u32)
        .filter(|i| *i != 4)
        .map(|i| (ts(i * 6), identifier(&format!("KTLX2024010{i}_V06"))))
        .collect();

    accept_scan_listing(
        test_loop_allocation(),
        &test_budgets(),
        &mut ls,
        "KTLX",
        scans,
    )
    .expect("accepted");

    assert_eq!(
        ls.scan_step_secs,
        Some(360),
        "the median rides over the missing scan rather than being pulled by it"
    );
    let budgets = test_budgets();
    assert_eq!(
        budgets.frames_for_span(ls.scan_step_secs),
        (1 + budgets.loop_span_secs / 360).min(budgets.loop_render_budget),
        "and the count spends that figure, not the arm's ceiling"
    );
}

/// **Every cap `rustdar_egui::ui_timeline`'s fidelity fixtures name is a cap
/// this workspace actually ships.**
///
/// That crate cannot see `crate::budget` or `crate::constants`, so its
/// measured-defect table writes the browser and desktop raster caps as bare
/// numbers. Its doc names this test as the half that reads the resolved budget,
/// and this is that half: the `held` column of the table, against the three
/// shipped arms of `MAX_LOOP_FRAMES`.
///
/// # It is here because the previous guard could not have worked
///
/// The job used to belong to
/// [`a_listing_one_scan_over_the_cap_is_recorded_as_sampled`], which asserts
/// `test_budgets().loop_frames_held == MAX_LOOP_FRAMES`. Both sides of that are
/// the arm *this build compiled* — desktop, on every host — so it is silent
/// about the browser row by construction. When `LOOP_SPAN_BUDGET_SECS` priced a
/// browser's 45 minutes at 14 frames and took `WASM_MAX_LOOP_FRAMES` from 12 to
/// 14, the table went on quoting "12 of a 17-scan listing, 29.4% dropped" — a
/// sentence about no shipped configuration — and every test in both crates
/// stayed green. Reading the *named arms* rather than the compiled one is the
/// whole difference.
///
/// What it does not check is the fixture array beneath the table, which is a
/// second hand-written copy of the same rows. That is stated rather than
/// papered over: this catches a cap that describes no arm, which is the failure
/// that happened twice, and not a table and a fixture drifting from each other.
#[test]
fn the_caption_fixtures_name_caps_this_workspace_ships() {
    const HEADER: &str = "| target | listing | held | dropped |";
    let source = include_str!("../../../rustdar-egui/src/ui_timeline/tests.rs");
    let caps: Vec<usize> = source
        .lines()
        .map(|line| line.trim().trim_start_matches("///").trim())
        .skip_while(|line| *line != HEADER)
        .skip(2) // the header and the alignment rule
        .take_while(|line| line.starts_with('|'))
        .map(|row| {
            let cells: Vec<&str> = row
                .split('|')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .collect();
            cells[2]
                .parse()
                .unwrap_or_else(|_| panic!("the `held` cell of {row:?} is not a frame count"))
        })
        .collect();
    assert_eq!(
        caps.len(),
        2,
        "the measured-defect table is no longer two rows this can read: {caps:?}",
    );

    let shipped: Vec<usize> = crate::budget::BudgetLimits::SHIPPED
        .iter()
        .map(|limits| limits.loop_frames_held.floor)
        .collect();
    for cap in caps {
        assert!(
            shipped.contains(&cap),
            "`ui_timeline`'s fidelity table quotes a {cap}-frame raster cap, and \
             the arms this workspace ships are {shipped:?}. The table is a claim \
             about a measured defect on a real target, so a cap belonging to \
             none of them makes it a claim about nothing — re-derive the row \
             against the arm that moved.",
        );
    }
}

// ── A hover over a looping pane ─────────────────────────────────────────────

/// A one-sweep volume at 0.5° with 360 radials of 300 reflectivity gates, whose
/// bytes vary in both axes so a wrong gate is visible in the number.
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

/// **Hovering a looping pane gives a number.**
///
/// It did not. Every loop frame shipped an empty value grid on purpose —
/// `MAX_LOOP_RENDER_BUDGET × side² × 4` was not affordable and still is not —
/// so the readout went quiet the moment a loop started and came back when it
/// stopped, over the same pixel of the same sweep. That was the defect this
/// change is for.
///
/// What makes it affordable is that the frame does not have to *carry* the
/// numbers. The loop's download cache is already holding every frame's volume
/// for as long as the loop lives, so the frame keeps 5.8 KiB of geometry and an
/// `Arc`, and [`frame_gates`] is the lookup that pairs them.
///
/// This fails without that pairing: with no volume attached, every point inside
/// the picture reads [`rustdar_radar::hover::Reading::NotResident`], which the
/// second half asserts.
#[test]
fn hovering_a_looping_pane_reads_a_value_out_of_the_frames_own_volume() {
    let (lat, lon) = (35.3333, -97.2778);
    let scan = scan_with_echo();

    // The geometry a loop frame carries: the render's own, with the numbers
    // stripped exactly as `deliver` strips them.
    let mut polar = rustdar_radar::render::render_radar_to_image(
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
        RenderTarget {
            site: "KTLX".into(),
            product: RadarProduct::Reflectivity,
            elevation: 0.5,
        },
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

    // Every point the still render has a number for, the looping frame has the
    // same number for.
    let mut read = 0u32;
    let mut az = 0.5f64;
    while az < 360.0 {
        let mut km = 1.0f64;
        while km < 70.0 {
            let looping = img.hover.read(az, km);
            assert_eq!(
                looping,
                resident.geometry().pick(az, km).map_or(
                    rustdar_radar::hover::Reading::Unpainted,
                    |at| resident.at(at).map_or(
                        rustdar_radar::hover::Reading::Unpainted,
                        rustdar_radar::hover::Reading::Value,
                    )
                ),
                "({az}°, {km} km)",
            );
            if matches!(looping, rustdar_radar::hover::Reading::Value(_)) {
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

    // And with no volume attached — a frame whose scan has been evicted, or a
    // product computed rather than measured — the readout says so rather than
    // reading as a blank sky.
    let orphan = rendered_image(&rr, &texture, None);
    assert_eq!(
        orphan.hover.read(90.0, 20.0),
        rustdar_radar::hover::Reading::NotResident,
    );
}
