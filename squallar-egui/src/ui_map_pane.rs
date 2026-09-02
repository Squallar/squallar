use crate::actions::GuiAction;
use crate::legend_ramp;
use crate::overlay_cache::{
    RenderSlot, current_quantized_zoom, draw_overlay_texture, plan_overlay_texture,
    viewport_geo_bounds,
};
use crate::pane::{LoopLoading, PaneState, RadarImageData, TimeMode};
use crate::point_painter::EguiPointPainter;
use squallar_overlays::render::draw::{DrawPointContext, HoverContext};
use squallar_overlays::render::overlay_state::{
    OverlayItem, OverlayLegend, OverlayRegistry, RenderMode, Signed, Surface,
};
use squallar_units::{HailSizeUnit, UserPreferences};
use std::sync::Arc;

use squallar_geo::KM_PER_DEGREE_LAT;
use squallar_radar::get_color_for_value;
use squallar_radar::hca::MeltingLayerSource;
use squallar_radar::hover::{HoverSource, Reading};
use squallar_radar::sites::RadarSite;
use squallar_source::id::{LayerId, known};
use squallar_source::product::FieldId;
use squallar_source::time::TimeAxis;

use super::super::map_overlays::{
    OverlayDrawContext, draw_tile_layer, is_pos_blocked, paint_labels,
};
use squallar_radar::fields as radar_fields;

/// Which of a pane's surfaces one call to [`render_pane_map_content`] paints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PaneSurfaces {
    GroundAndGlass,
    /// A 3D pane's off-screen floor strip: geography only — chrome down here
    /// would be mirrored onto the floor.
    GroundOnly,
}

impl PaneSurfaces {
    /// Whether this pass paints `surface` — the handler-declared [`Surface`].
    const fn paints(self, surface: Surface) -> bool {
        match self {
            Self::GroundAndGlass => true,
            Self::GroundOnly => matches!(surface, Surface::Ground),
        }
    }
}

/// Whether a pane's ground is a **3D mesh** rather than flat map pixels: the
/// floor strip's half of the question `VolumeFrameState::heights` is the
/// renderer's half of. Both are answered by `ui_map::pane_ground_heights`, so
/// the drape and the mesh cannot disagree about which ground is being drawn.
///
/// **A newtype and not a `bool`, and that is load-bearing.** Its field is
/// private to this module, so no caller outside `ui::map::pane_render` can
/// write `GroundIsMesh(true)`; the only two ways to obtain one are
/// [`PLAN_VIEW`](Self::PLAN_VIEW) and
/// [`from_height_field`](Self::from_height_field), and the latter answers
/// `true` only when handed the field the ground would actually be drawn
/// from. A `bool` here was compatible with a caller composing the answer out
/// of some belief of its own -- `|| pane.volume().is_some()` is one line, is
/// `true` for every 3D pane, and would strip the hillshade off every 3D
/// floor for ever while no mesh ever drew. That line does not typecheck
/// against this type, and `GroundHeightField` has no other production
/// construction to fabricate.
///
/// **The type is `pub(in crate::ui)` and the constructors are not**, and the
/// asymmetry is the point. The Base Map inspector lives outside `ui::map` and
/// has to hold one of these to decide whether the "Terrain shading" switch is
/// still doing anything, so it must be able to *name* the type; letting it
/// *mint* one would hand back exactly the belief the private field refuses.
/// So `PLAN_VIEW` and `from_height_field` stay `pub(super)`, and the only
/// door out of `ui::map` is [`super::pane_draws_3d_ground`], which reads the
/// one function.
///
/// **What it does NOT mean is "the mesh drew".** The strip is drawn before
/// the volume painter runs -- it has to be, it is the mirror that painter
/// samples -- so this can only be "the pane has a field to draw ground
/// from". The renderer stands down from the mesh at two further points the
/// strip cannot wait for, and both leave a flat lid under a strip that has
/// already dropped its hillshade:
///
/// * `GroundPlacement::for_box` declining to place the field over the drawn
///   box (`volume_bridge.rs`). How often that happens is a property of a
///   scheduler that does not exist yet, so this unit states no frequency for
///   it -- it is undetermined, not rare.
/// * `ensure_pane_heights` returning false because `upload_heights` refused
///   the field, which sets `map_floor` back on. That refusal is
///   `posts > min(max_texture_dimension_2d, MAX_POSTS_PER_AXIS)`: a
///   deterministic function of the adapter and the field, so it is refused
///   every frame, for ever, not transiently. `downlevel_webgl2_defaults()`
///   guarantees only **2048**, and the browser rig's legs select `Gl` -- so
///   on the target CLAUDE.md says governs, an over-large field means a
///   permanently unshaded ground. **Bounding the posts by the adapter's real
///   limit is B4's `HeightPlan::fit`**, and that is where the fix belongs;
///   naming it here is the declaration, not the remedy.
///
/// Closing either from this side needs the *previous* frame's outcome fed
/// back, which buys a transient with a permanent one-frame lag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) struct GroundIsMesh(bool);

impl GroundIsMesh {
    /// A pane drawing its map in plan: no mesh, no second light, nothing to
    /// ask. The only answer a 2D pane can be given, because the other one
    /// needs a height field and the plan view never has one.
    pub(super) const PLAN_VIEW: Self = Self(false);

    /// The only route to `true`, and it demands the field itself.
    pub(super) fn from_height_field(field: Option<&crate::volume_view::GroundHeightField>) -> Self {
        Self(field.is_some())
    }

    /// Whether the ground is a mesh.
    const fn yes(self) -> bool {
        self.0
    }
}

/// **Whether the scene light supersedes `id`'s shading on a pane whose ground
/// is `ground`** -- the one predicate behind every reader of "one ground, one
/// sun".
///
/// Three readers, and they must never disagree. The floor strip's paint walk
/// ([`PaneRenderCtx::double_shades`], which adds the pass conjunct) is what
/// stops the hillshade reaching the drape. [`ground_content_key`] mirrors it,
/// or the strip cache would key on a layer the strip does not draw. And the
/// Base Map inspector's "Terrain shading" switch goes inert against it and
/// names the light that took the work over. A switch that stayed live over a
/// strip that had dropped the layer reads as a bug -- which is the whole
/// reason this function has a third caller.
///
/// Only `TERRAIN` carries baked shading: the base tiles under it are unlit
/// colour and must keep drawing, or the mesh has nothing to wear.
pub(in crate::ui) fn scene_light_supersedes(ground: GroundIsMesh, id: &LayerId) -> bool {
    ground.yes() && *id == known::TERRAIN
}

/// What one `GroundOnly` pass left unresolved — the completeness half of the
/// floor-strip cache's skip decision.
///
/// Constructible only by [`render_pane_map_content`]: the fields are private
/// to this module, so the caller can read [`complete`](Self::complete) but
/// cannot mint a "resolved" answer out of a belief — the `GroundIsMesh`
/// rule, applied to the value that licenses skipping repaints.
#[derive(Clone, Copy, Debug)]
pub(super) struct StripResolution {
    /// Every tile arm that ran answered its whole span with exact tiles.
    tiles_exact: bool,
    /// Some texture overlay wants a raster this viewport **could not ask
    /// for**: `needs_rerender` said yes and the render slot refused, or a
    /// settle countdown is still running (`settle_is_counting_down`). While
    /// true the strip
    /// must keep repainting, because nothing else would re-ask.
    ///
    /// **Not "a raster is in flight".** A dispatch that went out carries its
    /// own wake-up — the arriving texture's identity is a
    /// [`ground_content_key`] input — so repainting across the flight buys
    /// nothing and costs a whole map render per frame. See the comment at the
    /// assignment for the measurement that made the distinction matter.
    overlay_work_owed: bool,
}

impl StripResolution {
    /// Whether the pass drew everything it could ever draw for its current
    /// inputs — the latch that allows the strip cache to skip. Pending tiles
    /// and owed rasters both hold it open, which is what keeps
    /// `request_once` re-asking and the settle machinery ticking.
    pub(super) fn complete(self) -> bool {
        self.tiles_exact && !self.overlay_work_owed
    }
}

/// Everything [`ground_content_key`] reads that is not on the pane itself.
pub(super) struct GroundKeyInputs<'a> {
    pub overlays: &'a OverlayRegistry,
    pub preferences: &'a UserPreferences,
    pub pane: &'a PaneState,
    pub pane_idx: usize,
    /// The off-screen rect the strip draws into.
    pub strip: egui::Rect,
    /// What the strip is centred on — [`super::floor_frame_for`]'s answer.
    pub centre: walkers::Position,
    /// The owned viewport the strip is drawn through. Its zoom (and, on the
    /// fallback arm, its detached position) is what places every pixel.
    pub memory: &'a walkers::MapMemory,
    /// The basemap source's tile put-generation, `None` while the slot is
    /// released. Per source: the terrain slot has its own.
    pub basemap_generation: Option<u64>,
    /// The terrain source's tile put-generation, likewise.
    pub terrain_generation: Option<u64>,
    pub tile_zoom_bias: u8,
    pub is_dark: bool,
    pub user_location: Option<(f64, f64)>,
    pub user_heading: Option<f32>,
    /// Presence only: the strip's paint reads the fix for a hover tooltip,
    /// and no pointer can hover an off-screen rect.
    pub user_fix_present: bool,
}

/// One degree quantized to ~0.55 m of latitude — fine enough that a walking
/// user's marker moves on the floor, coarse enough that GPS jitter at rest
/// does not repaint the strip every fix.
fn quantized_degrees(v: f64) -> i64 {
    (v * 200_000.0).round() as i64
}

/// The content key one 3D pane's floor strip is cached under: it moves
/// exactly when the strip's *pixels* would differ, and nothing else may move
/// it — a camera orbit leaves every input below untouched, which is the whole
/// lever.
///
/// The walk mirrors [`render_pane_map_content`]'s dispatch conditions arm for
/// arm: enabled, handled, `Surface::Ground`, and the hillshade suppression
/// for a mesh ground. Each drawn layer contributes its
/// [`overlay_cache_token`] (the ask — a data bump must repaint so the
/// dispatch loop runs) **and** the identity of the picture it would put on
/// the strip (the have — an arriving raster and a moving loop playhead must
/// repaint even though the token already moved a frame ago).
///
/// The day `ui_map::pane_ground_heights` stops returning `None`,
/// `GroundHeightField::id` joins these inputs with its own staleness fixture
/// — the terrain-wiring tripwire.
pub(super) fn ground_content_key(input: &GroundKeyInputs<'_>, ground: GroundIsMesh) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    let h = &mut hasher;

    // The viewport: the strip rect, what it is centred on, and the zoom it is
    // framed at (plus the detached position on the fallback arm, where the
    // pane's own memory places the strip).
    for bits in [
        input.strip.min.x.to_bits(),
        input.strip.min.y.to_bits(),
        input.strip.max.x.to_bits(),
        input.strip.max.y.to_bits(),
    ] {
        bits.hash(h);
    }
    input.centre.x().to_bits().hash(h);
    input.centre.y().to_bits().hash(h);
    input.memory.zoom().to_bits().hash(h);
    if let Some(detached) = input.memory.detached() {
        detached.x().to_bits().hash(h);
        detached.y().to_bits().hash(h);
    }
    input.tile_zoom_bias.hash(h);
    input.is_dark.hash(h);
    ground.yes().hash(h);
    // Units and timezone reach the per-frame point arms' labels.
    input.preferences.hash(h);

    let draw_order = input.pane.draw_order_vec();
    for id in &draw_order {
        if !input.pane.is_overlay_enabled(id) {
            continue;
        }
        let Some(handler) = input.overlays.handler_by_id(id) else {
            continue;
        };
        // The strip paints ground only; a mesh ground suppresses the
        // hillshade (`PaneRenderCtx::double_shades`).
        if !matches!(handler.surface(), Surface::Ground) {
            continue;
        }
        if scene_light_supersedes(ground, id) {
            continue;
        }
        id.hash(h);
        overlay_cache_token(
            input.overlays,
            input.pane_idx,
            input.pane,
            id,
            input.is_dark,
        )
        .hash(h);

        match id {
            id if *id == known::RADAR => {
                // The draw fork's two arms, keyed by which arm and which
                // picture: the loop's playhead frame while animating, the
                // live raster otherwise (an arriving radar render is a new
                // id too).
                //
                // **`active_image()` answers for a PLAN-VIEW loop only**, and
                // the difference matters to anything reasoning about what
                // moves this key. It narrows through
                // [`LoopFrameImage::plan_view`], so a `LoopFrameImage::Volume`
                // frame — a *named* resident grid, not a texture — reads
                // `None`, and a playing **volume** loop therefore hashes the
                // same constant on every tick. The floor still repaints per
                // tick under one, but it is the `overlay_cache_token` above
                // doing it: every `TimeAxis::EventLifetime` layer on the pane
                // re-tokenizes as the clock sweeps (a 60 s as-of quantum
                // against a tick worth minutes). Read
                // `a_playing_volume_loop_repaints_the_floor_per_tick_not_per_frame`
                // with that in mind — its non-vacuity rides on that layer, not
                // on this arm.
                let animating = input.pane.time_state(&known::RADAR).is_active();
                animating.hash(h);
                if animating {
                    input
                        .pane
                        .active_image()
                        .map(|img| img.texture.id())
                        .hash(h);
                } else {
                    input
                        .pane
                        .overlay_cache(id)
                        .and_then(|c| c.current())
                        .map(|tex| tex.texture.id())
                        .hash(h);
                }
            }
            id if *id == known::BASEMAP_TILES => {
                input.basemap_generation.hash(h);
            }
            id if *id == known::TERRAIN => {
                input.terrain_generation.hash(h);
            }
            id if *id == known::CITY_LABELS => {
                // The names are deferred out of the basemap tiles, so their
                // identity is the tiles'.
                input.basemap_generation.hash(h);
            }
            id if *id == known::USER_LOCATION => {
                input
                    .user_location
                    .map(|(lat, lon)| (quantized_degrees(lat), quantized_degrees(lon)))
                    .hash(h);
                // Half-degree steps: the wedge is 45 degrees wide, so finer
                // heading noise never moves a visible pixel.
                input
                    .user_heading
                    .map(|deg| (f64::from(deg) * 2.0).round() as i64)
                    .hash(h);
                input.user_fix_present.hash(h);
            }
            _ => {
                // Texture layers: the raster the draw fork would put on the
                // strip. Per-frame point layers have no texture and answer
                // `None` here; their pictures move with the content
                // signature already hashed above and the preferences.
                input
                    .pane
                    .overlay_texture_on_screen(id)
                    .map(|tex| tex.texture.id())
                    .hash(h);
            }
        }
    }

    hasher.finish()
}

/// Shared references needed for rendering a single pane's map content.
pub(super) struct PaneRenderCtx<'a> {
    pub pane_idx: usize,
    pub pane: &'a mut PaneState,
    pub overlays: &'a mut OverlayRegistry,
    pub user_location: Option<(f64, f64)>,
    pub user_heading: Option<f32>,
    pub user_fix: Option<squallar_location::Fix>,
    /// The basemap's place names, laid out by the `CityLabels` arm below.
    ///
    /// Starts empty and is filled by the `BasemapTiles` arm's
    /// [`draw_tile_layer`](super::super::map_overlays::draw_tile_layer) call
    /// in the ground phase, then taken by the `CityLabels` arm, so the names
    /// draw at that layer's position in the pane's order -- above the weather
    /// -- rather than under it with the ground they arrived on. A pane with
    /// CityLabels off never takes them and they are dropped with the context;
    /// a pane with **BasemapTiles** off never fills them, because the one
    /// tile draw is where the names come from.
    pub basemap_labels: Vec<walkers::Text>,
    /// The galley memo the `CityLabels` arm lays its names out through.
    ///
    /// Owned by [`crate::gui::Gui`] and lent for the frame, because the whole
    /// point of it is to outlive the frame: the same place names are laid out
    /// again on every one of them, and a cache built and dropped inside a
    /// frame would answer nothing. See [`walkers::GalleyCache`].
    pub galley_cache: &'a mut walkers::GalleyCache,
    /// The base tile source, taken out of `tiles::MapTileState` for the
    /// frame. `None` while the BasemapTiles layer is off in every visible
    /// pane (the slot is then released — a disabled layer costs zero
    /// network). Drawn by the `BasemapTiles` arm below, at the layer's own
    /// position in the pane's order: the bottom of the stack.
    pub basemap_tiles: Option<&'a mut crate::tile_source::HttpsTiles>,
    /// The terrain hillshade source, taken out of `tiles::MapTileState` for
    /// the frame the way the base tiles are. `None` while the Terrain layer
    /// is off in every visible pane (the slot is then released — a disabled
    /// layer costs zero network) or its archive is unusable. Drawn by the
    /// `Terrain` arm below, so the hillshade lands at the layer's own
    /// position in the pane's order: above the base ground, under the
    /// weather.
    pub terrain_tiles: Option<&'a mut crate::tile_source::HttpsTiles>,
    /// The tile zoom bias this pane's ground was drawn at, so the terrain
    /// tiles sample the same grid the basemap did this frame.
    pub tile_zoom_bias: u8,
    /// How many overlay rasters this pane and layer may have crossing at once —
    /// the device's `Budgets::concurrent_renders`. See
    /// [`crate::overlay_cache::RendersInFlight::admits`].
    pub overlay_render_limit: usize,
    pub actions: &'a mut Vec<GuiAction>,
    pub pane_rect: egui::Rect,
    /// Which halves of the pane's content this pass is for. See
    /// [`PaneSurfaces`].
    pub surfaces: PaneSurfaces,
    /// What can draw a vector tile's tessellated fills from the GPU, or
    /// `None` where nothing can. Read through
    /// [`Self::ground_mesh_painter`], which is where the floor strip is cut
    /// out of it.
    pub ground_meshes: Option<&'a std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
    /// Whether the pane this pass belongs to draws its ground as a 3D mesh
    /// rather than as flat map pixels. See [`GroundIsMesh`], which is a
    /// newtype rather than a `bool` for a reason this field is the whole
    /// point of.
    pub draws_3d_ground: GroundIsMesh,
    /// Whether this frame's color scale bars run along the bottom edge
    /// (`true`) or the right edge (`false`). Resolved once per map panel.
    pub horizontal_color_scale: bool,
    /// The lowest screen y the colour-scale legend may draw on: the map's
    /// bottom edge, less whatever the phone shell's bottom bar covered.
    pub color_scale_floor: f32,
    pub pointer_available: bool,
    /// Rects of chrome painted over the map with no egui layer of its own.
    pub excluded_rects: Vec<egui::Rect>,
    /// Screen position of an active long-press (for the radar value tooltip),
    /// or `None`. Only the touch pipeline ever produces one.
    pub long_press_pos: Option<egui::Pos2>,
    /// Screen position of a confirmed overlay click/tap, or `None` if no overlay
    /// click occurred this frame. On desktop this comes from egui's `any_click()`;
    /// on Android from the deferred single-tap detector.
    pub overlay_click_pos: Option<egui::Pos2>,
    /// Set by every handler that **acts** on
    /// [`overlay_click_pos`](Self::overlay_click_pos) — the consumption half of
    /// the fade trigger in `ui_fade.rs`.
    pub click_consumed: &'a mut bool,
    pub preferences: &'a UserPreferences,
    /// The kinds this pane dispatched, in the order they painted, with the
    /// egui layer each **arm** painted into. Two kinds on *different* layers
    /// composite in `GraphicLayers::drain`'s order, not in this sequence.
    #[cfg(test)]
    pub paint_order: Vec<(LayerId, egui::LayerId)>,
}

