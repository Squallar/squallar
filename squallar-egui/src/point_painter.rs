//! Egui implementation of [`squallar_overlays::render::draw::PointPainter`].

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, FontId, Pos2, Shape, Stroke};
use squallar_overlays::render::draw::{PointPainter, TextAnchor};
use squallar_source::id::LayerId;

pub(crate) struct EguiPointPainter<'a> {
    pub painter: &'a egui::Painter,
    pub center: Pos2,
    /// Whether this layer's GEOMETRY is already drawn somewhere else.
    ///
    /// A layer that rasterizes a picture has had its shapes drawn in the
    /// worker; drawing them again here would paint them twice and pay the
    /// tessellator for the copy that is not visible. Text is the exception and
    /// the reason this is a flag rather than a skipped call: `tiny_skia` has no
    /// fonts, so the picture carries no text and the frame thread is the only
    /// place a galley can be laid out.
    ///
    /// Set from `job_codec(id).is_some()` at the call site, so a layer that
    /// gains a picture stops double-drawing the moment it does, with nothing
    /// to remember.
    pub text_only: bool,
    /// The galley memo every `text` call on this painter goes through.
    ///
    /// A station model is several numbers per station and there are hundreds
    /// of stations on screen, so this path lays out more galleys per frame
    /// than the basemap's place names do. Lent by the pane walk, the same
    /// cache the `CityLabels` arm uses; see [`walkers::GalleyCache`].
    pub galleys: &'a mut walkers::GalleyCache,
    /// Where text goes instead of the painter, when the caller is collecting
    /// a layer's text to tessellate once. See [`PointTextMeshes`]. `None`
    /// paints each label as `Painter::galley` would.
    pub sink: Option<&'a mut Vec<Shape>>,
    /// The pass's `pixels_per_point`, read by the caller once for the layer.
    ///
    /// **Carried rather than asked for**, because this value is built per
    /// POINT: `Context::pixels_per_point` is `Context::write`, and a station
    /// model draws several strings, so asking inside
    /// [`walkers::GalleyCache::galley_for_point`] took an exclusive lock on
    /// the whole context once per string per station for a number that is
    /// fixed for the pass.
    pub pixels_per_point: f32,
}

impl EguiPointPainter<'_> {
    fn pos(&self, offset: [f32; 2]) -> Pos2 {
        Pos2::new(self.center.x + offset[0], self.center.y + offset[1])
    }

    fn color(rgba: [u8; 4]) -> Color32 {
        Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
    }
}

impl PointPainter for EguiPointPainter<'_> {
    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4]) {
        if self.text_only {
            return;
        }
        self.painter
            .circle_filled(self.pos(offset), radius, Self::color(color));
    }

    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4], width: f32) {
        if self.text_only {
            return;
        }
        self.painter.circle_stroke(
            self.pos(offset),
            radius,
            Stroke::new(width, Self::color(color)),
        );
    }

    fn text(
        &mut self,
        offset: [f32; 2],
        text: &str,
        color: [u8; 4],
        size: f32,
        anchor: TextAnchor,
    ) {
        let align = match anchor {
            TextAnchor::TopLeft => egui::Align2::LEFT_TOP,
            TextAnchor::TopRight => egui::Align2::RIGHT_TOP,
            TextAnchor::BottomLeft => egui::Align2::LEFT_BOTTOM,
            TextAnchor::BottomRight => egui::Align2::RIGHT_BOTTOM,
            TextAnchor::CenterLeft => egui::Align2::LEFT_CENTER,
            TextAnchor::CenterRight => egui::Align2::RIGHT_CENTER,
            TextAnchor::Center => egui::Align2::CENTER_CENTER,
            TextAnchor::CenterTop => egui::Align2::CENTER_TOP,
            TextAnchor::CenterBottom => egui::Align2::CENTER_BOTTOM,
        };
        // `Painter::text` spelled out, with the layout answered from the memo:
        // it is `layout_no_wrap` (which allocates a `String` from `text` and
        // takes `Context::write`), then `Align2::anchor_size`, then `galley`.
        // The placement arithmetic below is that function's, unchanged.
        let color = Self::color(color);
        let galley = self.galleys.galley_for_point(
            self.painter.ctx(),
            text,
            FontId::proportional(size),
            color,
            self.pixels_per_point,
        );
        let rect = align.anchor_size(self.pos(offset), galley.size());
        match self.sink.as_deref_mut() {
            // The same shape `Painter::galley` adds, including its refusal of
            // an empty galley, so the collected text is the painted text.
            Some(sink) => {
                if !galley.is_empty() {
                    sink.push(Shape::galley(rect.min, galley, color));
                }
            }
            None => self.painter.galley(rect.min, galley, color),
        }
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4], width: f32) {
        if self.text_only {
            return;
        }
        self.painter.line_segment(
            [self.pos(from), self.pos(to)],
            Stroke::new(width, Self::color(color)),
        );
    }

    fn filled_polygon(&mut self, points: &[[f32; 2]], color: [u8; 4]) {
        if self.text_only {
            return;
        }
        if points.len() < 3 {
            return;
        }
        let vertices: Vec<Pos2> = points.iter().map(|p| self.pos(*p)).collect();
        self.painter.add(Shape::convex_polygon(
            vertices,
            Self::color(color),
            Stroke::NONE,
        ));
    }
}

