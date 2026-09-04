use egui::{Color32, Pos2, Rect, Vec2, vec2};

#[derive(Debug, Clone)]
pub struct Text {
    pub text: String,
    pub position: Pos2,
    pub font_size: f32,
    pub text_color: Color32,
    pub angle: f32,
    /// Wrap width in ems of [`font_size`](Self::font_size), from
    /// `text-max-width`.
    ///
    /// Ems rather than points because that is the unit MapLibre defines it in,
    /// and because the conversion needs the font the text layer will actually
    /// use. `None` means "do not wrap".
    pub max_width_ems: Option<f32>,
    /// Row height in ems, from `text-line-height`. `None` leaves the text
    /// layer's own row height alone.
    pub line_height_ems: Option<f32>,
}

impl Text {
    pub fn new(
        position: Pos2,
        text: String,
        font_size: f32,
        text_color: Color32,
        angle: f32,
    ) -> Self {
        Self {
            position,
            text,
            font_size,
            text_color,
            angle,
            max_width_ems: None,
            line_height_ems: None,
        }
    }

    /// This text wrapped at `max_width_ems`, with rows `line_height_ems` apart.
    #[must_use]
    pub fn with_wrapping(
        mut self,
        max_width_ems: Option<f32>,
        line_height_ems: Option<f32>,
    ) -> Self {
        self.max_width_ems = max_width_ems;
        self.line_height_ems = line_height_ems;
        self
    }

    /// This label laid out: wrapped at `text-max-width`, rows spaced by
    /// `text-line-height`, and centred on its anchor.
    ///
    /// **The wrap width becomes points here and nowhere earlier.**
    /// `text-max-width` is defined in ems of the label's own size, and an em is
    /// only a number of points once the font that will lay the text out is
    /// known -- which is here and not in [`crate::mvt`].
    ///
    /// `halign` is [`egui::Align::Center`], matching MapLibre's default
    /// `text-justify`. The consequence a caller must handle is that the galley
    /// is measured about its own centre line, so `galley.rect.min.x` is
    /// negative; [`Self::shape`] takes the block's top-left corner and undoes
    /// that, and callers should place through it rather than passing a galley
    /// origin straight to [`egui::epaint::TextShape`].
    pub fn galley(&self, ctx: &egui::Context) -> std::sync::Arc<egui::Galley> {
        let mut job = egui::text::LayoutJob {
            halign: egui::Align::Center,
            ..Default::default()
        };

        if let Some(ems) = self.max_width_ems {
            job.wrap.max_width = ems * self.font_size;
        }

        job.append(
            &self.text,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(self.font_size),
                color: self.text_color,
                line_height: self.line_height_ems.map(|ems| ems * self.font_size),
                ..Default::default()
            },
        );

        ctx.fonts_mut(|fonts| fonts.layout_job(job))
    }

    /// [`Self::galley`], answered from `cache` when this exact label has
    /// already been laid out.
    ///
    /// **The saving is the lookup, not the shaping.** `Fonts::layout_job`
    /// already memoizes galleys by job hash, so the shaping was never repeated
    /// — what was repeated is everything needed to *reach* that memo:
    /// a `String` copy of the label into a fresh `LayoutJob`, the hash of that
    /// job, and `Context::fonts_mut`, which is `Context::write` and therefore
    /// an exclusive lock on the whole context. A basemap frame takes that lock
    /// once per label; this takes it once per label whose text or style is new.
    ///
    /// The key is every field [`Self::galley`] reads and no others. `position`
    /// and `angle` are deliberately absent: a galley is laid out about its own
    /// origin and placed by [`Self::shape`], so panning the map re-uses every
    /// entry rather than invalidating it — which is the case this exists for.
    pub fn galley_cached(
        &self,
        ctx: &egui::Context,
        cache: &mut GalleyCache,
    ) -> std::sync::Arc<egui::Galley> {
        cache.settle(ctx.pixels_per_point());

        let key = GalleyKey {
            text: self.text.clone(),
            font_size: self.font_size.to_bits(),
            text_color: self.text_color,
            max_width_ems: self.max_width_ems.map(f32::to_bits),
            line_height_ems: self.line_height_ems.map(f32::to_bits),
        };

        if let Some(hit) = cache.entries.get(&key) {
            cache.hits += 1;
            return hit.clone();
        }

        let galley = self.galley(ctx);
        cache.layouts += 1;
        cache.entries.insert(key, galley.clone());
        galley
    }

    /// The shape drawing `galley` with its block's top-left corner at
    /// `top_left`.
    ///
    /// **No halo, and that is a decision made on the glass rather than a gap.**
    /// Two approximations have now been tried and both looked worse than plain
    /// glyphs. `egui::TextFormat::background` fills the galley's bounding
    /// rectangle, so a style's `text-halo-color` came out as a translucent slab
    /// behind the whole label. Redrawing the glyphs at eight offsets around a
    /// circle -- the standard approximation short of an atlas -- reads as fuzzy
    /// and uneven, because each offset copy is alpha-blended anti-aliased text
    /// and the coverage stacks differently around different letter edges.
    ///
    /// A real halo is an SDF or a blurred mask over a glyph atlas, which this
    /// crate does not build. Until there is one, the honest option is the one
    /// that looks best: draw the text.
    ///
    /// The style properties are still parsed (`Paint::text_halo_color`,
    /// `Paint::text_halo_width`) because [`crate::style`] models the MapLibre
    /// spec rather than this renderer's subset; they simply reach nothing.
    pub fn shape(&self, galley: std::sync::Arc<egui::Galley>, top_left: Pos2) -> egui::Shape {
        // Rows carry negative offsets under `Align::Center`, so the galley's
        // own origin is not its top-left corner. Subtracting it puts the block
        // exactly where the collision box claimed it.
        let origin = top_left - galley.rect.min.to_vec2();

        egui::epaint::TextShape::new(origin, galley, self.text_color)
            .with_angle(self.angle)
            .into()
    }
}

