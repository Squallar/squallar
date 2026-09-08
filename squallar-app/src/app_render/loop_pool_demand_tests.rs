//! **The loop pool is divided among the loops somebody can see.**
//!
//! `App::dispatch_loop_renders` stopped *spending* on a loop whose pane draws
//! nothing on 2026-09-08, and `App::dispatch_overlay_loop_renders` on the
//! layer's own half the day before. Neither reached the walk that decides who
//! divides the pool. A loop keeps its timeline when its layer is switched off
//! — `PaneState::refresh_transport` keeps a running transport on purpose — so
//! `App::walk_panes` went on pushing a `LoopNeed` for it, and
//! `LoopPool::plan` pays every base before it spends a single balloon frame
//! and then gives each balloon frame to the coarsest **claimant**. A claimant
//! that is holding no pictures therefore takes both, and it takes them *from*
//! the panes the user is looking at: those panes get a smaller share and evict
//! sooner than they need to.
//!
//! # What each claim needs, and why they cannot share one arrangement
//!
//! **The division and the pool are two questions and only the first is on
//! trial here.** `Scene` is `fit`'s input — it picks the quality rung and
//! sizes the tile caches — and it is deliberately left charging the loop a
//! switched-off pane is still holding a frame list for. So the pool a scene
//! yields is unchanged by this land, which is why every magnitude claim below
//! plans against a pool this file chooses rather than against the machine's:
//! the subject is who divides it, and a figure that moved because the pool
//! moved would prove nothing about the division.
//!
//! **The identity claim is the strong one.** A demand is comparable
//! ([`LoopDemand`] is `PartialEq`), so "a loop nobody draws asks the pool for
//! nothing" is written as an equality against the same app with no such loop
//! at all — not as a count, which a bug that pushed the wrong need would
//! satisfy.
//!
//! **Both directions are asserted, and the back one is the trap.** A pane that
//! starts drawing again has to be a claimant on the very next walk, and a
//! sibling pane drawing the same site must not lose anything — which is why
//! the switched-off pane is kept out of `seen` as well as out of the needs. An
//! owner that holds no frames would make a drawing sibling an *alias* of a
//! grant that does not exist, and an alias with no owner falls through to the
//! "a pane the plan has not seen" fallback: the pane on screen would be sized
//! by the kind's ceiling instead of by a grant.

use super::loop_dispatch_tests::volume_with_sweeps;
use super::radar_timeline_addressing_tests::{active_loop, at, point_at_site};
use super::*;
use crate::app::tests::{n_pane_app, two_pane_app};
use crate::loop_pool::{LoopPoolLimits, loop_ceiling_frames};
use squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES;
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_source::id::known;

const SITE: &str = "KTLX";
const PRODUCT_ID: squallar_source::product::FieldId = squallar_radar::fields::known::REFLECTIVITY;
const TILT: f32 = 0.5;
/// The second pane's tilt in the two-loop fixtures. Another tilt of a
/// tilt-selecting product is another picture set, so the two panes are two
/// loops and not an alias pair — see [`two_loops`].
const OTHER_TILT: f32 = 1.5;

/// **No floor and no ceiling**, so a pool this file chooses is the pool the
/// plan is made against. `LoopPoolLimits::from_budgets` would clamp a
/// deliberately tight figure up to the bracket's floor and the division would
/// then be planned against a number nothing here wrote.
const UNBOUNDED: LoopPoolLimits = LoopPoolLimits {
    floor: 0,
    ceiling: usize::MAX,
};

fn stamps() -> Vec<chrono::NaiveDateTime> {
    vec![at(-10), at(-5), at(0)]
}

