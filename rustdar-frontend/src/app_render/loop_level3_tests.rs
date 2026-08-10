use super::*;
use crate::loop_downloads::{L3FrameState, LoopDownloadManager};
use nexrad_level3::model::{Level3Message, MessageHeader, ProductDescriptionBlock};
use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState};
use rustdar_radar::archive::Identifier;
use rustdar_radar::level3::{Level3Product, ProductStamp};
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::RadarProduct;

const SITE: &str = "KTLX";
/// Echo tops: one AWIPS code (`EET`), and the product whose loop this
/// exercises. Its `level3_products()` is read rather than the literal, so a
/// change to the mapping cannot leave these tests pairing a code the app no
/// longer fetches.
const L3: RadarProduct = RadarProduct::EchoTops;
const L2: RadarProduct = RadarProduct::Reflectivity;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minute as i64)
}

fn codes(product: RadarProduct) -> &'static [&'static str] {
    product
        .level3_products()
        .expect("a Level III product names its codes")
}

/// A frame plan for `n` volumes one minute apart, as `accept_scan_listing`
/// builds one.
fn plan(n: u32) -> crate::loop_downloads::FramePlan {
    crate::loop_downloads::FramePlan::new(
        SITE.to_string(),
        (0..n)
            .map(|i| {
                (
                    ts(i),
                    Identifier::new(format!("KTLX20240101_00{i:02}00_V06")),
                )
            })
            .collect(),
    )
}

