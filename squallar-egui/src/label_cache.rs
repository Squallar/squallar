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
//! hundred names sit on a 1920x1080 basemap pane. What it costs per *solve* is
//! the buffers all five write into, and those are [`LabelScratch`]'s: kept
//! between solves, so a pane that has solved once asks the allocator for
//! nothing when it solves again.
//!
//! # What is kept is the vertices, not the shapes
//!
//! This is [`crate::point_painter::PointTextMeshes`]' bargain applied to the
//! other text a pane draws, and it is kept the same way and for the same
//! reason: a `Vec<egui::Shape>` handed back to the painter is a list
//! `Context::tessellate` walks again on every frame, and every glyph in it is
//! re-placed, re-coloured and re-normalized against the font atlas to the same
//! vertex it had last frame. So the solve is tessellated once, where it is
//! made, and what a kept frame adds to the painter is one `egui::Shape::Mesh`.
//!
//! Measured on the native rig's scene A (one 1920x1080 pane, KTLX, every layer
//! on, the `pan-zoom-2d` script, private Xvfb), counted over 200 consecutive
//! passes: the pane hands the painter 430.2 label text shapes carrying
//! 14,663 glyph vertices per pane-frame, which is 79.7 % of every glyph vertex
//! the tessellator sees, and 59.5 % of pane-frames answer from this memo.
//!
//! **A kept mesh has the atlas coordinates baked in TWICE**, and a kept shape
//! list had them once: a `TextShape` carries a galley whose glyph UVs are
//! still in atlas *pixels*, and normalizing them by the atlas size is the last
//! thing tessellation does. So a mesh is wrong after an atlas *growth* as well
//! as after a repack — the divisor moved without a glyph moving.
//! [`LabelKey`]'s `atlas_generation` covers both, and it did before this: what
//! it did not have is anything that would go red if it stopped.
//! `ui_map_overlays::tests::a_repack_under_a_still_map_does_not_paint_the_old_atlas`
//! is that gate, and setting the term to a constant makes it red on the
//! growth arm.
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

/// The glyph raster a kept solve's mesh was built against.
///
/// A solved label carries a laid-out `egui::Galley`, which points into the
/// font atlas by pixel position, and the mesh it tessellates to carries those
/// coordinates a second time, divided by the atlas size. A re-rasterization
/// invalidates both even though the labels are unchanged, and so does a
/// growth, which moves the divisor without moving a glyph — `atlas_generation`
/// covers both, because [`walkers::GalleyCache::begin_frame`] bumps it on a
/// size change as well as on a fill that fell. Both terms are
/// [`crate::point_painter::PointTextKey`]'s, for the same reason and read the
/// same way: `pixels_per_point` because it re-rasterizes every glyph, and the
/// atlas generation because egui rebuilds the atlas under the pass and puts
/// the glyphs somewhere else.
///
/// **The atlas term is a generation and not a reading of the atlas, and that
/// distinction is the whole of a defect.** It used to be
/// [`walkers::AtlasStamp`]'s size and fill, taken here, once per pane per
/// frame. Those are levels, and the only way a repack shows in them is the
/// fill *falling* — so they answer only for two readings taken one pass apart.
/// This key is compared across a gap by construction: a pane whose labels have
/// not moved does not solve, so nothing here reads the atlas for as long as the
/// map is still, and the fill has climbed back past the stored one under a
/// height that is once again a power of two by the time anything looks. The
/// key then matched over a raster that had moved entirely, and the pane
/// re-painted galleys addressing the glyphs of whatever now occupies those
/// texels. [`walkers::GalleyCache::generation`] counts the moves instead, so a
/// key that skipped a thousand frames still compares.
///
/// The projector, the pane rect, the zoom and the theme are all absent, and
/// none of them is missing: each reaches the labels only by changing them — a
/// pan moves every `position`, a zoom moves `font_size` and the set itself, a
/// theme change moves `text_color` — and the list itself is compared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct LabelKey {
    pixels_per_point: u32,
    atlas_generation: u64,
}

impl LabelKey {
    /// No `Context::fonts` read: the atlas term is the memo's own generation,
    /// a `u64` load. The context lock this used to take once per pane per
    /// frame is now taken once per frame, in `Gui::ui_phased`, where the
    /// generation is maintained.
    pub(crate) fn new(ctx: &egui::Context, galleys: &walkers::GalleyCache) -> Self {
        Self {
            pixels_per_point: ctx.pixels_per_point().to_bits(),
            atlas_generation: galleys.generation(),
        }
    }
}