/// Two unlinked map panes on one site, each running a plan-view loop over
/// `stamps` — the second at [`OTHER_TILT`], so the two are **two** loops and
/// the pool has two claimants rather than an owner and an alias.
///
/// Each loop carries a cadence, which is what gives it a ceiling above its
/// base: `loop_ceiling_frames` answers the base itself for a loop that has
/// neither a listing nor a cadence, and a loop that cannot grow cannot show a
/// balloon frame going to the wrong pane.
fn two_loops() -> crate::app::App {
    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    let second = app.gui.pane_mut(1).expect("the fixture built two panes");
    second.layer_link = false;
    second.group = None;
    assert!(
        !app.gui.panes_layer_linked(0, 1),
        "premise: nothing links these two panes, so what one keeps for the \
         other is the pool's sharing and not the link's",
    );
    app.loop_mgr = LoopDownloadManager::new();
    for &stamp in &stamps() {
        app.loop_mgr
            .cache_scan(SITE, stamp, volume_with_sweeps(&[TILT, OTHER_TILT]));
    }
    for idx in 0..2 {
        let pane = app.gui.pane_mut(idx).expect("the fixture built two panes");
        // The user's lookback, and the second pane's is the wider of the
        // two: a balloon frame goes to whichever growable loop's frames stand
        // for the most seconds apiece, so this is what makes pane 1 the one
        // taking it while it is a claimant.
        //
        // **Both are wider than the rung's own span**, which is what leaves
        // room to grow at all: a loop's base is its lookback held to that span
        // (`Budgets::frames_for_span_of`) while its ceiling is the whole
        // window at its cadence, so a lookback inside the span has a ceiling
        // equal to its base and cannot take a balloon frame from anybody.
        // `the_fixture_loops_are_growable` is the standing floor under this.
        pane.time.span_secs = if idx == 0 { 3 * 3600 } else { 6 * 3600 };
        *pane.time_state_mut(&known::RADAR) = active_loop(&stamps());
        let ls = pane.time_state_mut(&known::RADAR);
        ls.cadence_secs = Some(300);
        if idx == 1 {
            ls.retarget_renders(&PRODUCT_ID, OTHER_TILT);
        }
    }
    assert_eq!(
        app.loop_demand().demand.shares(),
        2,
        "premise: two distinct picture sets are two claimants, or every \
         assertion below is about a pool nobody was contending for",
    );
    app
}

/// Two unlinked map panes on one site, product **and tilt** — one picture set,
/// so pane 1 is an alias of pane 0's loop.
fn one_shared_loop() -> crate::app::App {
    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    let second = app.gui.pane_mut(1).expect("the fixture built two panes");
    second.layer_link = false;
    second.group = None;
    app.loop_mgr = LoopDownloadManager::new();
    for &stamp in &stamps() {
        app.loop_mgr
            .cache_scan(SITE, stamp, volume_with_sweeps(&[TILT]));
    }
    for idx in 0..2 {
        let pane = app.gui.pane_mut(idx).expect("the fixture built two panes");
        pane.time.span_secs = 3600;
        *pane.time_state_mut(&known::RADAR) = active_loop(&stamps());
        pane.time_state_mut(&known::RADAR).cadence_secs = Some(300);
    }
    app
}

/// Switch a pane's radar layer on or off through the pane's own door — the
/// one the toggle in the layers menu calls.
fn set_radar(app: &mut crate::app::App, pane: usize, on: bool) {
    app.gui
        .pane_mut(pane)
        .expect("the fixture built this pane")
        .set_overlay_enabled(known::RADAR, on);
}

/// The panes the pool is divided among, in walk order.
fn claimants(app: &crate::app::App) -> Vec<usize> {
    app.loop_demand()
        .demand
        .needs()
        .iter()
        .map(|need| need.key.pane)
        .collect()
}

/// The whole demand one pane walk produced — the object the pool is planned
/// from, compared as a whole where the claim is that two arrangements ask for
/// the same thing.
fn demand_of(app: &crate::app::App) -> LoopDemand {
    app.loop_demand().demand
}

/// **A pool that pays every base and exactly one frame more**, so the balloon
/// is a single frame and which loop gets it is the whole question.
///
/// The bases are read off the planner rather than re-spelled here: a pool of
/// nothing still pays every base (`LoopPool::plan` has no downward arm —
/// ruling 15), so planning against zero and asking what it charged is the
/// model's own answer to "what do these loops cost at their bases".
fn pool_with_one_spare_frame(demand: &LoopDemand, model: LoopFrameModel) -> LoopPool {
    let at_bases = LoopPool::new(0, UNBOUNDED).plan(model, demand);
    let bases = at_bases.bytes();
    let one_frame = at_bases
        .grants()
        .iter()
        .map(|grant| grant.frame_bytes)
        .max()
        .expect("the demand must hold a loop for a pool to be sized around");
    LoopPool::new(bases + one_frame, UNBOUNDED)
}

fn frames_for(allocation: &LoopAllocation, pane: usize) -> Option<usize> {
    allocation.frames_for_pane(pane)
}