/// Everything the point pass's text is a function of, in a form that is `Eq`.
///
/// A station model's text is decided by the layer's data (`generation`), the
/// zoom tier and font size (`zoom`), the theme (`dark`), the glyph raster
/// (`pixels_per_point` and `atlas_generation`) and where the projector puts
/// each station on the pane (`projector`, `rect`). Two projected reference
/// points pin a Mercator projector — scale and translation — without reaching
/// into its fields, and the rect is the culling window the points were walked
/// with.
///
/// **A kept mesh has the atlas coordinates baked in twice over**, which is why
/// the raster terms are here at all: the glyph UVs are normalized by the atlas
/// size at tessellation time, so a mesh outlives both a repack that moves the
/// glyphs and a growth that moves the divisor. `atlas_generation` covers both —
/// see [`walkers::GalleyCache::generation`], which is bumped by a size change
/// as well as by a fill that fell. It replaced a reading of the atlas taken
/// here for the reason [`crate::label_cache::LabelKey`] gives at length: a
/// reading is a level, and a level says nothing across the frames nobody read
/// it on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct PointTextKey {
    generation: u64,
    zoom: u32,
    dark: bool,
    pixels_per_point: u32,
    rect: [u32; 4],
    projector: [u32; 4],
    atlas_generation: u64,
}

impl PointTextKey {
    pub(crate) fn new(
        ctx: &egui::Context,
        galleys: &walkers::GalleyCache,
        projector: &walkers::Projector,
        rect: egui::Rect,
        generation: u64,
        zoom: f32,
        dark: bool,
    ) -> Self {
        let a = projector.project(walkers::lat_lon(0.0, 0.0));
        let b = projector.project(walkers::lat_lon(45.0, 90.0));
        Self {
            generation,
            zoom: zoom.to_bits(),
            dark,
            pixels_per_point: ctx.pixels_per_point().to_bits(),
            rect: [
                rect.min.x.to_bits(),
                rect.min.y.to_bits(),
                rect.max.x.to_bits(),
                rect.max.y.to_bits(),
            ],
            projector: [a.x.to_bits(), a.y.to_bits(), b.x.to_bits(), b.y.to_bits()],
            atlas_generation: galleys.generation(),
        }
    }
}

/// The point pass's text, tessellated once per [`PointTextKey`] and kept per
/// pane and layer.
///
/// **What it saves is the per-shape work, not the vertices.** The mesh is
/// re-added every frame — egui retains no geometry across frames, so the
/// vertex and index counts a frame stages do not fall. What no longer happens
/// per frame is a thousand galley lookups, a thousand `Painter::add`s under
/// the context lock and a thousand text tessellations; the tessellator copies
/// one mesh instead. A `None` entry is a key under which the layer drew no
/// text at all (every station culled, or nothing to say), kept so that case
/// is not re-walked either.
#[derive(Default)]
pub(crate) struct PointTextMeshes {
    entries: HashMap<(usize, LayerId), Kept>,
    builds: u64,
    hits: u64,
}