/// A decoded object whose PDB reports `elevation_tenths / 10` degrees. Only
/// the fields the loop reads carry anything — no symbology, since nothing here
/// renders.
fn object(elevation_tenths: i16) -> Arc<Level3Product> {
    let pdb = ProductDescriptionBlock {
        block_divider: -1,
        latitude: 35.33,
        longitude: -97.27,
        height: 1200,
        product_code: 135,
        operational_mode: 2,
        vcp: 212,
        sequence_number: 0,
        volume_scan_number: 1,
        volume_scan_date: 19723,
        volume_scan_time: 0,
        generation_date: 19723,
        generation_time: 90,
        product_specific_1: 0,
        product_specific_2: 0,
        elevation_number: 1,
        product_specific_3: elevation_tenths,
        thresholds: [0; 16],
        product_specific_47_53: [0; 7],
        version: 0,
        spot_blank: 0,
        symbology_offset: 60,
        graphic_offset: 0,
        tabular_offset: 0,
    };
    Arc::new(Level3Product {
        message: Level3Message {
            header: MessageHeader {
                message_code: 135,
                date_of_message: 19723,
                time_of_message: 90,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb,
            symbology: None,
        },
        stamp: ProductStamp::from_key("TLX_EET_2024_01_01_00_01_30"),
        bytes: Arc::new(Vec::new()),
    })
}

/// A loop on [`SITE`] with `n` frames, retargeted to `product`.
fn loop_for(product: RadarProduct, n: u32) -> LoopPlaybackState {
    let mut ls = LoopPlaybackState::new_for_loop(
        3600,
        &RadarSite {
            name: SITE,
            lat: 35.33,
            lon: -97.27,
            heights: None,
        },
        rustdar_radar::types::RenderView::PlanView,
    );
    ls.phase = LoopPhase::Rendering;
    ls.frames = (0..n)
        .map(|i| LoopFrame {
            timestamp: ts(i),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    ls.retarget_renders(product, 0.5);
    ls
}

/// The core of the feature. A Level III loop's frames are the *same* volume
/// timeline a Level II loop's are — which is what keeps a mixed set of panes
/// animating in step, since they share one clock — but what each frame needs
/// downloaded is a bucket object per AWIPS code, not the ~10 MB archive volume.
///
/// Both halves are asserted. Queuing the pairings without dropping the volume
/// queue would work, animate correctly, and quietly spend a volume download per
/// frame on bytes no render reads.
#[test]
fn a_level3_loop_queues_a_pairing_per_frame_and_no_volume_downloads() {
    let mut mgr = LoopDownloadManager::new();
    mgr.set_plan(0, plan(4));

    assert!(mgr.plan_downloads_for(0, L3), "the first plan is a change");

    let pending = mgr
        .extract_pending_l3(0)
        .expect("a Level III product owes pairings");
    assert_eq!(pending.site, SITE, "the site travels with the queue");
    assert_eq!(pending.product, L3);
    assert_eq!(
        pending.queue.len(),
        4 * codes(L3).len(),
        "one pairing per frame per AWIPS code",
    );
    assert_eq!(
        pending.queue.front().map(|(t, c)| (*t, c.clone())),
        Some((ts(0), codes(L3)[0].to_string())),
        "oldest volume first, as the frame list is ordered",
    );
    assert!(
        mgr.extract_pending(0).is_none(),
        "a Level III loop must not download the volumes it never reads",
    );
}

/// A Level II loop is the mirror image: volumes queued, no pairings.
#[test]
fn a_level2_loop_queues_its_volumes_and_no_pairings() {
    let mut mgr = LoopDownloadManager::new();
    mgr.set_plan(0, plan(3));
    assert!(mgr.plan_downloads_for(0, L2));

    let pending = mgr.extract_pending(0).expect("volumes are owed");
    assert_eq!(pending.site, SITE);
    assert_eq!(pending.queue.len(), 3);
    assert!(mgr.extract_pending_l3(0).is_none());
}

/// Switching product mid-loop must re-derive the queues, in both directions.
/// The frame list does not change — the loop's timeline is the volumes either
/// way — so without this the frames would sit waiting on data nothing is
/// fetching, and `settle_loop_phase` would abandon the loop.
#[test]
fn retargeting_across_the_datasource_line_requeues_the_frames() {
    let mut mgr = LoopDownloadManager::new();
    mgr.set_plan(0, plan(2));

    assert!(mgr.plan_downloads_for(0, L2));
    assert!(
        mgr.plan_downloads_for(0, L3),
        "moving to Level III is a change",
    );
    assert!(
        mgr.extract_pending(0).is_none(),
        "the volume queue went with the old product",
    );
    let l3 = mgr.extract_pending_l3(0).expect("pairings queued");
    assert_eq!(l3.queue.len(), 2 * codes(L3).len());
    mgr.insert_pending_l3(0, l3);

    assert!(mgr.plan_downloads_for(0, L2), "and back again");
    assert_eq!(
        mgr.extract_pending(0).map(|p| p.queue.len()),
        Some(2),
        "the volumes are queued from the same plan, with no re-listing",
    );
    assert!(mgr.extract_pending_l3(0).is_none());
}

/// An unchanged product must not re-derive anything. `dispatch_loop_renders`
/// asks on every retarget, and an elevation change is a retarget — rebuilding
/// both queues every time the user nudges a tilt would re-queue every frame
/// that had already been fetched.
#[test]
fn an_unchanged_product_requeues_nothing() {
    let mut mgr = LoopDownloadManager::new();
    mgr.set_plan(0, plan(2));
    assert!(mgr.plan_downloads_for(0, L3));
    assert!(
        !mgr.plan_downloads_for(0, L3),
        "the same product is not a change",
    );
    // And a pane with no plan has nothing to derive from.
    assert!(!mgr.plan_downloads_for(7, L3));
}

/// The three answers a frame's Level III data can have, and the one that only
/// exists because gaps are normal: a volume the site generated no object for is
/// **Absent**, cached as such, and never asked about again.
///
/// The `Absent` case is what a re-pairing loop would otherwise cost: up to
/// `PAIRING_CANDIDATES` object fetches per dispatch pass, forever.
#[test]
fn a_frames_level3_state_distinguishes_ready_absent_and_pending() {
    let mut mgr = LoopDownloadManager::new();
    let code = codes(L3)[0];

    assert_eq!(
        mgr.l3_frame_state(SITE, L3, &ts(0)),
        L3FrameState::Pending,
        "nothing paired yet",
    );
    assert!(!mgr.l3_is_resolved(SITE, code, &ts(0)));

    mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));
    assert_eq!(mgr.l3_frame_state(SITE, L3, &ts(0)), L3FrameState::Ready);
    assert!(mgr.l3_is_resolved(SITE, code, &ts(0)));

    mgr.cache_l3_product(SITE, code, ts(1), None);
    assert_eq!(
        mgr.l3_frame_state(SITE, L3, &ts(1)),
        L3FrameState::Absent,
        "the site generated no object for that volume",
    );
    assert!(
        mgr.l3_is_resolved(SITE, code, &ts(1)),
        "a gap is an answer, so nothing re-pairs it",
    );
}

/// Another site's objects are never this frame's, even at the same volume time
/// — the same rule the volume cache follows, and for the same reason: two
/// sites' volume starts land on the same second often enough, and an image
/// drawn from one radar's object at another's coordinates looks entirely
/// consistent.
#[test]
fn a_paired_object_is_never_taken_from_another_site() {
    let mut mgr = LoopDownloadManager::new();
    let code = codes(L3)[0];
    mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));

    assert_eq!(mgr.l3_frame_state(SITE, L3, &ts(0)), L3FrameState::Ready);
    assert_eq!(
        mgr.l3_frame_state("KOUN", L3, &ts(0)),
        L3FrameState::Pending,
        "KOUN has paired nothing",
    );
    assert!(mgr.l3_frame_products("KOUN", L3, &ts(0)).is_none());
}

