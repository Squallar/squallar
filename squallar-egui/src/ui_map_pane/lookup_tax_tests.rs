//! **What the layer walk pays to find out *which layer this is*.**
//!
//! The walk in [`render_pane_map_content`] never holds a layer; it holds an
//! id and asks the registry and the pane's slot list for everything else. Both
//! answer by scanning — `OverlayRegistry::handler` walks its handler vector
//! calling the virtual [`OverlayHandler::id`] on each candidate, and
//! `PaneState::slot` walks the slot list comparing strings — so the walk's
//! identity cost is a product of three numbers nothing here controls: how many
//! layers are on, how many questions the walk asks per layer, and how far down
//! the registry each answer sits.
//!
//! These read that product off
//! [`squallar_overlays::render::overlay_state::lookup_ledger`], on a pane with
//! **every registered layer switched on** — the shape the six-pane feed seeds
//! use, and the shape every one-pane rig scene does not.
//!
//! **Denominators.** Every figure below is *per pane, per walk*, at
//! [`sources::all`]'s full registry with every layer enabled and no pointer in
//! the pane. A frame runs one walk per pane on a plan-view pane and two on a
//! pane wearing a 3D floor strip, so a six-pane frame is six of these and
//! nothing here is a frame figure.

use super::*;
use crate::pane::{PaneState, slot_ledger};
use squallar_overlays::render::overlay_state::{OverlayRegistry, lookup_ledger};

/// **The measured number of registry lookups one walk over an all-layers pane
/// makes** — filled in from this tree's own reading, not chosen.
///
/// A ceiling that may only fall, in the shape of the coupling ratchets: a walk
/// that asks more questions than this is a regression that has to be argued
/// for, not one that lands quietly.
const LOOKUPS_PER_WALK_CEILING: u64 = 87;

/// **The measured number of full `LayerId` comparisons one walk over an
/// all-layers pane makes asking its own pane where a layer sits** — the other
/// half of the same tax, and until the index below it was the larger half.
///
/// A ceiling in the same shape as [`LOOKUPS_PER_WALK_CEILING`], and it is set
/// at the number of *lookups* rather than at a scan's length on purpose: a
/// resolver that confirms exactly one candidate per question makes one of
/// these per hit and none per miss, so anything above the lookup count is a
/// scan that has come back.
const SLOT_COMPARES_PER_LOOKUP_CEILING: f64 = 1.0;

/// The two ledgers' figures for one walk over a pane with `panes` panes' worth
/// of walks: `((lookups, probes, vcalls), (slot lookups, marks, compares))`.
///
/// The two halves have **different denominators and are never added**: the
/// first is what the walk asks the *registry* (which handler is this id), the
/// second what it asks the *pane* (where in my stack is this id). A frame pays
/// both, per layer, per pane.
///
/// One registry and one pane list for the whole run, built once and walked
/// `panes` times, because that is what a frame does: `render_panes` loops the
/// panes over one registry.
fn walk_ledger(panes: usize) -> ((u64, u64, u64), (u64, u64, u64)) {
    walk_ledger_scene(panes, LayersOn::Every)
}

/// **Which of the registered layers the scene switches on.**
///
/// The six-pane feed seeds — and every rig scene this campaign owns — are
/// `Every`. A real user's panes are not, and the difference is invisible to any
/// figure measured on the seeds.
#[derive(Clone, Copy, Debug)]
enum LayersOn {
    /// Every registered layer, which is what `walk_ledger` measures.
    Every,
    /// **Every second registered layer, in registry order** — a constructed
    /// scene, and the construction is stated because nothing we own has one:
    /// the eighteen handlers `sources::all()` registers are switched on and off
    /// alternately, so roughly half the texture layers are off and the walk
    /// still has to decide that about each of them.
    EverySecond,
}

impl LayersOn {
    fn takes(self, idx: usize) -> bool {
        match self {
            Self::Every => true,
            Self::EverySecond => idx.is_multiple_of(2),
        }
    }
}