/// The allocation in force once the demand has held for its dwell — the plan
/// production actually reaches, not a bare one.
fn settled(app: &mut crate::app::App) -> LoopAllocation {
    let mut allocation = app.observe_loop_demand();
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        allocation = app.observe_loop_demand();
    }
    allocation
}

// ---------------------------------------------------------------------------
// 1. The claim: a loop nobody draws asks the pool for nothing.

/// **A loop on a pane that draws nothing is not one of the pool's claimants,
/// and its demand is exactly the demand of an application with no such loop.**
///
/// The equality is the assertion and the count is its floor. A count alone
/// would be satisfied by a need pushed under the wrong key, at the wrong
/// price, or for the wrong layer; `LoopDemand` compares whole, so the only
/// thing that satisfies this is asking for nothing.
#[test]
fn a_loop_on_a_pane_that_draws_nothing_asks_the_pool_for_nothing() {
    let mut app = two_loops();
    assert_eq!(
        claimants(&app),
        vec![0, 1],
        "floor: both panes must be claimants while both draw, or the \
         assertion below is about a pool that already had one claimant",
    );

    set_radar(&mut app, 1, false);

    assert_eq!(
        claimants(&app),
        vec![0],
        "a pane whose radar layer is switched off went on holding a share of \
         the pool: its base is paid before any balloon and its claim takes \
         balloon frames from the pane that is drawing",
    );

    // The same application with pane 1's loop never started at all. Its
    // timeline is the one difference, and the demand must not be able to tell.
    let mut never = two_loops();
    *never
        .gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .time_state_mut(&known::RADAR) = squallar_egui::pane::LayerTimeState::new();
    assert_eq!(
        claimants(&never),
        vec![0],
        "control: a pane with no timeline is not a claimant",
    );
    assert_eq!(
        demand_of(&app),
        demand_of(&never),
        "a switched-off loop must ask the pool for exactly what no loop at \
         all asks for — same needs, same prices, same aliases",
    );
}

/// **The pane that still draws takes the balloon frame the switched-off loop
/// was holding.**
///
/// The consequence in the terms the pool spends: one spare frame, and it goes
/// to the coarsest growable claimant. Pane 1's lookback is the wider of the
/// two, so while it is a claimant the frame is *its* — and the floor below
/// asserts exactly that, because a plan where the frame was already pane 0's
/// would show nothing when pane 1 stopped drawing.
#[test]
fn the_balloon_frame_a_switched_off_loop_was_taking_goes_to_the_pane_that_draws() {
    let mut app = two_loops();
    let budgets = test_budgets();
    let model = LoopFrameModel::from_budgets(&budgets);

    let contended = demand_of(&app);
    let pool = pool_with_one_spare_frame(&contended, model);
    let with_both = pool.plan(model, &contended);

    let base0 = with_both
        .grants()
        .iter()
        .find(|grant| grant.key.pane == 0)
        .expect("pane 0 is a claimant")
        .base;
    assert_eq!(
        frames_for(&with_both, 0),
        Some(base0),
        "floor: the spare frame must go to pane 1 while both draw — its \
         lookback is the wider, so its frames stand for the most seconds \
         apiece — or pane 0 already has everything and the claim below is \
         vacuous",
    );
    assert!(
        frames_for(&with_both, 1).is_some_and(|frames| frames
            > with_both
                .grants()
                .iter()
                .find(|grant| grant.key.pane == 1)
                .expect("pane 1 is a claimant")
                .base),
        "floor: and pane 1 must really be holding it",
    );

    set_radar(&mut app, 1, false);
    let alone = demand_of(&app);
    let ceiling = alone.needs()[0].max_frames;
    let drawn_only = pool.plan(model, &alone);

    assert!(
        frames_for(&drawn_only, 0).is_some_and(|frames| frames > base0),
        "the frame the switched-off loop was holding must go to the pane that \
         draws; a claimant holding no pictures took it instead",
    );
    assert_eq!(
        frames_for(&drawn_only, 0),
        Some(ceiling),
        "and it takes the whole of what the switched-off claimant was \
         holding, not one frame of it: a claim on this pool is a base paid \
         before any balloon as well as a hand out for every balloon frame, so \
         the pane that draws grows to everything its own listing can fill",
    );
    assert!(
        ceiling > base0,
        "floor: that figure is a real growth and not the base under another \
         name",
    );
    assert_eq!(
        frames_for(&drawn_only, 1),
        None,
        "and the switched-off pane must hold no grant at all",
    );
}

