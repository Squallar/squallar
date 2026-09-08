//! The basemap's place names, solved once and re-painted until they move.
//!
//! **The label phase is a pure function of the labels the ground phase
//! deferred.** `ui_map_overlays::solve_labels` takes a pane's whole list of
//! [`walkers::Text`], lays each name out, tests it against one
//! [`walkers::OccupiedAreas`] and against the repeat-distance rule, and hands
//! back the shapes that survived. Nothing else reaches it: no clock, no
//! pointer, no frame counter. So two frames handed the same list under the
//! same glyph raster emit the same shapes — and on a map that is not moving
//! that is every frame after the first.
//!
//! What that solve costs per name is a galley probe, a repeat-name probe, an
//! oriented rectangle, a bucketed collision search and a shape; several
//! hundred names sit on a 1920x1080 basemap pane. What re-painting a solved
//! list costs is a run of shape clones, each a refcount bump on a galley that
//! is already laid out.
//!
//! This is [`crate::point_painter::PointTextMeshes`]' bargain applied to the
//! other text a pane draws, and it keeps the same thing for the same reason:
//! **the per-shape work, not the vertices.** The shapes are re-added every
//! frame and egui tessellates them exactly as it always did, so what a frame
//! stages does not fall and no pixel moves.
//!
//! # The key is exact, not a hash
//!
//! The label list is kept beside the shapes and compared field by field,
//! rather than reduced to a digest. A memo answering from a 64-bit signature
//! is one birthday collision away from drawing last frame's names, and the
//! comparison is not what costs: the same name arrives in the same `Arc` from
//! the same cached tile frame after frame, so the text compare is a pointer
//! compare in the case this exists for. Floats are compared by bits, which is
//! stricter than equality — `-0.0` and `0.0` are two lists — and stricter is
//! the safe direction for a memo, exactly as [`walkers::GalleyCache`]'s key
//! argues: a spurious miss costs one solve, a spurious hit draws the wrong
//! map.
//!
//! # What is NOT in the key, deliberately
//!
//! The layer's **opacity**. It is applied by the painter as each shape is
//! added, exactly as it is to a freshly solved list, so a slider drag re-tints
//! rather than re-solving. That is the rule the overlay cache token already
//! follows: opacity is paint-time only.

use std::collections::HashMap;
use std::sync::Arc;

/// The glyph raster a kept solve's shapes were built against.
///
/// A solved label carries a laid-out `egui::Galley`, which points into the
/// font atlas by pixel position, so a re-rasterization invalidates it even
/// though the labels are unchanged. Both terms are
/// [`crate::point_painter::PointTextKey`]'s, for the same reason and read the
/// same way: `pixels_per_point` because it re-rasterizes every glyph, and the
/// atlas stamp because egui rebuilds the atlas under the pass and puts the
/// glyphs somewhere else. [`walkers::GalleyCache`] drops its own table on both
/// events; this is the same two facts, held for the same span.
///
/// The projector, the pane rect, the zoom and the theme are all absent, and
/// none of them is missing: each reaches the labels only by changing them — a
/// pan moves every `position`, a zoom moves `font_size` and the set itself, a
/// theme change moves `text_color` — and the list itself is compared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct LabelKey {
    pixels_per_point: u32,
    atlas_size: [usize; 2],
    atlas_fill: u32,
}

impl LabelKey {
    /// One `Context::fonts` read, through [`walkers::AtlasStamp`]: a write lock
    /// on the whole context, so this is taken once per pane per frame and
    /// never per label.
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        let atlas = walkers::AtlasStamp::read(ctx);
        Self {
            pixels_per_point: ctx.pixels_per_point().to_bits(),
            atlas_size: atlas.size,
            atlas_fill: atlas.fill.to_bits(),
        }
    }
}

/// One pane's kept solve: the key and the list it was solved from, and the
/// shapes that survived, in paint order.
struct Kept {
    key: LabelKey,
    labels: Vec<walkers::Text>,
    shapes: Vec<egui::Shape>,
}

/// The place-name solve each pane last made, kept between frames.
///
/// Owned by [`crate::gui::Gui`] and lent to the pane walk for the frame, on
/// [`walkers::GalleyCache`]'s terms: not a thread-local, not a process-wide
/// pool, so what it retains is bounded by one owner's lifetime and two tests
/// can hold two independent ones. Empty is always correct — every entry is
/// reproducible from the labels that made it.
///
/// **Bounded by the glass, not by the session.** One entry per pane, replaced
/// rather than accumulated, and each entry holds one viewport's labels. There
/// is no ceiling to trip because there is no growth: a map panned across a
/// country replaces the entry it has, it does not add to a table.
#[derive(Default)]
pub(crate) struct LabelCache {
    entries: HashMap<usize, Kept>,
    solves: u64,
    hits: u64,
}

impl LabelCache {
    /// This pane's kept shapes, if they were solved under `key` from exactly
    /// `labels`.
    pub(crate) fn lookup(
        &mut self,
        pane: usize,
        key: LabelKey,
        labels: &[walkers::Text],
    ) -> Option<&[egui::Shape]> {
        let kept = self.entries.get(&pane)?;
        if kept.key != key || !same_labels(&kept.labels, labels) {
            return None;
        }
        self.hits += 1;
        Some(&kept.shapes)
    }