/// One pane-and-layer's kept text: the key it was built under and the mesh,
/// `None` where the layer drew no text under that key.
struct Kept {
    key: PointTextKey,
    mesh: Option<Arc<egui::Mesh>>,
    /// The same mesh with a painter opacity already multiplied in. Every
    /// gridded layer's default opacity is below 1.0
    /// (`squallar_source::product::REFLECTIVITY_DEFAULT_OPACITY`), so this is
    /// the shipped case here and not the slider case. See [`add_kept_mesh`].
    tinted: TintMemo,
}

impl PointTextMeshes {
    /// The kept mesh for this pane and layer, if it was built under `key`.
    pub(crate) fn lookup(
        &mut self,
        pane: usize,
        layer: &LayerId,
        key: PointTextKey,
    ) -> Option<Option<Arc<egui::Mesh>>> {
        let kept = self.entries.get(&(pane, layer.clone()))?;
        if kept.key != key {
            return None;
        }
        self.hits += 1;
        Some(kept.mesh.clone())
    }

    pub(crate) fn store(
        &mut self,
        pane: usize,
        layer: &LayerId,
        key: PointTextKey,
        mesh: Option<Arc<egui::Mesh>>,
    ) {
        self.builds += 1;
        self.entries.insert(
            (pane, layer.clone()),
            Kept {
                key,
                mesh,
                tinted: None,
            },
        );
    }

    /// Paint this pane-and-layer's kept mesh through `painter`, tinting once
    /// per opacity rather than once per frame; see [`add_kept_mesh`].
    ///
    /// The entry is looked up again only when there is a tint to keep in it.
    pub(crate) fn paint(
        &mut self,
        pane: usize,
        layer: &LayerId,
        painter: &egui::Painter,
        mesh: Option<Arc<egui::Mesh>>,
    ) {
        let Some(mesh) = mesh else {
            return;
        };
        let Some(kept) = tint_wanted(painter)
            .then(|| self.entries.get_mut(&(pane, layer.clone())))
            .flatten()
        else {
            painter.add(egui::Shape::Mesh(mesh));
            return;
        };
        add_kept_mesh(painter, mesh, &mut kept.tinted);
    }

    /// Meshes built — one per key the pass has seen.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> u64 {
        self.builds
    }

    /// Frames answered from a kept mesh.
    #[cfg(test)]
    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }
}

/// A kept mesh, and the opacity already multiplied into the copy beside it.
///
/// `u32` is `f32::to_bits` of the factor, compared by bits so a slider that
/// lands on the same number twice is the same tint and `-0.0` is not `0.0`.
pub(crate) type TintMemo = Option<(u32, Arc<egui::Mesh>)>;

/// Add a kept mesh to `painter`, paying the painter's opacity **once per
/// opacity** instead of once per frame.
///
/// **`Painter::add` cannot tint a kept mesh without copying all of it.** Its
/// one transform is `multiply_opacity`, which reaches a `Shape::Mesh` through
/// `epaint::shape_transform::adjust_colors` — and that arm opens with
/// `Arc::make_mut`. The `Arc` a memo hands over is held by the memo as well,
/// so `make_mut` is never the in-place case: it deep-clones every vertex and
/// every index the pane's text has, on every frame, before multiplying a
/// factor that did not move into colours that did not move.
///
/// Doing it here instead is exactly what egui would have done, and the reason
/// it is exact is that **opacity is the only transform a `Painter::add` can
/// apply to a mesh**: `Painter::fade_to_color` is assigned in one place in the
/// whole of egui (`Painter::set_invisible`) and the value is always
/// `Color32::TRANSPARENT`, which `add` answers with `Shape::Noop` before
/// `transform_shape` runs. So there is no second transform to commute with and
/// no order to get wrong.
///
/// The two ends egui handles itself are left to it: at `1.0` there is no tint
/// to make, and at `0.0` `add` emits `Shape::Noop`, which is cheaper than any
/// mesh and is what the layer walk's "a transparent layer still hit-tests"
/// behaviour is built on.
/// Whether [`add_kept_mesh`] would keep a tint for this painter.
///
/// Asked by the memos **before** they look their entry up, so a pane at full
/// opacity — which is what every layer draws at until a user moves its slider
/// — reaches `Painter::add` through exactly the calls it reached it through
/// before, and pays nothing for a tint it will not make.
pub(crate) fn tint_wanted(painter: &egui::Painter) -> bool {
    let opacity = painter.opacity();
    opacity > 0.0 && opacity < 1.0
}