/// **An overlay loop on a layer the pane does not draw is not a claimant
/// either** — the other arm of the same walk, and it needs its own
/// arrangement because radar's timeline is what decides which arm a pane
/// takes.
///
/// A pane whose radar is not looping asks the pool for its first *drawn*
/// animating layer, and for nothing at all when none of them is drawn.
///
/// **The scene is deliberately unchanged**, and it is pinned here so the split
/// is a decision rather than an oversight: `Scene` is `fit`'s input — it picks
/// the quality rung and sizes the tile caches — and the pane really is still
/// holding this loop's frame list, so it goes on being priced. What moved is
/// only who divides the pool.
#[test]
fn an_overlay_loop_on_a_layer_the_pane_does_not_draw_is_not_a_claimant() {
    let mut app = n_pane_app(1, SITE);
    point_at_site(&mut app, 0);
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        pane.set_overlay_enabled(known::MRMS, true);
        *pane.time_state_mut(&known::MRMS) = squallar_egui::pane::LayerTimeState::begin(
            3600,
            squallar_radar::types::RenderView::PlanView,
            Box::new(()),
        );
        assert!(
            !pane.time_state(&known::RADAR).is_active(),
            "premise: radar must not be looping, or this pane takes the radar \
             arm of the walk and the overlay arm is never reached",
        );
    }
    assert_eq!(
        claimants(&app),
        vec![0],
        "floor: a pane looping a drawn overlay layer is a claimant",
    );

    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .set_overlay_enabled(known::MRMS, false);

    assert_eq!(
        claimants(&app),
        Vec::<usize>::new(),
        "a pane whose only animating layer is switched off went on holding a \
         share of the pool — and with nobody else claiming it, `share_bytes` \
         hands it the whole pool",
    );
    assert!(
        app.loop_demand().scene.panes[0].looping,
        "and the scene must go on pricing the frame list this pane is still \
         holding: `fit` reads it to pick the rung, and this land moved the \
         division and not the pool",
    );
}

/// **A pane with no map keeps its claim with the radar layer switched off** —
/// the one case where "drawn" is deliberately wider than the layer flag.
///
/// A cross-section or a 3D pane reads the volume without the map ever drawing
/// a radar image, and the toggle does not speak for it:
/// `Gui::render_radar_controls` says so where it hides the product and tilt
/// pickers — *"The Radar overlay toggle governs whether the map draws the
/// radar image over its tiles, which is not a question a pane with no map
/// has."* So `PaneState::draws_layer` composes `needs_radar_data` for radar
/// and `is_overlay_enabled` for every other layer, and asking the flag for
/// both would take the pool away from a pane that is drawing a section of
/// every frame it holds.
///
/// The floor under it is the map pane above, which the flag does govern:
/// spelled either way this pane must keep its claim, so the map arm is what
/// says the accessor is not simply answering *true*.
#[test]
fn a_pane_with_no_map_keeps_its_claim_with_the_radar_layer_off() {
    let mut app = n_pane_app(1, SITE);
    point_at_site(&mut app, 0);
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        pane.set_kind(squallar_egui::pane::PaneKind::CrossSection);
        pane.cross_section_mut()
            .expect("a section pane has a section")
            .line = Some(super::loop_section_tests::line());
        // Wider than the rung's own span, so this loop's ceiling really does
        // sit above its base — see `two_loops`. Without it the two figures
        // coincide and a need re-priced as *no drawn layers at all* would read
        // identical to the one below.
        pane.time.span_secs = 6 * 3600;
        let mut ls = active_loop(&stamps());
        ls.view = squallar_radar::types::RenderView::CrossSection;
        ls.cadence_secs = Some(300);
        *pane.time_state_mut(&known::RADAR) = ls;
    }
    assert_eq!(
        claimants(&app),
        vec![0],
        "floor: a section pane running a loop is a claimant",
    );
    let before = demand_of(&app);
    assert!(
        before.needs()[0].max_frames > before.needs()[0].base_frames,
        "floor: this loop must be able to grow past its base, or a need \
         re-priced with no drawn layers at all reads identical to this one",
    );

    set_radar(&mut app, 0, false);

    assert_eq!(
        claimants(&app),
        vec![0],
        "a pane with no map lost its share of the pool to a toggle that does \
         not speak for it: the Radar layer governs whether the MAP draws the \
         image, and this pane draws a section of every frame it holds",
    );
    assert_eq!(
        demand_of(&app),
        before,
        "and it must ask for the same thing: the need is priced over the \
         layers `animating_drawn_layers` yields, so a `draws_layer` that \
         asked the layer flag for radar too would leave this pane's walk \
         empty and fall through to the price-the-primary-alone arm — a \
         claimant still, at a ceiling collapsed onto its base",
    );
}