impl PaneRenderCtx<'_> {
    /// Whether this pass must skip `id` to stop the pane's ground being
    /// shaded twice.
    ///
    /// A 3D pane's ground is a lit mesh, and the floor strip is the texture
    /// that mesh is draped with. Hillshade is shading already baked into
    /// pixels, by a sun frozen in map space; drape it over a mesh the scene's
    /// own light shades and the terrain carries two shadows cast by two suns,
    /// one of which does not move when the camera does.
    ///
    /// Conjunct by conjunct. Only a [`GroundOnly`](PaneSurfaces::GroundOnly)
    /// pass is a floor strip, so nothing a 2D pane draws can move through
    /// here whatever a caller passes. Only a pane whose ground is a mesh has
    /// the second light. And only `TERRAIN` carries baked shading -- the base
    /// tiles beneath it are unlit colour and must keep drawing, or the mesh
    /// has nothing to wear.
    ///
    /// **THE SECOND SUN ARRIVES WITH C2, AND IS NOT IN THIS TREE.** Read the
    /// paragraph above as what this suppression is for, not as a description
    /// of today: `fs_ground` currently ends `out.colour = ground` -- the raw
    /// drape, with no light vector, no normal and no lambert term -- and
    /// `VolumeUniform`'s light lanes reach only the volume's own gradient
    /// shading. B3's drape oracle depends on exactly that, requiring the
    /// ground attachment to carry the checker cell's exact channel value,
    /// which no shading term would survive. So until C2 lights the mesh this
    /// condition *removes* shading from a 3D floor rather than
    /// de-duplicating it. The plan orders it this way on purpose -- C3
    /// depends on B3, not on C2 -- and C2 is what makes the picture whole.
    fn double_shades(&self, id: &LayerId) -> bool {
        self.surfaces == PaneSurfaces::GroundOnly
            && scene_light_supersedes(self.draws_3d_ground, id)
    }

    /// What may draw this pass's tile fills from the GPU — **never a floor
    /// strip**.
    ///
    /// A strip's primitives are copied into the mirror by
    /// `EguiRenderer::render_mirror`, which swaps every
    /// `Primitive::Callback` for an empty mesh before it draws: a callback
    /// would run its `prepare` twice otherwise, and `Renderer::render`
    /// ignores callbacks in that pass anyway. So a strip drawn through
    /// callbacks would reach the mirror with no ground in it at all, and the
    /// 3D floor would wear a map made of labels and roads. The strip keeps
    /// placing its fills on the CPU, which since the floor-strip cache is a
    /// cost it pays rarely rather than every frame.
    fn ground_mesh_painter(
        &self,
    ) -> Option<&std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>> {
        match self.surfaces {
            PaneSurfaces::GroundAndGlass => self.ground_meshes,
            PaneSurfaces::GroundOnly => None,
        }
    }
}

/// Render the map content for a single pane (SPC/NWS overlays, radar image,
/// city labels, radar sites, user location).
///
/// Answers what the pass left unresolved — the floor-strip cache's
/// completeness input. The plan-view caller discards it: a 2D pane repaints
/// whenever egui runs it, so it has no skip to license.
pub(super) fn render_pane_map_content(
    ui: &mut egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    ctx: &mut PaneRenderCtx<'_>,
) -> StripResolution {
    let mut resolution = StripResolution {
        tiles_exact: true,
        overlay_work_owed: false,
    };

    ctx.pane.hydrate_layer_states(ctx.overlays, ctx.pane_idx);

    // Cleared every frame and re-set by the radar arm below. That arm is the
    // only writer, and it runs only while Radar is enabled and has an image.
    ctx.pane.hover_value = None;

    // Sites take priority over the overlays beneath them. Kept out of
    // `ctx.excluded_rects`, which `handle_radar_site_interactions` reads
    // itself: with the icons in there, every site click was self-blocked.
    let visible_sites = visible_radar_sites(ui, projector, zoom, ctx.pane);
    // What the overlays *under* the sites must not be clicked through.
    let overlay_excluded_rects: Vec<egui::Rect> = ctx
        .excluded_rects
        .iter()
        .copied()
        .chain(visible_sites.iter().map(|s| s.icon_rect))
        .collect();

    // RadarSites requires `allocate_rect` (&mut ui), so it is deferred to Phase 2.
    {
        let overlay_ctx = OverlayDrawContext::new(
            ui,
            projector,
            ctx.pointer_available,
            ctx.pane_rect,
            &overlay_excluded_rects,
            ctx.overlay_click_pos,
        );

        let mut selected: Vec<Arc<dyn OverlayItem>> = Vec::new();
        // The stale-image notice, deferred out of the Radar arm's position:
        // it must read over every overlay drawn after the radar.
        let mut pending_notice: Option<(FieldId, f32)> = None;
        let mut melting_layer_caveat: Option<MeltingLayerSource> = None;

        let draw_order: Vec<LayerId> = ctx.pane.draw_order_vec();
        for id in &draw_order {
            if !ctx.pane.is_overlay_enabled(id) {
                continue;
            }
            // An id with no registered handler is RETAINED in the list and
            // skipped at draw, so a newer build's layer keeps its place.
            let Some(handler) = ctx.overlays.handler_by_id(id) else {
                continue;
            };
            // The ground/glass split: a pass not painting this layer's surface
            // skips the arm entirely, so it also skips the paint-order record.
            if !ctx.surfaces.paints(handler.surface()) {
                continue;
            }
            // And the strip of a pane whose ground is a lit mesh skips the
            // layer that would shade that mesh a second time -- same skip,
            // same place, so it too leaves the paint-order record alone.
            if ctx.double_shades(id) {
                continue;
            }
            // Every arm below paints through `ui.painter()` — the pane's own
            // paint list — so submission order IS `draw_order`.
            #[cfg(test)]
            let mut painted_layer = ui.painter().layer_id();
            match id {
                id if *id == known::RADAR => {
                    // **Radar-addressed, and it stays that way** (WI-1 left
                    // this site for WI-6 to judge). It is spelled
                    // `time_state(&known::RADAR)`, and everything under it reads
                    // radar's own picture: retargeting it at the transport
                    // would ask a model timeline whether to paint radar's
                    // texture. The generic arm below asks the same question of
                    // its own layer, which is the same rule, not a different
                    // one.
                    if ctx.pane.time_state(&known::RADAR).is_active() {
                        if let Some(img) = ctx.pane.active_image().cloned() {
                            render_radar_overlay(
                                ui,
                                projector,
                                &img,
                                ctx.pane,
                                ctx.pane_rect,
                                ctx.preferences,
                            );
                        }
                    } else {
                        let meta_snapshot = ctx
                            .pane
                            .overlay_cache(id)
                            .and_then(|c| c.current())
                            .and_then(|tex| tex.radar_meta.as_ref())
                            .map(|m| {
                                (
                                    m.lat,
                                    m.lon,
                                    m.max_range_km,
                                    std::sync::Arc::clone(&m.hover),
                                )
                            });

                        if let Some(tex) = ctx.pane.overlay_cache(id).and_then(|c| c.current()) {
                            let screen_rect = ui.max_rect();
                            draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                        }

                        if let Some((lat, lon, extent_km, hover)) = meta_snapshot {
                            render_radar_range_ring(ui, projector, lat, lon, extent_km);
                            update_pane_hover_value_from_meta(
                                ui,
                                projector,
                                &RadarHoverData {
                                    hover: &hover,
                                    lat,
                                    lon,
                                },
                                ctx.pane,
                                ctx.pane_rect,
                                ctx.preferences,
                            );
                        }
                    }

                    // The pixels above are not the selection every other label
                    // on this pane is describing — say which product they are.
                    pending_notice = ctx.pane.stale_image_on_screen();
                    // And what the classification behind them is standing on,
                    // when nobody measured it. Never both `Some`.
                    melting_layer_caveat = ctx
                        .pane
                        .displayed_melting_layer_source()
                        .filter(|source| !source.is_measured());
                }
                id if *id == known::BASEMAP_TILES => {
                    // The ground phase: paints the tile geometry and defers
                    // every label the tiles carry into `ctx.basemap_labels`
                    // for the `CityLabels` arm — this arm runs first because
                    // the layer's weight (1) is the lowest in the registry.
                    let ground = ctx.ground_mesh_painter().cloned();
                    if let Some(tiles) = ctx.basemap_tiles.as_deref_mut() {
                        let paint = draw_tile_layer(
                            ui,
                            projector,
                            zoom,
                            tiles,
                            ctx.tile_zoom_bias,
                            ground.as_ref(),
                        );
                        resolution.tiles_exact &= paint.coverage.complete();
                        ctx.basemap_labels = paint.labels;
                    }
                }
                id if *id == known::TERRAIN => {
                    // A raster layer defers no labels: only a vector tile
                    // carries text, so the returned list is empty by
                    // construction and there is nothing to keep.
                    // `None`, and by construction rather than by policy: a
                    // raster tile flattens to no fill runs, so a painter here
                    // would have nothing to hand it.
                    if let Some(tiles) = ctx.terrain_tiles.as_deref_mut() {
                        let paint =
                            draw_tile_layer(ui, projector, zoom, tiles, ctx.tile_zoom_bias, None);
                        resolution.tiles_exact &= paint.coverage.complete();
                    }
                }
                id if *id == known::CITY_LABELS => {
                    // One `OccupiedAreas` for the whole pane, which is what
                    // stops a name being drawn once per tile that carries it.
                    paint_labels(
                        ui.painter(),
                        std::mem::take(&mut ctx.basemap_labels),
                        ctx.galley_cache,
                    );
                }
                id if *id == known::RADAR_COVERAGE => {
                    if let Some(tex) = ctx.pane.overlay_cache(id).and_then(|c| c.current()) {
                        let screen_rect = ui.max_rect();
                        draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                    }
                }
                id if *id == known::RADAR_SITES => {
                    // Under the dots, so a marker is never hidden by the ring
                    // belonging to it.
                    draw_selected_site_ring(ui, projector, ctx.pane);
                    handle_radar_site_interactions(ui, zoom, &visible_sites, ctx);
                }
                id if *id == known::USER_LOCATION => {
                    if let Some((user_lat, user_lon)) = ctx.user_location {
                        render_user_location(
                            ui,
                            projector,
                            user_lat,
                            user_lon,
                            ctx.user_heading,
                            ctx.user_fix.as_ref(),
                        );
                    }
                }
                // Color scale legend (screen-space HUD) — painted through the
                // pane's own paint list, so `draw_order` genuinely places it.
                id if *id == known::COLOR_SCALE => {
                    let painter = ui.painter().with_clip_rect(ctx.pane_rect);
                    #[cfg(test)]
                    {
                        painted_layer = painter.layer_id();
                    }
                    render_color_scales(
                        &painter,
                        clear_of_bottom_chrome(ui.max_rect(), ctx.color_scale_floor),
                        ctx.horizontal_color_scale,
                        ctx.pane_idx,
                        ctx.pane,
                        ctx.overlays,
                        ctx.preferences,
                    );
                }
                _ => match handler.render_mode() {
                    RenderMode::Texture => {
                        // Shared, not mutable: the clickable set is only asked
                        // for if a click needs resolving.
                        let overlays = &*ctx.overlays;
                        // **The draw fork** (WI-6). An animating layer paints
                        // the frame under its own playhead — never the live
                        // cache, which still holds whatever instant was last
                        // rasterized and would leave that on the glass
                        // unlabelled. A frame with no picture yet paints
                        // nothing; see `overlay_texture_on_screen`.
                        selected.extend(overlay_ctx.draw_overlay(
                            ctx.pane.overlay_texture_on_screen(id),
                            overlays.map_labels(id),
                            || overlays.clickable_items(id, &ctx.pane.layer_ref(ctx.pane_idx, id)),
                        ));
                    }
                    RenderMode::PerFramePoint => {
                        selected.extend(render_per_frame_overlay(
                            ctx.galley_cache,
                            ui,
                            projector,
                            &PerFrameOverlayCtx {
                                overlays: ctx.overlays,
                                id,
                                zoom,
                                prefs: ctx.preferences,
                                overlay_click_pos: ctx.overlay_click_pos,
                                excluded_rects: &overlay_excluded_rects,
                                pane_rect: ctx.pane_rect,
                            },
                        ));
                    }
                    _ => {}
                },
            }
            #[cfg(test)]
            ctx.paint_order.push((id.clone(), painted_layer));
        }

        // The deferred stale-image notice, submitted after every kind so
        // nothing in `draw_order` can paint over it. Glass: a floor strip does
        // not draw it — `Gui::draw_volume_glass` does instead.
        if let Some((on_screen, elevation)) = &pending_notice
            && ctx.surfaces.paints(Surface::Glass)
        {
            let notice_painter = ui.painter().with_clip_rect(ctx.pane_rect);
            draw_pending_render_notice(
                &notice_painter,
                ctx.pane_rect,
                // The pill row's measured clearance, not the one-row
                // constant: a narrow pane wraps the row.
                crate::ui::pills::pill_row_clearance(ui.ctx(), ctx.pane_idx),
                on_screen,
                *elevation,
            );
        }

        // The other half of the same plate — mutually exclusive with the
        // notice above, so they cannot stack.
        if let Some(source) = melting_layer_caveat
            && ctx.surfaces.paints(Surface::Glass)
        {
            let notice_painter = ui.painter().with_clip_rect(ctx.pane_rect);
            draw_melting_layer_notice(
                &notice_painter,
                ctx.pane_rect,
                crate::ui::pills::pill_row_clearance(ui.ctx(), ctx.pane_idx),
                source,
            );
        }

        // The loading state (WI-7). While a loop's data is on its way the
        // pane paints NOTHING for that layer (WI-6), and this plate is what
        // makes the nothing legible: the quantity — which frame is owed, or
        // how long the frame listing has been out — never an apology. Same
        // slot as the two notices above and yielded to them, so the plates
        // cannot stack.
        if pending_notice.is_none()
            && melting_layer_caveat.is_none()
            && ctx.surfaces.paints(Surface::Glass)
            && let Some(loading) = ctx.pane.loop_loading(web_time::Instant::now())
        {
            let notice_painter = ui.painter().with_clip_rect(ctx.pane_rect);
            draw_top_notice(
                &notice_painter,
                ctx.pane_rect,
                crate::ui::pills::pill_row_clearance(ui.ctx(), ctx.pane_idx),
                loop_loading_notice(loading),
            );
            // The wait count ticks and the frames land without any input;
            // keep a slow heartbeat while the plate is up so both reach the
            // glass. It dies with the plate.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }

        if !selected.is_empty() {
            ctx.overlays.selected_overlays = selected;
            ctx.overlays.selected_overlay_page = 0;
            *ctx.click_consumed = true;
        }

        {
            let hover_pos = ui.ctx().pointer_hover_pos();
            ctx.pane.overlay_hover_value = None;
            if let Some(pos) = hover_pos
                && ctx.pane_rect.contains(pos)
                && !ui
                    .ctx()
                    .layer_id_at(pos)
                    .is_some_and(|l| l.order > egui::Order::Background)
            {
                let map_pos = projector.unproject(egui::vec2(pos.x, pos.y));
                let hover_lat = map_pos.y();
                let hover_lon = map_pos.x();
                for id in &draw_order {
                    if ctx.pane.is_overlay_enabled(id)
                        && let Some(text) = ctx.overlays.hover_value_at(
                            id,
                            hover_lat,
                            hover_lon,
                            &ctx.pane.layer_ref(ctx.pane_idx, id),
                        )
                    {
                        ctx.pane.overlay_hover_value = Some(text);
                        break;
                    }
                }
            }
        }

        let screen_rect = ui.max_rect();
        let viewport_bounds = viewport_geo_bounds(projector, screen_rect);
        let qzoom = current_quantized_zoom(zoom);
        // As much overdraw as the adapter's texture limit allows; egui only
        // `debug_assert!`s the bound, so exceeding it is a wgpu validation error.
        let max_texture_side = ui.ctx().input(|i| i.max_texture_side) as u32;
        // In physical pixels, not points: an overlay sized in points is one
        // texel per `ppp²` physical pixels.
        let tex_plan =
            plan_overlay_texture(screen_rect, max_texture_side, ui.ctx().pixels_per_point());
        // Whether a gesture is moving the zoom on this frame — the settle
        // test's whole input. Read once for the window, because egui's input
        // is the window's; see `ZoomDrive`.
        let zoom_drive = ui.input(crate::overlay_cache::ZoomDrive::of);

        // Whether any overlay on this pane still has settle frames to count
        // down — i.e. whether the pane must ask for another frame to run them.
        let mut settle_counting = false;

        // The live theme, read once per frame and mixed into every overlay's
        // cache token below — a theme flip re-rasterizes on the next frame.
        let is_dark = ui.ctx().global_style().visuals.dark_mode;

        let texture_ids: Vec<LayerId> = ctx
            .overlays
            .handlers()
            .filter(|h| h.render_mode() == RenderMode::Texture)
            .map(|h| h.id())
            .collect();
        for id in &texture_ids {
            // Radar rendering is driven by product/elevation changes (not viewport),
            // handled by dispatch_pane_renders() in the platform crate.
            if *id == known::RADAR {
                continue;
            }
            let enabled = ctx.pane.is_overlay_enabled(id);
            let token = overlay_cache_token(ctx.overlays, ctx.pane_idx, ctx.pane, id, is_dark);
            let has_data = ctx
                .overlays
                .has_data(id, &ctx.pane.layer_ref(ctx.pane_idx, id));
            let cache = ctx.pane.overlay_cache_mut(id);
            // Asked on every frame the overlay is live, and not gated on
            // `render_in_flight`: a skipped frame is missing from the settle clock.
            let stale = enabled
                && has_data
                && cache.needs_rerender(token, zoom, zoom_drive, &viewport_bounds, &tex_plan);
            let dispatched = stale
                && cache
                    .renders
                    .admits(RenderSlot::WHOLE, ctx.overlay_render_limit);
            if dispatched {
                ctx.actions.push(GuiAction::RenderOverlay {
                    pane_idx: ctx.pane_idx,
                    overlay_kind: id.clone(),
                    geo_bounds: viewport_bounds,
                    texture: tex_plan,
                    data_generation: token,
                    zoom: qzoom,
                });
            }
            // **Owed only while the ask could not go out**, which is a
            // narrower thing than "a raster is on its way".
            //
            // A dispatch that WAS admitted resolves itself without the strip
            // repainting for the whole flight: the arriving raster is a new
            // texture identity, and that identity is already a
            // `ground_content_key` input, so the frame it lands is the frame
            // the key moves and the strip repaints. Holding the latch open
            // across the flight instead repaints the strip on every frame of
            // it — a second whole map render plus the mirror pass, per pane,
            // per frame.
            //
            // Under a playing loop that used to be every frame, for ever.
            // Measured: a native E3 leg (KTLX volume loop at 10 fps under
            // orbit, 1920x1080, 75 s) read `964 paints, 964 incomplete`
            // against 964 frames, so WO-7's skip never fired once. A raster
            // was owed on every frame there because the pane clock sweeps its
            // whole window at the playback rate and every
            // `TimeAxis::EventLifetime` layer re-tokenizes per as-of bucket,
            // a 60 s quantum against a tick worth ~5 min.
            //
            // **The latch is no longer re-armed by that.** `needs_rerender`
            // now answers `false` — not "true but refused" — for a swept token
            // on a pane that has demonstrated it discards uploads, so `stale`
            // is false on those frames and this `|=` does not fire. See
            // `OverlayTextureCache::sweep_discarded`. What still latches is
            // what always should have: a raster the pane genuinely wants and
            // could not ask for.
            //
            // A dispatch that was REFUSED has no arrival to wait for and
            // nothing else would ever re-ask, so that one still latches --
            // this is the `request_once` retry, kept.
            resolution.overlay_work_owed |= stale && !dispatched;
            // `enabled && has_data` and not just `enabled`. A repaint asked for
            // on a frame that cannot dispatch anything is a wakeup nothing can
            // satisfy.
            if enabled && has_data && cache.settle_is_counting_down() {
                settle_counting = true;
            }
            if !enabled {
                cache.clear();
            }
        }

        // **Ask for the frames the countdown needs, and only those.** The
        // settle is counted in frames now, and the frames it counts are not
        // all free: egui asks for its own while a wheel notch's smoothing
        // still has something to apply, and stops the moment it drains —
        // which is before it calls the scroll action over. This bridges that
        // gap, and closes as soon as the countdown reaches zero and the
        // dispatch above has gone out.
        //
        // Immediate, and not a delay, because the countdown is frames: a
        // duration here would put the wall clock back into the settle by the
        // other door. It is bounded — `SETTLE_QUIET_FRAMES` frames past the
        // end of a gesture, plus whatever of egui's own 150 ms end-of-scroll
        // window is left after the smoothing drained — and it never runs while
        // the picture is already right, because `settle_is_counting_down` is
        // false once the countdown has expired.
        if settle_counting {
            resolution.overlay_work_owed = true;
            ui.ctx().request_repaint();
        }
    }

    // Long-press tooltip: show the radar value above the finger. Reached only
    // when the touch pipeline ran this frame (`InteractionState`).
    if let Some(touch_pos) = ctx.long_press_pos
        && ctx.pane_rect.contains(touch_pos)
    {
        let raw_meta = ctx
            .pane
            .overlay_cache(&known::RADAR)
            .and_then(|c| c.current())
            .and_then(|tex| tex.radar_meta.as_ref())
            .map(|m| (m.lat, m.lon, std::sync::Arc::clone(&m.hover)));
        if let Some((lat, lon, hover)) = raw_meta {
            draw_long_press_tooltip(
                ui,
                projector,
                &hover,
                lat,
                lon,
                touch_pos,
                ctx.pane,
                ctx.preferences,
            );
        } else if let Some(img) = ctx.pane.active_image().cloned() {
            draw_long_press_tooltip(
                ui,
                projector,
                &img.hover,
                img.lat,
                img.lon,
                touch_pos,
                ctx.pane,
                ctx.preferences,
            );
        }
    }

    resolution
}

/// The token a texture overlay's cached raster is keyed by: it moves exactly
/// when the picture would be different.
///
/// **Public because it has two callers and must never have two definitions.**
/// The draw loop calls it below to notice a stale raster; the arrival path
/// (WO-M13a) calls it to recompute a recorded dispatch's token *fresh*, and
/// the comparison it makes is only meaningful if both sides are this function.
pub fn overlay_cache_token(
    overlays: &OverlayRegistry,
    pane_idx: usize,
    pane: &PaneState,
    id: &LayerId,
    is_dark: bool,
) -> u64 {
    let base = if *id == known::RADAR_SITES {
        pane.radar_sites_render_gen
    } else {
        // Pane-aware since WO-M10b: two panes filtering the same layer
        // differently draw different pictures, and one token for both would be
        // one texture for both.
        overlays.content_signature(id, &pane.layer_ref(pane_idx, id))
    };
    let themed = is_dark && overlays.theme_sensitive(id);
    base ^ if themed { 0x9E37_79B9_7F4A_7C15 } else { 0 } ^ as_of_term(overlays, pane_idx, pane, id)
}

/// **The as-of half of the cache token, and it is `0` on a live pane.**
///
/// An [`TimeAxis::EventLifetime`] layer's picture is *which items are valid at
/// the depicted instant*, so a scrubbed pane must not be handed the texture
/// the live pane rasterized. It keys on the instant **quantized** by the
/// layer's own quantum rather than on the raw instant, so dragging the
/// scrubber re-uses rasters instead of minting one per frame.
///
/// Under [`TimeMode::Live`] this is `0` and the token is byte-for-byte what it
/// was before WO-E7c — which is what keeps a live pane's one-second lightning
/// quantum from re-rasterizing it every second. The `Live` fast path also
/// costs one enum test: the registry walk below only runs on a scrubbed pane.
///
/// It is mixed into `data_generation`, which is part of the key
/// `group_overlay_renders` shares one raster across panes on — so two panes on
/// two instants get two rasters without anything else having to know.
fn as_of_term(overlays: &OverlayRegistry, pane_idx: usize, pane: &PaneState, id: &LayerId) -> u64 {
    let TimeMode::AsOf(instant) = pane.time.mode else {
        return 0;
    };
    let Some(handler) = overlays.handlers().find(|h| h.id() == *id) else {
        return 0;
    };
    if !matches!(handler.time_axis(), TimeAxis::EventLifetime) {
        return 0;
    }
    // **The layer's own answer first, and the quantum only if it has none.**
    //
    // `as_of_quantum` is a proxy for "would the picture differ", and a loose
    // one: it moves on every bucket the depicted instant crosses whether or
    // not a single item began or ended in it. A pane clock at playback rate
    // crosses them continuously — a tick worth ~5 min against a 60 s quantum
    // — so the proxy minted a fresh whole-viewport raster per bucket for the
    // picture already on the glass. A layer that can name the items in force
    // at `instant` answers exactly, and a sweep across a stretch where
    // nothing begins or ends returns ONE value for the whole stretch.
    //
    // `None` is "I cannot say", and the fallback below is then byte-for-byte
    // what shipped — which is what keeps every layer that does not override
    // it unaffected.
    let term = match handler.as_of_signature(&pane.layer_ref(pane_idx, id), instant) {
        Some(signature) => signature,
        // Hashed rather than mixed raw: the bucket is a small integer and
        // adjacent buckets must not land on adjacent tokens beside a content
        // signature.
        None => crate::pane::as_of_bucket(instant, handler.as_of_quantum()) as u64,
    };
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&term, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

/// Render the radar image overlay, range ring, and hover tooltip (loop playback
/// path).
fn render_radar_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    img: &RadarImageData,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    prefs: &UserPreferences,
) {
    ui.painter().image(
        img.texture.id(),
        crate::overlay_cache::placed_rect(projector, &img.placed),
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    render_radar_range_ring(ui, projector, img.lat, img.lon, img.max_range_km);
    update_pane_hover_value_from_meta(
        ui,
        projector,
        &RadarHoverData {
            hover: &img.hover,
            lat: img.lat,
            lon: img.lon,
        },
        pane,
        pane_rect,
        prefs,
    );
}

/// Draw only the range ring for a radar site (used with overlay-cache rendering).
fn render_radar_range_ring(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    lat: f64,
    lon: f64,
    extent_km: f64,
) {
    let radar_center = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
    let north_edge = projector
        .project(walkers::lat_lon(lat + extent_km / KM_PER_DEGREE_LAT, lon))
        .to_pos2();
    let range_radius_pixels = (radar_center.y - north_edge.y).abs();
    ui.painter().circle_stroke(
        radar_center,
        range_radius_pixels,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(150, 150, 150, 80),
        ),
    );
}

/// The picture's gates and the site they were measured from — what a hover
/// query needs.
struct RadarHoverData<'a> {
    hover: &'a HoverSource,
    lat: f64,
    lon: f64,
}