pub struct OrientedRect {
    corners: [Pos2; 4],
    bbox: Rect,
}

impl OrientedRect {
    pub fn new(center: Pos2, angle: f32, size: Vec2) -> Self {
        let (s, c) = angle.sin_cos();
        let half = size * 0.5;

        let ux = vec2(half.x * c, half.x * s);
        let uy = vec2(-half.y * s, half.y * c);

        let corners = [
            center - ux - uy, // top-left
            center + ux - uy, // top-right
            center + ux + uy, // bottom-right
            center - ux + uy, // bottom-left
        ];

        Self {
            corners,
            bbox: Rect::from_points(&corners),
        }
    }

    pub fn top_left(&self) -> Pos2 {
        self.corners[0]
    }

    pub fn intersects(&self, other: &OrientedRect) -> bool {
        // Checking bbox first gives huge performance boost.
        self.bbox.intersects(other.bbox) && !separated(&self.corners, &other.corners)
    }
}

/// The separating-axis test: two convex polygons are disjoint exactly when some
/// axis perpendicular to an edge of one of them separates their projections.
/// Opposite edges of a rectangle are parallel, so two of each rectangle's four
/// edge normals are redundant and four candidate axes decide a pair.
///
/// Projections that merely touch are *not* a separation. That is deliberate: it
/// is what the bounding-box test above reports for touching boxes, so an
/// axis-aligned pair gets the same answer from both.
fn separated(a: &[Pos2; 4], b: &[Pos2; 4]) -> bool {
    let axes = [a[1] - a[0], a[3] - a[0], b[1] - b[0], b[3] - b[0]];

    axes.into_iter().any(|edge| {
        let axis = edge.rot90();
        let (a_min, a_max) = project(a, axis);
        let (b_min, b_max) = project(b, axis);
        a_max < b_min || b_max < a_min
    })
}

/// Extent of a rectangle's corners along `axis`, in units of `axis`'s own
/// length.
///
/// The axis is deliberately left un-normalised. A degenerate rectangle -- zero
/// width or zero height, which a zero-size galley produces -- contributes a
/// zero-length edge, and normalising it would divide by zero. Un-normalised it
/// collapses every projection to the same value, so that axis reports "not
/// separating" and carries no weight, which is the correct answer rather than a
/// special case.
fn project(rect: &[Pos2; 4], axis: Vec2) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;

    for corner in rect {
        let d = corner.to_vec2().dot(axis);
        min = min.min(d);
        max = max.max(d);
    }

    (min, max)
}

// Tracks areas occupied by texts to avoid overlapping them.
pub struct OccupiedAreas {
    areas: Vec<OrientedRect>,
}

impl OccupiedAreas {
    pub fn new() -> Self {
        Self { areas: Vec::new() }
    }

