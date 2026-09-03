//! **One copy of a 2D loop frame, drawn on every pane that shows it — linked
//! or not.** The ruling, verbatim: *"the same data on both should deduplicate
//! both resident memory and the work done to render."*
//!
//! Every fixture here puts the second pane in **no group with its layer link
//! off**, which is the arrangement that used to hold two textures, two hover
//! sources and pay the render twice. A linked fixture could not tell the
//! store's sharing from the link's.

use super::loop_dispatch_tests::volume_with_sweeps;
use super::radar_timeline_addressing_tests::{active_loop, at, point_at_site, textured};
use super::*;
use crate::app::tests::{drain_uploads, two_pane_app};
use crate::loop_frame_store::LoopFrameKey;
use crate::loop_pool::{
    LoopDemand, LoopKey, LoopKind, LoopNeed, LoopPool, LoopPoolLimits, LoopPoolState,
};
use squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES;
use squallar_egui::pane::TimeMode;
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_source::id::known;

const SITE: &str = "KTLX";
const PRODUCT_ID: squallar_source::product::FieldId = squallar_radar::fields::known::REFLECTIVITY;
const TILT: f32 = 0.5;

/// Two map panes on one site, product and tilt, each running a plan-view loop
/// over `stamps`, the second in no group with its layer link off.
fn two_unlinked_panes(stamps: &[chrono::NaiveDateTime]) -> crate::app::App {
    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    let second = app.gui.pane_mut(1).expect("the fixture built two panes");
    second.layer_link = false;
    second.group = None;
    assert!(
        !app.gui.panes_layer_linked(0, 1),
        "premise: nothing links these two panes",
    );
    app.loop_mgr = LoopDownloadManager::new();
    for &stamp in stamps {
        app.loop_mgr
            .cache_scan(SITE, stamp, volume_with_sweeps(&[TILT]));
    }
    for idx in 0..2 {
        *app.gui
            .pane_mut(idx)
            .expect("the fixture built two panes")
            .time_state_mut(&known::RADAR) = active_loop(stamps);
    }
    app
}

fn frame_texture(app: &crate::app::App, pane: usize, frame: usize) -> Option<egui::TextureId> {
    app.gui
        .pane(pane)?
        .time_state(&known::RADAR)
        .frames
        .get(frame)?
        .image
        .as_ref()?
        .plan_view()
        .map(|picture| picture.texture.id())
}

fn frame_hover(
    app: &crate::app::App,
    pane: usize,
    frame: usize,
) -> Option<Arc<squallar_radar::hover::HoverSource>> {
    app.gui
        .pane(pane)?
        .time_state(&known::RADAR)
        .frames
        .get(frame)?
        .image
        .as_ref()?
        .plan_view()
        .map(|picture| Arc::clone(&picture.hover))
}

fn in_flight(app: &crate::app::App, pane: usize, frame: usize) -> bool {
    app.gui
        .pane(pane)
        .expect("pane exists")
        .time_state(&known::RADAR)
        .frames[frame]
        .render_in_flight
}

fn target_of(app: &crate::app::App, pane: usize) -> RenderTarget {
    app.gui
        .pane(pane)
        .expect("pane exists")
        .time_state(&known::RADAR)
        .rendered_for
        .clone()
        .expect("the fixture loop is keyed")
}

/// A finished render for `pane`'s frame at `stamp` lands on the channel and
/// is drained.
fn deliver(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    pane: usize,
    stamp: chrono::NaiveDateTime,
) {
    app.channels
        .loop_render_sender
        .send(crate::channels::LoopRenderResponse {
            pane_idx: pane,
            timestamp: stamp,
            target: target_of(app, pane),
            snapped: TILT,
            site_lat: 35.33,
            site_lon: -97.27,
            image: Some(egui::ColorImage::from_rgba_unmultiplied([2, 2], &[7u8; 16])),
            max_range_km: 230.0,
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
            polar: Default::default(),
        })
        .expect("the receiver lives on the App");
    app.poll_loop_render_results(ctx);
}