/// A product needs *every* one of its AWIPS codes before a frame is ready, and
/// is a gap as soon as any one of them is missing.
///
/// Every Level III product rustdar draws today names one code, so for them this
/// reduces to the single-code case above. It is written over
/// `level3_products()` rather than over a literal because that is about to stop
/// being true: VIL density is being rebuilt as `DVL ÷ EET`, two codes paired to
/// one volume, and the moment it lands this test carries the all-or-nothing
/// rule without being touched.
#[test]
fn a_frame_needs_every_one_of_its_products_codes() {
    for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        let all = codes(*product);
        let mut mgr = LoopDownloadManager::new();
        // All but the last code paired.
        for code in &all[..all.len() - 1] {
            mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));
        }
        assert_eq!(
            mgr.l3_frame_state(SITE, *product, &ts(0)),
            L3FrameState::Pending,
            "{} was ready without {}",
            product.name(),
            all[all.len() - 1],
        );
        assert!(
            mgr.l3_frame_products(SITE, *product, &ts(0)).is_none(),
            "{} must not render against a missing input",
            product.name(),
        );

        mgr.cache_l3_product(SITE, all[all.len() - 1], ts(0), Some(object(0)));
        assert_eq!(
            mgr.l3_frame_state(SITE, *product, &ts(0)),
            L3FrameState::Ready,
        );
        assert_eq!(
            mgr.l3_frame_products(SITE, *product, &ts(0))
                .map(|p| p.len()),
            Some(all.len()),
            "{} renders from all of its codes, in order",
            product.name(),
        );
    }
}

/// The sweep a Level III frame is rendered at is its **object's own** PDB
/// elevation, not the pane's selection.
///
/// That is what the image actually depicts, and it is what makes the sibling
/// broadcast's sweep comparison mean anything: two panes resolving the same
/// `(site, code, volume)` share one cache entry and so one angle, while a
/// comparison against the selection would agree for every object regardless of
/// which cut it is. The fixture's object sits at 1.4° against a 0.5° selection
/// so the two cannot be confused.
#[test]
fn a_level3_frames_sweep_is_its_objects_own_elevation() {
    let mut mgr = LoopDownloadManager::new();
    let code = codes(L3)[0];
    let tgt = RenderTarget::new(SITE, L3, 0.5);

    assert!(
        matches!(frame_sweep(&mgr, &tgt, ts(0)), FrameSweep::Pending),
        "nothing paired yet, so the frame waits rather than being retired",
    );

    mgr.cache_l3_product(SITE, code, ts(0), Some(object(14)));
    match frame_sweep(&mgr, &tgt, ts(0)) {
        FrameSweep::At(sweep) => assert_eq!(sweep, 1.4),
        other => panic!("expected a renderable frame, got {:?}", DebugSweep(other)),
    }
}

/// A gap retires its frame, exactly as a Level II volume carrying no sweep for
/// the product does — and by the same route, so playback steps over it instead
/// of flashing an empty pane or raising an error.
#[test]
fn a_gap_makes_its_frame_unrenderable_rather_than_pending() {
    let mut mgr = LoopDownloadManager::new();
    mgr.cache_l3_product(SITE, codes(L3)[0], ts(0), None);
    assert!(matches!(
        frame_sweep(&mgr, &RenderTarget::new(SITE, L3, 0.5), ts(0)),
        FrameSweep::Unrenderable
    ));
}