pub(crate) fn add_kept_mesh(painter: &egui::Painter, mesh: Arc<egui::Mesh>, memo: &mut TintMemo) {
    let opacity = painter.opacity();
    if !tint_wanted(painter) {
        painter.add(egui::Shape::Mesh(mesh));
        return;
    }
    let tinted = tinted_mesh(memo, &mesh, opacity);
    // Set to 1.0 and not left alone: the factor is in the vertices now, and a
    // painter that still carried it would apply it twice.
    let mut painter = painter.clone();
    painter.set_opacity(1.0);
    painter.add(egui::Shape::Mesh(tinted));
}

/// `base` with `opacity` multiplied into every vertex colour, from `memo` when
/// it already holds that factor.
///
/// The body is `egui::painter::multiply_opacity`'s closure, including its
/// `Color32::PLACEHOLDER` guard — a galley painted with an overridden colour
/// tessellates to placeholder vertices, and egui leaves those for the renderer
/// rather than scaling them.
fn tinted_mesh(memo: &mut TintMemo, base: &Arc<egui::Mesh>, opacity: f32) -> Arc<egui::Mesh> {
    let bits = opacity.to_bits();
    if let Some((at, mesh)) = memo.as_ref()
        && *at == bits
    {
        return mesh.clone();
    }
    let mut mesh = (**base).clone();
    for v in &mut mesh.vertices {
        if v.color != egui::Color32::PLACEHOLDER {
            v.color = v.color.gamma_multiply(opacity);
        }
    }
    let mesh = Arc::new(mesh);
    *memo = Some((bits, mesh.clone()));
    mesh
}

/// One mesh from a pass's collected text shapes, tessellated exactly as egui
/// would tessellate them at the end of this frame.
///
/// The tessellator is built the way `Context::tessellate` builds its own —
/// the context's pixels-per-point, its tessellation options and the font
/// atlas size — so glyph placement, pixel rounding and UVs come out the
/// same; the test below holds it to that vertex for vertex. It is handed no
/// prepared discs because it is handed no circles. Its clip rect is left at
/// everything: egui's would skip a text row entirely outside the pane, and
/// this keeps such a row for the scissor to clip, which is the only way the
/// two outputs differ and only outside the pane.
pub(crate) fn tessellate_text_shapes(
    ctx: &egui::Context,
    shapes: Vec<Shape>,
) -> Option<Arc<egui::Mesh>> {
    tessellate_text_shapes_into(ctx, shapes, egui::Mesh::default())
}