// ---------------------------------------------------------------------------
// 2. The front: getting the share back.

/// **A pane that starts drawing again is a claimant on the very next walk,
/// asking for exactly what it asked for before.**
///
/// The walk is a re-derivation on a state and carries nothing across the
/// switch, so this is the whole of the way back at this seam — and the
/// need is compared whole rather than counted, because a claimant restored at
/// a different price or a different ceiling is not the loop that left.
#[test]
fn a_pane_that_starts_drawing_again_is_a_claimant_on_the_next_walk() {
    let mut app = two_loops();
    let before = demand_of(&app);

    set_radar(&mut app, 1, false);
    assert_eq!(
        claimants(&app),
        vec![0],
        "precondition: the switch must really have taken the claim away",
    );

    set_radar(&mut app, 1, true);

    assert_eq!(
        demand_of(&app),
        before,
        "a pane that starts drawing again must ask for exactly what it asked \
         for before, on the first walk after the switch",
    );
}

/// **And the plan itself gives the grant back**, through production's own
/// dwell — `LoopPoolState::worth_taking` treats a loop that *started* as a
/// reason to re-plan whatever the dead band says, which is the arm a returning
/// pane arrives on.
#[test]
fn the_plan_gives_a_returning_panes_grant_back() {
    let mut app = two_loops();
    assert!(
        settled(&mut app).grant_for_pane(1).is_some(),
        "floor: pane 1 must hold a grant while it draws",
    );

    set_radar(&mut app, 1, false);
    let without = settled(&mut app);
    assert_eq!(
        without.grants().len(),
        1,
        "the plan in force must stop charging the pool for a loop nobody sees",
    );

    set_radar(&mut app, 1, true);
    let back = settled(&mut app);

    assert!(
        back.grant_for_pane(1).is_some(),
        "a pane that draws again must get a grant again: `worth_taking` reads \
         a loop with no grant in force as *started*, which is the arm that \
         does not wait on the dead band's ratio",
    );
    assert!(
        back.grant_for_pane(0).is_some(),
        "and the pane that never stopped drawing keeps its own",
    );
}

/// **A sibling pane drawing the same picture set loses nothing when the pane
/// beside it stops drawing — it becomes the owner.**
///
/// This is what keeps the switched-off pane out of `seen` and not merely out
/// of the needs. An owner is what makes a later pane on the same identity an
/// **alias**, and an alias asks for nothing of its own because it reads its
/// owner's grant. Leave a pane that holds no frames owning the identity and
/// the pane on screen reads a grant that was never planned:
/// `LoopAllocation::grant_for_pane` resolves the alias to a pane with no
/// grant, `share_bytes_for` falls through to the pool's own summary and
/// `loop_render_budget` to the kind's ceiling — the sizing for "a loop the
/// plan has not seen", handed to a loop the plan is looking straight at.
#[test]
fn a_sibling_drawing_the_same_loop_becomes_its_owner_and_loses_nothing() {
    let mut app = one_shared_loop();
    let budgets = test_budgets();
    let model = LoopFrameModel::from_budgets(&budgets);

    let shared = demand_of(&app);
    assert_eq!(
        shared.shares(),
        1,
        "floor: one picture set is one claimant, or these two panes were \
         never an owner and an alias",
    );
    let pool = pool_with_one_spare_frame(&shared, model);
    let together = pool.plan(model, &shared);
    let read_as_an_alias = together
        .grant_for_pane(1)
        .expect("the alias reads its owner's grant");
    assert_eq!(
        read_as_an_alias.key.pane, 0,
        "floor: pane 1 must really be an alias of pane 0 here",
    );
    let held = read_as_an_alias.frames;

    // The **owner** stops drawing. The alias is the pane on screen.
    set_radar(&mut app, 0, false);
    let alone = pool.plan(model, &demand_of(&app));

    let now = alone
        .grant_for_pane(1)
        .expect("the pane that is drawing must hold a grant of its own");
    assert_eq!(
        now.key.pane, 1,
        "the pane still drawing must become the owner: a pane holding no \
         frames went on owning the identity, so this pane stayed an alias of \
         a grant that does not exist",
    );
    assert!(
        now.frames >= held,
        "and it must lose nothing by the change: {} frames where it read {held}",
        now.frames,
    );
    assert!(
        !alone.pane_shares_loop(1),
        "with nobody else drawing it, the set is this pane's alone",
    );
}