/// **Broadcast without a link, and the work done once.** Both unlinked panes
/// dispatched their own render for the frame — the duplicate the old gate
/// let through — and the first to finish serves both: one upload, one
/// texture id on both panes, one hover source, the sibling's own render
/// retired, and its reply dropped without a second upload.
#[test]
fn two_unlinked_panes_on_one_picture_hold_one_texture_and_upload_it_once() {
    let ctx = egui::Context::default();
    let mut app = two_unlinked_panes(&[at(0)]);
    for idx in 0..2 {
        app.gui
            .pane_mut(idx)
            .expect("the fixture built two panes")
            .time_state_mut(&known::RADAR)
            .frames[0]
            .render_in_flight = true;
    }
    drain_uploads(&ctx);

    deliver(&mut app, &ctx, 0, at(0));

    assert_eq!(
        drain_uploads(&ctx).len(),
        1,
        "one picture for two panes is one upload — the denominator is every \
         whole texture egui was handed on this context",
    );
    let origin = frame_texture(&app, 0, 0).expect("the origin pane takes its own frame");
    let sibling = frame_texture(&app, 1, 0).expect(
        "the unlinked sibling did not take the broadcast: sharing is riding the \
         link again instead of the picture's identity",
    );
    assert_eq!(origin, sibling, "two panes, one texture");
    assert!(
        Arc::ptr_eq(
            &frame_hover(&app, 0, 0).unwrap(),
            &frame_hover(&app, 1, 0).unwrap()
        ),
        "the hover source behind the picture is one Arc, not one per pane",
    );
    assert!(
        !in_flight(&app, 1, 0),
        "the sibling's own render is redundant once it holds the picture",
    );
    let key = LoopFrameKey::plan_view(target_of(&app, 0), at(0));
    assert_eq!(
        app.loop_frames.holders(&key),
        2,
        "both panes are recorded as holders"
    );
    assert_eq!(app.loop_frames.shared(), 1);

    // The sibling's own late reply: no frame is waiting for it, so it is
    // dropped — nothing uploaded, nothing replaced.
    deliver(&mut app, &ctx, 1, at(0));
    assert!(
        drain_uploads(&ctx).is_empty(),
        "the duplicate render's reply was uploaded: the work was done twice"
    );
    assert_eq!(frame_texture(&app, 1, 0), Some(sibling));

    // The re-said count sees it, on the same walk that counts residency.
    let (_, counts, _) = app.loop_demand();
    assert_eq!(counts.shared, 1, "the `loop state:` line's `shared`");
    assert_eq!(
        counts.resident, 2,
        "two slots hold the one picture: `resident` counts slots, `shared` pictures"
    );
}

/// **Donation without a link.** A picture one pane already holds is handed
/// to the other at dispatch, and no render is spawned for it.
#[test]
fn an_unlinked_pane_takes_the_finished_picture_from_the_store_instead_of_rendering() {
    let ctx = egui::Context::default();
    let mut app = two_unlinked_panes(&[at(0)]);
    app.gui
        .pane_mut(0)
        .expect("the fixture built two panes")
        .time_state_mut(&known::RADAR)
        .frames[0]
        .image = Some(textured(&ctx));
    let held = frame_texture(&app, 0, 0).expect("pane 0 holds the picture");
    drain_uploads(&ctx);

    app.dispatch_loop_renders();

    assert_eq!(
        frame_texture(&app, 1, 0),
        Some(held),
        "the unlinked pane did not take the picture its sibling holds",
    );
    assert!(
        !in_flight(&app, 1, 0),
        "a render went out for a picture already in hand",
    );
    assert_eq!(
        app.render.renders_in_flight.load(Ordering::Relaxed),
        0,
        "the render counter moved: the work was done twice",
    );
    assert!(drain_uploads(&ctx).is_empty(), "nothing new was uploaded");
}