    pub(crate) fn store(
        &mut self,
        pane: usize,
        key: LabelKey,
        labels: Vec<walkers::Text>,
        shapes: Vec<egui::Shape>,
    ) {
        self.solves += 1;
        self.entries.insert(
            pane,
            Kept {
                key,
                labels,
                shapes,
            },
        );
    }

    /// Label sets solved — one per list-and-raster the pane walk has seen.
    ///
    /// The shipped reading of this is `tile_mesh::ledger`'s `label solves`,
    /// which counts the same event process-wide; this is the per-instance one
    /// a test can hold without touching a global.
    #[cfg(test)]
    pub(crate) fn solves(&self) -> u64 {
        self.solves
    }

    /// Frames painted from a kept solve.
    #[cfg(test)]
    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }
}

/// Whether two label lists would solve to the same shapes.
///
/// Order matters and is not sorted away: the collision rule is "first to ask
/// keeps it", so two lists holding the same names in a different order are two
/// different maps.
fn same_labels(a: &[walkers::Text], b: &[walkers::Text]) -> bool {
    a.len() == b.len() && std::iter::zip(a, b).all(|(a, b)| same_label(a, b))
}

/// Every field `ui_map_overlays::solve_labels` reads off one label, and no
/// others.
///
/// The text is compared by pointer first. A name that has not moved arrives in
/// the same `Arc` from the same cached tile, so the case this memo exists for
/// costs one word compare; the `str` compare behind it is what covers a tile
/// that was re-decoded or re-styled into an equal name.
fn same_label(a: &walkers::Text, b: &walkers::Text) -> bool {
    (Arc::ptr_eq(&a.text, &b.text) || a.text == b.text)
        && a.position.x.to_bits() == b.position.x.to_bits()
        && a.position.y.to_bits() == b.position.y.to_bits()
        && a.font_size.to_bits() == b.font_size.to_bits()
        && a.text_color == b.text_color
        && a.angle.to_bits() == b.angle.to_bits()
        && a.max_width_ems.map(f32::to_bits) == b.max_width_ems.map(f32::to_bits)
        && a.line_height_ems.map(f32::to_bits) == b.line_height_ems.map(f32::to_bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(name: &str, x: f32) -> walkers::Text {
        walkers::Text::new(egui::pos2(x, 10.0), name, 12.0, egui::Color32::WHITE, 0.0)
    }

    fn key(fill: f32) -> LabelKey {
        LabelKey {
            pixels_per_point: 1.0f32.to_bits(),
            atlas_size: [64, 64],
            atlas_fill: fill.to_bits(),
        }
    }

    #[test]
    fn a_list_matches_itself_and_an_equal_copy() {
        let a = vec![label("Tulsa", 1.0), label("Norman", 2.0)];
        let b = a.clone();
        assert!(same_labels(&a, &a));
        assert!(same_labels(&a, &b), "an equal list must match");
    }

    /// Every field the solve reads is compared, one arm each: a list that
    /// differs in any of them is a different list.
    #[test]
    fn every_field_the_solve_reads_is_compared() {
        let base = label("Tulsa", 1.0);

        assert!(!same_label(&base, &label("Tulsa", 2.0)), "position");
        assert!(!same_label(&base, &label("Norman", 1.0)), "text");

        let mut resized = base.clone();
        resized.font_size = 13.0;
        assert!(!same_label(&base, &resized), "font_size");

        let mut recolored = base.clone();
        recolored.text_color = egui::Color32::BLACK;
        assert!(!same_label(&base, &recolored), "text_color");

        let mut turned = base.clone();
        turned.angle = 0.5;
        assert!(!same_label(&base, &turned), "angle");

        assert!(
            !same_label(&base, &base.clone().with_wrapping(Some(8.0), None)),
            "max_width_ems"
        );
        assert!(
            !same_label(&base, &base.clone().with_wrapping(None, Some(1.2))),
            "line_height_ems"
        );
    }

    /// The collision rule is "first to ask keeps it", so order is part of the
    /// list rather than a presentation detail.
    #[test]
    fn a_reordered_list_is_a_different_list() {
        let a = vec![label("Tulsa", 1.0), label("Norman", 2.0)];
        let b = vec![label("Norman", 2.0), label("Tulsa", 1.0)];
        assert!(!same_labels(&a, &b));
    }

    /// A shorter or longer list is never a hit, whatever its prefix.
    #[test]
    fn a_list_that_gained_a_name_is_a_different_list() {
        let a = vec![label("Tulsa", 1.0)];
        let b = vec![label("Tulsa", 1.0), label("Norman", 2.0)];
        assert!(!same_labels(&a, &b));
        assert!(!same_labels(&b, &a));
    }

    #[test]
    fn a_pane_answers_only_its_own_kept_solve() {
        let mut cache = LabelCache::default();
        let labels = vec![label("Tulsa", 1.0)];

        assert!(cache.lookup(0, key(0.5), &labels).is_none(), "nothing kept");
        cache.store(0, key(0.5), labels.clone(), Vec::new());
        assert!(cache.lookup(0, key(0.5), &labels).is_some(), "pane 0 kept");
        assert!(
            cache.lookup(1, key(0.5), &labels).is_none(),
            "pane 1 must not read pane 0's solve"
        );
        assert!(
            cache.lookup(0, key(0.9), &labels).is_none(),
            "a moved atlas is a new key"
        );
        assert_eq!(cache.solves(), 1);
        assert_eq!(cache.hits(), 1);
    }
}