fn walk_ledger_scene(panes: usize, on: LayersOn) -> ((u64, u64, u64), (u64, u64, u64)) {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();
    let mut overlays = OverlayRegistry::with_handlers(crate::sources::all());

    // Every layer this build registers, switched on. `all()` is the registry
    // the app runs, so this is the six-pane feed seed's layer set and not a
    // hand-listed subset that could drift from it.
    let ids: Vec<LayerId> = overlays.handlers().map(|h| h.id()).collect();
    let mut states: Vec<PaneState> = (0..panes)
        .map(|_| {
            let mut pane = PaneState::new();
            for (idx, id) in ids.iter().enumerate() {
                pane.set_overlay_enabled(id.clone(), on.takes(idx));
            }
            pane.hydrate_layer_states(&overlays, 0);
            pane
        })
        .collect();

    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
    let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));
    let preferences = UserPreferences::default();

    let mut actions = Vec::new();
    let mut click_consumed = false;
    let mut galley_cache = walkers::GalleyCache::default();
    let mut point_text_meshes = crate::point_painter::PointTextMeshes::default();
    let mut label_cache = crate::label_cache::LabelCache::default();

    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let mut ui = egui::Ui::new(
        egui_ctx.clone(),
        egui::Id::new("lookup_tax"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );

    // Reset AFTER the fixture is built: `hydrate_layer_states` and
    // `set_overlay_enabled` both ask the registry, and those are a config
    // load's cost, not a frame's.
    lookup_ledger::reset();
    slot_ledger::reset();
    for (pane_idx, pane) in states.iter_mut().enumerate() {
        let budget = std::cell::Cell::new(u64::MAX);
        let mut ctx = PaneRenderCtx {
            admission_notice: None,
            cost: None,
            pane_idx,
            pane,
            overlays: &mut overlays,
            user_location: None,
            user_heading: None,
            user_fix: None,
            basemap_labels: Vec::new(),
            galley_cache: &mut galley_cache,
            point_text_meshes: &mut point_text_meshes,
            label_cache: &mut label_cache,
            ground_meshes: None,
            radar_fan: None,
            basemap_tiles: None,
            terrain_tiles: None,
            tile_zoom_bias: 0,
            overlay_render_limit: 1,
            overlay_dispatch_budget: &budget,
            overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
            actions: &mut actions,
            pane_rect: canvas,
            surfaces: PaneSurfaces::GroundAndGlass,
            draws_3d_ground: GroundIsMesh::PLAN_VIEW,
            horizontal_color_scale: true,
            color_scale_floor: canvas.max.y,
            pointer_available: false,
            excluded_rects: Vec::new(),
            long_press_pos: None,
            overlay_click_pos: None,
            click_consumed: &mut click_consumed,
            preferences: &preferences,
            paint_order: Vec::new(),
        };
        render_pane_map_content(&mut ui, &projector, memory.zoom(), &mut ctx);
    }
    let read = (lookup_ledger::read(), slot_ledger::read());
    let _ = egui_ctx.end_pass();
    read
}

/// **The walk's identity questions, pinned as a ceiling that may only fall.**
///
/// Not a target and not a budget: a *measured* figure, pinned so that a change
/// that asks the registry more often than this has to say so. The ceiling is
/// the measured value with no headroom above it, because headroom is how a
/// regression lands inside a pin.
#[test]
fn one_walk_over_every_layer_asks_the_registry_a_bounded_number_of_times() {
    let ((lookups, probes, vcalls), _) = walk_ledger(1);
    // Printed whether or not the assertion fires: the figure is the finding.
    eprintln!("one pane, every layer on: {lookups} lookups, {probes} probes, {vcalls} vcalls");
    assert!(
        lookups <= LOOKUPS_PER_WALK_CEILING,
        "the walk asked the registry {lookups} times for one pane, over the \
         {LOOKUPS_PER_WALK_CEILING} this tree measured. Identity lookups are a \
         per-pane-per-frame cost and this ceiling may only fall."
    );
}

/// **The lookups are per pane, with nothing six panes share.**
///
/// The `ui:panes` cut is named for the thing it scales with, and every
/// frame-time figure this campaign holds was measured at one pane. This says
/// what six costs: exactly six times one, no better, because the walk carries
/// nothing across panes.
#[test]
fn registry_lookups_scale_one_for_one_with_panes() {
    let ((one, one_probes, _), _) = walk_ledger(1);
    let ((six, six_probes, _), _) = walk_ledger(6);
    eprintln!("one pane {one} lookups / {one_probes} probes; six panes {six} / {six_probes}");
    assert_eq!(
        six,
        one * 6,
        "six panes cost {six} lookups against one pane's {one}: the walk either \
         gained a term six panes share, or shed one, and either way the \
         one-pane figures this campaign is steering by no longer scale."
    );
}