/// **Eviction is over the union of the holders' render sets.** Pane 0 parked
/// on frame 3 and pane 1 on frame 9, both holding every frame, under a plan
/// that lets each keep four textured: pane 0's eviction pass takes its own
/// copies of frames outside its set, and frame 9 — which pane 1 shows — is
/// still in the store, still pane 1's, and never re-rendered; a frame neither
/// pane names is gone.
#[test]
fn a_pane_scrubbing_away_cannot_evict_the_frame_another_pane_shows() {
    let ctx = egui::Context::default();
    let stamps: Vec<_> = (0..12).map(at).collect();
    let mut app = two_unlinked_panes(&stamps);
    for idx in 0..12 {
        app.gui
            .pane_mut(0)
            .expect("the fixture built two panes")
            .time_state_mut(&known::RADAR)
            .frames[idx]
            .image = Some(textured(&ctx));
    }
    for (pane, frame) in [(0usize, 3usize), (1, 9)] {
        let pane = app.gui.pane_mut(pane).expect("the fixture built two panes");
        pane.time.mode = TimeMode::AsOf(at(frame as i64));
        pane.time_state_mut(&known::RADAR)
            .settle_playhead(TimeMode::AsOf(at(frame as i64)));
    }
    let frame_nine = frame_texture(&app, 0, 9).expect("pane 0 holds frame 9");

    // A pass under a plan that does not bind: pane 0's pictures are filed
    // and pane 1 takes every one of them.
    app.dispatch_loop_renders();
    assert_eq!(
        app.loop_frames.len(),
        12,
        "precondition: every picture pane 0 holds is filed in the store",
    );
    assert_eq!(
        frame_texture(&app, 1, 9),
        Some(frame_nine),
        "precondition: pane 1 shows frame 9 with the shared texture",
    );

    // Now a plan that holds the loop to four textured frames, settled past
    // the pool's dwell so it is the allocation in force on the next pass.
    let budgets = app.budgets;
    let model = LoopFrameModel::from_budgets(&budgets);
    let bytes = 4 * model.plan_view;
    let pool = LoopPool::new(
        bytes,
        LoopPoolLimits {
            floor: bytes,
            ceiling: bytes,
        },
    );
    let mut demand = LoopDemand::default();
    demand.push(LoopNeed {
        key: LoopKey { pane: 0 },
        kind: LoopKind::PlanView,
        span_secs: 3600,
        cadence_secs: None,
        frame_bytes: model.plan_view,
        base_frames: 12,
        max_frames: 12,
    });
    demand.alias(1, 0);
    let mut state = LoopPoolState::new(pool, model);
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, demand.clone());
    }
    assert_eq!(
        state.allocation().frames_for_pane(0),
        Some(4),
        "precondition: the plan holds the loop to four textured frames",
    );
    assert_eq!(state.allocation().frames_for_pane(1), Some(4));
    app.loop_pool_state = state;

    app.dispatch_loop_renders();

    let textured_on = |pane: usize| -> Vec<usize> {
        (0..12)
            .filter(|&idx| frame_texture(&app, pane, idx).is_some())
            .collect()
    };
    assert_eq!(
        textured_on(0),
        vec![2, 3, 4, 5],
        "pane 0 keeps its four around frame 3 and nothing else",
    );
    assert_eq!(
        textured_on(1),
        vec![8, 9, 10, 11],
        "pane 1 keeps its four around frame 9 and nothing else",
    );
    assert_eq!(
        frame_texture(&app, 1, 9),
        Some(frame_nine),
        "pane 1's frame 9 is the very texture pane 0 rendered — not re-rendered, \
         not evicted by pane 0's pass",
    );
    let target = target_of(&app, 0);
    let filed = |minute: i64| {
        app.loop_frames
            .holders(&LoopFrameKey::plan_view(target.clone(), at(minute)))
    };
    assert_eq!(filed(9), 1, "frame 9 stays filed, held by pane 1 alone");
    assert_eq!(filed(3), 1, "frame 3 stays filed, held by pane 0 alone");
    for orphan in [0, 1, 6, 7] {
        assert_eq!(
            filed(orphan),
            0,
            "frame {orphan}: named by nobody, gone from the store"
        );
    }
    assert_eq!(
        app.loop_frames.len(),
        8,
        "the union of two render sets of four"
    );
    assert_eq!(
        app.render.renders_in_flight.load(Ordering::Relaxed),
        0,
        "nothing was re-rendered to keep either pane's set textured",
    );
}