/// One pane's kept solve: the key and the list it was solved from, and the
/// mesh the surviving shapes tessellated to.
///
/// `None` is a solve that placed no label at all — every name refused by the
/// collision or repeat rule, which a crowded pane at a low zoom reaches — kept
/// so that case is not re-solved either.
struct Kept {
    key: LabelKey,
    labels: Vec<walkers::Text>,
    mesh: Option<Arc<egui::Mesh>>,
    /// The same mesh with a painter opacity already multiplied in, and the
    /// factor it was multiplied by. Empty at full opacity, which is what the
    /// layer draws at unless a user has moved its slider, so a pane that never
    /// dims its names never holds a second mesh at all. Dropped with the entry
    /// it sits in, so a new solve — or a moved atlas — retints rather than
    /// re-serving.
    tinted: crate::point_painter::TintMemo,
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
/// rather than accumulated, and each entry holds one viewport's labels and the
/// one mesh they drew. There is no ceiling to trip because there is no growth:
/// a map panned across a country replaces the entry it has, it does not add to
/// a table.
#[derive(Default)]
pub(crate) struct LabelCache {
    entries: HashMap<usize, Kept>,
    /// The buffers a solve fills, lent to whichever pane is solving.
    ///
    /// **One, not one per pane.** A scratch lives only for the length of the
    /// solve that fills it — [`LabelScratch::begin`] empties it before
    /// anything reads it — so two panes solving on the same frame share it in
    /// sequence and neither can see the other's. The kept solves above are
    /// per-pane because they are read on a later frame; this is not.
    scratch: LabelScratch,
    solves: u64,
    hits: u64,
    recycled: u64,
}

impl LabelCache {
    /// This pane's kept mesh, if it was solved under `key` from exactly
    /// `labels`.
    ///
    /// The outer `Option` is the memo's answer, the inner one the solve's: a
    /// hit that placed nothing is `Some(None)`, and it is a hit.
    pub(crate) fn lookup(
        &mut self,
        pane: usize,
        key: LabelKey,
        labels: &[walkers::Text],
    ) -> Option<Option<Arc<egui::Mesh>>> {
        let kept = self.entries.get(&pane)?;
        if kept.key != key || !same_labels(&kept.labels, labels) {
            return None;
        }
        self.hits += 1;
        // A refcount bump on geometry that is already tessellated.
        Some(kept.mesh.clone())
    }

    /// Paint this pane's kept mesh through `painter`.
    ///
    /// Here rather than at the call site because the painter's opacity is
    /// multiplied into a copy that lives in the entry, and that copy is made
    /// once per factor instead of once per frame; see
    /// [`crate::point_painter::add_kept_mesh`] for why `Painter::add` cannot
    /// do it without deep-cloning the mesh the memo is holding, and for the
    /// argument that doing it here is the same picture.
    pub(crate) fn paint(
        &mut self,
        pane: usize,
        painter: &egui::Painter,
        mesh: Option<Arc<egui::Mesh>>,
    ) {
        let Some(mesh) = mesh else {
            return;
        };
        // The entry is looked up again only when there is a tint to keep in
        // it, so a pane at full opacity pays the `Painter::add` it always paid
        // and no more.
        let Some(kept) = crate::point_painter::tint_wanted(painter)
            .then(|| self.entries.get_mut(&pane))
            .flatten()
        else {
            painter.add(egui::Shape::Mesh(mesh));
            return;
        };
        crate::point_painter::add_kept_mesh(painter, mesh, &mut kept.tinted);
    }