/// **Answering "which handler is this id" must not ask a handler.**
///
/// `OverlayHandler::id` is a virtual call that builds and drops a `LayerId`,
/// and a resolver that calls it per candidate makes the walk's identity cost
/// proportional to the registry's size *in indirect calls*, not just in string
/// compares. The registry is built once and never gains or loses a handler
/// after `with_handlers`, so it can keep the ids it was built with and answer
/// from them.
///
/// **This is the gate on that**, and it reads red on any tree whose resolver
/// scans by asking.
#[test]
fn registry_lookups_ask_no_handler_its_own_id() {
    let ((lookups, _, vcalls), _) = walk_ledger(1);
    assert_eq!(
        vcalls, 0,
        "{lookups} lookups made {vcalls} `OverlayHandler::id` calls between \
         them. The resolver is asking each candidate handler its own identity \
         to answer a question the registry already knows the answer to."
    );
}

/// **The other half of the same question, and the one nothing was counting.**
///
/// The registry answers "which handler is this id"; the pane answers "where in
/// my stack is this id", and until the index landed it answered by the same
/// linear scan — over `LayerSlot`s rather than over a flat id list, so each
/// probe strode a whole slot struct to reach the `LayerId` at its front.
///
/// Printed whether or not it asserts, because the figure is the finding.
#[test]
fn one_walk_asks_its_own_pane_where_a_layer_is_more_often_than_it_asks_the_registry() {
    let ((registry_lookups, registry_probes, _), (slot_lookups, marks, compares)) = walk_ledger(1);
    eprintln!(
        "one pane, every layer on: registry {registry_lookups} lookups / \
         {registry_probes} probes; pane {slot_lookups} lookups / {marks} marks / \
         {compares} compares"
    );
    assert!(
        slot_lookups > 0,
        "the walk asked its pane where a layer sits zero times, which means \
         this instrument is not wired to the walk it claims to measure"
    );
}

/// **A confirmed candidate per question, and no scan behind it.**
///
/// This is the gate on the index. A resolver that scans makes one full
/// `LayerId` comparison per slot it strides past; one that indexes makes
/// exactly one, on the candidate it is about to return, and none at all on a
/// miss. So `compares <= lookups` is the property, and it reads red on any
/// tree whose `PaneState::slot` is `self.layers.iter().find(..)`.
#[test]
fn the_pane_confirms_one_candidate_per_question_rather_than_scanning() {
    let (_, (lookups, marks, compares)) = walk_ledger(1);
    let per_lookup = compares as f64 / lookups as f64;
    eprintln!(
        "pane lookups {lookups}, marks {marks}, full id compares {compares} \
         ({per_lookup:.2} per lookup)"
    );
    assert!(
        per_lookup <= SLOT_COMPARES_PER_LOOKUP_CEILING,
        "the walk made {compares} full `LayerId` comparisons over {lookups} \
         questions ({per_lookup:.2} each, over the \
         {SLOT_COMPARES_PER_LOOKUP_CEILING} an index makes). The pane is \
         answering `where is this layer` by walking its slot list."
    );
}

/// **The pane's half scales with panes exactly as the registry's does.**
///
/// Six panes is six times one, because a pane's stack is its own and nothing
/// is shared across them — the same property
/// `registry_lookups_scale_one_for_one_with_panes` pins on the other ledger,
/// and it is pinned here too so that a future term shared between panes cannot
/// appear on one side without the other noticing.
#[test]
fn pane_lookups_scale_one_for_one_with_panes() {
    let (_, (one, one_marks, one_compares)) = walk_ledger(1);
    let (_, (six, six_marks, six_compares)) = walk_ledger(6);
    eprintln!(
        "one pane {one} lookups / {one_marks} marks / {one_compares} compares; \
         six panes {six} / {six_marks} / {six_compares}"
    );
    assert_eq!(
        six,
        one * 6,
        "six panes asked their panes {six} times against one pane's {one}: the \
         walk either gained a term six panes share or shed one, and either way \
         the one-pane figures this campaign steers by no longer scale."
    );
}