/// **The pool prices one loop per identity.** Two panes on one picture set
/// over one window are one need and one grant, the second an alias of the
/// first; another tilt, another lookback or another parked instant makes two.
#[test]
fn two_panes_on_one_picture_set_price_one_loop_and_two_sets_price_two() {
    let mut app = two_unlinked_panes(&[at(0)]);

    let (demand, _, scene) = app.loop_demand();
    assert_eq!(
        demand.shares(),
        1,
        "one identity, one loop the pool is given to"
    );
    assert!(scene.panes[0].looping, "the first pane carries the need");
    assert!(
        !scene.panes[1].looping,
        "the second pane is an alias and fit does not charge its frames again",
    );
    // The pool takes a new plan only once the demand has held for its dwell.
    let mut allocation = app.observe_loop_demand();
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        allocation = app.observe_loop_demand();
    }
    assert_eq!(
        allocation.grant_for_pane(1).map(|g| g.key),
        Some(LoopKey { pane: 0 }),
        "the alias reads its owner's grant",
    );
    assert_eq!(
        allocation.grants().len(),
        1,
        "one grant for two panes: the pool charged the picture set once",
    );

    // Another tilt of a tilt-selecting product is another picture set.
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .time_state_mut(&known::RADAR)
        .retarget_renders(&PRODUCT_ID, 1.5);
    let (demand, _, scene) = app.loop_demand();
    assert_eq!(demand.shares(), 2, "two tilts, two loops");
    assert!(scene.panes[0].looping && scene.panes[1].looping);

    // The same picture over another window is not the same frames.
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .time_state_mut(&known::RADAR)
        .retarget_renders(&PRODUCT_ID, TILT);
    assert_eq!(
        app.loop_demand().0.shares(),
        1,
        "control: back on one tilt, one loop"
    );
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .time
        .span_secs *= 2;
    assert_eq!(app.loop_demand().0.shares(), 2, "two lookbacks, two loops");
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .time
        .span_secs /= 2;
    assert_eq!(
        app.loop_demand().0.shares(),
        1,
        "control: one lookback, one loop"
    );
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .time
        .mode = TimeMode::AsOf(at(-30));
    assert_eq!(
        app.loop_demand().0.shares(),
        2,
        "one live and one parked, two loops"
    );
}

/// The section arm of the same pin: two panes cutting one line through one
/// volume are one loop; another line is another.
#[test]
fn two_section_panes_on_one_line_price_one_loop_and_two_lines_price_two() {
    let mut app = super::loop_section_tests::two_section_panes(&[0]);
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .group = None;
    assert_eq!(app.loop_demand().0.shares(), 1, "one line, one loop");

    let other = squallar_egui::pane::SectionLoopKey::new(
        squallar_egui::pane::SectionLine::new(
            squallar_geo::GeoPoint {
                lat: 35.2,
                lon: -98.0,
            },
            squallar_geo::GeoPoint {
                lat: 36.2,
                lon: -97.0,
            },
        )
        .expect("two distinct points on Earth"),
        None,
        squallar_radar::srv::SrvFallback::default(),
    );
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .time_state_mut(&known::RADAR)
        .retarget_renders_for(&PRODUCT_ID, TILT, Some(other));
    assert_eq!(app.loop_demand().0.shares(), 2, "two lines, two loops");
}
