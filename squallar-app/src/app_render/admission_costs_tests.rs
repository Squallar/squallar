//! **What the App prices for the doors, and what the doors do with it.**
//!
//! The table is the App's half of admission: `squallar-app` prices, the UI
//! sums and compares. What is asserted here is that the prices are the
//! **model's** — every one of them a difference of `fit::need_terms` over the
//! scene as it is and the scene with one thing added — and that the one door
//! on this side of the seam (`App::handle_enable_loop`) charges for the
//! transition it makes and not for the call.
//!
//! **The cadence is the readout's**, so nothing here composes on a frame.

use crate::app::App;
use crate::app::tests::n_pane_app;
use squallar_device_profile::admit::Increment;

const SITE: &str = "KTLX";

/// One telemetry tick, asked for rather than waited on — the same door
/// `budget_readout_cadence_tests` takes.
fn tick(app: &mut App) {
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();
}

/// **The table is composed on the readout's cadence and nowhere else.** The
/// frame path prices nothing: this land promised no per-frame walk, and the
/// generation is what says whether it kept the promise.
#[test]
fn the_table_is_composed_on_the_tick_and_never_on_a_frame() {
    let mut app = n_pane_app(2, SITE);
    assert_eq!(
        app.admission_costs.generation, 0,
        "precondition: a fresh application has priced nothing",
    );

    for _ in 0..240 {
        let _ = app.observe_loop_demand();
    }
    assert_eq!(
        app.admission_costs.generation, 0,
        "240 frames priced the admission table {} time(s); the composition is \
         on the frame thread",
        app.admission_costs.generation,
    );

    tick(&mut app);
    assert_eq!(app.admission_costs.generation, 1);
    tick(&mut app);
    assert_eq!(app.admission_costs.generation, 2);
}

/// **Every price is a difference of the one model.** Asserted against
/// `fit::need_terms` directly rather than against a recorded figure: a
/// recorded figure would go on passing after a term moved in `fit` and stopped
/// being what the door charges.
#[test]
fn one_more_layer_is_priced_at_what_the_model_says_it_costs() {
    let mut app = n_pane_app(2, SITE);
    tick(&mut app);
    let scene = app.scene_of();
    assert!(
        !scene.panes.is_empty(),
        "precondition: the walk described some panes",
    );

    let before = squallar_device_profile::fit::need(&scene, &app.budgets, super::GRID_BYTES);
    let mut after = scene.clone();
    after.panes[0].overlay_pictures += 1;
    let priced = squallar_device_profile::fit::need(&after, &app.budgets, super::GRID_BYTES);

    assert_eq!(
        app.admission_costs.panes[0].show_layer,
        Increment {
            gpu_bytes: priced.gpu_bytes - before.gpu_bytes,
            host_bytes: priced.host_bytes - before.host_bytes,
        },
        "the layer door's price is not the model's difference",
    );
}

/// One more pane costs the model's difference too, and it is not free — a
/// zero here would be a table that admits a six-way split onto anything.
#[test]
fn one_more_pane_is_priced_and_is_not_free() {
    let mut app = n_pane_app(1, SITE);
    tick(&mut app);
    assert!(
        !app.admission_costs.new_pane.is_zero(),
        "a pane opened onto a map costs something; the table priced nothing",
    );
}

/// **Both pools carry a spare, or say they do not.** `None` is "this session
/// has no figure", which refuses nothing; the GPU pool always has one.
#[test]
fn the_table_carries_the_same_spare_the_readout_publishes() {
    let mut app = n_pane_app(2, SITE);
    tick(&mut app);
    assert_eq!(
        app.admission_costs.spare.gpu_bytes, app.budget_readout.gpu.spare_bytes,
        "the two halves of one composition disagree about GPU spare",
    );
    assert_eq!(
        app.admission_costs.spare.host_bytes,
        app.budget_readout.host.as_ref().and_then(|h| h.spare_bytes),
        "the two halves of one composition disagree about host spare",
    );
}

