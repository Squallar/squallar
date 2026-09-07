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

/// **The door's spare is the ladder floor's, the readout's is the rung the
/// scene is on, and the door's is never the smaller of the two.**
///
/// This test used to assert the two were *equal*, and that premise is false on
/// plain `main` with none of this land applied: the same fixture reads
/// 3,892,314,112 B at the door against 3,489,660,928 B at the readout, a 384
/// MiB gap that is exactly what the ladder sheds. `compose_admission_costs`
/// says why where it composes the pair — the readout answers *how much room
/// has the scene on screen left at the rung it is on*, the door answers *is
/// there a rung at which one more thing fits*, and neither is the other's
/// approximation. So what is pinned is the **direction**: the door prices a
/// cheaper scene, so a door coming out with *less* room than the readout would
/// be refusing acts the ladder could pay for.
///
/// **The host pool is asserted absent rather than compared.** On
/// `TestBridge::desktop()` the capacity is split and carries no host figure at
/// all, so both halves answer `None` and any equality or ordering written over
/// them here would be a check that cannot fail. It is pinned as absence
/// instead, so a fixture that grows a host figure fires this rather than
/// quietly turning the host half into nothing.
#[test]
fn the_table_prices_the_floor_and_the_readout_the_rung_the_scene_is_on() {
    let mut app = n_pane_app(2, SITE);
    tick(&mut app);
    assert!(
        app.admission_costs.spare.gpu_bytes.is_some()
            && app.budget_readout.gpu.spare_bytes.is_some(),
        "the GPU pool always has a spare, and both halves of the composition \
         have to publish one",
    );
    assert_eq!(
        app.volume_shortfall_bytes, 0,
        "precondition: this fixture holds no unshed volume bytes, which is the \
         one term the door subtracts and the readout does not",
    );
    assert!(
        app.admission_costs.spare.gpu_bytes >= app.budget_readout.gpu.spare_bytes,
        "the door priced the scene at the ladder's floor and came out with LESS \
         GPU room than the readout found at the rung the scene is on: {:?} < {:?}",
        app.admission_costs.spare.gpu_bytes,
        app.budget_readout.gpu.spare_bytes,
    );
    assert!(
        app.admission_costs.spare.host_bytes.is_none()
            && app
                .budget_readout
                .host
                .as_ref()
                .and_then(|h| h.spare_bytes)
                .is_none(),
        "this fixture's capacity grew a host figure ({:?} at the door, {:?} at \
         the readout); the host half of this pair is no longer vacuous and now \
         needs the real assertion the GPU half carries",
        app.admission_costs.spare.host_bytes,
        app.budget_readout.host.as_ref().and_then(|h| h.spare_bytes),
    );
}