/// Update hover value using radar metadata from the overlay cache.
fn update_pane_hover_value_from_meta(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    radar: &RadarHoverData<'_>,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    prefs: &UserPreferences,
) {
    let Some(hover_pos) = ui.ctx().pointer_hover_pos() else {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    };

    if !pane_rect.contains(hover_pos) {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    };

    if ui
        .ctx()
        .layer_id_at(hover_pos)
        .is_some_and(|l| l.order > egui::Order::Background)
    {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    }

    // Recomputed every frame the pointer is over the pane, stationary or not:
    // `render_pane_map_content` clears `hover_value` at its top.
    pane.last_hover_pos = Some(hover_pos);

    let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
    let map_pos = projector.unproject(screen_vec);

    pane.hover_value = Some(super::compute_hover_info_raw(
        radar.hover,
        &super::HoverInput {
            site_lat: radar.lat,
            site_lon: radar.lon,
            hover_lat: map_pos.y(),
            hover_lon: map_pos.x(),
        },
        &pane.selected_product(),
        prefs,
    ));
}

/// A hover readout pinned to the pointer, on a layer that cannot claim it.
fn map_hover_tooltip(
    ctx: &egui::Context,
    id: egui::Id,
    pos: egui::Pos2,
    width: Option<f32>,
    content: impl FnOnce(&mut egui::Ui),
) {
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .interactable(false)
        .constrain(true)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(width.unwrap_or_else(|| ui.spacing().tooltip_width));
                content(ui);
            });
        });
}

/// One radar site that landed near enough to this pane to matter, with the
/// projection already done.
struct VisibleSite {
    /// The row itself, not its position in the table: a table resolved at
    /// runtime can change length, so an index would name a different radar.
    site: &'static RadarSite,
    /// Screen position of the site marker's centre.
    screen: egui::Pos2,
    /// The clickable icon box around `screen`.
    icon_rect: egui::Rect,
}

/// Project the radar site table once, keeping the sites within a 100 px margin
/// of this pane. Empty when the layer is off.
fn visible_radar_sites(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    pane: &PaneState,
) -> Vec<VisibleSite> {
    if !pane.is_overlay_enabled(&known::RADAR_SITES) {
        return Vec::new();
    }
    // The margin is what lets a site just off the edge still draw its label and
    // take a click on the icon straddling the boundary.
    let near = ui.max_rect().expand(100.0);
    let icon_size = (10.0 + zoom as f32 * 2.0).clamp(8.0, 24.0);
    // **The turn this pane is looking at.** `Projector::project` is linear in
    // longitude and folds nothing, so a station written -165.30 seen from a map
    // centred at 170E lands 335 degrees west of centre instead of 25 degrees
    // east: off the canvas, culled, and missing from the map entirely. The
    // width of one turn in points is measured from the projector rather than
    // derived from the zoom, so it cannot disagree with it; two projections buy
    // it for the whole 208-row table.
    let world_width = crate::site_marker::world_width_in_points(projector);
    let centre_x = near.center().x;
    visible_sites_in(
        squallar_radar::sites::radars(),
        near,
        icon_size,
        |lat, lon| {
            let p = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
            egui::pos2(
                crate::site_marker::fold_into_turn(p.x, centre_x, world_width),
                p.y,
            )
        },
    )
}

/// Paint the selected station's 230 km coverage ring, and nothing else's.
///
/// **Its own pass over the table, not a filter of [`visible_radar_sites`].**
/// That walk keeps sites within 100 pt of the pane, which is the right margin
/// for a dot and its label and the wrong one for a ring: a station a whole
/// screen off the edge still covers ground the pane is looking at. The cull
/// here is against the ring.
///
/// Draws at most one ring — the map's answer to "nothing selected" is dots and
/// no rings, and the map's answer to "one station selected" is exactly one.
fn draw_selected_site_ring(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    pane: &PaneState,
) -> Option<crate::site_marker::RingPlacement> {
    if !pane.is_overlay_enabled(&known::RADAR_SITES) {
        return None;
    }
    let selected = pane.selected_site()?;
    let row = squallar_radar::sites::get_radar_site(selected)?;

    let rect = ui.max_rect();
    let placement = crate::site_marker::ring_placement(
        projector,
        row.lat,
        row.lon,
        rect.center().x,
        crate::site_marker::world_width_in_points(projector),
    )?;
    if !rect.expand(placement.radius).contains(placement.center) {
        return None;
    }

    let role = crate::site_marker::MarkerRole::for_station(
        selected,
        pane.site(),
        pane.loading_site.as_deref(),
    );
    crate::site_marker::draw_coverage_ring(ui.painter(), placement, role);
    Some(placement)
}

/// The order the station names compete for screen in.
///
/// Split out so the rule is one readable function rather than a sort key buried
/// in the draw loop: the selected station's name is never the one dropped, the
/// pane's own station comes next, and a WSR-88D outranks a terminal radar
/// sitting inside its coverage — `TDTW` is what makes Detroit's `KDTX` illegible.
/// Everything after that is the site table's own fixed order, which does not
/// move when the map does.
fn site_label_ranks(sites: &[VisibleSite], pane: &PaneState) -> Vec<crate::site_marker::LabelRank> {
    use crate::site_marker::LabelRank;
    let selected = pane.selected_site();
    let current = pane.site();
    sites
        .iter()
        .map(|s| {
            if selected == Some(s.site.name) {
                LabelRank::Selected
            } else if current == s.site.name {
                LabelRank::Current
            } else if s.site.network == squallar_radar::sites::RadarNetwork::Wsr88d {
                LabelRank::Primary
            } else {
                LabelRank::Secondary
            }
        })
        .collect()
}

/// The walk itself, over whichever table it is handed. The table is an argument
/// rather than a global read so a test can hand it two tables of different
/// lengths.
fn visible_sites_in(
    rows: &'static [RadarSite],
    near: egui::Rect,
    icon_size: f32,
    project: impl Fn(f64, f64) -> egui::Pos2,
) -> Vec<VisibleSite> {
    let mut visible = Vec::new();
    for site in rows {
        let screen = project(site.lat, site.lon);
        if !near.contains(screen) {
            continue;
        }
        visible.push(VisibleSite {
            site,
            screen,
            icon_rect: egui::Rect::from_center_size(screen, egui::vec2(icon_size, icon_size)),
        });
    }
    visible
}