/// **What the walk pays for a layer the pane has switched OFF.**
///
/// The cache-token pass asks every texture layer for its content signature, its
/// theme sensitivity, its as-of term and whether it has data — four registry
/// resolutions and four slot lookups — and then, for a disabled layer, throws
/// all four answers away: every consumer of them sits inside an `enabled &&`
/// conjunction.
///
/// **This is worth exactly nothing on any scene this campaign measures**, which
/// all switch every layer on, and it is paid by every user whose panes are not
/// all-on. So the figure is quoted against a scene constructed for it —
/// [`LayersOn::EverySecond`] — and the two are printed side by side so the
/// denominators cannot be confused.
///
/// The property pinned is the one that survives a scene change: switching a
/// layer OFF must not cost MORE than leaving it on, per layer walked.
#[test]
fn a_layer_the_pane_switched_off_is_not_probed_as_if_it_were_on() {
    let ((all_on_registry, ..), (all_on_pane, ..)) = walk_ledger_scene(1, LayersOn::Every);
    let ((half_off_registry, ..), (half_off_pane, ..)) =
        walk_ledger_scene(1, LayersOn::EverySecond);
    eprintln!(
        "one pane: every layer on = {all_on_registry} registry / {all_on_pane} pane \
         lookups; every second layer on = {half_off_registry} / {half_off_pane}"
    );
    assert!(
        half_off_registry < all_on_registry,
        "a pane with half its layers switched off asked the registry \
         {half_off_registry} times against an all-on pane's {all_on_registry}. \
         The walk is pricing a layer the user cannot see as if it drew."
    );
    assert!(
        half_off_pane < all_on_pane,
        "a pane with half its layers switched off asked its own stack \
         {half_off_pane} times against an all-on pane's {all_on_pane}."
    );
}

/// **The hovering pane's own walk: what it asks, and that it still answers.**
///
/// The layer walk resolves a handler for every enabled layer under the pointer
/// to ask [`OverlayHandler::hover_value_at`], and fifteen of the eighteen
/// registered layers cannot answer anything but `None`.
/// `OverlayRegistry::may_answer_hover` decides that before the resolution;
/// these say the decision costs the layers that CAN answer nothing, and that
/// the saving is real.
///
/// The fixture's whole point is the pointer. `render_pane_map_content`'s hover
/// block runs only inside `pane_rect.contains(pointer_hover_pos())`, so
/// [`walk_ledger_scene`]'s pointer-free scene — the one every figure above is
/// measured on — never enters it at all, and no ceiling above moves whatever
/// the block does.
mod hover_pass {
    use super::*;
    use squallar_overlays::render::overlay_state::{
        FetchPayload, OverlayHandler, OverlayItem, PaneRef, RenderMode, Surface,
    };
    use squallar_source::time::TimeAxis;
    use std::sync::Arc;

    /// A layer that answers hover with a fixed string, and says separately
    /// whether it declares that it does.
    struct HoverProbe {
        id: LayerId,
        weight: u32,
        answers: bool,
        says: &'static str,
    }