/// A frame's render data is resolved from the product on its own target, so a
/// Level III frame gets objects and a Level II frame gets a volume with no
/// caller deciding which.
#[test]
fn frame_data_follows_the_targets_own_product() {
    let mut mgr = LoopDownloadManager::new();
    mgr.cache_l3_product(SITE, codes(L3)[0], ts(0), Some(object(0)));

    match frame_data(&mgr, &RenderTarget::new(SITE, L3, 0.5), ts(0)) {
        Some(LoopFrameData::Products(objects)) => assert_eq!(objects.len(), codes(L3).len()),
        _ => panic!("a Level III target must resolve to its objects"),
    }
    assert!(
        frame_data(&mgr, &RenderTarget::new(SITE, L2, 0.5), ts(0)).is_none(),
        "a Level II target reads the volume cache, which holds nothing here",
    );
}

/// Readiness has to be asked about the loop's own *product*, not only its site,
/// and this is the failure it prevents.
///
/// `render_set_settled` reads "this frame has no data yet" as settled — nothing
/// is owed to a frame with nothing to render — and leaves the arriving half to
/// the download check. So a Level III loop judged against the **volume** cache,
/// which nothing fills for it, reads as fully settled the moment its pairings
/// are dispatched: no frame has a texture, nothing is in flight, and
/// `settle_loop_phase` concludes nothing will ever render and switches the loop
/// off. The pane silently falls back to its static image, which is precisely
/// "an L3 product that does not loop".
///
/// Asked about the product, a paired frame *is* data-available, so the batch
/// stays unsettled until its render lands.
#[test]
fn a_level3_loops_batch_settles_on_its_pairings_not_on_volumes() {
    let mut ls = loop_for(L3, 3);
    let mut mgr = LoopDownloadManager::new();
    let code = codes(L3)[0];

    // Every frame's object is paired; none has rendered.
    for i in 0..3 {
        mgr.cache_l3_product(SITE, code, ts(i), Some(object(0)));
    }
    assert!(
        !loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET),
        "three renderable frames and no textures: renders are owed",
    );
    // The contrast, spelled out: the same batch judged the old way — against
    // the volume cache — reads as settled, which is the abandonment above.
    assert!(
        ls.render_set_settled(MAX_LOOP_RENDER_BUDGET, |f| mgr
            .is_cached(SITE, &f.timestamp)),
        "precondition: a volume-cache check settles this batch, and the loop \
             would then be switched off with everything it needs in hand",
    );
    assert!(
        !settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET),
        "so the loop must be left in Rendering, waiting on its renders",
    );
    assert_eq!(ls.phase, LoopPhase::Rendering);

    // Caching the volumes changes nothing either way: this loop never reads them.
    for i in 0..3 {
        mgr.cache_scan(SITE, ts(i), volume());
    }
    assert!(!loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET));

    // One rendered, one gap, one rendered: the batch settles and the loop is
    // promoted rather than abandoned — the gap is not held against it.
    ls.frames[0].image = Some(rustdar_egui::pane::LoopFrameImage::PlanView(image()));
    ls.frames[2].image = Some(rustdar_egui::pane::LoopFrameImage::PlanView(image()));
    mgr.cache_l3_product(SITE, code, ts(1), None);
    ls.frames[1].render_failed = true;
    assert!(loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET));
    assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
    assert_eq!(ls.phase, LoopPhase::Ready);
}

/// A pairing in flight holds the loop open, the way a volume download does.
/// Without it, a Level III loop is abandoned on the pass right after its first
/// batch is dispatched: no frame has a texture yet, nothing is *rendering*, and
/// the only thing outstanding is on the other datasource's in-flight set.
#[test]
fn a_pairing_in_flight_keeps_the_loop_from_being_abandoned() {
    let mut ls = loop_for(L3, 3);
    let mut mgr = LoopDownloadManager::new();
    mgr.mark_l3_in_flight(SITE, codes(L3)[0], ts(0));

    assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
    assert_eq!(ls.phase, LoopPhase::Rendering, "still working");

    // Undispatched pairings hold it open too — the queue, not just the marks.
    let mut mgr = LoopDownloadManager::new();
    mgr.set_plan(0, plan(3));
    mgr.plan_downloads_for(0, L3);
    assert!(!mgr.is_pane_done(0), "pairings are still owed");
    assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
    assert_eq!(ls.phase, LoopPhase::Rendering);
}