    pub fn try_occupy(&mut self, rect: OrientedRect) -> bool {
        if !self.areas.iter().any(|existing| existing.intersects(&rect)) {
            self.areas.push(rect);
            true
        } else {
            false
        }
    }
}

/// A memo of laid-out galleys for [`Text::galley_cached`].
///
/// **Owned by the caller, never a thread-local or a process-wide pool**, so
/// what it retains is bounded by the caller's own lifetime and a test can hold
/// two independent ones. Empty is always correct: every entry is reproducible
/// from the [`Text`] that made it.
///
/// Two things invalidate an entry and both are handled here rather than by the
/// caller. A change of `pixels_per_point` re-rasterizes every glyph, so the
/// whole table is dropped when it moves. And the table is dropped when it
/// exceeds [`Self::MAX_ENTRIES`], because a map being panned across a country
/// retires label text continuously and a memo with no ceiling would hold every
/// name the session had ever drawn.
///
/// **The drop is bounded by the working set, not by the ceiling.** Only the
/// labels a frame actually draws are looked up, so the frame after a drop lays
/// out that frame's labels and no others — the other entries were off-screen
/// and are simply never asked for. Measured on native scene A at 1920x1080,
/// a pane hands `paint_labels` 604 names per frame and 534 of them reach a
/// layout, so a drop costs one frame at the cost this cache exists to remove
/// and the table is rebuilt to the working set on that same frame. That is a
/// degradation, not a cliff: it is never worse than having no cache at all,
/// and it self-heals immediately rather than persisting. A ceiling that
/// stopped *inserting* instead of dropping would avoid the frame and pay for
/// it for ever, which is the worse trade.
///
/// It does **not** watch the font definitions. A galley laid out under one
/// `FontDefinitions` is wrong under another, and nothing here would notice;
/// the caller must drop the cache if it ever installs fonts after startup.
#[derive(Default)]
pub struct GalleyCache {
    pixels_per_point: f32,
    /// The font atlas as it stood when the table was last checked against it.
    /// See [`Self::begin_frame`].
    atlas: AtlasStamp,
    entries: std::collections::HashMap<GalleyKey, std::sync::Arc<egui::Galley>>,
    /// The point-label table, keyed in two levels so a lookup can borrow.
    ///
    /// **Two levels because the hot path holds a `&str`, not a `String`.**
    /// `EguiPointPainter` draws a station model from text it already owns, and
    /// a single-level map keyed by a struct containing `String` cannot be
    /// probed without building that `String` first — which is the very
    /// allocation `Painter::text`'s `to_string()` was making. Splitting the
    /// style out leaves an inner map keyed by `Box<str>`, and `Box<str>:
    /// Borrow<str>`, so a hit costs a hash of the text and no allocation.
    points: std::collections::HashMap<
        PointStyle,
        std::collections::HashMap<Box<str>, std::sync::Arc<egui::Galley>>,
    >,
    /// Entries across every inner map of `points`, kept as a running count so
    /// the ceiling check stays O(1).
    points_len: usize,
    layouts: u64,
    hits: u64,
}

impl GalleyCache {
    /// The entry ceiling past which the table is dropped whole.
    pub const MAX_ENTRIES: usize = 4096;

    /// The galley `egui::Painter::text` would lay out, answered from the memo.
    ///
    /// **Borrows the text rather than owning it, and that is the whole point.**
    /// `Painter::text` takes `impl ToString` and calls `to_string()` on every
    /// call, so a station model drawing four numbers allocated four `String`s
    /// per station per frame before any lock was taken. This probes the table
    /// with the `&str` the caller already holds and allocates only on a miss.
    ///
    /// Unwrapped, matching `Painter::layout_no_wrap`: the caller places the
    /// result with `Align2::anchor_size` exactly as `Painter::text` does, so
    /// the drawn output is the same galley at the same origin.
    pub fn galley_for_point(
        &mut self,
        ctx: &egui::Context,
        text: &str,
        font: egui::FontId,
        color: Color32,
    ) -> std::sync::Arc<egui::Galley> {
        self.settle(ctx.pixels_per_point());
        let style = PointStyle {
            font_size: font.size.to_bits(),
            family: font.family.clone(),
            color,
        };
        if let Some(hit) = self.points.get(&style).and_then(|inner| inner.get(text)) {
            let hit = hit.clone();
            self.hits += 1;
            return hit;
        }
        let galley = ctx.fonts_mut(|f| f.layout(text.to_owned(), font, color, f32::INFINITY));
        self.layouts += 1;
        if self
            .points
            .entry(style)
            .or_default()
            .insert(text.into(), galley.clone())
            .is_none()
        {
            self.points_len += 1;
        }
        galley
    }