// ---------------------------------------------------------------------------
// 3. The divisor, inside one pane.

/// **A layer switched off beside a drawn one stops halving its share.**
///
/// The same misallocation one level down, and it has to move with the need or
/// the fix makes things worse: `pane_loop_need` prices a pane's need at the
/// dearest frame times the layers it is sized for, and `layer_share` divides
/// what the pane was granted by the layers animating on it. Sized over one
/// layer and divided by two, a pane's only drawn layer would come out of this
/// land holding *fewer* frames than it did before its neighbour was switched
/// off.
#[test]
fn a_layer_switched_off_beside_a_drawn_one_stops_halving_its_share() {
    let mut app = n_pane_app(1, SITE);
    point_at_site(&mut app, 0);
    app.loop_mgr = LoopDownloadManager::new();
    for &stamp in &stamps() {
        app.loop_mgr
            .cache_scan(SITE, stamp, volume_with_sweeps(&[TILT]));
    }
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        *pane.time_state_mut(&known::RADAR) = active_loop(&stamps());
        pane.time_state_mut(&known::RADAR).cadence_secs = Some(300);
        // A second animating layer, switched on: two layers dividing one
        // pane's bytes, which is what the need must be sized for.
        pane.set_overlay_enabled(known::MRMS, true);
        *pane.time_state_mut(&known::MRMS) = active_loop(&stamps());
        pane.time_state_mut(&known::MRMS).cadence_secs = Some(300);
    }

    let two_layers = demand_of(&app);
    let radar_alone_price = LoopFrameModel::from_budgets(&test_budgets())
        .bytes_for(squallar_radar::types::RenderView::PlanView);
    assert_eq!(
        two_layers.needs().len(),
        1,
        "premise: the pool sees a pane as one loop however many layers animate \
         on it",
    );
    assert!(
        two_layers.needs()[0].frame_bytes > radar_alone_price,
        "floor: two drawn layers must price the pane above one layer's frame, \
         or there is no halving here to stop",
    );

    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .set_overlay_enabled(known::MRMS, false);

    assert_eq!(
        demand_of(&app).needs()[0].frame_bytes,
        radar_alone_price,
        "a layer the pane does not draw went on being priced into the pane's \
         need while `layer_share` divided the grant by the layers that draw: \
         the pane is charged for two frames and buys one",
    );
}

// ---------------------------------------------------------------------------
// 4. The listing that lands after the switch-off.

/// Run a radar scan listing through the production arrival path for a pane
/// whose radar layer is `drawn`, and answer the app it left behind.
///
/// The listing has to land either way: it is what the frame list is made of,
/// nothing re-asks for a listing a pane stopped waiting for, and a pane left
/// in `FetchingScanList` waits there for the life of the session.
fn app_after_a_listing(drawn: bool) -> crate::app::App {
    use squallar_overlays::render::overlay_state::SourceEvent;
    use squallar_source::time::{FrameListing, FrameStamp};

    let listed = stamps();
    let range = (at(-15), at(0));

    let mut app = n_pane_app(1, SITE);
    point_at_site(&mut app, 0);
    app.loop_mgr = LoopDownloadManager::new();
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
            900,
            squallar_radar::sites::get_radar_site(SITE).expect("KTLX is a real radar"),
            squallar_radar::types::RenderView::PlanView,
        );
        pane.time_state_mut(&known::RADAR).asked_range = Some(range);
        assert_eq!(
            pane.time_state(&known::RADAR).phase,
            squallar_egui::pane::LoopPhase::FetchingScanList,
            "precondition: radar must be waiting on a listing, or the arrival \
             has nothing to answer",
        );
    }
    set_radar(&mut app, 0, drawn);

    let scans: Vec<(chrono::NaiveDateTime, squallar_radar::archive::Identifier)> = listed
        .iter()
        .map(|&valid| {
            (
                valid,
                squallar_radar::archive::Identifier::new(format!("{SITE}{valid}")),
            )
        })
        .collect();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: known::RADAR,
            listing: FrameListing {
                range,
                frames: listed
                    .iter()
                    .map(|&valid| FrameStamp { valid, run: None })
                    .collect(),
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: SITE.to_string(),
                range,
                scans,
            }),
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();
    app
}