/// Per-frame radar site marker and label rendering, and interaction detection.
///
/// **The marker is drawn here, in points, and not baked into the layer's
/// raster.** A raster is placed by its geographic corners, so everything in it
/// scales with the map — right for a coastline, wrong for a station dot, whose
/// own click target (`VisibleSite::icon_rect`) is sized in points from the live
/// zoom. Baked, the dot left its hit box behind the moment a gesture started,
/// ran up to four times its size two zoom levels in, and snapped back half a
/// second after the zoom went still.
fn handle_radar_site_interactions(
    ui: &egui::Ui,
    zoom: f64,
    sites: &[VisibleSite],
    ctx: &mut PaneRenderCtx<'_>,
) {
    // Destructuring borrows the fields disjointly, so `pane` and `actions` stay
    // mutable while `excluded_rects` is read.
    let PaneRenderCtx {
        pane,
        actions,
        pane_idx,
        preferences: prefs,
        overlay_click_pos,
        pane_rect,
        excluded_rects,
        click_consumed,
        ..
    } = ctx;
    let pane_idx = *pane_idx;
    let pane_rect = *pane_rect;

    let zoom_f32 = zoom as f32;
    let icon_size = (10.0 + zoom_f32 * 2.0).clamp(8.0, 24.0);
    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

    let hover_pos = ui.ctx().pointer_hover_pos();
    let click_pos = *overlay_click_pos;

    let is_dark = ui.ctx().global_style().visuals.dark_mode;
    let text_color = if is_dark {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    };

    // Read once, before the click arm below borrows `pane` mutably. Which
    // station is current and which is loading is what the three marker fills
    // say, and it is the only thing the marker needs from the pane.
    let current_site = pane.site().to_string();
    let loading_site = pane.loading_site.clone();

    // **Every marker is drawn, at every zoom, for every station.** The dots are
    // what makes the network discoverable, and only the ring is gated by the
    // selection. This loop runs first and separately from the labels because a
    // dot must not be able to lose a contest to a name.
    for site in sites {
        let role = crate::site_marker::MarkerRole::for_station(
            site.site.name,
            &current_site,
            loading_site.as_deref(),
        );
        crate::site_marker::draw_site_marker(ui.painter(), site.screen, zoom, role);
    }

    // **One set of claimed areas for the whole pane**, exactly as the city
    // labels run — see `ui_map_overlays::paint_labels`. Asking in
    // `label_order`'s order is what makes the result stable: the first name to
    // ask finds nothing claimed and therefore always draws, so a viewport with
    // any station in it can never come back with every name suppressed.
    if zoom >= 5.0 {
        let mut occupied = walkers::OccupiedAreas::new();
        let ranks = site_label_ranks(sites, pane);
        for idx in crate::site_marker::label_order(&ranks) {
            let site = &sites[idx];
            let text_pos = egui::pos2(site.screen.x, site.screen.y + icon_size / 2.0 + 3.0);
            crate::site_marker::try_draw_site_label(
                ui.painter(),
                &mut occupied,
                text_pos,
                site.site.name,
                egui::FontId::monospace(font_size),
                text_color,
                is_dark,
            );
        }
    }

    for site in sites {
        let radar_site = site.site;
        let icon_rect = site.icon_rect;

        if let Some(pos) = click_pos
            && icon_rect.contains(pos)
            && !is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects)
        {
            // **The tap moves the ring, and asks for the radar exactly as it
            // always did.** One gesture, two effects, and the switch half is
            // unchanged: `App::handle_radar_action` already does nothing to a
            // pane whose site is not moving (`if pane.site() != site`), so
            // tapping the station you are already on to put its ring away costs
            // no fetch.
            pane.toggle_ring_selection(radar_site.name);
            // **Only where the site really moves.** `loading_site` is what
            // paints a marker purple until the volume lands, and a pane that is
            // already on this station will never be handed one — so setting it
            // here would leave the dot purple for as long as the pane lived.
            if current_site != radar_site.name {
                pane.loading_site = Some(radar_site.name.to_string());
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            }
            actions.push(GuiAction::SwitchRadarSite {
                site: radar_site.name.to_string(),
                pane_idx,
            });
            **click_consumed = true;
        }

        if let Some(pos) = hover_pos
            && icon_rect.contains(pos)
            && !is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects)
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            // The feedhorn, not the ground: it is the figure a published
            // station record quotes as the radar's elevation.
            let elev_str = match radar_site.height_ft(squallar_radar::sites::Datum::Feedhorn) {
                Some(e) => {
                    let converted = prefs.height.convert_from_feet(e as f32);
                    format!("{:.0} {}", converted, prefs.height.suffix())
                }
                None => "N/A".to_string(),
            };
            let tooltip_text = format!(
                "{}\nLat: {:.3}°, Lon: {:.3}°\nElev: {}",
                radar_site.name, radar_site.lat, radar_site.lon, elev_str
            );
            map_hover_tooltip(
                ui.ctx(),
                egui::Id::new(("site_tooltip", radar_site.name)),
                pos,
                None,
                |tooltip_ui| {
                    tooltip_ui.label(tooltip_text);
                },
            );
        }
    }
}

/// Draw user location blue dot indicator with optional heading wedge and hover popup.
fn render_user_location(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    user_lat: f64,
    user_lon: f64,
    heading: Option<f32>,
    fix: Option<&squallar_location::Fix>,
) {
    let user_screen = projector
        .project(walkers::lat_lon(user_lat, user_lon))
        .to_pos2();

    let screen_rect = ui.max_rect();
    if !screen_rect.expand(50.0).contains(user_screen) {
        return;
    }

    let blue = egui::Color32::from_rgb(30, 130, 255);

    if let Some(heading_deg) = heading {
        let wedge_radius = 28.0;
        let half_angle = 22.5_f32.to_radians(); // 45° total wedge
        let center_rad = (heading_deg - 90.0).to_radians(); // egui: 0° = right

        let num_segments = 16;
        let mut points = Vec::with_capacity(num_segments + 2);
        points.push(user_screen);
        for i in 0..=num_segments {
            let t = i as f32 / num_segments as f32;
            let angle = center_rad - half_angle + t * 2.0 * half_angle;
            points.push(egui::pos2(
                user_screen.x + wedge_radius * angle.cos(),
                user_screen.y + wedge_radius * angle.sin(),
            ));
        }

        let wedge_color = egui::Color32::from_rgba_unmultiplied(30, 130, 255, 140);
        let wedge_stroke = egui::Color32::from_rgba_unmultiplied(30, 130, 255, 200);
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            wedge_color,
            egui::Stroke::new(1.0, wedge_stroke),
        ));
    }

    ui.painter().circle_filled(
        user_screen,
        14.0,
        egui::Color32::from_rgba_unmultiplied(30, 130, 255, 40),
    );
    ui.painter().circle_stroke(
        user_screen,
        7.0,
        egui::Stroke::new(2.5, egui::Color32::WHITE),
    );
    ui.painter().circle_filled(user_screen, 7.0, blue);

    if let Some(fix) = fix {
        let dot_rect = egui::Rect::from_center_size(user_screen, egui::vec2(28.0, 28.0));
        if let Some(hover_pos) = ui.ctx().pointer_hover_pos()
            && dot_rect.contains(hover_pos)
        {
            map_hover_tooltip(
                ui.ctx(),
                egui::Id::new("gps_fix_tooltip"),
                hover_pos,
                None,
                |tooltip_ui| {
                    tooltip_ui.label(format!(
                        "Lat: {:.5}°  Lon: {:.5}°",
                        fix.point.lat, fix.point.lon
                    ));
                    if let Some(alt) = fix.altitude_m {
                        tooltip_ui.label(format!("Alt: {:.0} m", alt));
                    }
                    if let Some(speed) = fix.speed_mps {
                        let speed_kts = speed * 1.94384;
                        tooltip_ui.label(format!("Speed: {:.1} m/s ({:.1} kts)", speed, speed_kts));
                    }
                    if let Some(hdg) = fix.heading_deg {
                        tooltip_ui.label(format!("Course: {:.0}°", hdg));
                    }
                    if let Some(sats) = fix.satellites {
                        tooltip_ui.label(format!("Sats: {}", sats));
                    }
                    tooltip_ui.label(format!("Fix: {}", fix.fix_quality.label()));
                    if let Some(hdop) = fix.hdop {
                        tooltip_ui.label(format!("HDOP: {:.1}", hdop));
                    }
                },
            );
        }
    }
}

// ── Color scale legend ────────────────────────────────────────────────────

/// Bar width in logical pixels.
pub(super) const SCALE_BAR_WIDTH: f32 = 20.0;
/// Margin from pane edge in logical pixels.
const SCALE_MARGIN: f32 = 16.0;
/// Extra margin reserved for the unit title above/beside the bar.
const SCALE_TITLE_MARGIN: f32 = 16.0;
/// Font size for value labels.
const SCALE_FONT_SIZE: f32 = 11.0;
/// Font size for the unit title label.
pub(super) const SCALE_TITLE_FONT_SIZE: f32 = 12.0;
/// Outline offset for text shadow.
const SHADOW_OFFSET: f32 = 1.0;
/// Minimum pixel spacing between labels before thinning kicks in.
const MIN_LABEL_SPACING: f32 = 14.0;
/// Gap between two stacked colour-scale bars, logical pixels: the room the
/// inner one's value labels are read in.
const SCALE_STACK_GAP: f32 = 40.0;
/// How thick a fold marker is across the bar's long axis, logical pixels.
const FOLD_TICK_THICKNESS: f32 = 2.0;
/// How far a fold marker sticks out past each face of the bar, logical pixels.
const FOLD_TICK_OVERHANG: f32 = 3.0;
/// Side of the range-folded key swatch, logical pixels — small enough to stand
/// in the [`SCALE_MARGIN`] past the end of the bar, and nowhere near the
/// 20-point bar width the strip classifier looks for.
const RF_SWATCH_SIZE: f32 = 10.0;
/// What the range-folded swatch is labelled, matching the two-letter form the
/// hydrometeor classification bar already uses for its own folded class.
const RF_SWATCH_LABEL: &str = "RF";
/// Baseline-to-baseline distance for the fold annotation stacked under the unit
/// title, logical pixels — [`SCALE_FONT_SIZE`] plus the shadow's own offset.
const FOLD_TITLE_LINE: f32 = SCALE_FONT_SIZE + SHADOW_OFFSET + 1.0;
/// The gap between a **vertical** bar's inner face and the value labels read
/// against it, logical pixels. Drawn `RIGHT_CENTER` at this offset.
const SCALE_LABEL_GAP: f32 = 4.0;
/// The gap between a **horizontal** bar's top edge and the value labels read
/// against it, logical pixels. Drawn `CENTER_BOTTOM` at this offset.
const SCALE_LABEL_LIFT: f32 = 2.0;
/// How far in from the pane's own edge the fold annotation is hung, logical
/// pixels.
const FOLD_TITLE_INSET: f32 = 2.0;

/// How far in from the pane edge it stands on the colour-scale block reaches,
/// logical pixels — `0.0` when this pane draws no legend at all.
pub(super) fn color_scale_gutter(
    measure: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
    prefs: &UserPreferences,
) -> f32 {
    // The same gate `Gui::draw_volume_glass` and the `ColorScale` arm put in
    // front of `render_color_scales`: layer off, nothing painted, no gutter.
    if !pane.is_overlay_enabled(&known::COLOR_SCALE) {
        return 0.0;
    }
    let product = pane.selected_product();
    let legend = crate::field_facts::facts(&product).scale;
    if legend.thresholds.len() < 2 {
        return 0.0;
    }
    // And the "pane too small" bail both painters take, restated from the same
    // expressions so a pane that draws no bar reserves no room for one.
    let bar_length = if horizontal {
        pane_rect.width() - SCALE_MARGIN * 2.0
    } else {
        pane_rect.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN
    };
    if bar_length < 40.0 {
        return 0.0;
    }

    // The radar bar stands on the margin; each stacked overlay bar stands one
    // bar-and-gap further in. Every one is measured, not the innermost alone.
    let view = pane.view(pane_idx);
    let ticks = memoized_ticks(measure.ctx(), pane, prefs);
    let mut reach = legend_block_reach(
        measure,
        horizontal,
        0.0,
        &ticks,
        crate::field_facts::unit_label(&product, prefs),
    );
    let mut offset = 0.0;
    for id in pane.draw_order() {
        if *id == known::COLOR_SCALE || !pane.is_overlay_enabled(id) {
            continue;
        }
        let Some(overlay) = overlays.legend(id, &view.layer(id)) else {
            continue;
        };
        if overlay.items.thresholds.len() < 2 {
            continue;
        }
        offset += SCALE_BAR_WIDTH + SCALE_STACK_GAP;
        let ticks = memoized_overlay_ticks(measure.ctx(), id, &overlay);
        reach = reach.max(legend_block_reach(
            measure,
            horizontal,
            offset,
            &ticks,
            overlay.items.unit_label,
        ));
    }
    let mut gutter = SCALE_MARGIN + reach;

    // The legend's second line is hung off the pane's own edge rather than off
    // a bar, so it is a floor under the whole gutter. Read through
    // `legend_second_line`, the same function the painter draws from.
    if !horizontal && let Some(line) = legend_second_line(pane, prefs) {
        gutter = gutter.max(FOLD_TITLE_INSET + laid_out_width(measure, &line, SCALE_FONT_SIZE));
    }
    gutter
}

/// How wide `text` lays out at `size`, logical pixels.
fn laid_out_width(measure: &egui::Painter, text: &str, size: f32) -> f32 {
    measure
        .layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(size),
            egui::Color32::WHITE,
        )
        .rect
        .width()
}

/// How far in from the pane edge one bar's block reaches: the bar itself, the
/// value labels read against it, and the unit title centred on it.
fn legend_block_reach(
    measure: &egui::Painter,
    horizontal: bool,
    offset: f32,
    ticks: &[String],
    title: &str,
) -> f32 {
    let past_the_bar = if horizontal {
        let row = measure
            .layout_no_wrap(
                "0".to_owned(),
                egui::FontId::proportional(SCALE_FONT_SIZE),
                egui::Color32::WHITE,
            )
            .rect
            .height();
        SCALE_LABEL_LIFT + row
    } else {
        // Every threshold, not the drawn subset: `MIN_LABEL_SPACING` thinning
        // drops labels a short bar has no room for, and re-deriving which
        // survived would be a second copy of the painter's arithmetic.
        let widest = ticks
            .iter()
            .map(|tick| laid_out_width(measure, tick, SCALE_FONT_SIZE))
            .fold(0.0_f32, f32::max);
        let title = laid_out_width(measure, title, SCALE_TITLE_FONT_SIZE);
        (SCALE_LABEL_GAP + widest).max((title - SCALE_BAR_WIDTH) / 2.0)
    };
    offset + SCALE_BAR_WIDTH + past_the_bar
}

/// The part of `pane_rect` the colour scale has *not* claimed: where a pane's
/// floating chrome may sit without printing through a legend. See
/// [`color_scale_gutter`].
pub(super) fn color_scale_free_rect(
    measure: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
    prefs: &UserPreferences,
) -> egui::Rect {
    let gutter = color_scale_gutter(
        measure, pane_rect, horizontal, pane_idx, pane, overlays, prefs,
    );
    let mut free = pane_rect;
    if horizontal {
        // Bars along the bottom; the titles sit beside them, on the same edge.
        free.max.y -= gutter;
    } else {
        free.max.x -= gutter;
    }
    // A pane too small for both keeps its own rect rather than an inverted one.
    if free.width() < 1.0 || free.height() < 1.0 {
        return pane_rect;
    }
    free
}

/// The strip **under** a horizontal colour scale: between the bar's bottom
/// edge and the pane's own, which is the [`SCALE_MARGIN`] the bar is inset by.
///
/// The counterpart to [`color_scale_free_rect`], which answers for the space
/// *above* the bar. A portrait pane's basemap credit is placed here, so the
/// margin's arithmetic stays in the module that paints the bar rather than
/// being spelled a second time at the call site — the same reason the credit
/// asks [`color_scale_free_rect`] instead of re-deriving the gutter.
///
/// `None` when the scale is vertical — there is no strip under a bar that runs
/// down the right edge — or when the pane is too short for the margin to be a
/// strip at all.
pub(super) fn color_scale_under_rect(
    pane_rect: egui::Rect,
    horizontal: bool,
) -> Option<egui::Rect> {
    if !horizontal || pane_rect.height() <= SCALE_MARGIN {
        return None;
    }
    Some(egui::Rect::from_min_max(
        egui::pos2(pane_rect.left(), pane_rect.bottom() - SCALE_MARGIN),
        pane_rect.max,
    ))
}