    /// Galleys this cache has had to lay out — the figure a memo is judged by.
    pub fn layouts(&self) -> u64 {
        self.layouts
    }

    /// Lookups answered without laying anything out.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// How many galleys are held.
    pub fn len(&self) -> usize {
        self.entries.len() + self.points_len
    }

    /// Whether the table holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop every entry, keeping the counters.
    pub fn clear(&mut self) {
        self.drop_all();
    }

    /// Bring the table to `pixels_per_point`, dropping it if that moved or if
    /// it has outgrown [`Self::MAX_ENTRIES`].
    fn settle(&mut self, pixels_per_point: f32) {
        if self.pixels_per_point != pixels_per_point {
            self.pixels_per_point = pixels_per_point;
            self.drop_all();
        }
        if self.len() >= Self::MAX_ENTRIES {
            self.drop_all();
        }
    }

    /// Check the table against egui's font atlas, once per frame.
    ///
    /// **A kept galley points into the atlas by pixel position, and egui
    /// rebuilds the atlas.** `Fonts::begin_pass` replaces the whole atlas when
    /// it is over 80 % full or the text options change; every glyph is then
    /// re-rasterized wherever the new cursor puts it, and a galley laid out
    /// against the old atlas draws the wrong pixels. egui's own galley cache is
    /// replaced in the same breath, which is why egui never notices; this one
    /// outlives the pass on purpose and has to look. Grown atlases are dropped
    /// too — over-invalidation a handful of times as the atlas doubles at
    /// startup, and nothing after.
    ///
    /// Once per frame per caller and not per lookup, because
    /// `Context::fonts` is a write lock on the whole context.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        let stamp = AtlasStamp::read(ctx);
        if stamp.invalidates(self.atlas) {
            self.drop_all();
        }
        self.atlas = stamp;
    }

    fn drop_all(&mut self) {
        self.entries.clear();
        self.points.clear();
        self.points_len = 0;
    }
}

/// The font atlas as seen from outside egui: its size and how full it is.
///
/// egui exposes no atlas generation. What it does expose moves in a usable
/// way: between rebuilds the atlas only ever allocates, so its fill ratio is
/// non-decreasing and its size only grows; a rebuild starts a fresh atlas
/// whose fill is what this pass alone has placed. A stamp whose fill fell or
/// whose size changed therefore means glyphs may now sit at other positions.
/// The one case this cannot see — a rebuild that re-placed the identical glyph
/// set in the identical order — puts every glyph back where it was, because
/// allocation is cursor-driven and deterministic, so a mesh or galley kept
/// across it is still right.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct AtlasStamp {
    pub size: [usize; 2],
    pub fill: f32,
}

impl AtlasStamp {
    /// One `Context::fonts` read — a write lock on the context; call it once
    /// per frame, not per lookup.
    pub fn read(ctx: &egui::Context) -> Self {
        ctx.fonts(|f| Self {
            size: f.font_image_size(),
            fill: f.font_atlas_fill_ratio(),
        })
    }

    /// Whether glyph positions recorded under `earlier` may no longer hold.
    pub fn invalidates(self, earlier: Self) -> bool {
        self.size != earlier.size || self.fill < earlier.fill
    }
}

/// The style half of a point label's key — everything but the text itself.
///
/// Split from the text so the text can be probed as a borrowed `&str`; see
/// [`GalleyCache::points`]. `FontId` is **not** `Eq` — it carries the size as a
/// bare `f32` — so it is taken apart here and the size keyed by its bits, for
/// the same reason and with the same "stricter is the safe direction" argument
/// as [`GalleyKey`]: a spurious miss costs one layout, a spurious hit draws the
/// wrong text.
#[derive(PartialEq, Eq, Hash, Clone)]
struct PointStyle {
    font_size: u32,
    family: egui::FontFamily,
    color: Color32,
}