/// A loop every one of whose frames is a gap is switched off, so the pane falls
/// back to its static image rather than animating nothing. The same dead end a
/// Level II loop with no renderable frame reaches, by the same route.
#[test]
fn a_level3_loop_that_is_all_gaps_is_switched_off() {
    let mut ls = loop_for(L3, 3);
    let mut mgr = LoopDownloadManager::new();
    for i in 0..3 {
        mgr.cache_l3_product(SITE, codes(L3)[0], ts(i), None);
        ls.frames[i as usize].render_failed = true;
    }

    assert!(
        settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET),
        "the caller has to release this pane's loop state",
    );
    assert!(!ls.is_active());
}

/// A pane whose loop has never dispatched has no product to judge its frames
/// by, so nothing is settled — rather than everything being, which would
/// promote a loop with no frames rendered.
#[test]
fn a_loop_before_its_first_dispatch_has_settled_nothing() {
    let mut ls = loop_for(L3, 2);
    ls.rendered_for = None;
    let mgr = LoopDownloadManager::new();
    assert!(!loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET));
}

/// The days a Level III listing covers come from the loop's own frames, not
/// from wall clock: a loop rebuilt around a historic scan pairs against
/// yesterday's prefix, and listing today's would find nothing — which is
/// indistinguishable from "the site served no objects" and would retire every
/// frame as a gap.
#[test]
fn the_listed_days_come_from_the_frames_and_span_midnight() {
    let code = codes(L3)[0].to_string();
    let jan2 = chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
    let jan1 = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let dec31 = chrono::NaiveDate::from_ymd_opt(2023, 12, 31).unwrap();

    // Frames inside one UTC day: that day plus the one before, since an
    // object for an early volume can sit under the previous prefix.
    let same_day: VecDeque<_> = [(ts(10), code.clone()), (ts(20), code.clone())]
        .into_iter()
        .collect();
    assert_eq!(pairing_days_for_frames(&same_day), vec![jan1, dec31]);

    // A window that crosses 00Z lists all three, each once.
    let across: VecDeque<_> = [
        (ts(23 * 60 + 50), code.clone()),
        (ts(24 * 60 + 5), code.clone()),
    ]
    .into_iter()
    .collect();
    assert_eq!(pairing_days_for_frames(&across), vec![jan1, dec31, jan2]);

    assert!(
        pairing_days_for_frames(&VecDeque::new()).is_empty(),
        "nothing left to pair, nothing to list",
    );
}

/// A key listing is claimed once. Two panes looping one site want the same
/// keys, and the listing is the expensive half of a pairing — a round-trip per
/// UTC day, against a few hundred kilobytes of object per pairing.
#[test]
fn a_key_listing_is_claimed_once_and_shared() {
    let mut mgr = LoopDownloadManager::new();
    let code = codes(L3)[0];

    assert!(mgr.claim_l3_listing(SITE, code), "the first caller owes it");
    assert!(
        !mgr.claim_l3_listing(SITE, code),
        "the second waits on the first",
    );
    assert!(
        mgr.claim_l3_listing("KOUN", code),
        "another site is another listing",
    );
    assert!(mgr.l3_keys(SITE, code).is_none(), "not landed yet");

    mgr.cache_l3_keys(SITE, code, vec!["TLX_EET_2024_01_01_00_01_30".to_string()]);
    assert_eq!(mgr.l3_keys(SITE, code).map(|k| k.len()), Some(1));
    assert!(
        !mgr.claim_l3_listing(SITE, code),
        "and is not listed a second time once cached",
    );
}

/// An empty listing is an answer, not a failure to record. Discarded, the
/// pairings would wait on a listing that already happened and the loop would
/// hang in `Rendering`; cached, every frame pairs to a gap and the loop retires
/// to the pane's static image.
#[test]
fn an_empty_key_listing_is_cached_as_the_answer() {
    let mut mgr = LoopDownloadManager::new();
    let code = codes(L3)[0];
    assert!(mgr.claim_l3_listing(SITE, code));
    mgr.cache_l3_keys(SITE, code, Vec::new());

    assert_eq!(
        mgr.l3_keys(SITE, code).map(|k| k.len()),
        Some(0),
        "an empty list is stored, so the pairings can proceed and find nothing",
    );
    assert!(!mgr.claim_l3_listing(SITE, code));
}