    impl OverlayHandler for HoverProbe {
        fn id(&self) -> LayerId {
            self.id.clone()
        }
        fn surface(&self) -> Surface {
            Surface::Glass
        }
        fn draw_order_weight(&self) -> u32 {
            self.weight
        }
        fn display_name(&self) -> &str {
            "HoverProbe"
        }
        fn render_mode(&self) -> RenderMode {
            RenderMode::PerFrameDirect
        }
        fn data_generation(&self) -> u64 {
            0
        }
        fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
            true
        }
        fn is_fetching(&self) -> bool {
            false
        }
        fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
        fn fetch_time(&self) -> Option<web_time::Instant> {
            None
        }
        fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
        fn retain_selections(
            &self,
            _selections: &mut Vec<Arc<dyn OverlayItem>>,
            _pane: &PaneRef<'_>,
        ) {
        }
        fn time_axis(&self) -> TimeAxis {
            TimeAxis::Live
        }
        fn answers_hover(&self) -> bool {
            self.answers
        }
        fn hover_value_at(&self, _lat: f64, _lon: f64, _pane: &PaneRef<'_>) -> Option<String> {
            Some(self.says.to_string())
        }
    }

    /// One `(id, draw weight, declares hover, what it answers)` probe.
    type Probe = (LayerId, u32, bool, &'static str);

    /// Run one walk over a pane holding every registered layer plus `probes`,
    /// with the pointer parked in the middle of the pane or absent. Returns
    /// what the walk left in `overlay_hover_value` and the registry lookups it
    /// made.
    fn hover_walk(probes: Vec<Probe>, pointer_in_pane: bool) -> (Option<String>, u64) {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let pointer = canvas.center();
        let egui_ctx = egui::Context::default();
        let mut handlers = crate::sources::all();
        for (id, weight, answers, says) in probes {
            handlers.push(Box::new(HoverProbe {
                id,
                weight,
                answers,
                says,
            }));
        }
        let mut overlays = OverlayRegistry::with_handlers(handlers);
        let mut weights: Vec<(LayerId, u32)> = overlays
            .handlers()
            .map(|h| (h.id(), h.draw_order_weight()))
            .collect();
        weights.sort_by_key(|(_, w)| *w);
        let order: Vec<LayerId> = weights.into_iter().map(|(id, _)| id).collect();
        let mut pane = PaneState::new();
        for id in &order {
            pane.set_overlay_enabled(id.clone(), true);
        }
        pane.set_draw_order(&order);
        pane.hydrate_layer_states(&overlays, 0);

        let mut memory = walkers::MapMemory::default();
        memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));
        let preferences = UserPreferences::default();
        let mut actions = Vec::new();
        let mut click_consumed = false;
        let mut galley_cache = walkers::GalleyCache::default();
        let mut point_text_meshes = crate::point_painter::PointTextMeshes::default();
        let mut label_cache = crate::label_cache::LabelCache::default();

        // The pointer is the fixture. Without it `pointer_hover_pos()` is
        // `None` and the block under test never runs.
        egui_ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            events: if pointer_in_pane {
                vec![egui::Event::PointerMoved(pointer)]
            } else {
                Vec::new()
            },
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            egui_ctx.clone(),
            egui::Id::new("hover_tax"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(canvas),
        );
        assert_eq!(
            ui.ctx().pointer_hover_pos(),
            pointer_in_pane.then_some(pointer),
            "the fixture did not place the pointer it was asked for, so the \
             hover block did not run and nothing below means anything"
        );
        lookup_ledger::reset();
        let budget = std::cell::Cell::new(u64::MAX);
        let mut ctx = PaneRenderCtx {
            admission_notice: None,
            cost: None,
            pane_idx: 0,
            pane: &mut pane,
            overlays: &mut overlays,
            user_location: None,
            user_heading: None,
            user_fix: None,
            basemap_labels: Vec::new(),
            galley_cache: &mut galley_cache,
            point_text_meshes: &mut point_text_meshes,
            label_cache: &mut label_cache,
            ground_meshes: None,
            radar_fan: None,
            basemap_tiles: None,
            terrain_tiles: None,
            tile_zoom_bias: 0,
            overlay_render_limit: 1,
            overlay_dispatch_budget: &budget,
            overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
            actions: &mut actions,
            pane_rect: canvas,
            surfaces: PaneSurfaces::GroundAndGlass,
            draws_3d_ground: GroundIsMesh::PLAN_VIEW,
            horizontal_color_scale: true,
            color_scale_floor: canvas.max.y,
            pointer_available: pointer_in_pane,
            excluded_rects: Vec::new(),
            long_press_pos: None,
            overlay_click_pos: None,
            click_consumed: &mut click_consumed,
            preferences: &preferences,
            paint_order: Vec::new(),
        };
        render_pane_map_content(&mut ui, &projector, memory.zoom(), &mut ctx);
        let (lookups, _, _) = lookup_ledger::read();
        let value = pane.overlay_hover_value.clone();
        let _ = egui_ctx.end_pass();
        (value, lookups)
    }

    /// **A declaring layer is still asked, and its answer still lands.**
    ///
    /// The gate on the skip, and the one no pixel digest can stand in for: a
    /// walk that filtered too hard leaves `overlay_hover_value` at `None` and
    /// the status bar's pointer readout blank, with every pixel of the map
    /// identical.
    #[test]
    fn a_declaring_layer_still_puts_its_value_on_the_pane() {
        let (value, _) = hover_walk(vec![(LayerId::new("HoverA"), 900, true, "A says 7")], true);
        assert_eq!(
            value.as_deref(),
            Some("A says 7"),
            "a layer that declares `answers_hover` and answers was not asked"
        );
    }

    /// **Draw order decides, exactly as it did.**
    ///
    /// Two declaring layers both answer; the walk takes the first in the
    /// pane's own bottom-to-top order. `PaneState::slots()` is the same list
    /// `draw_order()` iterates, and this says so through the walk rather than
    /// through the container.
    #[test]
    fn the_lowest_answering_layer_in_the_stack_wins() {
        let (value, _) = hover_walk(
            vec![
                (LayerId::new("HoverHigh"), 950, true, "high"),
                (LayerId::new("HoverLow"), 900, true, "low"),
            ],
            true,
        );
        assert_eq!(
            value.as_deref(),
            Some("low"),
            "the walk took the higher layer's answer: draw order is no longer \
             what decides the pointer readout"
        );
    }

    /// **What the pointer costs the walk, as a ceiling that may only fall.**
    ///
    /// The same pane and the same registry, walked twice with the pointer as
    /// the only difference, so the figure is the pointer's own tax rather than
    /// the walk's total. The hover block is what used to make that tax scale
    /// with the registry — one resolution per *enabled* layer — and the bound
    /// says it now scales with the layers that **declare** hover.
    ///
    /// No headroom above the declaring count, in the shape of every other
    /// ceiling in this file: the walk's other pointer arms — the per-frame
    /// point pass's hover position, the site icons' — cost **zero** further
    /// registry resolutions on this fixture, measured, so the bound is the
    /// declaring layers and nothing else.
    #[test]
    fn hovering_costs_a_lookup_only_for_the_layers_that_can_answer() {
        // ModelData, Mrms, Gmgsi, and the probe.
        const DECLARING: u64 = 4;
        let probes: Vec<Probe> = vec![(LayerId::new("HoverA"), 900, true, "A says 7")];
        let (_, hovering) = hover_walk(probes.clone(), true);
        let (_, still) = hover_walk(probes, false);
        let tax = hovering - still;
        eprintln!(
            "same pane, pointer on = {hovering} registry lookups, pointer off \
             = {still}: the pointer costs {tax}, of which {DECLARING} are the \
             layers that declare hover"
        );
        assert!(
            tax <= DECLARING,
            "the pointer cost the walk {tax} registry lookups over a \
             pointer-free walk of the same pane, above the {DECLARING} layers \
             that declare hover. The hover block is resolving handlers that \
             cannot answer."
        );
    }
}