/// [`tessellate_text_shapes`], filling buffers the caller already owns.
///
/// **A pane's place names are ~850 kB of vertices, and a pan re-solves them on
/// nearly half its frames.** Handed `Mesh::default()`, every one of those
/// solves asks the allocator for that buffer and gives it back a frame later;
/// handed the buffers of the solve it is replacing, it writes into memory it
/// already has. Measured on this box against a 430-name, 4,329-glyph pane
/// (the scratch bench, RTX 3090 box under load — an ARM figure, not a p99):
/// the whole miss-frame path runs at 261 us with a fresh mesh and 74 us with
/// the retired one, against a mesh *build* that is only 35 us either way.
///
/// `mesh` must be empty; [`crate::label_cache::LabelCache::recycle`] is what
/// empties it without dropping the buffers, which is what `Mesh::clear` would
/// do.
pub(crate) fn tessellate_text_shapes_into(
    ctx: &egui::Context,
    shapes: Vec<Shape>,
    mut mesh: egui::Mesh,
) -> Option<Arc<egui::Mesh>> {
    debug_assert!(mesh.is_empty(), "a recycled mesh must be emptied first");
    if shapes.is_empty() {
        return None;
    }
    let options = ctx.tessellation_options(|o| *o);
    let font_tex_size = ctx.fonts(|f| f.font_image_size());
    let mut tessellator =
        egui::epaint::Tessellator::new(ctx.pixels_per_point(), options, font_tex_size, Vec::new());
    for shape in shapes {
        tessellator.tessellate_shape(shape, &mut mesh);
    }
    (!mesh.is_empty()).then(|| Arc::new(mesh))
}

#[cfg(test)]
mod point_text_tests {
    use super::*;

    const SCREEN: egui::Vec2 = egui::vec2(800.0, 600.0);