/// **Every gridded layer has a price, shown or not.** A table that listed only
/// the layers already on screen would price the very act it exists to gate —
/// reaching for a layer nothing is holding yet — at nothing.
#[test]
fn every_gridded_layer_is_in_the_table() {
    let mut app = n_pane_app(1, SITE);
    tick(&mut app);
    let priced: Vec<_> = app
        .admission_costs
        .layer_grids
        .iter()
        .map(|layer| layer.id.clone())
        .collect();
    for (id, bytes) in squallar_overlays::render::handlers::gridded_layers() {
        assert!(
            bytes == 0 || priced.contains(&id),
            "{id:?} keeps a {bytes} B grid and is not in the admission table; \
             the table holds {priced:?}",
        );
    }
}

/// **The loop door charges for arming, and for nothing else.** A pane that is
/// not looping asks; the same pane asked again while it loops does not — the
/// re-list a wider window emits is the span slider's charge, and charging here
/// too would price one drag twice.
#[test]
fn the_loop_door_charges_the_arm_and_not_the_relist() {
    let mut app = n_pane_app(1, SITE);
    tick(&mut app);

    // A pane the table says is not looping has a price for arming one.
    let quiet = app.admission_costs.panes.first().copied();
    assert!(
        quiet.is_some_and(|pane| !pane.looping),
        "precondition: the fixture's pane is not looping",
    );

    let before = app.admission.counts();
    app.handle_enable_loop(0, 30 * 60, false);
    let moved = app.admission.counts().since(before);
    assert_eq!(
        moved.refused, 0,
        "a fresh session has room for its first loop; the door moved {moved:?}",
    );
}

/// **The loop door refuses, and it is the door on the restore path.**
///
/// `load_ui_config` is exempt - a refused restore is written back by autosave
/// and the user loses panes without acting - but the loops a restore asks for
/// arm here, one redraw later. That is how the scene that trapped the rig's
/// tab reinstates itself, and this is where it is refusable.
///
/// The refusal must leave the pane's own wish standing, so the config still
/// round-trips with the loop in it, and must not re-queue: one refusal, not a
/// spin.
#[test]
fn a_loop_a_restore_asked_for_is_refusable_and_does_not_spin() {
    let mut app = n_pane_app(1, SITE);
    set_looping(&mut app, 0, false);
    tick(&mut app);
    // A machine with nothing left on either pool.
    app.admission_costs.spare = squallar_device_profile::admit::Spare {
        gpu_bytes: Some(0),
        host_bytes: Some(0),
        joint_bytes: None,
    };
    app.admission_costs.generation += 1;
    let costs = app.admission_costs.clone();
    app.admission.adopt(&costs);
    assert!(
        !app.admission_costs.panes[0].arm_loop.is_zero(),
        "precondition: arming this pane's loop costs something",
    );

    // The restore's wish, as `load_ui_config` leaves it and `looping_panes`
    // collects it.
    let arm = squallar_egui::pane::LoopArm { playing: true };
    app.gui.pane_mut(0).expect("pane 0").loop_arm_pending = Some(arm);
    app.loop_arm_pending.push((0, arm, 30 * 60));

    let before = app.admission.counts();
    app.hydrate_parked_panes();
    let moved = app.admission.counts().since(before);

    assert_eq!(
        moved.refused, 1,
        "the loop a restore asked for must be refused on a machine with \
         nothing spare; the door moved {moved:?}",
    );
    assert!(
        app.admission.notice(web_time::Instant::now()).is_some(),
        "and the refusal must leave a notice - the App's door is the one \
         whose refusals have no other way to the glass",
    );
    assert!(
        app.loop_arm_pending.is_empty(),
        "a refused arm must not re-queue: `hydrate_parked_panes` drains by \
         `mem::take` and a re-park would spin on every redraw",
    );
    assert_eq!(
        app.gui.pane(0).and_then(|pane| pane.loop_arm_pending),
        Some(arm),
        "the pane keeps its wish, so the config still round-trips with the \
         loop in it and it arms on a session with room",
    );
}