fn frame_stamps(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    app.gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR)
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect()
}

/// **A listing that lands after the switch-off becomes the frame list and
/// buys not one volume.**
///
/// The listing itself has to land — see [`app_after_a_listing`] — so what is
/// held back is the spending it used to drag behind it: a `FramePlan` handed
/// to the download manager and one batch dispatched off it, bounded by
/// `concurrent_loop_downloads` and paid once per user-initiated listing.
///
/// `plan_frame_count` and not `pending_queue_count`, for the reason
/// `app_fetch::loop_restore_race_tests` gives in as many words: the pending
/// queue is *queued and undispatched* and the dispatch drains it in the same
/// pass, so it reads zero on the spending path too and an assertion against it
/// could not fail.
#[test]
fn a_listing_that_lands_after_the_switch_off_buys_nothing_and_still_becomes_the_frame_list() {
    let drawn = app_after_a_listing(true);
    assert_eq!(
        frame_stamps(&drawn),
        stamps(),
        "floor: the listing must become the frame list on the drawing arm",
    );
    let planned = drawn.loop_mgr.plan_frame_count(0);
    assert_eq!(
        planned,
        stamps().len(),
        "floor: and must hand the download manager every frame it means to \
         draw, or the assertion below is about a path that buys nothing \
         either way",
    );

    let off = app_after_a_listing(false);

    assert_eq!(
        frame_stamps(&off),
        stamps(),
        "the listing must still become the frame list: nothing re-asks for a \
         listing a pane stopped waiting for, so refusing it here strands the \
         loop in FetchingScanList for the life of the session",
    );
    assert_eq!(
        off.loop_mgr.plan_frame_count(0),
        0,
        "a listing landing after the switch-off went on handing the download \
         manager a plan and dispatching a batch of volumes for a layer nobody \
         is looking at",
    );
}

/// **And the loop comes back whole on the first pass after the layer does.**
///
/// The unset plan is itself the way back:
/// `LoopDownloadManager::plan_describes` answers `false` for a pane it has
/// never heard of, which is the state a re-derivation is keyed on, so
/// `dispatch_loop_renders` re-derives the queue from the very frame list the
/// listing left behind.
#[test]
fn the_loop_a_held_back_listing_left_is_re_derived_when_the_layer_returns() {
    let mut app = app_after_a_listing(false);
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        0,
        "precondition: nothing was planned while the layer was off",
    );

    set_radar(&mut app, 0, true);
    app.dispatch_loop_renders();

    assert!(
        app.loop_mgr
            .plan_describes(0, SITE, frame_stamps(&app).into_iter()),
        "the queue was never re-derived: the frame list stood, the plan was \
         never set, and nothing brought one back — the loop would hold frames \
         it never asks for",
    );
    assert_eq!(
        frame_stamps(&app),
        stamps(),
        "and the frames the listing left are the frames it comes back to",
    );
}

// ---------------------------------------------------------------------------
// 5. The ceiling this file's fixtures depend on.

/// **A loop with a cadence really can grow past its base**, which every
/// balloon claim above assumes. `loop_ceiling_frames` answers the base itself
/// for a loop with neither a listing nor a cadence, and against such a demand
/// `LoopPool::plan` has nothing to hand anybody — every assertion about where
/// a spare frame went would read green on a plan that never spent one.
#[test]
fn the_fixture_loops_are_growable() {
    let app = two_loops();
    let budgets = test_budgets();
    for need in demand_of(&app).needs() {
        assert!(
            need.max_frames > need.base_frames,
            "pane {} cannot grow: base {}, ceiling {}",
            need.key.pane,
            need.base_frames,
            need.max_frames,
        );
        assert_eq!(
            need.max_frames,
            loop_ceiling_frames(
                None,
                need.span_secs,
                need.cadence_secs,
                need.base_frames,
                budgets.loop_frames_held,
            ),
            "and its ceiling is the one the model states for its window",
        );
    }
}