/// **The point pass, priced against the number of points it draws.**
///
/// A registry with exactly one layer in it, whose only interesting property is
/// how many points it puts on the map. Everything else answers the trait's own
/// default, so the walk exercised below is the point pass and nothing else.
mod point_pass {
    use super::*;
    use squallar_overlays::render::draw::MapPoint;
    use squallar_overlays::render::overlay_state::{
        FetchPayload, OverlayHandler, OverlayItem, PaneRef, PopupContent, RenderMode, Surface,
    };
    use squallar_source::time::TimeAxis;
    use std::sync::Arc;

    /// The selection a point carries. Never opened by these tests -- the walk
    /// only clones it on a click, and no walk here has one.
    #[derive(Debug)]
    struct Sel;

    impl OverlayItem for Sel {
        fn layer_id(&self) -> LayerId {
            known::METAR
        }
        fn popup_content(&self, _prefs: &UserPreferences) -> PopupContent {
            PopupContent {
                title: String::new(),
                accent_rgb: [0, 0, 0],
                width: 1.0,
                sections: Vec::new(),
                actions: Vec::new(),
            }
        }
        fn matches(&self, _other: &dyn OverlayItem) -> bool {
            false
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// A per-frame point layer with `n` points, all of them on the glass in
    /// front of the fixture's camera so the walk's geo cull keeps every one.
    struct PointProbe {
        points: Vec<MapPoint>,
    }

    impl PointProbe {
        fn with(n: u32) -> Self {
            let sel: Arc<dyn OverlayItem> = Arc::new(Sel);
            // A lattice a tenth of a degree across, centred on the fixture's
            // KTLX camera: at zoom 7 every one of these is well inside the
            // 800x600 canvas, so `geo_bounds.contains_point` keeps them and
            // the loop body runs `n` times rather than culling early.
            let points = (0..n)
                .map(|i| MapPoint {
                    lat: 35.33 + f64::from(i % 10) * 0.01,
                    lon: -97.28 + f64::from(i / 10) * 0.01,
                    id: i,
                    selection: Arc::clone(&sel),
                })
                .collect();
            Self { points }
        }
    }

    impl OverlayHandler for PointProbe {
        fn id(&self) -> LayerId {
            known::METAR
        }
        fn surface(&self) -> Surface {
            Surface::Glass
        }
        fn draw_order_weight(&self) -> u32 {
            100
        }
        fn display_name(&self) -> &str {
            "PointProbe"
        }
        fn render_mode(&self) -> RenderMode {
            RenderMode::PerFramePoint
        }
        fn data_generation(&self) -> u64 {
            0
        }
        fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
            true
        }
        fn is_fetching(&self) -> bool {
            false
        }
        fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
        fn fetch_time(&self) -> Option<web_time::Instant> {
            None
        }
        fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
        fn retain_selections(
            &self,
            _selections: &mut Vec<Arc<dyn OverlayItem>>,
            _pane: &PaneRef<'_>,
        ) {
        }
        fn time_axis(&self) -> TimeAxis {
            TimeAxis::Live
        }
        fn per_frame_points(&self) -> &[MapPoint] {
            &self.points
        }
        fn point_hit_radius(&self, _zoom: f32) -> f32 {
            5.0
        }
    }

