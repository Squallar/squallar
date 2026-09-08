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
            for id in &ids {
                pane.set_overlay_enabled(id.clone(), true);
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
            ground_meshes: None,
            basemap_tiles: None,
            terrain_tiles: None,
            tile_zoom_bias: 0,
            overlay_render_limit: 1,
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
            ground_meshes: None,
            basemap_tiles: None,
            terrain_tiles: None,
            tile_zoom_bias: 0,
            overlay_render_limit: 1,
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