/// Every field [`Text::galley`] reads, in a form that is `Hash` and `Eq`.
///
/// The three `f32`s are keyed by their bits rather than by value because `f32`
/// is not `Eq`. That is stricter than equality — `-0.0` and `0.0` are two keys
/// — and stricter is the safe direction for a memo: a spurious miss costs one
/// layout, a spurious hit draws the wrong text.
#[derive(PartialEq, Eq, Hash)]
struct GalleyKey {
    text: String,
    font_size: u32,
    text_color: Color32,
    max_width_ems: Option<u32>,
    line_height_ems: Option<u32>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_fallen_atlas_fill_drops_kept_galleys_and_a_risen_one_keeps_them() {
        let ctx = egui::Context::default();
        let mut cache = super::GalleyCache::default();
        ctx.begin_pass(Default::default());
        {
            let ctx = &ctx;
            cache.begin_frame(ctx);
            let _ = cache.galley_for_point(
                ctx,
                "72",
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
            assert_eq!(cache.len(), 1);
            // The same atlas, or one that only allocated more: kept.
            cache.begin_frame(ctx);
            assert_eq!(cache.len(), 1);
            let mut fuller = cache.atlas;
            fuller.fill += 0.1;
            assert!(!fuller.invalidates(cache.atlas));
            // A fresh atlas reads emptier than the one the galley was laid out
            // against, and everything laid out against the old one goes.
            let mut rebuilt = cache.atlas;
            rebuilt.fill = 0.0;
            assert!(rebuilt.invalidates(cache.atlas));
            cache.atlas = super::AtlasStamp {
                size: cache.atlas.size,
                fill: 1.0,
            };
            cache.begin_frame(ctx);
            assert_eq!(cache.len(), 0, "a fallen fill must drop the table");
        }
        let _ = ctx.end_pass();
    }

    use super::*;
    use egui::pos2;
    use std::f32::consts::FRAC_PI_4;

    /// A context with fonts available: `Fonts` exists only after a pass has
    /// begun, and `galley` panics without it.
    fn ctx_with_fonts() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                vec2(800.0, 600.0),
            )),
            ..Default::default()
        });
        ctx
    }

    fn label(text: &str) -> Text {
        Text {
            text: text.to_owned(),
            position: pos2(10.0, 20.0),
            font_size: 14.0,
            text_color: Color32::WHITE,
            angle: 0.0,
            max_width_ems: None,
            line_height_ems: None,
        }
    }

    /// The identity that makes the memo safe: a cached galley is the galley.
    ///
    /// Not "the same size" — the same glyphs, rows and metrics, asserted field
    /// by field against one laid out the uncached way on the same context.
    #[test]
    fn a_cached_galley_is_identical_to_a_freshly_laid_out_one() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();

        for name in ["Washita River", "Oklahoma City", "Lake Thunderbird"] {
            let text = label(name);
            let fresh = text.galley(&ctx);
            let cached = text.galley_cached(&ctx, &mut cache);
            // Second time through is the one that comes off the table.
            let hit = text.galley_cached(&ctx, &mut cache);

            assert_eq!(fresh.text(), cached.text());
            assert_eq!(fresh.text(), hit.text());
            assert_eq!(fresh.size(), cached.size());
            assert_eq!(fresh.size(), hit.size());
            assert_eq!(fresh.rows.len(), cached.rows.len());
            assert_eq!(fresh.rows.len(), hit.rows.len());
            assert_eq!(fresh.rect, cached.rect);
            assert_eq!(fresh.rect, hit.rect);
        }
    }

    /// **The count gate.** A second pass over labels nothing has changed lays
    /// out zero galleys.
    ///
    /// This is the figure the change exists to move, and it is a count rather
    /// than a clock. The baseline semantics — no memo at all — are spelled in
    /// the second half: a cache dropped between passes lays the same labels out
    /// again, which is what this asserts must NOT happen when it is kept.
    #[test]
    fn an_unchanged_second_pass_lays_out_nothing() {
        let ctx = ctx_with_fonts();
        let names = ["Washita River", "Oklahoma City", "Lake Thunderbird"];

        let mut kept = GalleyCache::default();
        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut kept);
        }
        let after_first = kept.layouts();
        assert_eq!(after_first, names.len() as u64);

        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut kept);
        }
        assert_eq!(
            kept.layouts(),
            after_first,
            "an unchanged second pass laid out {} more galleys",
            kept.layouts() - after_first,
        );
        assert_eq!(kept.hits(), names.len() as u64);

        // Baseline semantics, for contrast: without the memo surviving the
        // pass, the same three labels are laid out all over again.
        let mut dropped = GalleyCache::default();
        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut dropped);
        }
        dropped.clear();
        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut dropped);
        }
        assert_eq!(dropped.layouts(), 2 * names.len() as u64);
    }

    /// Everything the galley depends on invalidates it, one field at a time.
    ///
    /// A memo that answers a stale galley draws the wrong text, so each of
    /// these must MISS. Written as one test per field rather than one blanket
    /// assertion so a failure names the field that stopped being keyed.
    #[test]
    fn every_field_the_layout_reads_is_keyed() {
        let ctx = ctx_with_fonts();
        let base = label("Washita River");

        let variants: Vec<(&str, Text)> = vec![
            (
                "text",
                Text {
                    text: "Canadian River".to_owned(),
                    ..base.clone()
                },
            ),
            (
                "font_size",
                Text {
                    font_size: 18.0,
                    ..base.clone()
                },
            ),
            (
                "text_color",
                Text {
                    text_color: Color32::RED,
                    ..base.clone()
                },
            ),
            (
                "max_width_ems",
                Text {
                    max_width_ems: Some(6.0),
                    ..base.clone()
                },
            ),
            (
                "line_height_ems",
                Text {
                    line_height_ems: Some(1.5),
                    ..base.clone()
                },
            ),
        ];

        for (field, variant) in variants {
            let mut cache = GalleyCache::default();
            let _ = base.galley_cached(&ctx, &mut cache);
            assert_eq!(cache.layouts(), 1, "{field}: setup");
            let _ = variant.galley_cached(&ctx, &mut cache);
            assert_eq!(
                cache.layouts(),
                2,
                "{field} changed and the memo answered the old galley",
            );
        }
    }

    /// `position` and `angle` are deliberately NOT keyed: a galley is laid out
    /// about its own origin, so panning the map must re-use every entry. This
    /// is the property the whole memo rests on.
    #[test]
    fn moving_a_label_does_not_lay_it_out_again() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();
        let base = label("Washita River");
        let _ = base.galley_cached(&ctx, &mut cache);

        for (x, y) in [(11.0, 20.0), (400.0, 300.0), (-50.0, 900.0)] {
            let moved = Text {
                position: pos2(x, y),
                angle: FRAC_PI_4,
                ..base.clone()
            };
            let _ = moved.galley_cached(&ctx, &mut cache);
        }
        assert_eq!(cache.layouts(), 1, "a moved label was laid out again");
        assert_eq!(cache.hits(), 3);
    }

    /// A `pixels_per_point` change re-rasterizes every glyph, so the table goes
    /// with it rather than answering galleys built for the old scale.
    #[test]
    fn a_pixels_per_point_change_drops_the_table() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();
        let text = label("Washita River");

        let _ = text.galley_cached(&ctx, &mut cache);
        assert_eq!(cache.layouts(), 1);
        assert_eq!(cache.len(), 1);

        // Through the viewport, which is how a real display change arrives.
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&input.viewport_id)
            .unwrap()
            .native_pixels_per_point = Some(2.0);
        ctx.begin_pass(input);
        assert_eq!(ctx.pixels_per_point(), 2.0, "the test did not move ppp");

        let _ = text.galley_cached(&ctx, &mut cache);
        assert_eq!(
            cache.layouts(),
            2,
            "the table survived a pixels_per_point change",
        );
    }

    /// **The identity for the point path: the memo returns the galley
    /// `Painter::text` would have laid out.**
    ///
    /// `Painter::text` calls `layout_no_wrap`, which is
    /// `fonts.layout(text, font, color, f32::INFINITY)`. This asserts the
    /// cached answer matches that call field for field, including on the
    /// second lookup — the one that actually comes off the table.
    #[test]
    fn a_cached_point_galley_is_what_painter_text_would_have_laid_out() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();
        let font = egui::FontId::proportional(11.0);

        for body in ["24", "-3", "1013.2", "KTLX"] {
            let direct = ctx.fonts_mut(|f| {
                f.layout(body.to_owned(), font.clone(), Color32::WHITE, f32::INFINITY)
            });
            let first = cache.galley_for_point(&ctx, body, font.clone(), Color32::WHITE);
            let second = cache.galley_for_point(&ctx, body, font.clone(), Color32::WHITE);

            assert_eq!(direct.text(), first.text());
            assert_eq!(direct.text(), second.text());
            assert_eq!(direct.size(), first.size());
            assert_eq!(direct.size(), second.size());
            assert_eq!(direct.rect, first.rect);
            assert_eq!(direct.rect, second.rect);
            assert_eq!(direct.rows.len(), second.rows.len());
        }
    }

    /// The count gate for the point path: a station whose reading has not
    /// changed is laid out once, not once per frame.
    #[test]
    fn an_unchanged_station_reading_lays_out_once() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();
        let font = egui::FontId::proportional(11.0);
        let readings = ["24", "-3", "1013.2", "KTLX"];

        for _frame in 0..5 {
            for body in readings {
                let _ = cache.galley_for_point(&ctx, body, font.clone(), Color32::WHITE);
            }
        }
        assert_eq!(cache.layouts(), readings.len() as u64);
        assert_eq!(cache.hits(), 4 * readings.len() as u64);
    }

    /// Style is keyed as well as text, so the same reading at two sizes or two
    /// colours is two galleys rather than one wrong one.
    #[test]
    fn a_point_galleys_style_is_keyed_too() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();
        let base = egui::FontId::proportional(11.0);

        let _ = cache.galley_for_point(&ctx, "24", base.clone(), Color32::WHITE);
        assert_eq!(cache.layouts(), 1);
        let _ =
            cache.galley_for_point(&ctx, "24", egui::FontId::proportional(14.0), Color32::WHITE);
        assert_eq!(cache.layouts(), 2, "font size was not keyed");
        let _ = cache.galley_for_point(&ctx, "24", base.clone(), Color32::RED);
        assert_eq!(cache.layouts(), 3, "colour was not keyed");
        let _ = cache.galley_for_point(&ctx, "25", base.clone(), Color32::WHITE);
        assert_eq!(cache.layouts(), 4, "text was not keyed");

        // **Without this the test is vacuous.** The four assertions above all
        // count misses, and a memo that cached nothing at all would satisfy
        // every one of them. Re-asking for the first key proves the table is
        // actually answering, so "these are four keys" and "nothing is stored"
        // stop being indistinguishable.
        let _ = cache.galley_for_point(&ctx, "24", base, Color32::WHITE);
        assert_eq!(
            cache.layouts(),
            4,
            "a repeat of a keyed style laid out again"
        );
        assert_eq!(cache.hits(), 1);
    }

    /// Both tables answer to one ceiling, and a drop takes both — the label
    /// memo and the point memo share a `Gui` and must share a bound.
    #[test]
    fn the_ceiling_spans_both_tables() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();
        let font = egui::FontId::proportional(11.0);
        let _ = label("Washita River").galley_cached(&ctx, &mut cache);
        for i in 0..GalleyCache::MAX_ENTRIES {
            let _ = cache.galley_for_point(&ctx, &format!("r{i}"), font.clone(), Color32::WHITE);
        }
        assert!(cache.len() <= GalleyCache::MAX_ENTRIES);
    }

    /// The table is bounded: a session panning across a country retires label
    /// text continuously, and a memo with no ceiling would hold all of it.
    #[test]
    fn the_table_is_dropped_once_it_outgrows_its_ceiling() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();
        for i in 0..=GalleyCache::MAX_ENTRIES {
            let _ = label(&format!("name {i}")).galley_cached(&ctx, &mut cache);
        }
        assert!(cache.len() <= GalleyCache::MAX_ENTRIES);
    }

    fn rect(cx: f32, cy: f32, angle: f32, w: f32, h: f32) -> OrientedRect {
        OrientedRect::new(pos2(cx, cy), angle, vec2(w, h))
    }

    /// Every expected value below was read off the `geo::Polygon`-based
    /// predicate this test module replaced, on these exact inputs, before it
    /// was deleted.
    ///
    /// For an axis-aligned pair the bounding-box test is already exact, so the
    /// separating-axis test has to agree with it case for case -- including the
    /// two touching cases, where both say "overlapping".
    #[test]
    fn axis_aligned_answers_match_the_bounding_box_exactly() {
        let a = || rect(5., 5., 0., 10., 10.);

        let cases = [
            ("disjoint", rect(25., 5., 0., 10., 10.), false),
            ("touching along an edge", rect(15., 5., 0., 10., 10.), true),
            ("touching at a corner", rect(15., 15., 0., 10., 10.), true),
            ("overlapping", rect(8., 8., 0., 10., 10.), true),
            ("one contains the other", rect(5., 5., 0., 2., 2.), true),
            ("identical", rect(5., 5., 0., 10., 10.), true),
        ];

        for (name, b, expected) in cases {
            let a = a();
            assert_eq!(a.intersects(&b), expected, "{name}");
            assert_eq!(b.intersects(&a), expected, "{name}, reversed");
            assert_eq!(
                a.intersects(&b),
                a.bbox.intersects(b.bbox),
                "{name}: axis-aligned, so the bounding box is already the exact answer"
            );
        }
    }

    #[test]
    fn a_rotated_rect_overlapping_an_axis_aligned_one_is_detected() {
        let square = rect(0., 0., 0., 4., 4.);
        let bar = rect(2.5, 0., FRAC_PI_4, 4., 1.);

        assert!(square.intersects(&bar));
        assert!(bar.intersects(&square));
    }

    /// The case that proves the axis test is doing real work: two thin bars
    /// crossed at right angles, far enough apart to be plainly disjoint, whose
    /// bounding boxes nonetheless overlap. Anything that answered from the
    /// bounding box alone would report these as colliding and suppress a label
    /// that has room to draw.
    #[test]
    fn a_rotated_pair_the_bounding_box_calls_overlapping_is_separated() {
        let a = rect(0., 0., FRAC_PI_4, 10., 1.);
        let b = rect(6., -6., -FRAC_PI_4, 10., 1.);

        assert!(
            a.bbox.intersects(b.bbox),
            "the premise: an AABB-only test would call these overlapping"
        );
        assert!(!a.intersects(&b));
        assert!(!b.intersects(&a));
    }

    /// The reason all four axes are candidates rather than two. A rectangle's
    /// two distinct edge normals point along its own length and its own width,
    /// and a rotated rect can be cleared of a neighbour along either -- past its
    /// end, or off its side. Neither case is decided by the other's axis, nor by
    /// either axis of the axis-aligned square, whose two are just `x` and `y`
    /// and are already spent by the bounding-box check.
    ///
    /// Asserted both ways round. Between the four assertions here, every one of
    /// the four slots in `separated`'s axis list is load-bearing: replacing any
    /// single one with a duplicate of its neighbour turns one of them red.
    #[test]
    fn a_rotated_rect_is_separated_along_either_of_its_own_axes() {
        let bar = || rect(0., 0., FRAC_PI_4, 10., 6.);

        for (name, square) in [
            ("past the bar's end", rect(6.5, 6.5, 0., 4., 4.)),
            ("off the bar's side", rect(-6., 6., 0., 4., 4.)),
        ] {
            let bar = bar();

            assert!(
                bar.bbox.intersects(square.bbox),
                "{name}: the premise -- an AABB-only test would call these overlapping"
            );
            assert!(!bar.intersects(&square), "{name}");
            assert!(!square.intersects(&bar), "{name}, reversed");
        }
    }

    #[test]
    fn degenerate_rects_do_not_panic() {
        let area = rect(5., 5., 0., 10., 10.);

        let point_inside = rect(5., 5., 0., 0., 0.);
        let point_outside = rect(50., 50., 0., 0., 0.);
        let segment_inside = rect(5., 5., 0., 10., 0.);
        let segment_touching = rect(15., 5., 0., 10., 0.);
        let rotated_segment = rect(5., 5., FRAC_PI_4, 10., 0.);

        assert!(area.intersects(&point_inside));
        assert!(point_inside.intersects(&area));
        assert!(!area.intersects(&point_outside));
        assert!(!point_outside.intersects(&area));
        assert!(area.intersects(&segment_inside));
        assert!(area.intersects(&segment_touching));
        assert!(area.intersects(&rotated_segment));
        assert!(point_inside.intersects(&point_inside));
        assert!(!point_inside.intersects(&point_outside));
    }

    #[test]
    fn occupied_areas_refuses_the_second_of_two_overlapping_labels() {
        let mut occupied = OccupiedAreas::new();

        assert!(occupied.try_occupy(rect(5., 5., 0., 10., 10.)));
        assert!(!occupied.try_occupy(rect(8., 8., 0., 10., 10.)));
        assert!(occupied.try_occupy(rect(25., 5., 0., 10., 10.)));
    }

    #[test]
    fn top_left_is_the_corner_the_galley_is_drawn_from() {
        let unrotated = rect(5., 5., 0., 10., 4.);
        assert_eq!(unrotated.top_left(), pos2(0., 3.));
    }
}