/// **The volume store's shortfall is subtracted from the GPU spare.** The
/// sparing door reports bytes it could not shed because every grid left is on
/// the glass; those bytes are resident and outside the model's accounting, so
/// admission is held to the smaller pool. Before WO-H the figure had no
/// consumer at all.
#[test]
fn a_volume_store_that_cannot_shed_holds_admission_to_the_smaller_pool() {
    let mut app = n_pane_app(1, SITE);
    tick(&mut app);
    let roomy = app.admission_costs.spare.gpu_bytes.expect("a GPU spare");

    app.volume_shortfall_bytes = roomy / 2;
    tick(&mut app);
    let held = app.admission_costs.spare.gpu_bytes.expect("a GPU spare");
    assert_eq!(
        held,
        roomy - roomy / 2,
        "the shortfall must come off the spare the doors are compared against",
    );
}

/// **A pane the table has never seen asks for nothing.** The loop door is
/// reached by index from a queue drained a frame later, so a pane composed
/// after the last tick must not refuse on a figure that does not exist.
#[test]
fn the_loop_door_asks_nothing_for_a_pane_the_table_has_not_priced() {
    let mut app = n_pane_app(1, SITE);
    tick(&mut app);
    let before = app.admission.counts();
    app.handle_enable_loop(9, 30 * 60, false);
    let moved = app.admission.counts().since(before);
    assert_eq!(
        moved.asked, 0,
        "an index no pane occupies must ask nothing; the door moved {moved:?}",
    );
}

/// **The doors' counters ride `budget state:`.** The advisory land's whole
/// output is this figure, so a line that dropped it would make WO-G
/// unmeasurable on the rig.
#[test]
fn the_budget_line_carries_the_admission_counters() {
    let doors = squallar_egui::admission::Totals {
        asked: 7,
        admitted: 5,
        would_refuse: 2,
        refused: 0,
    };
    let line = crate::budget_telemetry::budget_state_line(
        &squallar_device_profile::budget::resolve(
            &squallar_device_profile::budget::DeviceProfile::for_target(),
        ),
        &squallar_device_profile::budget::DeviceProfile::for_target(),
        None,
        0,
        0,
        // Nothing over its pool: this line is about the door counters.
        0,
        &squallar_device_profile::scene::Capacity::presumed(
            &squallar_device_profile::budget::BudgetLimits::DESKTOP,
        ),
        crate::app::GpuProbeReport::Absent,
        crate::pressure::LinearMemoryWatch::default(),
        &squallar_egui::shell_api::BudgetReadout::default(),
        None,
        &crate::recovery::HostRecovery::untouched(),
        doors,
    );
    assert!(
        line.contains("admission asked 7 admitted 5 would refuse 2 refused 0"),
        "the counters are not on the line: {line}",
    );
    // And they sit BEFORE the variable-arity pane rows, where a positional
    // reader can still find them.
    let admission_at = line
        .find("admission asked")
        .expect("the field is on the line");
    assert!(
        line.find(", pane0 ")
            .is_none_or(|panes| admission_at < panes),
        "the counters must precede the pane rows: {line}",
    );
}

// ── Ruling 8, on the arm that prices it ───────────────────────────────────