/// The generic tick form: whole numbers bare, one decimal otherwise. Short is
/// the point — a tick label sits in the margin beside a 20px bar.
fn short_tick(value: f32) -> String {
    if value.fract().abs() < 0.01 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// Every value label `render_color_scale` writes beside `product`'s bar, in
/// order, before `MIN_LABEL_SPACING` thinning. The **formatting**, not the
/// per-frame answer: [`memoized_ticks`] calls this on a miss.
pub(super) fn legend_ticks(product: &FieldId, prefs: &UserPreferences) -> Vec<String> {
    crate::field_facts::facts(product)
        .scale
        .thresholds
        .iter()
        .map(|&(value, _)| format_legend_value(product, value, prefs))
        .collect()
}

/// [`legend_ticks`], formatted at most once per preferences change. The version
/// key is the preferences themselves rather than a hash: a collision would show
/// as a bar labelled in the wrong unit.
fn memoized_ticks(
    ctx: &egui::Context,
    pane: &PaneState,
    prefs: &UserPreferences,
) -> std::sync::Arc<Vec<String>> {
    let product = pane.selected_product();
    legend_ramp::labels(
        ctx,
        // The memo key is still (field, prefs); the field half is a `FieldId`
        // rather than the enum since WO-E9e, and `FieldId` hashes by its bytes.
        egui::Id::new(("squallar::legend_ticks::radar", product.as_str())),
        prefs.clone(),
        || legend_ticks(&product, prefs),
    )
}

/// An overlay bar's value labels, formatted at most once per legend signature.
fn memoized_overlay_ticks(
    ctx: &egui::Context,
    id: &LayerId,
    legend: &Signed<OverlayLegend>,
) -> std::sync::Arc<Vec<String>> {
    legend_ramp::labels(
        ctx,
        egui::Id::new(("squallar::legend_ticks::overlay", id.as_str())),
        legend.signature,
        || {
            legend
                .items
                .thresholds
                .iter()
                .map(|&(value, _)| format!("{value:.0}"))
                .collect()
        },
    )
}

/// The baked ramp for `pane`'s radar colour bar. Sampled through
/// [`get_color_for_value`] at the legend's own values, so the bar is the
/// palette's answer; the palette is a compile-time table, so the ramp is
/// [`legend_ramp::IMMUTABLE`].
fn radar_ramp(ctx: &egui::Context, pane: &PaneState, horizontal: bool) -> egui::TextureHandle {
    let product = pane.selected_product();
    let scale = crate::field_facts::facts(&product).scale;
    let min = scale.min_value;
    let range = scale.max_value - min;
    // The palette is keyed by the radar layer's own field type, so the id is
    // resolved once here rather than per sample. A field this build does not
    // register has no palette to bake, and the ramp falls back to the default
    // field's — the same fallback `field_facts::facts` takes, for the same
    // reason.
    let ramp_product = radar_fields::product_for(&product)
        .or_else(|| radar_fields::product_for(&radar_fields::known::REFLECTIVITY))
        .expect("the default field is registered by the radar crate");
    legend_ramp::ramp(
        ctx,
        egui::Id::new(("squallar::legend_ramp::radar", product.as_str(), horizontal)),
        legend_ramp::IMMUTABLE,
        "legend_ramp_radar",
        horizontal,
        |t| {
            let (r, g, b, a) = get_color_for_value(ramp_product, min + t * range);
            [r, g, b, a]
        },
    )
}

/// Format a legend label value. For HHC uses category names; for others, a short numeric string.
///
/// **The conversion is the registry's, the precision is the bar's.** The value
/// is converted by the field's own [`Quantity`](squallar_units::Quantity) —
/// which is where WO-E9a put the unit each field's numbers live in — while how
/// many decimals survive is a property of a 20 px colour bar, not of the field,
/// so it stays here. The arms below therefore compare field *identity*, which
/// after WO-E9e is a `FieldId` rather than a source's enum; `FieldId` is an
/// open string, so these are comparisons and not `match` patterns.
fn format_legend_value(product: &FieldId, value: f32, prefs: &UserPreferences) -> String {
    use radar_fields::known;

    // The one discrete domain: the RPG's own displayed codes from `hc.lgd`.
    if *product == known::HYDROMETEOR_CLASSIFICATION {
        return match value as u16 {
            10 => "Bio".into(),
            20 => "AP".into(),
            30 => "IC".into(),
            40 => "DS".into(),
            50 => "WS".into(),
            60 => "RA".into(),
            70 => "HR".into(),
            80 => "BD".into(),
            90 => "GR".into(),
            100 => "HA".into(),
            110 => "LH".into(),
            120 => "GH".into(),
            // `hc.lgd`'s own displayed code for melting snow.
            130 => "MS".into(),
            140 => "UK".into(),
            150 => "RF".into(),
            _ => format!("{value:.0}"),
        };
    }

    let converted = crate::field_facts::facts(product)
        .quantity
        .convert(value, prefs);

    // Speeds and echo tops: whole numbers, in the reader's own unit. Both
    // echo-tops fields are titled off `HeightUnit::kilo_suffix` and read out
    // through `convert_kft_to_kilo`, which is what `Quantity::HeightKft` does.
    if *product == known::VELOCITY
        || *product == known::STORM_RELATIVE_VELOCITY
        || *product == known::SPECTRUM_WIDTH
        || *product == known::ECHO_TOPS
        || *product == known::ECHO_TOPS_INTERPOLATED
    {
        return format!("{converted:.0}");
    }

    if *product == known::PRECIPITATION_RATE {
        return if converted < 1.0 {
            format!("{converted:.2}")
        } else {
            format!("{converted:.1}")
        };
    }

    // The ramp's stops are the NWS quarter-inch reporting steps; the ticks
    // are whatever unit the reader thinks in. Inches keep the generic short
    // form; cm and mm take the unit's own precision, which keeps `25.40`
    // off a 20px bar.
    if *product == known::MAX_EXPECTED_HAIL_SIZE {
        return match prefs.hail_size {
            HailSizeUnit::Inches => short_tick(converted),
            unit => {
                let decimals = unit.decimals();
                format!("{converted:.decimals$}")
            }
        };
    }

    // The remaining arms print the raw value: every one of them is a
    // `Quantity::Unitless` field, whose `convert` is the identity, so
    // `converted` and `value` are the same number here.
    if *product == known::CORRELATION_COEFFICIENT {
        return format!("{value:.2}");
    }
    if *product == known::DIFFERENTIAL_REFLECTIVITY
        || *product == known::SPECIFIC_DIFFERENTIAL_PHASE
    {
        return format!("{value:.1}");
    }
    short_tick(value)
}

// ── Pending-render notice ─────────────────────────────────────────────────

/// Font size of the pending-render notice. The color scale's title size, so the
/// notice reads as part of the same chrome rather than as an alert.
const PENDING_FONT_SIZE: f32 = 12.0;
/// Padding inside the notice's backing plate.
const PENDING_PADDING: egui::Vec2 = egui::vec2(8.0, 3.0);

/// What a pane says while the image on screen is not yet the product and tilt it
/// has selected — the one piece of information nothing else on the pane carries.
fn pending_render_notice(product: &FieldId, elevation: f32) -> String {
    format!(
        "\u{27f3} showing {} {:.1}\u{b0}",
        crate::field_facts::name(product),
        elevation
    )
}

/// Draw the notice across the top of the pane, over the imagery. Non-blocking:
/// the stale image stays fully visible and undimmed. Wrapped rather than
/// clipped — the longest product name is wider than a pane in a six-way split.
pub(super) fn draw_pending_render_notice(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    top_margin: f32,
    product: &FieldId,
    elevation: f32,
) {
    draw_top_notice(
        painter,
        pane_rect,
        top_margin,
        pending_render_notice(product, elevation),
    );
}

/// What a pane says when the classification on screen is standing on a melting
/// layer nobody measured for the volume it is drawn from — only for the two
/// unmeasured sources, see [`MeltingLayerSource::is_measured`]. Same plate and
/// colour as the pending-render notice, and no icon: every calm enough glyph
/// (`ⓘ`, `ℹ`) is missing from egui's proportional family.
fn melting_layer_notice(source: MeltingLayerSource) -> String {
    source.caption().to_owned()
}

/// Draw the melting-layer qualification across the top of the pane. Shares the
/// pending notice's position and cannot collide with it:
/// [`PaneState::displayed_melting_layer_source`] is gated through
/// `stale_image_on_screen`.
pub(super) fn draw_melting_layer_notice(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    top_margin: f32,
    source: MeltingLayerSource,
) {
    draw_top_notice(painter, pane_rect, top_margin, melting_layer_notice(source));
}

/// What a pane says while a loop's data is on its way (WI-7) — the quantity,
/// never an apology: which frame of how many is owed a picture, or how long
/// the frame listing has been out. No icon, like the melting-layer notice.
fn loop_loading_notice(loading: LoopLoading) -> String {
    match loading {
        LoopLoading::Listing { waited } => {
            format!("loading frames - {}s", waited.as_secs())
        }
        LoopLoading::Frame { index, total } => {
            format!("frame {} of {} loading", index + 1, total)
        }
    }
}

/// The rounded plate every top-of-pane notice is drawn on. Non-blocking: the
/// imagery stays fully visible and undimmed. Wrapped rather than clipped.
fn draw_top_notice(painter: &egui::Painter, pane_rect: egui::Rect, top_margin: f32, text: String) {
    let font = egui::FontId::proportional(PENDING_FONT_SIZE);
    let wrap_width = (pane_rect.width() - SCALE_MARGIN * 2.0 - PENDING_PADDING.x * 2.0).max(1.0);
    let galley = painter.layout(text, font, egui::Color32::WHITE, wrap_width);
    let plate = egui::Rect::from_center_size(
        egui::pos2(
            pane_rect.center().x,
            pane_rect.top() + top_margin + galley.size().y / 2.0 + PENDING_PADDING.y,
        ),
        galley.size() + PENDING_PADDING * 2.0,
    );
    painter.rect_filled(plate, 4.0, egui::Color32::from_black_alpha(200));
    painter.galley(plate.min + PENDING_PADDING, galley, egui::Color32::WHITE);
}

/// Draw text with a dark shadow for readability on the map.
fn draw_shadowed_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    anchor: egui::Align2,
    text: &str,
    font: egui::FontId,
) {
    painter.text(
        pos + egui::vec2(SHADOW_OFFSET, SHADOW_OFFSET),
        anchor,
        text,
        font.clone(),
        egui::Color32::from_black_alpha(200),
    );
    painter.text(pos, anchor, text, font, egui::Color32::WHITE);
}

/// Every colour-scale legend a pane shows: the radar product's own bar, and
/// one more for each enabled overlay that carries a legend of its own.
pub(super) fn render_color_scales(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
    prefs: &UserPreferences,
) {
    render_color_scale(painter, pane_rect, horizontal, pane, prefs);
    render_overlay_color_scales(painter, pane_rect, horizontal, pane_idx, pane, overlays);
}

/// The part of `pane_rect` the colour-scale legend may draw in: the pane, less
/// whatever the phone shell's bottom bar covers.
pub(super) fn clear_of_bottom_chrome(pane_rect: egui::Rect, floor: f32) -> egui::Rect {
    if !floor.is_finite() || floor >= pane_rect.bottom() {
        return pane_rect;
    }
    egui::Rect::from_min_max(
        pane_rect.min,
        egui::pos2(pane_rect.right(), floor.max(pane_rect.top())),
    )
}

pub(super) fn render_color_scale(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    let product = pane.selected_product();
    let legend = crate::field_facts::facts(&product).scale;
    if legend.thresholds.len() < 2 {
        return;
    }

    // Orientation follows the map panel's shape, not the platform: a portrait
    // panel gets horizontal bars along the bottom, a landscape one vertical
    // bars on the right. See `pane::ColorScaleOrientation`.
    let bar_length = if horizontal {
        pane_rect.width() - SCALE_MARGIN * 2.0
    } else {
        pane_rect.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN
    };

    if bar_length < 40.0 {
        return; // pane too small
    }

    let bar_rect = if horizontal {
        let left = pane_rect.left() + SCALE_MARGIN;
        let bottom = pane_rect.bottom() - SCALE_MARGIN;
        let top = bottom - SCALE_BAR_WIDTH;
        egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(left + bar_length, bottom))
    } else {
        // Vertical bar along the right, origin at bottom-right
        let right = pane_rect.right() - SCALE_MARGIN;
        let left = right - SCALE_BAR_WIDTH;
        let bottom = pane_rect.bottom() - SCALE_MARGIN;
        let top = bottom - bar_length;
        egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
    };

    let min_val = legend.min_value;
    let max_val = legend.max_value;
    let range = max_val - min_val;
    if range.abs() < f32::EPSILON {
        return;
    }

    let n = legend.thresholds.len();

    if legend.is_gradient {
        // Gradient scales: one image over a ramp baked once per product.
        // See `crate::legend_ramp`.
        painter.image(
            radar_ramp(painter.ctx(), pane, horizontal).id(),
            bar_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        // Discrete scales: equal-sized blocks, one per threshold. Left as
        // blocks on purpose: these are hard edges at exact fractions of the
        // bar, and a stretched `NEAREST` texture would move each boundary.
        for i in 0..n {
            let (_, rgb) = legend.thresholds[i];
            let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);

            let t0 = i as f32 / n as f32;
            let t1 = (i + 1) as f32 / n as f32;

            if horizontal {
                let x0 = bar_rect.left() + t0 * bar_rect.width();
                let x1 = bar_rect.left() + t1 * bar_rect.width();
                let strip = egui::Rect::from_min_max(
                    egui::pos2(x0, bar_rect.top()),
                    egui::pos2(x1, bar_rect.bottom()),
                );
                painter.rect_filled(strip, 0.0, color);
            } else {
                let y0 = bar_rect.bottom() - t0 * bar_rect.height();
                let y1 = bar_rect.bottom() - t1 * bar_rect.height();
                let strip = egui::Rect::from_min_max(
                    egui::pos2(bar_rect.left(), y1),
                    egui::pos2(bar_rect.right(), y0),
                );
                painter.rect_filled(strip, 0.0, color);
            }
        }
    }

    // --- Fold markers: where the picture on the glass wraps ---
    let folds_at = pane
        .displayed_nyquist_ms()
        .filter(|ms| ms.is_finite() && *ms > 0.0);
    if let Some(nyquist_ms) = folds_at {
        for value in fold_marker_positions(nyquist_ms as f32, min_val, max_val)
            .into_iter()
            .flatten()
        {
            let t = (value - min_val) / range;
            let marker = if horizontal {
                egui::Rect::from_min_size(
                    egui::pos2(
                        bar_rect.left() + t * bar_rect.width() - FOLD_TICK_THICKNESS / 2.0,
                        bar_rect.top() - FOLD_TICK_OVERHANG,
                    ),
                    egui::vec2(
                        FOLD_TICK_THICKNESS,
                        SCALE_BAR_WIDTH + FOLD_TICK_OVERHANG * 2.0,
                    ),
                )
            } else {
                egui::Rect::from_min_size(
                    egui::pos2(
                        bar_rect.left() - FOLD_TICK_OVERHANG,
                        bar_rect.bottom() - t * bar_rect.height() - FOLD_TICK_THICKNESS / 2.0,
                    ),
                    egui::vec2(
                        SCALE_BAR_WIDTH + FOLD_TICK_OVERHANG * 2.0,
                        FOLD_TICK_THICKNESS,
                    ),
                )
            };
            // The same dark backing `draw_shadowed_text` gives every label on
            // this bar: a bare white line reads as a highlight over mid green.
            painter.rect_filled(
                marker.expand(1.0),
                0.0,
                egui::Color32::from_black_alpha(200),
            );
            painter.rect_filled(marker, 0.0, egui::Color32::WHITE);
        }
    }

    let label_font = egui::FontId::proportional(SCALE_FONT_SIZE);
    let title_font = egui::FontId::proportional(SCALE_TITLE_FONT_SIZE);

    let mut label_positions: Vec<(f32, &str)> = Vec::new();
    let tick_text = memoized_ticks(painter.ctx(), pane, prefs);
    for ((i, &(val, _)), text) in legend.thresholds.iter().enumerate().zip(tick_text.iter()) {
        let pixel_pos = if legend.is_gradient {
            let t = (val - min_val) / range;
            if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            }
        } else {
            let t = i as f32 / n as f32;
            if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            }
        };
        label_positions.push((pixel_pos, text));
    }

    let mut prev_pos: Option<f32> = None;
    let thinned: Vec<(f32, &str)> = label_positions
        .iter()
        .filter(|(pos, _)| {
            if let Some(prev) = prev_pos
                && (pos - prev).abs() < MIN_LABEL_SPACING
            {
                return false;
            }
            prev_pos = Some(*pos);
            true
        })
        .copied()
        .collect();

    for (pixel_pos, text) in &thinned {
        if horizontal {
            let pos = egui::pos2(*pixel_pos, bar_rect.top() - SCALE_LABEL_LIFT);
            draw_shadowed_text(
                painter,
                pos,
                egui::Align2::CENTER_BOTTOM,
                text,
                label_font.clone(),
            );
        } else {
            let pos = egui::pos2(bar_rect.left() - SCALE_LABEL_GAP, *pixel_pos);
            draw_shadowed_text(
                painter,
                pos,
                egui::Align2::RIGHT_CENTER,
                text,
                label_font.clone(),
            );
        }
    }

    // --- Title: unit label above the bar (desktop) or under it (mobile),
    //     with velocity's fold annotation on the line after it ---
    let unit = crate::field_facts::unit_label(&product, prefs);
    let fold_line = legend_second_line(pane, prefs);
    if horizontal {
        // Under the bar's left end, reading left to right: `mph  folds ±50`.
        let title_pos = egui::pos2(pane_rect.left() + 2.0, bar_rect.bottom() + 1.0);
        draw_shadowed_text(
            painter,
            title_pos,
            egui::Align2::LEFT_TOP,
            unit,
            title_font.clone(),
        );
        if let Some(line) = &fold_line {
            // Measured rather than reserved, because the gap between the two
            // has to look the same after `m/s` as after `km/h`.
            let unit_width = painter
                .layout_no_wrap(unit.to_owned(), title_font, egui::Color32::WHITE)
                .rect
                .width();
            draw_shadowed_text(
                painter,
                title_pos + egui::vec2(unit_width + 6.0, 0.0),
                egui::Align2::LEFT_TOP,
                line,
                label_font.clone(),
            );
        }
    } else {
        // Two lines stacked above the bar, unit on top. `SCALE_TITLE_MARGIN`
        // reserves 16 points and the pane's edge gives the second line 16 more.
        let stacked = fold_line.as_ref().map_or(0.0, |_| FOLD_TITLE_LINE);
        let title_pos = egui::pos2(bar_rect.center().x, bar_rect.top() - 4.0 - stacked);
        draw_shadowed_text(
            painter,
            title_pos,
            egui::Align2::CENTER_BOTTOM,
            unit,
            title_font,
        );
        if let Some(line) = &fold_line {
            // Hung off the pane's own edge rather than centred on the bar:
            // `folds ±229` is 52 points over a 20-point bar 16 points in.
            draw_shadowed_text(
                painter,
                egui::pos2(pane_rect.right() - FOLD_TITLE_INSET, bar_rect.top() - 4.0),
                egui::Align2::RIGHT_BOTTOM,
                line,
                label_font.clone(),
            );
        }
    }

    // --- The range-folded key ---
    if range_folded_is_painted(&product, pane) {
        // In both orientations the key stands past the end of the bar, in the
        // pane's bottom-right corner, label reading outward from the swatch —
        // a label on the bar's own side prints through the ±80 tick.
        let (swatch, label_pos, label_anchor) = if horizontal {
            let swatch = egui::Rect::from_min_size(
                egui::pos2(
                    bar_rect.right() + (SCALE_MARGIN - RF_SWATCH_SIZE) / 2.0,
                    bar_rect.center().y - RF_SWATCH_SIZE / 2.0,
                ),
                egui::Vec2::splat(RF_SWATCH_SIZE),
            );
            (
                swatch,
                egui::pos2(swatch.center().x, swatch.bottom() + 1.0),
                egui::Align2::CENTER_TOP,
            )
        } else {
            let swatch = egui::Rect::from_min_size(
                egui::pos2(
                    bar_rect.center().x - RF_SWATCH_SIZE / 2.0,
                    bar_rect.bottom() + (SCALE_MARGIN - RF_SWATCH_SIZE) / 2.0,
                ),
                egui::Vec2::splat(RF_SWATCH_SIZE),
            );
            (
                swatch,
                egui::pos2(swatch.right() + 3.0, swatch.center().y),
                egui::Align2::LEFT_CENTER,
            )
        };
        let (r, g, b, a) = squallar_radar::RANGE_FOLDED;
        painter.rect_filled(
            swatch,
            0.0,
            egui::Color32::from_rgba_unmultiplied(r, g, b, a),
        );
        draw_shadowed_text(
            painter,
            label_pos,
            label_anchor,
            RF_SWATCH_LABEL,
            label_font,
        );
    }
}

