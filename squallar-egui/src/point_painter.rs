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
/// (`pixels_per_point` and the font atlas) and where the projector puts each
/// station on the pane (`projector`, `rect`). Two projected reference points
/// pin a Mercator projector — scale and translation — without reaching into
/// its fields, and the rect is the culling window the points were walked with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct PointTextKey {
    generation: u64,
    zoom: u32,
    dark: bool,
    pixels_per_point: u32,
    rect: [u32; 4],
    projector: [u32; 4],
    atlas_size: [usize; 2],
    atlas_fill: u32,
}

impl PointTextKey {
    pub(crate) fn new(
        ctx: &egui::Context,
        projector: &walkers::Projector,
        rect: egui::Rect,
        generation: u64,
        zoom: f32,
        dark: bool,
    ) -> Self {
        let a = projector.project(walkers::lat_lon(0.0, 0.0));
        let b = projector.project(walkers::lat_lon(45.0, 90.0));
        let atlas = walkers::AtlasStamp::read(ctx);
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
            atlas_size: atlas.size,
            atlas_fill: atlas.fill.to_bits(),
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
        self.entries
            .insert((pane, layer.clone()), Kept { key, mesh });
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
    if shapes.is_empty() {
        return None;
    }
    let options = ctx.tessellation_options(|o| *o);
    let font_tex_size = ctx.fonts(|f| f.font_image_size());
    let mut tessellator =
        egui::epaint::Tessellator::new(ctx.pixels_per_point(), options, font_tex_size, Vec::new());
    let mut mesh = egui::Mesh::default();
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
            let key = |generation| PointTextKey::new(ctx, &projector, rect, generation, 7.0, false);

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
                PointTextKey::new(ctx, &projector, rect, 1, 7.0, true),
                "the theme is part of the key"
            );
            assert_ne!(
                key(1),
                PointTextKey::new(
                    ctx,
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
    /// `job_codec(id).is_some()` means the next layer to gain a picture stops
    /// double-drawing the moment it does. Spelling it as "is this METAR" would
    /// be a line that silently rots into a double-draw — geometry painted
    /// twice, once invisibly, with the tessellator billed for both.
    ///
    /// Source-scanned rather than driven, because what is being pinned is the
    /// SHAPE of the condition; a behavioural test would pass just as happily
    /// on the hardcoded spelling this exists to forbid.
    const PANE: &str = include_str!("ui_map_pane.rs");

    #[test]
    fn the_frame_thread_asks_the_registry_whether_a_layer_has_a_picture() {
        assert!(
            PANE.contains("let text_only = pf.overlays.job_codec(pf.id).is_some();"),
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