    /// Run one pass with `paint` given the root painter; hand back what the
    /// pass emitted.
    fn shapes_of_one_pass(
        ctx: &egui::Context,
        galleys: &mut walkers::GalleyCache,
        paint: impl FnOnce(&egui::Painter, &mut walkers::GalleyCache),
    ) -> Vec<egui::epaint::ClippedShape> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            ..Default::default()
        };
        ctx.begin_pass(input);
        let painter = egui::Painter::new(
            ctx.clone(),
            egui::LayerId::background(),
            egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN),
        );
        paint(&painter, galleys);
        ctx.end_pass().shapes
    }

    fn draw_station_text(ep: &mut EguiPointPainter<'_>, temp: &str, dewp: &str) {
        ep.text(
            [-6.0, -6.0],
            temp,
            [255, 80, 80, 255],
            11.0,
            TextAnchor::BottomRight,
        );
        ep.text(
            [-6.0, 6.0],
            dewp,
            [80, 200, 80, 255],
            11.0,
            TextAnchor::TopRight,
        );
    }

    fn stations() -> Vec<(Pos2, &'static str, &'static str)> {
        (0..40)
            .map(|i| {
                let x = 40.0 + (i % 8) as f32 * 90.0;
                let y = 60.0 + (i / 8) as f32 * 100.0;
                (
                    egui::pos2(x, y),
                    ["72", "-5", "101", "8"][i % 4],
                    ["55", "-12", "9", "60"][i % 4],
                )
            })
            .collect()
    }

    fn paint_all(
        painter: &egui::Painter,
        galleys: &mut walkers::GalleyCache,
        sink: Option<&mut Vec<Shape>>,
    ) {
        let mut sink = sink;
        for (center, temp, dewp) in stations() {
            let mut ep = EguiPointPainter {
                painter,
                center,
                galleys,
                text_only: true,
                sink: sink.as_deref_mut(),
                pixels_per_point: painter.ctx().pixels_per_point(),
            };
            draw_station_text(&mut ep, temp, dewp);
        }
    }

    fn vertices(mesh: &egui::Mesh) -> Vec<(egui::Pos2, egui::Pos2, egui::Color32)> {
        mesh.vertices
            .iter()
            .map(|v| (v.pos, v.uv, v.color))
            .collect()
    }

    /// **The kept mesh is the painted text, vertex for vertex.** The direct
    /// path adds every label through `Painter::galley` and egui tessellates
    /// them at the end of the pass; the kept path collects the same labels
    /// and tessellates them once. In-pane, both must emit identical vertices
    /// — positions, UVs and colours — in identical order.
    #[test]
    fn a_collected_and_tessellated_layer_matches_what_egui_paints_directly() {
        let ctx = egui::Context::default();
        let mut galleys = walkers::GalleyCache::default();
        // Warm the font atlas with every glyph both passes use, so neither
        // pass grows it under the other (a growth changes normalised UVs and
        // would make the comparison a comparison of atlases).
        let _ = shapes_of_one_pass(&ctx, &mut galleys, |p, g| paint_all(p, g, None));

        // Direct: egui tessellates the pass's shapes.
        let direct_shapes = shapes_of_one_pass(&ctx, &mut galleys, |p, g| paint_all(p, g, None));
        let direct = ctx.tessellate(direct_shapes, ctx.pixels_per_point());
        let mut direct_mesh = egui::Mesh::default();
        for prim in direct {
            if let egui::epaint::Primitive::Mesh(m) = prim.primitive {
                direct_mesh.append(m);
            }
        }

        // Kept: the same labels collected, tessellated once.
        let mut collected = Vec::new();
        let _ = shapes_of_one_pass(&ctx, &mut galleys, |p, g| {
            paint_all(p, g, Some(&mut collected))
        });
        assert_eq!(collected.len(), 80, "the fixture collected no text");
        let kept = tessellate_text_shapes(&ctx, collected).expect("text tessellates to a mesh");

        assert!(!direct_mesh.is_empty(), "the direct pass painted nothing");
        assert_eq!(vertices(&kept), vertices(&direct_mesh));
        assert_eq!(kept.indices, direct_mesh.indices);
    }

    /// The mesh is built once per key and answered from the table while the
    /// key holds; a moved data generation is a new key.
    #[test]
    fn a_layer_is_tessellated_once_per_key_and_rebuilt_when_its_data_moves() {
        let ctx = egui::Context::default();
        let layer = LayerId::from_static("Fixture");
        let mut meshes = PointTextMeshes::default();
        ctx.begin_pass(Default::default());
        {
            let ctx = &ctx;
            let memory = walkers::MapMemory::default();
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
            let projector = walkers::Projector::new(rect, &memory, walkers::lat_lon(35.0, -97.0));
            let galleys = walkers::GalleyCache::default();
            let key = |generation| {
                PointTextKey::new(ctx, &galleys, &projector, rect, generation, 7.0, false)
            };

            assert!(
                meshes.lookup(0, &layer, key(1)).is_none(),
                "nothing kept yet"
            );
            meshes.store(0, &layer, key(1), None);
            assert_eq!(meshes.builds(), 1);
            for _ in 0..3 {
                assert!(meshes.lookup(0, &layer, key(1)).is_some());
            }
            assert_eq!(meshes.hits(), 3);
            assert!(
                meshes.lookup(0, &layer, key(2)).is_none(),
                "new data under the same view must rebuild"
            );
            assert!(
                meshes.lookup(1, &layer, key(1)).is_none(),
                "another pane's mesh is not this pane's"
            );
            assert_ne!(
                key(1),
                PointTextKey::new(ctx, &galleys, &projector, rect, 1, 7.0, true),
                "the theme is part of the key"
            );
            assert_ne!(
                key(1),
                PointTextKey::new(
                    ctx,
                    &galleys,
                    &projector,
                    rect.translate(egui::vec2(1.0, 0.0)),
                    1,
                    7.0,
                    false
                ),
                "the culling window is part of the key"
            );
        }
        let _ = ctx.end_pass();
    }
}

#[cfg(test)]
mod tests {
    /// **The frame thread suppresses a layer's geometry by RULE, not by name.**
    ///
    /// The rule is "a layer that rasterizes a picture has already drawn its
    /// shapes in the worker, so do not draw them again here". Spelling it as
    /// `job_codec().is_some()` means the next layer to gain a picture stops
    /// double-drawing the moment it does. Spelling it as "is this METAR" would
    /// be a line that silently rots into a double-draw — geometry painted
    /// twice, once invisibly, with the tessellator billed for both.
    ///
    /// Source-scanned rather than driven, because what is being pinned is the
    /// SHAPE of the condition; a behavioural test would pass just as happily
    /// on the hardcoded spelling this exists to forbid.
    ///
    /// **Two halves, because the question moved without the rule moving.** The
    /// codec used to be fetched by id per question — `overlays.job_codec(id)` —
    /// and the point pass now resolves the layer's handler ONCE at the top and
    /// asks that. So the rule is pinned in two pieces: the decision is still
    /// `job_codec`, and the thing it is asked of is still the registry's answer
    /// for this pane's layer id. Together those are strictly what one
    /// `overlays.job_codec(pf.id)` used to say, and neither half alone is.
    const PANE: &str = include_str!("ui_map_pane.rs");