/// Which ends of the fold, in the ramp's own m/s domain, have a place on the
/// bar — both, or neither.
fn fold_marker_positions(nyquist_ms: f32, min_val: f32, max_val: f32) -> Option<[f32; 2]> {
    if !nyquist_ms.is_finite() || nyquist_ms <= 0.0 {
        return None;
    }
    if -nyquist_ms < min_val || nyquist_ms > max_val {
        return None;
    }
    Some([-nyquist_ms, nyquist_ms])
}

/// Where this pane's picture folds, in the unit the reader chose — the legend's
/// second line. Converted through `squallar-units`, which moves neither the ramp
/// nor the marker: those are positioned in the palette's own m/s domain.
fn fold_title_line(nyquist_ms: f64, prefs: &UserPreferences) -> String {
    let converted = prefs.speed.convert_from_ms(nyquist_ms as f32);
    format!("folds \u{b1}{converted:.0}")
}

/// What this pane's storm-relative picture was shifted by: the vector, then one
/// short word for where it came from — `SRM 32 kt @ 240\u{b0} (NWS)`. See
/// [`squallar_radar::srv::StormMotionSource::tag`]. The direction is a compass
/// bearing and stays in degrees, three digits wide.
fn srm_title_line(motion: squallar_radar::srv::SrvMotion, prefs: &UserPreferences) -> String {
    let speed = prefs.speed.convert_from_knots(motion.speed_kt);
    format!(
        "SRM {speed:.0} {} @ {:03.0}\u{b0} ({})",
        prefs.speed.suffix(),
        motion.direction_deg,
        motion.source.tag(),
    )
}

/// The legend's second line — under the unit title on a right-edge bar and
/// after it on a bottom-edge one — or `None`.
fn legend_second_line(pane: &PaneState, prefs: &UserPreferences) -> Option<String> {
    if let Some(nyquist_ms) = pane
        .displayed_nyquist_ms()
        .filter(|ms| ms.is_finite() && *ms > 0.0)
    {
        return Some(fold_title_line(nyquist_ms, prefs));
    }
    pane.displayed_storm_motion()
        .filter(|motion| motion.speed_kt.is_finite() && motion.direction_deg.is_finite())
        .map(|motion| srm_title_line(motion, prefs))
}

/// Whether the purple [`squallar_radar::RANGE_FOLDED`] can appear in this pane's
/// picture, and therefore needs a key beside it.
fn range_folded_is_painted(product: &FieldId, pane: &PaneState) -> bool {
    // SRV is rasterized from `srv::compute_srv_grid`'s finished `f32` field,
    // whose NaNs are skipped, so there is no purple on an SRV raster to key.
    //
    // A comparison rather than a `matches!`: a `FieldId` is an open string, so
    // its consts are not patterns. The two fields named are the same two.
    (*product == radar_fields::known::VELOCITY || *product == radar_fields::known::SPECTRUM_WIDTH)
        && pane.is_map()
}

/// Render color scale legends for overlay layers that provide their own legend
/// (e.g. model data CIN). Drawn to the left of the radar color scale.
fn render_overlay_color_scales(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    // Same panel-wide orientation as the radar color scale.
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
) {
    let view = pane.view(pane_idx);
    // Offset each overlay legend to the left of (vertical) or above
    // (horizontal) the radar scale.
    let mut bar_offset = 0;

    for id in pane.draw_order() {
        if !pane.is_overlay_enabled(id) || *id == known::COLOR_SCALE {
            continue;
        }
        let Some(legend) = overlays.legend(id, &view.layer(id)) else {
            continue;
        };
        if legend.items.thresholds.len() < 2 {
            continue;
        }

        bar_offset += 1;
        let offset_px = bar_offset as f32 * (SCALE_BAR_WIDTH + 40.0);

        let bar_length = if horizontal {
            pane_rect.width() - SCALE_MARGIN * 2.0
        } else {
            pane_rect.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN
        };
        if bar_length < 40.0 {
            continue;
        }

        let bar_rect = if horizontal {
            let left = pane_rect.left() + SCALE_MARGIN;
            let bottom = pane_rect.bottom() - SCALE_MARGIN - offset_px;
            let top = bottom - SCALE_BAR_WIDTH;
            egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(left + bar_length, bottom))
        } else {
            let right = pane_rect.right() - SCALE_MARGIN - offset_px;
            let left = right - SCALE_BAR_WIDTH;
            let bottom = pane_rect.bottom() - SCALE_MARGIN;
            let top = bottom - bar_length;
            egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
        };

        let min_val = legend.items.min_value;
        let max_val = legend.items.max_value;
        let range = max_val - min_val;
        if range.abs() < f32::EPSILON {
            continue;
        }

        // One image over a ramp baked once per legend signature, and the ramp
        // is sampled through `overlay_bar_color_at` — which is what makes a
        // banded scale draw bands. See `crate::legend_ramp`.
        painter.image(
            legend_ramp::ramp(
                painter.ctx(),
                egui::Id::new(("squallar::legend_ramp::overlay", id.as_str(), horizontal)),
                legend.signature,
                "legend_ramp_overlay",
                horizontal,
                overlay_ramp_sampler(&legend.items),
            )
            .id(),
            bar_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        let label_font = egui::FontId::proportional(SCALE_FONT_SIZE);
        let title_font = egui::FontId::proportional(SCALE_TITLE_FONT_SIZE);

        let tick_text = memoized_overlay_ticks(painter.ctx(), id, &legend);
        let mut label_positions: Vec<(f32, &str)> = Vec::new();
        let stop_count = legend.items.thresholds.len();
        for ((i, &(val, _)), text) in legend
            .items
            .thresholds
            .iter()
            .enumerate()
            .zip(tick_text.iter())
        {
            // A banded bar's blocks are equal-width, one per stop — the same
            // convention the radar bar's discrete scales draw under — so the
            // label for stop `i` goes at the foot of block `i`, not at where
            // its *value* falls. Placing it by value on a banded bar puts every
            // label a fraction of a block off and squeezes the top band to
            // nothing.
            let t = if legend.items.is_gradient {
                (val - min_val) / range
            } else {
                band_start_fraction(i, stop_count)
            };
            let pixel_pos = if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            };
            label_positions.push((pixel_pos, text));
        }

        let mut prev_pos: Option<f32> = None;
        let thinned: Vec<(f32, &str)> = label_positions
            .iter()
            .filter(|(pos, _)| {
                if let Some(prev) = prev_pos
                    && (pos - prev).abs() < MIN_LABEL_SPACING
                {
                    return false;
                }
                prev_pos = Some(*pos);
                true
            })
            .copied()
            .collect();

        for (pixel_pos, text) in &thinned {
            if horizontal {
                let pos = egui::pos2(*pixel_pos, bar_rect.top() - SCALE_LABEL_LIFT);
                draw_shadowed_text(
                    painter,
                    pos,
                    egui::Align2::CENTER_BOTTOM,
                    text,
                    label_font.clone(),
                );
            } else {
                let pos = egui::pos2(bar_rect.left() - SCALE_LABEL_GAP, *pixel_pos);
                draw_shadowed_text(
                    painter,
                    pos,
                    egui::Align2::RIGHT_CENTER,
                    text,
                    label_font.clone(),
                );
            }
        }

        let unit = legend.items.unit_label;
        if horizontal {
            // Under its own bar, for the reason the radar bar's title is: 12
            // points is not enough to lay `kg/m²` out in, and the pane's clip
            // rect turns the shortfall into a cut-off label.
            let title_pos = egui::pos2(pane_rect.left() + 2.0, bar_rect.bottom() + 1.0);
            draw_shadowed_text(painter, title_pos, egui::Align2::LEFT_TOP, unit, title_font);
        } else {
            let title_pos = egui::pos2(bar_rect.center().x, bar_rect.top() - 4.0);
            draw_shadowed_text(
                painter,
                title_pos,
                egui::Align2::CENTER_BOTTOM,
                unit,
                title_font,
            );
        }
    }
}

/// Where the `i`-th of `n` bands begins, as a fraction of a banded bar's length.
///
/// One function for both halves of such a bar — the colour
/// [`overlay_bar_color_at`] returns and the tick written beside it — so the
/// blocks and the labels cannot drift apart.
fn band_start_fraction(i: usize, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    i as f32 / n as f32
}

/// The colour an overlay colour bar shows `t` of the way along its length.
///
/// **This is the fix for a bar that lied about its own raster.** The overlay
/// bars were baked through `interpolate_legend_color` unconditionally, under a
/// comment reading "always gradient for overlay legends", while the rasters
/// under them were painted by [`squallar_source::product::LegendScale`]'s
/// `is_gradient` — so the MRMS mosaic drew fifteen flat dBZ bands and the bar
/// beside it drew a continuous wash the picture contained nowhere.
///
/// A banded bar is `n` equal blocks, one per stop, which is the convention the
/// radar bar's own discrete scales already draw under. Equal blocks rather than
/// blocks the width of each stop's value interval: the top stop's interval is
/// unbounded above and the bar is not, so laying the bands out on the value
/// axis would shrink the highest band to a single texel — on MRMS, the 75 dBZ
/// white would never appear on the bar at all.
///
/// Pinned by
/// [`legend_ladder_tests::an_overlay_legend_bands_when_its_scale_says_bands`],
/// whose floor is that a genuinely gradient overlay scale still draws a wash.
fn overlay_bar_color_at(items: &OverlayLegend, t: f32) -> [u8; 3] {
    let thresholds = &items.thresholds;
    if thresholds.is_empty() {
        return [0, 0, 0];
    }
    if items.is_gradient {
        let range = items.max_value - items.min_value;
        return interpolate_legend_color(thresholds, items.min_value + t * range);
    }
    let n = thresholds.len();
    // A scan over the block feet rather than `(t * n) as usize`, so the block a
    // texel lands in is decided by the same fractions the labels are placed at
    // instead of by a second, independently-rounded formula.
    let block = (0..n)
        .take_while(|&i| band_start_fraction(i, n) <= t)
        .last()
        .unwrap_or(0);
    thresholds[block].1
}

/// The closure an overlay bar's ramp is baked through. Split out so what the
/// painter samples and what
/// [`legend_ladder_tests::an_overlay_legend_bands_when_its_scale_says_bands`]
/// probes are the same function and not two spellings of one intention.
fn overlay_ramp_sampler(items: &OverlayLegend) -> impl Fn(f32) -> [u8; 4] + '_ {
    move |t| {
        let [r, g, b] = overlay_bar_color_at(items, t);
        [r, g, b, 255]
    }
}

/// Interpolate an RGB color from a sorted threshold list for a given value.
fn interpolate_legend_color(thresholds: &[(f32, [u8; 3])], value: f32) -> [u8; 3] {
    if thresholds.is_empty() {
        return [0, 0, 0];
    }
    if value <= thresholds[0].0 {
        return thresholds[0].1;
    }
    if value >= thresholds[thresholds.len() - 1].0 {
        return thresholds[thresholds.len() - 1].1;
    }
    for i in 1..thresholds.len() {
        if value <= thresholds[i].0 {
            let (v0, c0) = thresholds[i - 1];
            let (v1, c1) = thresholds[i];
            let t = if (v1 - v0).abs() < f32::EPSILON {
                0.0
            } else {
                (value - v0) / (v1 - v0)
            };
            return [
                (c0[0] as f32 + (c1[0] as f32 - c0[0] as f32) * t) as u8,
                (c0[1] as f32 + (c1[1] as f32 - c0[1] as f32) * t) as u8,
                (c0[2] as f32 + (c1[2] as f32 - c0[2] as f32) * t) as u8,
            ];
        }
    }
    thresholds[thresholds.len() - 1].1
}

/// Context for per-frame point overlay rendering.
struct PerFrameOverlayCtx<'a> {
    overlays: &'a OverlayRegistry,
    id: &'a LayerId,
    zoom: f64,
    prefs: &'a UserPreferences,
    /// Pre-filtered click position (dialog clicks already stripped).
    /// See `PaneRenderCtx::overlay_click_pos` and the pre-filter in `ui_map.rs`.
    overlay_click_pos: Option<egui::Pos2>,
    excluded_rects: &'a [egui::Rect],
    pane_rect: egui::Rect,
}

/// Per-frame rendering for point overlays (e.g. METAR station model plots).
fn render_per_frame_overlay(
    galleys: &mut walkers::GalleyCache,
    ui: &egui::Ui,
    projector: &walkers::Projector,
    pf: &PerFrameOverlayCtx<'_>,
) -> Vec<Arc<dyn OverlayItem>> {
    let points = pf.overlays.per_frame_points(pf.id);
    if points.is_empty() {
        return Vec::new();
    }

    let zoom_f32 = pf.zoom as f32;
    let is_dark = ui.ctx().global_style().visuals.dark_mode;
    let draw_ctx = DrawPointContext {
        zoom: zoom_f32,
        is_dark,
    };
    let hit_radius = pf.overlays.point_hit_radius(pf.id, zoom_f32);
    let hover_ctx = HoverContext { prefs: pf.prefs };

    let screen_rect = ui.max_rect();
    let margin = hit_radius + 40.0; // extra margin for station model elements
    let expanded = screen_rect.expand(margin);
    // Pre-compute viewport geo-bounds (with margin) so we can skip the
    // expensive Mercator projection for points that are clearly off-screen.
    let geo_bounds = viewport_geo_bounds(projector, expanded);

    let painter = ui.painter();

    // Blocked-ness is a property of the *position*, not of the point tested
    // against it, so it is settled once here. Per-station it was ~41,000 rect
    // tests and 200 egui memory-lock acquisitions per pane per frame.
    let blocked = |pos: egui::Pos2| is_pos_blocked(ui.ctx(), pos, pf.pane_rect, pf.excluded_rects);
    let hover_pos = ui.ctx().pointer_hover_pos().filter(|&p| !blocked(p));
    let click_pos = pf.overlay_click_pos.filter(|&p| !blocked(p));

    let mut selected = Vec::new();
    let mut closest_hover: Option<(f32, u32)> = None; // (distance², id)

    for pt in points {
        // Fast geo-bounds rejection before the costly projection.
        if !geo_bounds.contains_point(pt.lat, pt.lon) {
            continue;
        }

        let screen = projector
            .project(walkers::lat_lon(pt.lat, pt.lon))
            .to_pos2();

        if !expanded.contains(screen) {
            continue;
        }

        let mut ep = EguiPointPainter {
            painter,
            center: screen,
            galleys,
        };
        pf.overlays.draw_point(pf.id, pt.id, &mut ep, &draw_ctx);

        // Click detection — layer blocking already applied by pre-filter in ui_map.rs.
        if let Some(click_pos) = click_pos {
            let dx = click_pos.x - screen.x;
            let dy = click_pos.y - screen.y;
            if dx * dx + dy * dy <= hit_radius * hit_radius {
                selected.push(pt.selection.clone());
            }
        }

        // Hover detection — a blocked cursor was already dropped above.
        if let Some(hp) = hover_pos {
            let dx = hp.x - screen.x;
            let dy = hp.y - screen.y;
            let d2 = dx * dx + dy * dy;
            if d2 <= hit_radius * hit_radius
                && closest_hover.is_none_or(|(best_d2, _)| d2 < best_d2)
            {
                closest_hover = Some((d2, pt.id));
            }
        }
    }

    if let Some((_, id)) = closest_hover
        && let Some(hp) = hover_pos
        && let Some(text) = pf.overlays.hover_text(pf.id, id, &hover_ctx)
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        map_hover_tooltip(
            ui.ctx(),
            egui::Id::new(("per_frame_overlay_hover", pf.id.as_str())),
            hp,
            Some(400.0),
            |tooltip_ui| {
                tooltip_ui.label(text);
            },
        );
    }

    selected
}

/// Vertical offset (points) from the touch point to the tooltip centre, so the
/// tooltip sits above the finger rather than under it.
const TOOLTIP_OFFSET_Y: f32 = 60.0;