    /// This pane's retired mesh, emptied but keeping its buffers, for the
    /// solve that is replacing it to fill.
    ///
    /// **The entry being replaced owns exactly the right allocation.** A
    /// pane's names tessellate to on the order of 850 kB of vertices and
    /// indices, and a map being panned re-solves on nearly half its frames, so
    /// a solve that starts from `Mesh::default()` takes that buffer from the
    /// allocator and hands it back one frame later, every frame — which is
    /// most of what a miss-frame solve costs. See
    /// [`crate::point_painter::tessellate_text_shapes_drain`] for the figures.
    ///
    /// **Nothing else can be holding it.** The painter's clone of a kept mesh
    /// dies with the paint list `Context::tessellate` consumed at the end of
    /// the frame it was added on, so by the time the next pass reaches this
    /// the cache's is the only reference. `Arc::try_unwrap` is what makes that
    /// a fact rather than an argument: a reference that somehow survived gives
    /// a fresh mesh and one wasted allocation, never a mutation of geometry
    /// something is still drawing.
    ///
    /// The entry is REMOVED, so a caller that takes the buffers must store a
    /// new solve; `paint_labels` calls this only on the miss path it is about
    /// to store from.
    pub(crate) fn recycle(&mut self, pane: usize) -> egui::Mesh {
        let Some(mesh) = self.entries.remove(&pane).and_then(|kept| kept.mesh) else {
            return egui::Mesh::default();
        };
        let Ok(mut mesh) = Arc::try_unwrap(mesh) else {
            return egui::Mesh::default();
        };
        // Not `Mesh::clear`, which replaces the vertex buffer with a fresh
        // empty one and so throws away the whole point of this.
        mesh.vertices.clear();
        mesh.indices.clear();
        mesh.texture_id = egui::TextureId::default();
        self.recycled += 1;
        mesh
    }

    /// The buffers the next solve is to fill.
    pub(crate) fn scratch(&mut self) -> &mut LabelScratch {
        &mut self.scratch
    }