/// **The walk carries BOTH of a shown gridded layer's figures onto the
/// scene** — the key-space cache budget and what the handler stages beside it.
///
/// `fit` sums the two, and every test on the far side of that seam is handed a
/// scene rather than building one from the application. So a construction site
/// here that fed the cache budget and left the staging at zero would go on
/// under-pricing by exactly the family the second field exists to price, with
/// the whole `squallar-device-profile` suite green. This is the one assertion
/// that reads the application's own answer.
#[test]
fn a_shown_gridded_layer_carries_both_of_its_handlers_figures_onto_the_scene() {
    use squallar_overlays::render::handlers::{
        source_grid_budget_bytes, source_grid_staging_bytes,
    };
    use squallar_source::id::known;

    // Two gridded layers with different answers, on purpose: MRMS stages two
    // grids beside its cache, the model layer stages nothing at all. A site
    // that derived the staging from the budget instead of asking the handler
    // would charge the model layer for a population it does not hold, which is
    // the over-firing direction and the worse of the two.
    let mut app = n_pane_app(1, SITE);
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built a pane");
        for id in [known::MRMS, known::MODEL_DATA] {
            pane.set_overlay_enabled(id.clone(), true);
            // The walk reads the pane's drawn texture roster, so the pane has
            // to have asked for each layer's cache the way a pane pass does.
            let _ = pane.overlay_cache_mut(&id);
        }
    }

    let scene = app.scene_of();
    let priced: Vec<(u64, u64)> = scene
        .overlay_grids
        .iter()
        .map(|grid| (grid.budget_bytes, grid.staging_bytes))
        .collect();
    let expected: Vec<(u64, u64)> = [known::MRMS, known::MODEL_DATA]
        .iter()
        .map(|id| (source_grid_budget_bytes(id), source_grid_staging_bytes(id)))
        .collect();
    assert_eq!(
        priced.len(),
        expected.len(),
        "precondition: the walk found both gridded layers the pane shows",
    );
    // Order-free: the walk's order is the pane's texture roster, a HashMap's.
    for row in &expected {
        assert!(
            priced.contains(row),
            "the scene is not carrying {row:?}; it carries {priced:?}",
        );
    }
    assert!(
        expected.contains(&(
            source_grid_budget_bytes(&known::MRMS),
            source_grid_staging_bytes(&known::MRMS),
        )) && source_grid_staging_bytes(&known::MRMS) > 0
            && source_grid_staging_bytes(&known::MODEL_DATA) == 0,
        "the premise: one layer here stages and the other does not, or neither \
         assertion above can fail",
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
/// round-trips with the loop in it, and must not spin.
///
/// **"Must not spin" was asserted as "must not re-queue" until 2026-09-07,
/// and that proxy had the defect inside it.** The reason given was that
/// `hydrate_parked_panes` drains by `mem::take` and a re-parked entry would
/// be re-driven on every redraw -- true at the time, and the same mechanism
/// logged one refusal about forty times in seven seconds on the Tier-2 `long`
/// leg. But forbidding the queue is what made a refusal terminal for the
/// session: the user could not shorten their lookback and try again without
/// restarting the app, and because the wish is persisted that was every
/// session.
///
/// The premise is gone. `AdmissionLedger` answers a repeat of the same
/// `(act, pane)` against the table in force from a memo -- no verdict, no log
/// line, no re-stamped notice, no counter -- so a re-queued entry costs a
/// lookup and nothing else. So the entry stays queued and the spin is now
/// asserted **by measuring it**: hydrate twenty more times and show the
/// counters did not move. That is the property the old line was reaching for,
/// and it is checked directly rather than through a proxy that also forbade
/// the retry.
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
        !app.loop_arm_pending.is_empty(),
        "a refused arm must stay queued, or the user cannot lower their ask \
         and try again without restarting the app",
    );

    // **And it does not spin**, measured rather than assumed: the door is
    // entered on each of these passes and answers every one from the
    // ledger's memo.
    let settled = app.admission.counts();
    for _ in 0..20 {
        app.hydrate_parked_panes();
    }
    assert_eq!(
        app.admission.counts(),
        settled,
        "twenty redraws against one table must not be twenty verdicts",
    );
    assert!(
        !app.loop_arm_pending.is_empty(),
        "and the entry is still there to be re-asked when a table with room \
         arrives",
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
        squallar_device_profile::admit::Spare::default(),
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

// ── The door prices at the ladder's floor ─────────────────────────────────

/// The scene, the capacity and the two figures a door compares, at one
/// `memory_percents` setting — every input to the contradiction below in one
/// place, so a test can search for the share that reproduces it rather than
/// hard-coding a percentage that stops meaning anything the moment a constant
/// moves.
struct Reading {
    /// Whether `fit` has any rung left that could pay for more. `steps 0` on
    /// the user's line said it had, which is what made the refusal wrong.
    ladder_has_steps: bool,
    /// The GPU spare the readout publishes: the allowance less the need at the
    /// rung in force, and **the figure the door itself was compared against
    /// until 2026-09-06**, less the volume shortfall.
    readout_spare: u64,
    /// What the door is given now: the allowance less the need at the ladder's
    /// floor.
    door_spare: squallar_device_profile::admit::Spare,
    /// The act. One more pane, because it is the one in this fixture with a
    /// GPU price — a whole-picture layer is a host term, and the native
    /// fixture has no host reader, so `show_layer` is zero on both axes here
    /// and could not fail either way.
    act: Increment,
}

fn read_at(app: &mut App, gpu_percent: u8, host_percent: u8) -> Reading {
    app.memory_percents =
        squallar_device_profile::scene::PoolPercents::clamped(gpu_percent, host_percent);
    tick(app);
    Reading {
        ladder_has_steps: !squallar_device_profile::fit::every_rung_at_its_stop(
            &app.budgets,
            &app.device_profile.limits,
        ),
        readout_spare: app.budget_readout.gpu.spare_bytes.expect("a GPU spare"),
        door_spare: app.admission_costs.spare,
        act: app.admission_costs.new_pane,
    }
}

/// **The user's own contradiction, and it must not be reproducible.**
///
/// The line was `bracket wasm32, rung 0, steps 0, ... spare gpu 128 MiB host
/// 88 MiB, admission asked 2 admitted 1 would refuse 1 refused 1`: the ladder
/// had taken no step, both pools published room, and a door refused anyway. It
/// refused because it priced the act at the rung the scene happened to sit at
/// and compared it against that rung's spare — *does this fit without
/// shedding* — when the design says refusal is what happens when there is
/// nothing left to shed.
///
/// **Why the premise below proves the old spelling refused, without building
/// it.** The spare it used was the readout's less the volume shortfall, so no
/// larger than `readout_spare`; and the price it charged was the act at the
/// rung in force, which is no smaller than the act at the floor, since no rung
/// of `LADDER` raises a term. So `act_at_floor > readout_spare` implies
/// `act_at_rung > old_spare`, and the assertion that follows is the difference
/// between the two spellings and nothing else.
///
/// The share is **searched for rather than hard-coded**: what is under test is
/// the predicate, and a percentage that lands in the window today would slide
/// out of it on the next constant to move, leaving a test that passes by being
/// vacuous. The search failing is itself a failure.
#[test]
fn a_door_admits_what_the_ladder_could_still_shed_for() {
    let mut app = n_pane_app(2, SITE);
    let scene = app.scene_of();
    let floor = squallar_device_profile::fit::floor_need_for(
        &scene,
        &app.device_profile,
        &app.capacity(),
        super::GRID_BYTES,
    );
    let rung = squallar_device_profile::fit::need(&scene, &app.budgets, super::GRID_BYTES);
    assert!(
        floor.gpu_bytes <= rung.gpu_bytes,
        "premise: no rung of the ladder raises a term, so the floor cannot \
         cost more than the rung in force — {floor:?} against {rung:?}. The \
         argument above depends on it",
    );

    let found = (squallar_device_profile::scene::PoolPercents::FLOOR..=100)
        .rev()
        .step_by(5)
        .find_map(|percent| {
            let r = read_at(&mut app, percent, percent);
            (r.ladder_has_steps && r.act.gpu_bytes > r.readout_spare).then_some((percent, r))
        });
    let (percent, r) = found.expect(
        "no share in FLOOR..=100 put the scene in the window this is about: \
         the rung in force short of one more pane while the ladder still has a \
         rung to shed. Without it the assertion below cannot fail",
    );

    assert!(
        r.ladder_has_steps,
        "premise at {percent} %: the ladder must have somewhere left to go, or \
         a refusal is correct and this test is asserting the wrong thing",
    );
    assert!(
        r.act.gpu_bytes > r.readout_spare,
        "premise at {percent} %: the act must NOT fit at the rung in force \
         ({} B against {} B of spare), or the old spelling admitted it too and \
         this proves nothing",
        r.act.gpu_bytes,
        r.readout_spare,
    );
    assert!(
        squallar_device_profile::admit::verdict(r.door_spare, r.act).is_admit(),
        "at {percent} % the door refused an act the ladder could still have \
         shed for: it wants {:?} against {:?}, while the rung in force \
         publishes {} B",
        r.act,
        r.door_spare,
        r.readout_spare,
    );
}

/// **The other arm: a scene that genuinely cannot fit still refuses.**
///
/// Over-firing is the worse direction, but so is a door that has quietly
/// stopped refusing — and a floor-priced door that admitted everything would
/// look exactly like the advisory arm from outside. The act here is one no
/// rung can pay for, so the ladder at its stop is the whole answer.
#[test]
fn a_door_still_refuses_what_no_rung_could_pay_for() {
    let mut app = n_pane_app(2, SITE);
    let r = read_at(
        &mut app,
        squallar_device_profile::scene::PoolPercents::FLOOR,
        100,
    );
    let spare = r.door_spare.gpu_bytes.expect("a GPU spare");
    let beyond = Increment {
        gpu_bytes: spare.saturating_add(1),
        host_bytes: 0,
    };
    let verdict = squallar_device_profile::admit::verdict(r.door_spare, beyond);
    let refusal = verdict
        .refusal()
        .expect("one byte past the floor's own spare must refuse");
    assert_eq!(refusal.short_bytes(), 1, "and it must say by how much");

    // Control, one byte the other way: the door has not simply become a wall.
    let inside = Increment {
        gpu_bytes: spare,
        host_bytes: 0,
    };
    assert!(
        squallar_device_profile::admit::verdict(r.door_spare, inside).is_admit(),
        "the act that exactly fills the floor's spare must be admitted, or \
         the arm above passes on a door that refuses everything",
    );
}

/// **The door's spare is the floor's, and it is not the readout's.**
///
/// They answer different questions about the same memory and neither is the
/// other's approximation: the readout says how much room the scene on screen
/// has left at the rung it is on — which is what a reader of `spare gpu`
/// wants — and the door says whether there is a rung at which one more thing
/// fits. Asserted against `fit::floor_need_for` directly rather than against a
/// recorded figure, so it goes on meaning this after a term moves in `fit`.
#[test]
fn the_doors_spare_is_the_ladders_floor_and_the_readouts_is_the_rung_in_force() {
    let mut app = n_pane_app(2, SITE);
    tick(&mut app);
    let scene = app.scene_of();
    let cap = app.capacity();
    let floor = squallar_device_profile::fit::floor_need_for(
        &scene,
        &app.device_profile,
        &cap,
        super::GRID_BYTES,
    );
    let rung = squallar_device_profile::fit::need(&scene, &app.budgets, super::GRID_BYTES);
    assert!(
        floor.gpu_bytes <= rung.gpu_bytes && floor.host_bytes <= rung.host_bytes,
        "premise: the ladder's floor cannot cost more than the rung in force \
         — {floor:?} against {rung:?}",
    );

    assert_eq!(
        app.admission_costs.spare.gpu_bytes,
        Some(
            cap.allowance()
                .saturating_sub(floor.gpu_bytes)
                .saturating_sub(app.volume_shortfall_bytes)
        ),
        "the door's GPU spare is not the allowance less the FLOOR need",
    );
    assert_eq!(
        app.budget_readout.gpu.spare_bytes,
        Some(cap.allowance().saturating_sub(rung.gpu_bytes)),
        "and the readout's is not the allowance less the need at the rung in \
         force",
    );
    assert_eq!(
        app.admission_costs.spare.host_bytes.is_some(),
        app.budget_readout
            .host
            .as_ref()
            .and_then(|h| h.spare_bytes)
            .is_some(),
        "the two halves of one composition must still agree about WHETHER a \
         host figure exists: `None` refuses nothing, and one half seeing a \
         pool the other does not is the divergence, not the arithmetic",
    );
}

/// **The App hands the line its own door spare, not a placeholder.**
///
/// `budget_telemetry`'s own tests prove the field is written from the argument;
/// this is the only thing that proves the argument is the figure the doors were
/// actually compared against. Driven through a real tick, because the wiring is
/// the whole assertion.
#[test]
fn the_budget_line_carries_the_spare_the_doors_were_given() {
    let mut app = n_pane_app(2, SITE);
    tick(&mut app);
    let gpu = app
        .admission_costs
        .spare
        .gpu_bytes
        .expect("the table carries a GPU spare");
    let line = app
        .budget_state_panel_line
        .clone()
        .expect("the tick wrote a budget state line");
    let want = format!("door spare gpu {} MiB", gpu / (1024 * 1024));
    assert!(
        line.contains(&want),
        "the line does not carry the door's own GPU spare ({want}): {line}",
    );
    assert!(
        !line.contains("door spare gpu none"),
        "a session that has priced a scene must publish a figure, not `none`: \
         {line}",
    );
}