/// Draw a floating tooltip above the finger during a long press, showing the
/// radar value at the touched position. Reached only from the touch pipeline.
#[allow(clippy::too_many_arguments)]
fn draw_long_press_tooltip(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    hover: &HoverSource,
    lat: f64,
    lon: f64,
    touch_pos: egui::Pos2,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    // The same question the pointer readout asks, through the same two
    // functions: where is this point from the radar, and what did the render
    // paint there.
    let map_pos = projector.unproject(egui::vec2(touch_pos.x, touch_pos.y));
    let (azimuth, ground_km) =
        squallar_geo::site_bearing_range_km(lat, lon, map_pos.y(), map_pos.x());

    let text = match hover.read(azimuth, ground_km) {
        Reading::Value(value) => {
            crate::field_facts::format_value(&pane.selected_product(), value, prefs)
        }
        Reading::Unpainted => "No data".to_string(),
        Reading::NotResident => "No value held for this frame".to_string(),
    };

    let tooltip_pos = egui::pos2(touch_pos.x, touch_pos.y - TOOLTIP_OFFSET_Y);

    let painter = ui.painter();
    let font = egui::FontId::proportional(14.0);
    let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
    let text_size = galley.size();
    let padding = egui::vec2(8.0, 4.0);
    let bg_rect = egui::Rect::from_center_size(tooltip_pos, text_size + padding * 2.0);

    painter.rect_filled(bg_rect, 4.0, egui::Color32::from_black_alpha(200));
    painter.galley(bg_rect.min + padding, galley, egui::Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_units::SpeedUnit;

    /// The ticks `render_color_scale` would paint for `product`, in order.
    fn ticks(product: &FieldId, prefs: &UserPreferences) -> Vec<String> {
        legend_ticks(product, prefs)
    }

    /// Every legend tick string and every colour-bar unit title, for every
    /// registered field under three unit preferences, against literals
    /// **captured from the build before WO-E9e re-keyed this file**.
    ///
    /// The order's Reopen-1:1 rule is that a bar's labels render byte-identical
    /// across the move off the product enum, and this is the only thing that
    /// can say so: the conversion now runs through the field's own
    /// [`squallar_units::Quantity::convert`] instead of a hand-written arm per
    /// product, and the unit title through `Quantity::suffix` instead of the
    /// enum's `unit_label`. The expectations below were **measured** at
    /// `ed5a1f9b` and pasted in, not derived from the new code, so a formula
    /// that is consistently wrong in both spellings cannot pass.
    ///
    /// **One field's row is no longer that capture, and it has moved twice.**
    /// Reflectivity's ticks — the same string in all three sets, dBZ having no
    /// unit choice — read `0|2.5|5|7.5|10|…|75|80|85|90|95` at `ed5a1f9b`. The
    /// 2026-08-23 unification of the three dBZ ladders took the low end to the
    /// one the overlay layers draw, which has no 7.5 dBZ stop, and `e6091e47`
    /// also capped the bar at 75; restoring radar's hail band put `80|85|90|95`
    /// back. So the row now reads the `ed5a1f9b` capture with its 7.5 gone and
    /// its tail returned — `0|2.5|5|10|…|70|75|80|85|90|95`. Both moves are the
    /// bar's, not this formatter's; every other row is still the `ed5a1f9b`
    /// reading untouched.
    #[test]
    fn every_tick_and_unit_string_is_what_it_was_before_the_field_ids() {
        let sets: [(&str, UserPreferences); 3] = [
            ("default", UserPreferences::default()),
            (
                "metric",
                UserPreferences {
                    speed: SpeedUnit::MetersPerSec,
                    height: squallar_units::HeightUnit::Meters,
                    precip_rate: squallar_units::PrecipRateUnit::MillimetersPerHour,
                    hail_size: HailSizeUnit::Centimeters,
                    ..UserPreferences::default()
                },
            ),
            (
                "mm",
                UserPreferences {
                    hail_size: HailSizeUnit::Millimeters,
                    ..UserPreferences::default()
                },
            ),
        ];
        // (preference set, field id, the ticks joined by `|`, the unit title)
        const EXPECTED: [(&str, &str, &str, &str); 51] = [
            (
                "default",
                "Reflectivity",
                "0|2.5|5|10|15|20|25|30|35|40|45|50|55|60|65|70|75|80|85|90|95",
                "dBZ",
            ),
            (
                "default",
                "Velocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            ("default", "SpectrumWidth", "0|5|9|14|18|23", "mph"),
            (
                "default",
                "DifferentialPhase",
                "0|15|30|45|60|75|90|105|120|135|150|165|180|195|210|225|240|255|270|285|300|315|330|345",
                "°",
            ),
            (
                "default",
                "CorrelationCoefficient",
                "0.45|0.55|0.75|0.80|0.90|0.96|0.98",
                "CC",
            ),
            (
                "default",
                "DifferentialReflectivity",
                "-2.0|-1.0|0.0|0.2|1.0|1.5|2.0|2.5|3.0|4.0|5.0|5.5",
                "dB",
            ),
            (
                "default",
                "StormRelativeVelocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            (
                "default",
                "SpecificDifferentialPhase",
                "-2.0|-1.0|-0.5|0.0|1.0|1.5|2.0|2.5|3.0|4.0|5.0|6.0|6.5",
                "°/km",
            ),
            (
                "default",
                "EchoTops",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "default",
                "EchoTopsInterpolated",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "default",
                "VerticallyIntegratedLiquid",
                "1|5|10|15|20|25|30|35|40|50|60",
                "kg/m²",
            ),
            (
                "default",
                "VilDensity",
                "0.5|1|1.5|2|2.5|3|3.5|4|4.5|5|6",
                "g/m³",
            ),
            (
                "default",
                "ProbabilityOfSevereHail",
                "10|20|30|40|50|60|70|80|90|100",
                "%",
            ),
            (
                "default",
                "MaxExpectedHailSize",
                "0.2|0.5|0.8|1|1.2|1.5|1.8|2|2.5|3|3.5|4",
                "in",
            ),
            (
                "default",
                "HydrometeorClassification",
                "Bio|AP|IC|DS|WS|RA|HR|BD|GR|HA|LH|GH|MS|UK|RF",
                "HHC",
            ),
            (
                "default",
                "PrecipitationRate",
                "0.01|0.10|0.25|0.50|1.0|2.0|3.0|4.0|6.0|8.0|12.0",
                "in/hr",
            ),
            (
                "default",
                "NormalizedRotation",
                "-2|-1|-1.0|-0.2|0.2|1.0|1|1.5|1.5|2.0|2|2.5|2.5|3.0|3",
                "NROT",
            ),
            (
                "metric",
                "Reflectivity",
                "0|2.5|5|10|15|20|25|30|35|40|45|50|55|60|65|70|75|80|85|90|95",
                "dBZ",
            ),
            (
                "metric",
                "Velocity",
                "-36|-31|-26|-21|-15|-10|-5|-0|0|5|10|15|21|26|31|36",
                "m/s",
            ),
            ("metric", "SpectrumWidth", "0|2|4|6|8|10", "m/s"),
            (
                "metric",
                "DifferentialPhase",
                "0|15|30|45|60|75|90|105|120|135|150|165|180|195|210|225|240|255|270|285|300|315|330|345",
                "°",
            ),
            (
                "metric",
                "CorrelationCoefficient",
                "0.45|0.55|0.75|0.80|0.90|0.96|0.98",
                "CC",
            ),
            (
                "metric",
                "DifferentialReflectivity",
                "-2.0|-1.0|0.0|0.2|1.0|1.5|2.0|2.5|3.0|4.0|5.0|5.5",
                "dB",
            ),
            (
                "metric",
                "StormRelativeVelocity",
                "-36|-31|-26|-21|-15|-10|-5|-0|0|5|10|15|21|26|31|36",
                "m/s",
            ),
            (
                "metric",
                "SpecificDifferentialPhase",
                "-2.0|-1.0|-0.5|0.0|1.0|1.5|2.0|2.5|3.0|4.0|5.0|6.0|6.5",
                "°/km",
            ),
            ("metric", "EchoTops", "2|3|5|6|8|9|11|12|14|15|17|18", "km"),
            (
                "metric",
                "EchoTopsInterpolated",
                "2|3|5|6|8|9|11|12|14|15|17|18",
                "km",
            ),
            (
                "metric",
                "VerticallyIntegratedLiquid",
                "1|5|10|15|20|25|30|35|40|50|60",
                "kg/m²",
            ),
            (
                "metric",
                "VilDensity",
                "0.5|1|1.5|2|2.5|3|3.5|4|4.5|5|6",
                "g/m³",
            ),
            (
                "metric",
                "ProbabilityOfSevereHail",
                "10|20|30|40|50|60|70|80|90|100",
                "%",
            ),
            (
                "metric",
                "MaxExpectedHailSize",
                "0.6|1.3|1.9|2.5|3.2|3.8|4.4|5.1|6.3|7.6|8.9|10.2",
                "cm",
            ),
            (
                "metric",
                "HydrometeorClassification",
                "Bio|AP|IC|DS|WS|RA|HR|BD|GR|HA|LH|GH|MS|UK|RF",
                "HHC",
            ),
            (
                "metric",
                "PrecipitationRate",
                "0.25|2.5|6.3|12.7|25.4|50.8|76.2|101.6|152.4|203.2|304.8",
                "mm/hr",
            ),
            (
                "metric",
                "NormalizedRotation",
                "-2|-1|-1.0|-0.2|0.2|1.0|1|1.5|1.5|2.0|2|2.5|2.5|3.0|3",
                "NROT",
            ),
            (
                "mm",
                "Reflectivity",
                "0|2.5|5|10|15|20|25|30|35|40|45|50|55|60|65|70|75|80|85|90|95",
                "dBZ",
            ),
            (
                "mm",
                "Velocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            ("mm", "SpectrumWidth", "0|5|9|14|18|23", "mph"),
            (
                "mm",
                "DifferentialPhase",
                "0|15|30|45|60|75|90|105|120|135|150|165|180|195|210|225|240|255|270|285|300|315|330|345",
                "°",
            ),
            (
                "mm",
                "CorrelationCoefficient",
                "0.45|0.55|0.75|0.80|0.90|0.96|0.98",
                "CC",
            ),
            (
                "mm",
                "DifferentialReflectivity",
                "-2.0|-1.0|0.0|0.2|1.0|1.5|2.0|2.5|3.0|4.0|5.0|5.5",
                "dB",
            ),
            (
                "mm",
                "StormRelativeVelocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            (
                "mm",
                "SpecificDifferentialPhase",
                "-2.0|-1.0|-0.5|0.0|1.0|1.5|2.0|2.5|3.0|4.0|5.0|6.0|6.5",
                "°/km",
            ),
            (
                "mm",
                "EchoTops",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "mm",
                "EchoTopsInterpolated",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "mm",
                "VerticallyIntegratedLiquid",
                "1|5|10|15|20|25|30|35|40|50|60",
                "kg/m²",
            ),
            (
                "mm",
                "VilDensity",
                "0.5|1|1.5|2|2.5|3|3.5|4|4.5|5|6",
                "g/m³",
            ),
            (
                "mm",
                "ProbabilityOfSevereHail",
                "10|20|30|40|50|60|70|80|90|100",
                "%",
            ),
            (
                "mm",
                "MaxExpectedHailSize",
                "6|13|19|25|32|38|44|51|64|76|89|102",
                "mm",
            ),
            (
                "mm",
                "HydrometeorClassification",
                "Bio|AP|IC|DS|WS|RA|HR|BD|GR|HA|LH|GH|MS|UK|RF",
                "HHC",
            ),
            (
                "mm",
                "PrecipitationRate",
                "0.01|0.10|0.25|0.50|1.0|2.0|3.0|4.0|6.0|8.0|12.0",
                "in/hr",
            ),
            (
                "mm",
                "NormalizedRotation",
                "-2|-1|-1.0|-0.2|0.2|1.0|1|1.5|1.5|2.0|2|2.5|2.5|3.0|3",
                "NROT",
            ),
        ];
        assert_eq!(
            EXPECTED.len(),
            sets.len() * radar_fields::known::ALL.len(),
            "the table must cover every registered field in every preference \
             set, or a field could drop out of it with nothing going red",
        );
        for (label, field, want_ticks, want_unit) in EXPECTED {
            let id = FieldId::from_static(field);
            assert!(
                radar_fields::known::ALL.contains(&id),
                "{field} is not a field this crate has a const for",
            );
            let (_, prefs) = sets
                .iter()
                .find(|(name, _)| *name == label)
                .expect("a preference set named in the table");
            assert_eq!(
                ticks(&id, prefs).join("|"),
                want_ticks,
                "{field} under {label} preferences: the colour bar's labels moved",
            );
            assert_eq!(
                crate::field_facts::unit_label(&id, prefs),
                want_unit,
                "{field} under {label} preferences: the colour bar's unit title moved",
            );
        }
    }

    /// The MEHS colour bar is labelled in the user's hail-size unit; its stops
    /// are authored in inches, so the colours must not move.
    #[test]
    fn the_mehs_colour_bar_is_labelled_in_the_users_hail_size_unit() {
        let expected = [
            (
                HailSizeUnit::Inches,
                [
                    "0.2", "0.5", "0.8", "1", "1.2", "1.5", "1.8", "2", "2.5", "3", "3.5", "4",
                ],
            ),
            (
                HailSizeUnit::Centimeters,
                [
                    "0.6", "1.3", "1.9", "2.5", "3.2", "3.8", "4.4", "5.1", "6.3", "7.6", "8.9",
                    "10.2",
                ],
            ),
            (
                HailSizeUnit::Millimeters,
                [
                    "6", "13", "19", "25", "32", "38", "44", "51", "64", "76", "89", "102",
                ],
            ),
        ];
        for (unit, labels) in expected {
            let prefs = UserPreferences {
                hail_size: unit,
                ..UserPreferences::default()
            };
            assert_eq!(
                ticks(&radar_fields::known::MAX_EXPECTED_HAIL_SIZE, &prefs),
                labels,
                "{unit:?} ticks",
            );
        }

        // The stops themselves are untouched by the preference: this is a
        // relabelling, not a repalettising.
        let inch_stops: Vec<f32> =
            crate::field_facts::facts(&radar_fields::known::MAX_EXPECTED_HAIL_SIZE)
                .scale
                .thresholds
                .iter()
                .map(|&(v, _)| v)
                .collect();
        assert_eq!(
            inch_stops,
            [
                0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 3.5, 4.0
            ],
            "the palette's stops are inches whatever the preference says",
        );
    }

    /// A tick and the hover readout are the same number in the same unit.
    #[test]
    fn a_mehs_tick_and_the_hover_readout_are_the_same_number() {
        for unit in [HailSizeUnit::Centimeters, HailSizeUnit::Millimeters] {
            let prefs = UserPreferences {
                hail_size: unit,
                ..UserPreferences::default()
            };
            let product = radar_fields::known::MAX_EXPECTED_HAIL_SIZE;
            for &(stop, _) in &crate::field_facts::facts(&product).scale.thresholds {
                let tick = format_legend_value(&product, stop, &prefs);
                assert_eq!(
                    crate::field_facts::format_value(&product, stop, &prefs),
                    format!(
                        "MEHS: {tick} {}",
                        crate::field_facts::unit_label(&product, &prefs)
                    ),
                    "{unit:?} at the {stop} in stop",
                );
            }
        }
    }

    /// Every other product's ticks are unchanged, and no product picked up a
    /// hail-size conversion.
    #[test]
    fn no_other_products_ticks_moved() {
        let prefs = UserPreferences {
            hail_size: HailSizeUnit::Millimeters,
            ..UserPreferences::default()
        };
        let default = UserPreferences::default();
        for product in radar_fields::known::ALL.iter() {
            if *product == radar_fields::known::MAX_EXPECTED_HAIL_SIZE {
                continue;
            }
            assert_eq!(
                ticks(product, &prefs),
                ticks(product, &default),
                "{product:?} reads the hail-size preference and should not",
            );
        }
        // And the generic form itself, which every unnamed product falls back
        // to: whole numbers bare, one decimal otherwise.
        assert_eq!(short_tick(4.0), "4");
        assert_eq!(short_tick(0.25), "0.2");
        assert_eq!(short_tick(-1.5), "-1.5");
    }

    /// **Both** echo-tops bars are labelled in the user's height unit; their
    /// stops are authored in kft and both are titled off `HeightUnit::kilo_suffix`.
    #[test]
    fn both_echo_tops_bars_are_labelled_in_the_users_height_unit() {
        use squallar_units::HeightUnit;

        let feet = UserPreferences {
            height: HeightUnit::Feet,
            ..UserPreferences::default()
        };
        let metres = UserPreferences {
            height: HeightUnit::Meters,
            ..UserPreferences::default()
        };
        for product in [
            radar_fields::known::ECHO_TOPS,
            radar_fields::known::ECHO_TOPS_INTERPOLATED,
        ] {
            assert_eq!(
                ticks(&product, &feet).last().map(String::as_str),
                Some("60"),
                "{product:?} in feet is the bar as it has always been labelled",
            );
            assert_eq!(
                ticks(&product, &metres).last().map(String::as_str),
                Some("18"),
                "{product:?} in metres is labelled in kft: 60 kft is 18 km",
            );
            // And the number on the bar is the number the readout gives for the
            // same stop, to the tick's own precision.
            let top = crate::field_facts::facts(&product)
                .scale
                .thresholds
                .last()
                .expect("the echo-tops ramp has stops")
                .0;
            let readout = crate::field_facts::format_value(&product, top, &metres);
            assert!(
                readout.contains("18.3 km"),
                "{product:?} reads out {readout:?} for the stop its bar calls \
                 {:?}",
                ticks(&product, &metres).last(),
            );
        }
    }

    /// The velocity ramp's own reach, m/s — what a fold marker has to fall
    /// inside to be drawable.
    fn velocity_bounds() -> (f32, f32) {
        let legend = crate::field_facts::facts(&radar_fields::known::VELOCITY).scale;
        (legend.min_value, legend.max_value)
    }

    /// A real Doppler declaration that sits **inside** the bar, m/s: KTLX's
    /// 0.5° cut on 2026-08-11 at 10:09. A WSR-88D and not a TDWR — a TDWR
    /// declares `nyquist_velocity = 0` on every cut.
    const INSIDE_THE_BAR_MS: f32 = 23.84;

    /// A declaration past the end of the bar, m/s — wider than KFFC cut 12's
    /// 62.94, the fastest measured, and the widest speed the velocity moment
    /// itself encodes (±63.5 m/s in half-metre steps).
    const PAST_THE_BAR_MS: f32 = 63.5;

    /// The fold annotation is the declared limit converted, and nothing else —
    /// every user-facing number goes through `squallar-units`.
    #[test]
    fn the_fold_annotation_is_the_declared_limit_in_the_users_speed_unit() {
        // 23.84 m/s in each unit, rounded as the annotation rounds: 53.33 mph,
        // 85.82 km/h, 46.34 kt.
        let expected = [
            (SpeedUnit::Mph, "folds \u{b1}53"),
            (SpeedUnit::MetersPerSec, "folds \u{b1}24"),
            (SpeedUnit::KilometersPerHour, "folds \u{b1}86"),
            (SpeedUnit::Knots, "folds \u{b1}46"),
        ];
        for (speed, line) in expected {
            let prefs = UserPreferences {
                speed,
                ..UserPreferences::default()
            };
            assert_eq!(
                fold_title_line(f64::from(INSIDE_THE_BAR_MS), &prefs),
                line,
                "{speed:?}",
            );
        }
    }

    /// Both ends of a fold inside the ramp are marked; a declaration past its
    /// reach is marked nowhere. The off-scale answer is *nothing*, not a marker
    /// parked at the end.
    #[test]
    fn a_fold_off_the_end_of_the_ramp_is_marked_nowhere() {
        let (min_val, max_val) = velocity_bounds();
        assert!(
            (max_val - 36.01).abs() < 0.01 && (min_val + 36.01).abs() < 0.01,
            "the velocity ramp moved: it now spans {min_val}..{max_val} m/s, \
             so the fixtures below no longer describe one fold inside the bar \
             and one past it",
        );

        assert_eq!(
            fold_marker_positions(INSIDE_THE_BAR_MS, min_val, max_val),
            Some([-INSIDE_THE_BAR_MS, INSIDE_THE_BAR_MS]),
            "a fold inside the bar must be marked at both ends",
        );
        assert_eq!(
            fold_marker_positions(PAST_THE_BAR_MS, min_val, max_val),
            None,
            "a {PAST_THE_BAR_MS} m/s fold was marked on a bar that stops at \
             36.01",
        );
        // Exactly at the end still counts — that marker is drawable, and it is
        // the boundary a clamp would have been written around.
        assert_eq!(
            fold_marker_positions(max_val, min_val, max_val),
            Some([-max_val, max_val]),
        );
        // A declaration of zero or a non-finite one describes no fold at all.
        // Zero is the live case — every TDWR declares it.
        for absurd in [0.0, -22.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                fold_marker_positions(absurd, min_val, max_val),
                None,
                "{absurd} was taken for a fold limit",
            );
        }
    }

    /// A projection with no map in it: one screen point per degree, so a
    /// row's coordinates and the pixel it lands on are the same numbers.
    fn degrees_as_pixels(lat: f64, lon: f64) -> egui::Pos2 {
        egui::pos2(lon as f32, lat as f32)
    }

    /// Everything, so the walk's on-screen filter never decides anything here.
    fn everywhere() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(-400.0, -400.0), egui::pos2(400.0, 400.0))
    }

    /// A [`VisibleSite`] keeps naming its own radar after the table grows
    /// under it — the table is resolved at runtime, so an index would not.
    #[test]
    fn a_visible_site_names_its_own_radar_after_the_table_grows() {
        let position = |lat_udeg, lon_udeg| squallar_radar::site_position::SitePosition {
            lat_udeg,
            lon_udeg,
            site_height_m: 100,
            tower_height_m: 20,
        };
        // Three radars in the empty South Pacific, and a smaller table holding
        // only the first. The binary carries no radars at all.
        let learned = squallar_radar::sites::SiteFix::Learned;
        // `ZZZA` rather than `ZZZZ`: arrivals are sorted by identifier, so the
        // incumbent must sort first for the zip below to compare like with like.
        let incumbent = ("ZZZA", learned(position(-29_000_000, -139_000_000)));
        let smaller = squallar_radar::sites::build_table([incumbent]);
        let bigger = squallar_radar::sites::build_table([
            incumbent,
            ("ZZZY", learned(position(-30_000_000, -140_000_000))),
            ("ZZZX", learned(position(-31_000_000, -141_000_000))),
        ]);
        assert_eq!(
            bigger.rows().len(),
            smaller.rows().len() + 2,
            "precondition: the two tables must be different lengths, or this \
             test cannot tell an index from a reference",
        );

        let walked = |rows| visible_sites_in(rows, everywhere(), 18.0, degrees_as_pixels);

        // Every site the smaller table produced is still named by the larger
        // one's walk, at the same place.
        let before = walked(smaller.rows());
        let after = walked(bigger.rows());
        assert!(
            !before.is_empty(),
            "the smaller table must produce some visible sites"
        );
        for (old, new) in before.iter().zip(after.iter()) {
            assert_eq!(
                old.site.name, new.site.name,
                "a row changed identity when the table grew",
            );
            assert_eq!(old.screen, new.screen);
        }

        // And every row names the radar whose coordinates put it there, which
        // is the property an index cannot promise across two tables.
        for visible in &after {
            assert_eq!(
                visible.screen,
                degrees_as_pixels(visible.site.lat, visible.site.lon),
                "{} is drawn somewhere other than its own position",
                visible.site.name,
            );
        }

        // The arrivals are among them, reachable and named.
        let names: Vec<&str> = after.iter().map(|v| v.site.name).collect();
        assert!(names.contains(&"ZZZY"), "got {names:?}");
        assert!(names.contains(&"ZZZX"));
    }
}