    pub(crate) fn store(
        &mut self,
        pane: usize,
        key: LabelKey,
        labels: Vec<walkers::Text>,
        mesh: Option<Arc<egui::Mesh>>,
    ) {
        self.solves += 1;
        self.entries.insert(
            pane,
            Kept {
                key,
                labels,
                mesh,
                tinted: None,
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

    /// Solves that filled the retired solve's buffers instead of asking the
    /// allocator for new ones.
    #[cfg(test)]
    pub(crate) fn recycled(&self) -> u64 {
        self.recycled
    }
}

/// The buffers one label solve fills, kept between solves.
///
/// **`LabelCache::recycle` made this argument for the mesh and stopped
/// there.** A solve that misses the memo builds, uses and drops four more
/// things whose size is a property of the pane and not of the frame: the
/// repeat-name index, one `Vec<Pos2>` per name that drew, the shape list, and
/// the claimed areas. On the native rig's scene A that is on the order of a
/// name index grown from empty to several hundred entries — rehashing every
/// name it already holds each time it doubles — plus one heap allocation per
/// placed name, plus a 64-byte-per-shape list, on every frame of a pan.
/// Measured there with `perf` against the shipped binary, the solve's own body
/// (everything outside the galley memo, the collision search and the
/// tessellator) was 28.2 % of the whole `CityLabels` arm, and 63 of its 133
/// samples were inside `__rust_alloc`, `grow_one` and `reserve_rehash`.
///
/// So the solve is handed these instead of making them, and it empties them
/// rather than dropping them: after the first solve a pane's solve asks the
/// allocator for nothing at all. That is the same bargain, and the same
/// safety argument, as the mesh — the buffers are the cache's own, nothing
/// else can be holding them, and empty is always correct because every entry
/// is rebuilt from the labels before it is read.
///
/// **Nothing here is part of the memo key.** These are scratch: what a solve
/// leaves in them is overwritten by the next solve before it is read, and a
/// scratch that arrived full would produce the same shapes as one that
/// arrived empty. See [`Self::begin`], which is what makes that true.
#[derive(Default)]
pub(crate) struct LabelScratch {
    /// Where each name has already been drawn, so a fragmented river is named
    /// once per stretch of screen rather than once per OSM way.
    ///
    /// **The key is owned rather than borrowed, and that is what lets this
    /// outlive the solve.** It used to be `&Arc<str>` into the caller's list,
    /// which costs nothing per name but pins the map to one call. Owned, an
    /// insert costs one refcount bump on an `Arc` the tile is holding anyway
    /// — against the heap allocation per name that the borrowed spelling's
    /// `Vec<Pos2>` value cost. A *lookup* still borrows: `Arc<str>: Borrow<str>`,
    /// so a probe hashes the name's bytes and touches no refcount.
    ///
    /// The value is the head of a chain in [`Self::anchors`], not a vector, so
    /// a name that draws once — which is nearly all of them — costs no
    /// allocation of its own.
    names: HashMap<Arc<str>, u32>,
    /// The anchor chains themselves, `(anchor, next link)`, in one arena.
    ///
    /// One `Vec` for the whole solve rather than one per name, for the reason
    /// [`walkers::OccupiedAreas`]' own `filed` arena gives: a map of per-name
    /// vectors allocates once per name that drew, per solve, to save a walk
    /// that costs less than that.
    anchors: Vec<(egui::Pos2, u32)>,
    /// The shapes the solve placed, in paint order, drained into the
    /// tessellator so the list's buffer survives with the scratch.
    shapes: Vec<egui::Shape>,
    /// The screen the solve's labels have claimed.
    occupied: walkers::OccupiedAreas,
}

/// The end of an anchor chain.
const NO_ANCHOR: u32 = u32::MAX;

impl LabelScratch {
    /// Empty every buffer, keeping every allocation.
    ///
    /// Called at the head of a solve rather than at its end, so what a solve
    /// reads is what that solve wrote and nothing else — a scratch left full
    /// by the previous solve is indistinguishable from a fresh one from the
    /// moment this returns.
    pub(crate) fn begin(&mut self) {
        self.names.clear();
        self.anchors.clear();
        self.shapes.clear();
        self.occupied.clear();
    }

    /// The claimed areas, for the solve to test against.
    pub(crate) fn occupied(&mut self) -> &mut walkers::OccupiedAreas {
        &mut self.occupied
    }

    /// Whether `name` has already drawn within `distance` of `at`.
    pub(crate) fn drawn_near(&self, name: &str, at: egui::Pos2, distance: f32) -> bool {
        let mut link = match self.names.get(name) {
            Some(&head) => head,
            None => return false,
        };
        while link != NO_ANCHOR {
            let (anchor, next) = self.anchors[link as usize];
            if anchor.distance(at) < distance {
                return true;
            }
            link = next;
        }
        false
    }

    /// Record that `name` drew at `at`, and file `shape` in paint order.
    pub(crate) fn placed(&mut self, name: &Arc<str>, at: egui::Pos2, shape: egui::Shape) {
        let link = self.anchors.len() as u32;
        // **One hash on the placing path**, which is what the borrowed
        // spelling's `entry(..).or_default()` cost too. `get_mut` then
        // `insert` would be two on the miss, and a placed name is usually a
        // miss: a pane's names are mostly distinct. The `Arc` clone `entry`
        // needs up front is a refcount bump, given straight back when the name
        // was already there.
        let head = match self.names.entry(name.clone()) {
            std::collections::hash_map::Entry::Occupied(mut at) => {
                std::mem::replace(at.get_mut(), link)
            }
            std::collections::hash_map::Entry::Vacant(at) => {
                at.insert(link);
                NO_ANCHOR
            }
        };
        self.anchors.push((at, head));
        self.shapes.push(shape);
    }

    /// The shapes this solve placed, in paint order.
    pub(crate) fn shapes(&mut self) -> &mut Vec<egui::Shape> {
        &mut self.shapes
    }

    /// What each buffer can hold without asking the allocator.
    ///
    /// The figure the reuse is gated on — see
    /// `ui_map_overlays::tests::a_solve_keeps_the_buffers_the_last_one_grew`.
    #[cfg(test)]
    pub(crate) fn capacities(&self) -> (usize, usize, usize) {
        (
            self.names.capacity(),
            self.anchors.capacity(),
            self.shapes.capacity(),
        )
    }

    /// What each buffer is holding.
    ///
    /// The other half of the same gate: capacity says a buffer was kept, and
    /// this says it was emptied. A `begin` that kept the anchors would grow
    /// this without limit while every capacity figure still read healthy.
    #[cfg(test)]
    pub(crate) fn lens(&self) -> (usize, usize, usize) {
        (self.names.len(), self.anchors.len(), self.shapes.len())
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

    fn key(generation: u64) -> LabelKey {
        LabelKey {
            pixels_per_point: 1.0f32.to_bits(),
            atlas_generation: generation,
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

        assert!(cache.lookup(0, key(3), &labels).is_none(), "nothing kept");
        cache.store(0, key(3), labels.clone(), None);
        assert!(cache.lookup(0, key(3), &labels).is_some(), "pane 0 kept");
        assert!(
            cache.lookup(1, key(3), &labels).is_none(),
            "pane 1 must not read pane 0's solve"
        );
        assert!(
            cache.lookup(0, key(4), &labels).is_none(),
            "a moved atlas is a new key"
        );
        assert_eq!(cache.solves(), 1);
        assert_eq!(cache.hits(), 1);
    }
}