/// Switching site drops every trace of the Level III half too. A pairing left
/// behind would land against a loop that no longer exists, and a key listing
/// left behind would be re-used for a site it was never made for — which
/// `clear_all`'s whole job is to prevent.
#[test]
fn clear_all_empties_the_level3_state_as_well() {
    let mut mgr = LoopDownloadManager::new();
    let code = codes(L3)[0];
    mgr.set_plan(0, plan(2));
    mgr.plan_downloads_for(0, L3);
    mgr.cache_l3_keys(SITE, code, vec!["TLX_EET_2024_01_01_00_01_30".to_string()]);
    mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));
    mgr.mark_l3_in_flight(SITE, code, ts(1));
    assert!(!mgr.is_pane_done(0), "precondition: pairings are owed");

    mgr.clear_all();

    assert!(mgr.is_pane_done(0));
    assert!(mgr.pending_l3_pane_indices().is_empty());
    assert!(mgr.l3_keys(SITE, code).is_none());
    assert!(!mgr.l3_is_resolved(SITE, code, &ts(0)));
    assert!(!mgr.l3_is_in_flight(SITE, code, &ts(1)));
    // And the plan is gone, so nothing can re-derive a queue from the site the
    // pane has just left.
    assert!(!mgr.plan_downloads_for(0, L3));
}

/// The two queues are reported by two separate index lists, which is why a
/// completion drain has to iterate both.
///
/// One concurrency budget serves them, and each drain is the only thing that
/// frees a slot. A drain that re-dispatched only its own kind starves the other:
/// with the budget full of volume downloads nothing re-triggers the pairing
/// queue, because no pairing was ever spawned to complete. That is what
/// `dispatch_freed_loop_slots` exists to prevent, and this pins the shape it
/// depends on — neither list can stand in for the other.
#[test]
fn the_two_queues_are_reported_separately_so_both_must_be_dispatched() {
    let mut mgr = LoopDownloadManager::new();
    mgr.set_plan(0, plan(2));
    mgr.plan_downloads_for(0, L2);
    mgr.set_plan(1, plan(2));
    mgr.plan_downloads_for(1, L3);

    assert_eq!(mgr.pending_pane_indices(), vec![0], "pane 0 owes volumes");
    assert_eq!(
        mgr.pending_l3_pane_indices(),
        vec![1],
        "pane 1 owes pairings, and iterating the volume list alone never \
             reaches it",
    );
    assert!(!mgr.is_pane_done(0));
    assert!(!mgr.is_pane_done(1));
}

/// Switching the loop off releases both queues and the plan behind them.
#[test]
fn removing_a_panes_pending_work_takes_both_queues_and_the_plan() {
    let mut mgr = LoopDownloadManager::new();
    mgr.set_plan(0, plan(2));
    mgr.plan_downloads_for(0, L3);
    assert!(!mgr.is_pane_done(0));

    mgr.remove_pending(0);

    assert!(mgr.is_pane_done(0));
    assert!(
        !mgr.plan_downloads_for(0, L2),
        "the plan went with the queues, so nothing refills from it",
    );
}

/// `FrameSweep` is not `Debug` in production — nothing logs it — so the
/// panic message above wraps it.
struct DebugSweep(FrameSweep);

impl std::fmt::Debug for DebugSweep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            FrameSweep::At(a) => write!(f, "At({a})"),
            FrameSweep::Unrenderable => write!(f, "Unrenderable"),
            FrameSweep::Pending => write!(f, "Pending"),
        }
    }
}

/// A Level II volume with one reflectivity sweep, so the volume cache holds
/// something real when a test needs to prove it is *not* being read.
fn volume() -> Arc<nexrad_model::data::Scan> {
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };
    let radial = Radial::new(
        0,
        0,
        0.0,
        1.0,
        RadialStatus::ElevationStart,
        1,
        0.5,
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
        vec![Sweep::new(1, vec![radial])],
    ))
}

/// A 1x1 texture standing in for a rendered frame. Nothing here reads pixels.
fn image() -> rustdar_egui::pane::RadarImageData {
    let ctx = egui::Context::default();
    rustdar_egui::pane::RadarImageData {
        texture: ctx.load_texture(
            "test",
            egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        ),
        lat: 35.33,
        lon: -97.27,
        max_range_km: 100.0,
        value_data: Arc::new(Vec::new()),
    }
}