#[cfg(test)]
mod legend_ladder_tests {
    use super::*;
    use squallar_overlays::hrrr::ModelParameter;
    use squallar_overlays::mrms::MrmsProduct;

    fn legend_of(scale: &squallar_source::product::LegendScale) -> OverlayLegend {
        OverlayLegend {
            thresholds: scale.thresholds.clone(),
            is_gradient: scale.is_gradient,
            min_value: scale.min_value,
            max_value: scale.max_value,
            unit_label: "dBZ",
        }
    }

    fn mrms_reflectivity() -> OverlayLegend {
        legend_of(squallar_overlays::mrms::fields::spec(MrmsProduct::ReflectivityComposite).scale)
    }

    /// **The acceptance for the overlay bar that drew a wash over a banded
    /// raster.** `render_overlay_color_scales` baked every overlay ramp through
    /// `interpolate_legend_color` regardless of the scale's own `is_gradient`,
    /// so MRMS's mosaic painted fifteen flat dBZ bands while the bar beside it
    /// painted a continuous gradient.
    ///
    /// Probed through [`overlay_ramp_sampler`], which is the closure the
    /// painter hands `legend_ramp::ramp` — not a re-derivation of what it ought
    /// to do.
    #[test]
    fn an_overlay_legend_bands_when_its_scale_says_bands() {
        let mrms = mrms_reflectivity();
        assert!(!mrms.is_gradient, "precondition: MRMS's dBZ bar is banded");
        let n = mrms.thresholds.len();
        assert!(
            n >= 10,
            "precondition: {n} stops is too few to read bands off"
        );

        let sample = overlay_ramp_sampler(&mrms);

        // Inside one block the colour does not move...
        for i in 0..n {
            let lo = band_start_fraction(i, n);
            let hi = band_start_fraction(i + 1, n);
            let inner = [
                lo + (hi - lo) * 0.1,
                lo + (hi - lo) * 0.5,
                hi - (hi - lo) * 0.05,
            ];
            for t in inner {
                assert_eq!(
                    sample(t),
                    sample(lo),
                    "band {i} of {n} is not flat: t={t} differs from its own foot",
                );
            }
            assert_eq!(
                sample(lo),
                [
                    mrms.thresholds[i].1[0],
                    mrms.thresholds[i].1[1],
                    mrms.thresholds[i].1[2],
                    255,
                ],
                "band {i} does not show stop {i}'s own colour",
            );
        }

        // ...and it does move across every boundary, so "one colour for the
        // whole bar" is not what the flatness above would accept.
        for i in 1..n {
            let foot = band_start_fraction(i, n);
            assert_ne!(
                sample(foot - 1.0 / 4096.0),
                sample(foot),
                "the bar does not change colour at the foot of band {i}",
            );
        }

        // The floor: a genuinely gradient overlay scale still draws a wash, so
        // the fix is not "everything is bands now".
        let precip =
            legend_of(squallar_overlays::mrms::fields::spec(MrmsProduct::PrecipRate).scale);
        assert!(
            precip.is_gradient,
            "precondition: MRMS precip rate is a continuous ramp",
        );
        let wash = overlay_ramp_sampler(&precip);
        let moved = (0..64u8)
            .filter(|&i| {
                let t = f32::from(i) / 64.0;
                wash(t) != wash(t + 1.0 / 128.0)
            })
            .count();
        assert!(
            moved > 40,
            "a gradient overlay bar must keep moving between its stops; it \
             changed at only {moved} of 64 probes",
        );
    }

    /// **The one place all three dBZ ladders are visible at once**, and the one
    /// place the agreement and the divergence can both be stated against the
    /// real bars rather than against the substrate's tables.
    ///
    /// `squallar-overlays` may not name `squallar-radar` — the charter cuts that
    /// edge — so the agreement is pinned there against the substrate and here
    /// against the radar palette itself. This is the test that would have
    /// caught the original defect: the same dBZ read one colour on a radar tilt
    /// and a different one on the MRMS mosaic drawn in the same pane.
    ///
    /// **It is bounded at 70 dBZ, and both bounds are asserted.** Above 70 the
    /// layers part on purpose — radar carries the hail band up to 95 and the two
    /// gridded bars cap at 75 white, because their grids do not reach up there.
    /// So a re-convergence at 75 (which is what `e6091e47` shipped, painting
    /// every hail core one flat white on a tilt) reddens on the divergence
    /// half, and any drift at or below 70, or a tail growing on an overlay bar,
    /// reddens on the agreement half.
    #[test]
    fn every_layer_that_draws_dbz_paints_the_same_ladder_through_seventy() {
        use squallar_source::product::{
            REFLECTIVITY_DIVERGENCE_DBZ, REFLECTIVITY_OVERLAY_FLOOR, REFLECTIVITY_RADAR_STOPS,
        };

        let radar = crate::field_facts::facts(&radar_fields::known::REFLECTIVITY).scale;
        let mrms = squallar_overlays::mrms::fields::spec(MrmsProduct::ReflectivityComposite).scale;
        let hrrr =
            squallar_overlays::hrrr::fields::spec(ModelParameter::CompositeReflectivity).scale;

        assert_eq!(
            radar.thresholds,
            REFLECTIVITY_RADAR_STOPS.to_vec(),
            "the radar bar is not the substrate's radar ladder",
        );

        // ── agreement, through 70 dBZ ──
        let below_divergence = |stops: &[(f32, [u8; 3])]| -> Vec<(f32, [u8; 3])> {
            stops
                .iter()
                .copied()
                .filter(|&(dbz, _)| dbz < REFLECTIVITY_DIVERGENCE_DBZ)
                .collect()
        };
        let radar_shared = below_divergence(&radar.thresholds);
        for (layer, scale) in [("MRMS", mrms), ("HRRR", hrrr)] {
            assert_eq!(
                below_divergence(&scale.thresholds),
                radar_shared[REFLECTIVITY_OVERLAY_FLOOR..].to_vec(),
                "{layer}'s dBZ ladder is not the radar bar's from 5 dBZ through \
                 70",
            );
        }
        assert!(
            radar_shared.len() >= 12,
            "a shared core of {} stops is too short for this to mean much",
            radar_shared.len(),
        );

        // ── divergence, at and above 75 dBZ ──
        let at_divergence = |scale: &squallar_source::product::LegendScale| {
            scale
                .thresholds
                .iter()
                .find(|&&(dbz, _)| dbz == REFLECTIVITY_DIVERGENCE_DBZ)
                .copied()
                .unwrap_or_else(|| {
                    panic!("every dBZ bar carries a {REFLECTIVITY_DIVERGENCE_DBZ} stop")
                })
        };
        let radar_top = at_divergence(radar);
        for (layer, scale) in [("MRMS", mrms), ("HRRR", hrrr)] {
            assert_ne!(
                at_divergence(scale).1,
                radar_top.1,
                "{layer}'s bar has re-converged with the radar bar at \
                 {REFLECTIVITY_DIVERGENCE_DBZ} dBZ. The two are meant to \
                 differ: a tilt shows the hail band and a gridded composite \
                 does not produce values there.",
            );
            assert_eq!(
                scale.max_value, REFLECTIVITY_DIVERGENCE_DBZ,
                "{layer}'s bar must stop where the layers part, not advertise a \
                 range its raster cannot reach",
            );
        }
        assert_eq!(
            radar.max_value, 95.0,
            "the radar bar must run to the top of the hail band; a 75 here is \
             the regression this test was re-scoped for",
        );
        assert!(
            radar
                .thresholds
                .iter()
                .filter(|&&(dbz, _)| dbz >= REFLECTIVITY_DIVERGENCE_DBZ)
                .count()
                >= 5,
            "the radar bar's hail band is not a band; it has {} stops at or \
             above {REFLECTIVITY_DIVERGENCE_DBZ} dBZ",
            radar
                .thresholds
                .iter()
                .filter(|&&(dbz, _)| dbz >= REFLECTIVITY_DIVERGENCE_DBZ)
                .count(),
        );

        // Banding is the per-layer decision, and it is genuinely different
        // here — which is what stops the equality above from being a claim
        // that the three layers are the same object.
        assert!(radar.is_gradient, "a radar tilt is drawn as a wash");
        assert!(!mrms.is_gradient, "the mosaic is drawn as bands");
        assert!(
            !hrrr.is_gradient,
            "the forecast composite is drawn as bands"
        );
    }

    /// **The acceptance for the alpha unification: the two paths hand back the
    /// same opacity at the same dBZ.**
    ///
    /// Radar painted dBZ through `squallar-radar`'s `TRANSPARENCY` (180) and the
    /// gridded overlays through `render::gridded`'s `ALPHA` (160), so a tilt and
    /// the MRMS mosaic enabled on one pane drew the same quantity at two
    /// opacities with nothing comparing them. 160 is the survivor —
    /// `squallar_source::product::REFLECTIVITY_ALPHA` records why that one and
    /// not a third number.
    ///
    /// **This asks the two painters, not the two constants.** A test that
    /// compared `REFLECTIVITY_ALPHA` against a literal would pass while either
    /// path stopped reading it; this one walks real dBZ through
    /// `squallar_radar::get_color_for_value` and through the `FieldPaint` the
    /// rasterizer actually resolves for the mosaic, and compares the alpha byte
    /// they return. It is also this crate's job because nothing lower can see
    /// both: the overlays→radar edge is cut.
    ///
    /// HRRR's composite is in it too — it resolves its own ramp rather than the
    /// generic one over a `LegendScale`, so it is a third painter and not a
    /// second reader of the same code.
    #[test]
    fn a_tilt_and_a_mosaic_paint_the_same_dbz_at_the_same_opacity() {
        let expected = squallar_source::product::REFLECTIVITY_ALPHA;
        // Resolved through the field id, the way `render_radar_color_ramp`
        // resolves what it bakes a bar from. The `arch_ratchets` row that holds
        // the radar product enum out of this crate is at 0 and may only fall,
        // so the enum is never spelled here — only carried.
        let dbz_id = radar_fields::known::REFLECTIVITY;
        let tilt_dbz =
            radar_fields::product_for(&dbz_id).expect("the radar crate registers reflectivity");
        let mrms_paint = squallar_overlays::render::gridded::field_paint(
            &squallar_overlays::mrms::fields::spec(MrmsProduct::ReflectivityComposite).id,
        )
        .expect("the mosaic's dBZ field is registered for painting");
        let hrrr_paint = squallar_overlays::render::gridded::field_paint(
            &squallar_overlays::hrrr::fields::spec(ModelParameter::CompositeReflectivity).id,
        )
        .expect("the forecast composite's dBZ field is registered for painting");

        // Every 0.5 dBZ from the overlays' floor to the top of their bars: the
        // whole range in which all three layers paint something.
        let mut probes = 0usize;
        let mut dbz = 5.0f32;
        while dbz <= 75.0 {
            let tilt = get_color_for_value(tilt_dbz, dbz).3;
            let mosaic = mrms_paint.color_for_value(dbz)[3];
            let forecast = hrrr_paint.color_for_value(dbz)[3];
            assert_eq!(
                (tilt, mosaic, forecast),
                (expected, expected, expected),
                "at {dbz} dBZ a tilt, the mosaic and the forecast composite \
                 paint at three different opacities. They are the same \
                 quantity and can be drawn in the same pane; \
                 REFLECTIVITY_ALPHA is the one number they all read.",
            );
            probes += 1;
            dbz += 0.5;
        }
        // precondition: the sweep is a sweep.
        assert_eq!(probes, 141);

        // The floor that stops this passing on three transparent answers: every
        // layer really paints in this range.
        assert!(
            [
                get_color_for_value(tilt_dbz, 40.0).3,
                mrms_paint.color_for_value(40.0)[3],
                hrrr_paint.color_for_value(40.0)[3],
            ]
            .iter()
            .all(|&a| a > 0),
            "precondition: a 40 dBZ core must be opaque on all three layers, or \
             the agreement above is an agreement about nothing",
        );

        // And the control: this is not a claim that everything paints at 160.
        let rho_id = radar_fields::known::CORRELATION_COEFFICIENT;
        let rho = radar_fields::product_for(&rho_id)
            .expect("the radar crate registers the correlation coefficient");
        // The radar crate's other scales keep their own TRANSPARENCY, which is
        // what makes the reflectivity arm a deliberate exception rather than a
        // crate-wide edit.
        assert_ne!(
            get_color_for_value(rho, 0.95).3,
            expected,
            "ρHV now paints at the dBZ alpha too, so the unification leaked out \
             of the field it was scoped to",
        );
    }
}

#[path = "ui_map_pane/raster_registration_tests.rs"]
#[cfg(test)]
mod raster_registration_tests;

#[path = "ui_map_pane/theme_flip_tests.rs"]
#[cfg(test)]
mod theme_flip_tests;

#[path = "ui_map_pane/as_of_token_tests.rs"]
#[cfg(test)]
mod as_of_token_tests;

#[path = "ui_map_pane/floor_strip_shading_tests.rs"]
#[cfg(test)]
mod floor_strip_shading_tests;