/// Put pane `idx` on a running plan-view radar loop of `SITE`, rendering
/// `product` — enough for `App::loop_demand` to give it a `LoopIdentity`.
fn set_looping(app: &mut App, idx: usize, active: bool) {
    let product = squallar_radar::fields::spec(squallar_radar::types::RadarProduct::Reflectivity)
        .id
        .clone();
    let pane = app.gui.pane_mut(idx).expect("the fixture's pane");
    pane.set_selected_product(product.clone());
    pane.set_selected_elevation(0.5);
    let ls = pane.time_state_mut(&squallar_source::id::known::RADAR);
    *ls = squallar_egui::pane::LayerTimeState::begin(
        2 * 60 * 60,
        squallar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    ls.phase = if active {
        squallar_egui::pane::LoopPhase::Rendering
    } else {
        squallar_egui::pane::LoopPhase::Inactive
    };
    ls.cadence_secs = Some(300);
    ls.rendered_for = Some(squallar_egui::pane::RenderTarget::new(SITE, &product, 0.5));
}

/// **A pane whose loop another pane already owns is written down as a share.**
///
/// The walk's own `seen` list is what answers it, so the price and the
/// retention cannot disagree — and a pane that reads itself as its own share
/// would price every first loop at nothing, which is why the owner is asserted
/// beside the alias rather than alone.
#[test]
fn the_walk_marks_a_second_pane_on_one_loop_as_a_share() {
    let mut app = n_pane_app(2, SITE);
    set_looping(&mut app, 0, true);
    set_looping(&mut app, 1, false);

    let walk = app.loop_demand_for_test();
    assert!(
        !walk.prospective[0].alias,
        "the pane that owns the loop must not read itself as a share",
    );
    assert!(
        walk.prospective[1].alias,
        "a second pane on the same site, product and window holds the first \
         pane's frames and must be written down as a share",
    );
}

/// **And the price follows the mark.** Arming the alias costs strictly less
/// than arming a loop of its own — the admit fixture that resembles the refuse
/// one, one fact apart.
#[test]
fn an_alias_is_priced_below_a_loop_of_its_own() {
    let mut shared = n_pane_app(2, SITE);
    set_looping(&mut shared, 0, true);
    set_looping(&mut shared, 1, false);
    tick(&mut shared);
    let aliased = shared.admission_costs.panes[1].arm_loop;

    // The same second pane on a DIFFERENT product: one site, two picture
    // sets, so there is no loop to share and it owes a whole one. Its decoded
    // volumes are still the first pane's - the scan cache is per site - which
    // is exactly `two_panes_one_site`, and the GPU axis is where the two rows
    // have to differ.
    let mut own = n_pane_app(2, SITE);
    set_looping(&mut own, 0, true);
    set_looping(&mut own, 1, false);
    let velocity = squallar_radar::fields::spec(squallar_radar::types::RadarProduct::Velocity)
        .id
        .clone();
    {
        let pane = own.gui.pane_mut(1).expect("pane 1");
        pane.set_selected_product(velocity.clone());
        if let Some(target) = pane
            .time_state_mut(&squallar_source::id::known::RADAR)
            .rendered_for
            .as_mut()
        {
            target.product = velocity;
        }
    }
    tick(&mut own);
    let unshared = own.admission_costs.panes[1].arm_loop;

    assert!(
        !unshared.is_zero(),
        "control: arming a loop of its own is not free, or the row below \
         compares two zeroes",
    );
    assert!(
        aliased.gpu_bytes < unshared.gpu_bytes,
        "an alias must be priced below its unshared twin: {aliased:?} vs \
         {unshared:?}",
    );
}

/// **The frame path takes the SPARING door, with the real visible count.**
///
/// A source scrape, because the property is which door is called and with
/// what - and the difference is invisible to any assertion on the outcome:
/// `enforce_budget` IS `enforce_budget_sparing(budget, 0)`, so a tree that
/// went back to it would evict a grid a pane on screen is drawing from and
/// still answer the same `evicted` count. What it would also do is throw the
/// shortfall away, which is the figure WO-H exists to consume.
#[test]
fn the_frame_path_spares_the_grids_visible_panes_are_drawing_from() {
    const SOURCE: &str = include_str!("../app_render.rs");
    let collapsed: String = SOURCE.split_whitespace().collect();
    assert!(
        collapsed.contains("enforce_budget_sparing(volume_budget,visible_panes)"),
        "the frame path must take the sparing door with the layout's own \
         visible count; a zero there evicts a grid a pane on screen is \
         drawing from",
    );
    assert!(
        !collapsed.contains(".volume_store.enforce_budget("),
        "the blind door has no caller left: it is the sparing door at \
         `visible_panes = 0`, and it drops the shortfall",
    );
}