    /// Registry lookups for one walk over a pane holding only `PointProbe`
    /// with `n` points.
    fn lookups_for(n: u32) -> u64 {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let egui_ctx = egui::Context::default();
        let mut overlays = OverlayRegistry::with_handlers(vec![
            Box::new(PointProbe::with(n)) as Box<dyn OverlayHandler>
        ]);
        let mut pane = crate::pane::PaneState::new();
        pane.set_overlay_enabled(known::METAR, true);
        pane.hydrate_layer_states(&overlays, 0);

        let mut memory = walkers::MapMemory::default();
        memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));
        let preferences = UserPreferences::default();
        let mut actions = Vec::new();
        let mut click_consumed = false;
        let mut galley_cache = walkers::GalleyCache::default();
        let mut point_text_meshes = crate::point_painter::PointTextMeshes::default();
        let mut label_cache = crate::label_cache::LabelCache::default();

        egui_ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            egui_ctx.clone(),
            egui::Id::new("point_pass_lookup_tax"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(canvas),
        );
        lookup_ledger::reset();
        let budget = std::cell::Cell::new(u64::MAX);
        let mut ctx = PaneRenderCtx {
            admission_notice: None,
            cost: None,
            pane_idx: 0,
            pane: &mut pane,
            overlays: &mut overlays,
            user_location: None,
            user_heading: None,
            user_fix: None,
            basemap_labels: Vec::new(),
            galley_cache: &mut galley_cache,
            point_text_meshes: &mut point_text_meshes,
            label_cache: &mut label_cache,
            ground_meshes: None,
            radar_fan: None,
            basemap_tiles: None,
            terrain_tiles: None,
            tile_zoom_bias: 0,
            overlay_render_limit: 1,
            overlay_dispatch_budget: &budget,
            overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
            actions: &mut actions,
            pane_rect: canvas,
            surfaces: PaneSurfaces::GroundAndGlass,
            draws_3d_ground: GroundIsMesh::PLAN_VIEW,
            horizontal_color_scale: true,
            color_scale_floor: canvas.max.y,
            pointer_available: false,
            excluded_rects: Vec::new(),
            long_press_pos: None,
            overlay_click_pos: None,
            click_consumed: &mut click_consumed,
            preferences: &preferences,
            paint_order: Vec::new(),
        };
        render_pane_map_content(&mut ui, &projector, memory.zoom(), &mut ctx);
        let (lookups, _, _) = lookup_ledger::read();
        let _ = egui_ctx.end_pass();
        lookups
    }

    /// **A point is not a layer, and must not be priced as one.**
    ///
    /// `OverlayRegistry::draw_point` takes a `&LayerId` and resolves it, and
    /// the point pass called it *from inside the point loop* -- so a station
    /// table cost one identity resolution per station per pane per frame to
    /// reach a handler that had already been resolved to draw the layer at
    /// all. The interact frames the campaign bar is measured on are exactly
    /// the frames where that loop runs: the kept text mesh's key carries the
    /// projector, so a pan or a zoom re-collects every station.
    ///
    /// One point and a thousand must cost the registry the same.
    #[test]
    fn point_pass_lookups_do_not_scale_with_the_number_of_points() {
        let one = lookups_for(1);
        let many = lookups_for(1000);
        eprintln!("point pass: 1 point = {one} lookups, 1000 points = {many}");
        assert_eq!(
            one, many,
            "a thousand points cost {many} registry lookups against one \
             point's {one}. The point pass is resolving its own layer's \
             identity per point."
        );
    }
}