    #[test]
    fn the_frame_thread_asks_the_registry_whether_a_layer_has_a_picture() {
        assert!(
            PANE.contains("let Some(handler) = pf.overlays.handler_by_id(pf.id) else {"),
            "the point pass no longer resolves its layer through the registry \
             by id, so whatever `text_only` reads below is not the registry's \
             answer about this layer",
        );
        assert!(
            PANE.contains("let text_only = handler.job_codec().is_some();"),
            "the point painter's `text_only` is no longer set from the \
             registry, so either the geometry suppression is gone or it is \
             hardcoded to one layer",
        );
        assert!(
            !PANE.contains("text_only = pf.id == squallar_source::id::known::METAR"),
            "`text_only` is decided by naming a layer; a second layer with a \
             picture would double-draw and nothing would say so",
        );
    }
}

/// **The tint a painter would have applied, applied once instead of once a
/// frame** — [`add_kept_mesh`]'s gates.
#[cfg(test)]
mod tint_tests {
    use super::*;

    const CANVAS: egui::Rect =
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 300.0));
    /// A layer's shipped opacity, not a round number: every gridded handler
    /// answers `default_opacity` with
    /// `squallar_source::product::REFLECTIVITY_DEFAULT_OPACITY`.
    const DIMMED: f32 = 0.63;

    /// A mesh shaped like the one the caches keep: quads with real colours,
    /// and one `Color32::PLACEHOLDER` vertex.
    ///
    /// The placeholder is the fixture's whole reason for being a fixture and
    /// not a single quad. `egui::painter::multiply_opacity` skips that colour
    /// and leaves it for the renderer to substitute; a tint written without
    /// that guard scales it, and every other vertex in the mesh would still
    /// agree.
    fn kept_mesh() -> Arc<egui::Mesh> {
        let colors = [
            egui::Color32::WHITE,
            egui::Color32::from_rgba_unmultiplied(200, 40, 40, 128),
            egui::Color32::PLACEHOLDER,
            egui::Color32::from_rgb(10, 90, 200),
        ];
        let mut mesh = egui::Mesh::default();
        for (i, color) in colors.iter().enumerate() {
            let x = 20.0 + i as f32 * 30.0;
            let base = mesh.vertices.len() as u32;
            for (dx, dy) in [(0.0, 0.0), (20.0, 0.0), (20.0, 20.0), (0.0, 20.0)] {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: egui::pos2(x + dx, 40.0 + dy),
                    uv: egui::pos2(dx / 64.0, dy / 64.0),
                    color: *color,
                });
            }
            mesh.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        assert!(mesh.is_valid(), "fixture: the mesh must be paintable");
        Arc::new(mesh)
    }

    /// One pass at `opacity`, tessellated, flattened to the one mesh it drew.
    fn painted(opacity: f32, draw: impl FnOnce(&egui::Painter)) -> egui::Mesh {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(CANVAS),
            ..Default::default()
        });
        let mut painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), CANVAS);
        painter.set_opacity(opacity);
        draw(&painter);
        let shapes = ctx.end_pass().shapes;
        let mut out = egui::Mesh::default();
        for prim in ctx.tessellate(shapes, ctx.pixels_per_point()) {
            if let egui::epaint::Primitive::Mesh(m) = prim.primitive {
                out.append(m);
            }
        }
        out
    }

    fn describe(m: &egui::Mesh) -> Vec<(egui::Pos2, egui::Pos2, egui::Color32)> {
        m.vertices.iter().map(|v| (v.pos, v.uv, v.color)).collect()
    }

    /// **The glass does not move.** What a dimmed painter puts on it through
    /// [`add_kept_mesh`] is what it put there when `Painter::add` did the
    /// tinting, vertex for vertex and index for index.
    ///
    /// Shown red three ways: by scaling the factor (`opacity * 0.99`), by
    /// dropping the `PLACEHOLDER` guard, and by leaving the painter's own
    /// opacity in place so the factor lands twice.
    #[test]
    fn a_dimmed_painter_draws_what_it_drew_before() {
        let mesh = kept_mesh();
        let by_egui = painted(DIMMED, |p| {
            p.add(egui::Shape::Mesh(mesh.clone()));
        });
        let by_memo = painted(DIMMED, |p| {
            add_kept_mesh(p, mesh.clone(), &mut None);
        });
        assert!(!by_egui.is_empty(), "fixture: egui's arm drew nothing");
        assert_eq!(describe(&by_memo), describe(&by_egui));
        assert_eq!(by_memo.indices, by_egui.indices);
    }

    /// The two ends are egui's, and stay egui's: at 1.0 there is nothing to
    /// multiply, and at 0.0 `Painter::add` files a `Shape::Noop`, which is
    /// what the layer walk's transparent-but-still-hit-testing behaviour
    /// stands on.
    #[test]
    fn full_and_zero_opacity_are_left_to_the_painter() {
        let mesh = kept_mesh();
        for opacity in [1.0, 0.0] {
            let by_egui = painted(opacity, |p| {
                p.add(egui::Shape::Mesh(mesh.clone()));
            });
            let mut memo = None;
            let by_memo = painted(opacity, |p| add_kept_mesh(p, mesh.clone(), &mut memo));
            assert_eq!(describe(&by_memo), describe(&by_egui), "at {opacity}");
            assert!(
                memo.is_none(),
                "at {opacity} nothing should have been tinted"
            );
        }
    }

    /// **The copy is made once, not once a frame** — which is the whole cut.
    ///
    /// Asserted on the identity of the `Arc` the paint list received, not on a
    /// counter: two frames that hand the painter the same allocation are two
    /// frames that did not clone the mesh between them. `Painter::add` on the
    /// kept mesh would have handed over a different one every time, because
    /// `Arc::make_mut` clones whenever the memo is still holding it.
    #[test]
    fn a_second_frame_at_the_same_opacity_reuses_the_tinted_mesh() {
        let mesh = kept_mesh();
        let mut memo = None;
        let mut seen = Vec::new();
        for _ in 0..3 {
            let ctx = egui::Context::default();
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(CANVAS),
                ..Default::default()
            });
            let mut painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), CANVAS);
            painter.set_opacity(DIMMED);
            add_kept_mesh(&painter, mesh.clone(), &mut memo);
            let shapes = ctx.end_pass().shapes;
            let egui::Shape::Mesh(m) = &shapes[0].shape else {
                panic!("a kept mesh must be added as a mesh");
            };
            seen.push(Arc::as_ptr(m));
            let _ = ctx.tessellate(shapes, ctx.pixels_per_point());
        }
        assert!(
            seen.windows(2).all(|w| w[0] == w[1]),
            "every frame tinted the mesh again: {seen:?}"
        );
    }

    /// A moved slider is a new tint, and the old one is not served under it.
    #[test]
    fn a_changed_opacity_retints() {
        let mesh = kept_mesh();
        let mut memo = None;
        let dim = painted(DIMMED, |p| add_kept_mesh(p, mesh.clone(), &mut memo));
        let dimmer = painted(0.25, |p| add_kept_mesh(p, mesh.clone(), &mut memo));
        assert_ne!(describe(&dim), describe(&dimmer));
        assert_eq!(
            describe(&dimmer),
            describe(&painted(0.25, |p| {
                p.add(egui::Shape::Mesh(mesh.clone()));
            })),
            "the retint must be what the painter would have made"
        );
    }
}
