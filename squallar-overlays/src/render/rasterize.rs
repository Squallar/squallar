//! Rasterize overlay polygons to RGBA textures using tiny-skia.

use std::collections::HashSet;

use squallar_geo::lat_rad_to_mercator_y;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use std::sync::Arc;

use crate::nws::alert::AlertCategory;
use crate::render::overlay_state::{HitItems, OverlayItem};
pub use crate::render::raster_buf::RasterBuf;
use crate::spc::colors::{md_fill_color, md_stroke_color};
use crate::spc::reports::StormReportKind;
use crate::types::OverlayFeature;
use ecolor::Color32;
use squallar_geo::{GeoBounds, GeoPolygonRing};

/// Occupied cell index to the item indices drawn into it.
///
/// **Not hashed with `RandomState`.** The key is `qy * width + qx`, computed
/// here from a pixel the rasterizer just drew, and it is bounded by the
/// texture's own quarter-resolution grid — nothing outside this process picks
/// it, and no feed can widen the key space past `width * height` however many
/// features it sends. What `RandomState` buys is per-process seeding against an
/// attacker who chooses keys; there is no such attacker at this key, and
/// `HitCells::record` runs once per drawn point. `FxHashMap` is a multiply and
/// a rotate instead of SipHash-1-3.
///
/// Iteration order changes with the hasher, and one place cares: the reply wire
/// in [`crate::render::jobs::encode_overlay_out`], which already sorts by cell
/// index for exactly this reason and is unaffected. Every other reader takes
/// `values()`, `len()` or `is_empty()`.
pub type HitCellMap = rustc_hash::FxHashMap<u32, Vec<u32>>;

/// The portable half of click detection: which quarter-resolution cells the
/// rasterizer drew which item **indices** into — positions in its input list,
/// so both halves must come from one order.
#[derive(Debug, Clone, PartialEq)]
pub struct HitCells {
    pub width: u32,
    pub height: u32,
    pub cells: HitCellMap,
}

impl HitCells {
    pub fn new(full_width: u32, full_height: u32) -> Self {
        Self {
            width: full_width.div_ceil(4),
            height: full_height.div_ceil(4),
            cells: HitCellMap::default(),
        }
    }

    pub fn record(&mut self, px: f32, py: f32, item_id: u32) {
        // **A float-to-int `as` cast saturates**: -12.0 and NaN both become 0,
        // which passes the bound below as cell column 0 rather than failing it.
        // A stamp whose disc lies entirely off the left or top edge therefore
        // recorded its hits against the edge cells, and a click on that edge
        // answered an item that is nowhere near the pointer. Reachable: GLM
        // culls a flash only at `px < -base_size` while its hit disc is
        // `0.72 * base_size` at the widest, so a flash just inside the cull
        // draws no ink at all and still recorded a column of hits.
        //
        // Only the near edges need the guard. A coordinate past `width` casts
        // to a number past `width`, and infinity saturates to `u32::MAX`, so
        // the `>=` tests below already reject those.
        if px.is_nan() || py.is_nan() || px < 0.0 || py < 0.0 {
            return;
        }
        let qx = (px as u32) / 4;
        let qy = (py as u32) / 4;
        if qx >= self.width || qy >= self.height {
            return;
        }
        let idx = qy * self.width + qx;
        let ids = self
            .cells
            .entry(idx)
            .or_insert_with(|| Vec::with_capacity(1));
        if !ids.contains(&item_id) {
            ids.push(item_id);
        }
    }

    pub fn ids_at(&self, u: f32, v: f32) -> &[u32] {
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return &[];
        }
        let qx = ((u * self.width as f32) as u32).min(self.width.saturating_sub(1));
        let qy = ((v * self.height as f32) as u32).min(self.height.saturating_sub(1));
        let idx = qy * self.width + qx;
        self.cells.get(&idx).map_or(&[], Vec::as_slice)
    }

    pub fn max_id(&self) -> Option<u32> {
        self.cells.values().flatten().copied().max()
    }

    /// Bytes this grid holds on the heap: the table's buffer — one
    /// `(u32, Vec<u32>)` and a control byte per slot of its `capacity()` —
    /// plus every occupied cell's id vector at **its** `capacity()`. The
    /// allocator's figures, not `len()`s: [`Self::record`] grows a cell's
    /// vector by doubling, so a cell holding three ids is holding four. Priced
    /// through [`ItemFootprint`](squallar_source::footprint::ItemFootprint)'s
    /// table impl so a hit grid and every other table in the census are
    /// measured with one rule.
    pub fn resident_bytes(&self) -> usize {
        let bytes = squallar_source::footprint::ItemFootprint::owned_bytes(&self.cells);
        usize::try_from(bytes).unwrap_or(usize::MAX)
    }
}

/// **Deliberately not `Clone`.** A hit map is one `FxHashMap<u32, Vec<u32>>`
/// entry per quarter-cell the layer touched, and it used to be cloned once per
/// destination pane per arriving raster, on the frame thread: measured at 49%
/// of the whole `Apply` frame-pump walk on scene E2. It is delivered behind an
/// `Arc` now ([`crate::render::rasterize::HitMap`] rides
/// `OverlayRenderResponse::hit_map`), every pane shares the one allocation,
/// and the absent `Clone` is what stops a deep copy coming back: there is no
/// `&mut self` method on this type, so nothing needs a private copy.
pub struct HitMap {
    cells: HitCells,
    /// The items a cell's recorded indices name, **positionally**.
    ///
    /// This was a `HashMap<u32, Arc<dyn OverlayItem>>` whose keys were exactly
    /// `0..items.len()` — a dense range hashed with SipHash to find a slot the
    /// index already named. A positional lookup answers the same question with
    /// a bounds check, so the whole build hashes nothing.
    ///
    /// A [`HitItems::Rows`] clone is that vector of pointers and one refcount
    /// bump each; a [`HitItems::Slab`] clone is one refcount bump, and its
    /// items are built by the handful a click actually names.
    items: HitItems,
}

impl HitMap {
    /// `items.get(i)` **must** answer the item whose row travelled at position
    /// `i` of the described input, because a cell records positions and
    /// nothing else.
    pub fn from_cells(cells: HitCells, items: &HitItems) -> Self {
        Self {
            cells,
            items: items.clone(),
        }
    }

    pub fn hit_test(&self, u: f32, v: f32) -> Vec<Arc<dyn OverlayItem>> {
        self.cells
            .ids_at(u, v)
            .iter()
            .filter_map(|id| self.items.get(*id as usize))
            .collect()
    }

    /// Bytes this map holds on the heap: [`HitCells::resident_bytes`] for the
    /// index, plus the items half **only when it is a list**.
    ///
    /// A [`HitItems::Rows`] is a vector of `Arc` pointers this map owns — the
    /// clone [`Self::from_cells`] took — priced at its `capacity()` times the
    /// pointer size. The bodies those pointers reach are the layer's own
    /// items, priced in the `overlay items` family, and are not added here.
    ///
    /// A [`HitItems::Slab`] adds **nothing**: the handle sits inline in this
    /// struct, [`HitResolve`](squallar_source::hit::HitResolve) exposes no
    /// byte figure, and the block it reaches is the layer's own — GLM's flash
    /// slab under `overlay items`, the storm-report pointer vector under the
    /// parked family — so a figure here would put one block into two families
    /// a reader is invited to add. Both shipped hit-map layers answer a slab,
    /// so for them this is the cells alone.
    pub fn resident_bytes(&self) -> usize {
        let items = match &self.items {
            HitItems::Rows(rows) => rows.capacity() * size_of::<Arc<dyn OverlayItem>>(),
            HitItems::Slab(_) => 0,
        };
        self.cells.resident_bytes().saturating_add(items)
    }
}

/// Which of the two RGBA conventions a rasterizer's bytes are written in;
/// picking the wrong one shifts every translucent colour. tiny-skia output is
/// [`Self::Premultiplied`], [`rasterize_gridded`] is [`Self::Straight`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Premultiplied,
    Straight,
}

/// Whether any pixel of a **premultiplied** RGBA buffer would change the frame
/// it is drawn on.
///
/// **Exact, not a sample.** Premultiplication is what makes it exact: a pixel
/// that contributes nothing has zero in all four bytes, so "no non-zero byte"
/// and "paints nothing" are the same statement. A sampled or strided version
/// would miss a picture whose only ink is one polygon, which is the ordinary
/// shape of an alerts raster.
///
/// **Short-circuits.** A picture with ink in its first row costs a handful of
/// loads; only a picture with no ink at all pays the whole pass, and that is
/// the reading this exists to take.
///
/// **It lives here, below the wire, because the wire needs it.** The answer is
/// what decides whether a reply carries a picture-sized payload at all
/// ([`RasterizeOutput::settle_blank`]), and the encoder is in this crate;
/// `squallar_egui::overlay_cache::ledger` — which is where the reading was
/// first taken and where its prose still lives — re-exports this one function
/// rather than keeping a second spelling of it. Two predicates that can
/// disagree about "is this blank" would be a picture uploaded against a pane
/// told to clear.
///
/// **A word at a time.** "Any non-zero byte" and "any non-zero 64-bit word" are
/// the same question asked of the same bytes, and the wide spelling is the one
/// the autovectorizer can widen further; the byte loop this replaced could not
/// be. No figure is quoted here: the readings taken so far were on a loaded
/// box, where load biases attribution directionally, so they are leads and not
/// results. `pod_align_to` splits off the
/// leading and trailing bytes no aligned word covers, so the answer does not
/// depend on where the buffer landed or on its length being whole words, and
/// scanning the head first keeps the short-circuit on ink in the first pixel.
/// `has_ink_tests` pins this spelling equal to the byte scan element for
/// element across sizes, alignments, and ink in the first, last and no byte.
pub fn has_ink(rgba: &[u8]) -> bool {
    let (head, words, tail) = bytemuck::pod_align_to::<u8, u64>(rgba);
    head.iter().any(|&b| b != 0) || words.iter().any(|&w| w != 0) || tail.iter().any(|&b| b != 0)
}

/// **The texel window of a picture the rasterizer actually painted into.**
///
/// A point or small-polygon overlay covers a fraction of the viewport it is
/// dispatched for: measured by `valgrind --tool=dhat` on the REST1 scene, the
/// four sparse rows — alerts, storm reports, METAR and radar coverage —
/// allocated 233,625,600 B of `tiny_skia::Pixmap` across a leg and *wrote*
/// 6,862,992 B of it, **2.9 %**. Storm reports alone wrote 0.13 % of their four
/// pictures. The rest was allocated, scanned by [`has_ink`], premultiplied
/// where a row declares straight alpha, carried to the pane and uploaded, and
/// was transparent in every byte.
///
/// This is the window the pixmap is allocated at instead. `x`/`y` are its
/// offset in the **dispatched** grid — the one [`JobGeometry`] names and the
/// one `MercatorBounds::project` still projects into — so the picture's texels
/// coincide exactly with the texels the whole-viewport picture would have had
/// there. That is what makes the crop *byte*-identical rather than merely
/// close: every drawing coordinate is the whole-picture coordinate minus an
/// **integer**, and an integer subtracted from an `f32` whose magnitude it does
/// not exceed is exact in binary floating point — the difference is a multiple
/// of the minuend's ulp and no larger than it, so it is representable. No
/// coordinate is rescaled and no path is resampled.
///
/// **`None` is the whole picture** and stays the meaning of an absent crop
/// everywhere below, so a row that computes no extent — and any row that has
/// not adopted this at all — behaves exactly as it did.
///
/// # What it is not
///
/// It is **not** the door's unit. `MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING` is
/// debited at dispatch from `OverlayTexturePlan::bytes()`, a whole-viewport
/// figure decided a rasterization before this window exists, and nothing here
/// changes that charge. What it does move is the *held* term of
/// `OverlayTextureCache::outstanding_bytes`, which prices the picture a cache
/// is really holding — so the door's occupancy falls without its admission
/// arithmetic being touched. Report those separately.
///
/// It is also **not** the rebuild gate's unit.
/// `OverlayTextureCache::needs_rerender_with_policy` compares a held picture's
/// `width`/`height` against the plan's to catch a display-density change, and
/// those two fields stay the **plan's** on every picture. A crop that reached
/// that comparison would read as a resize on every frame and re-render for
/// ever.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PictureCrop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// **The grid this is a window into** — the dispatched picture's own texel
    /// pair, carried on the window rather than looked up beside it.
    ///
    /// Two things need it and neither has it to hand. The **draw** reads the
    /// window out of the whole picture's screen rect as a fraction, so it needs
    /// the denominator of that fraction. And the **arrival** is what decides
    /// whether the pane's rebuild gate is looking at a plan-sized pair or a
    /// window-sized one; before this field the picture's own `size` was the
    /// plan's, and once a window exists it is not.
    ///
    /// Carried and not restated, because the alternative is a second spelling
    /// of the dispatch's own `width`/`height` at every hop — and the arrival
    /// checks this pair against the dispatch it answers precisely so that a
    /// window from some other dispatch cannot be placed against this one.
    pub of_width: u32,
    pub of_height: u32,
}

impl PictureCrop {
    /// The bytes a picture of this window costs, at 4 per texel.
    pub const fn bytes(&self) -> u64 {
        // Saturating for the same reason [`Self::fits`] is checked: the pair
        // arrives off a wire, `u32::MAX` texels each way is `4 * (2^32 - 1)^2`
        // which a `u64` cannot hold, and a wrapped product is a *small* byte
        // figure — it would price an impossible picture as a cheap one at the
        // cache's occupancy door. Saturating leaves every real window exact.
        (self.width as u64)
            .saturating_mul(self.height as u64)
            .saturating_mul(4)
    }

    /// Whether this window is the whole of the grid it names — the case a
    /// rasterizer answers when its content spans the viewport, and the one the
    /// fires counter must not count as a saving.
    pub const fn is_whole(&self) -> bool {
        self.x == 0 && self.y == 0 && self.width == self.of_width && self.height == self.of_height
    }

    /// Whether this window sits inside the grid it names and has texels in it.
    ///
    /// **Asked at the arrival, before anything is placed**, because nothing
    /// downstream can: a window reaching past its grid places a picture off the
    /// ground it was rendered for, which is a visibly misplaced overlay rather
    /// than a failed render, and no counter in the tree would say so.
    pub const fn fits(&self) -> bool {
        // **`checked_add` and not `+`, because the values are wire-decoded and
        // the shipped profile does not check overflow.** `[profile.release]`
        // sets no `overflow-checks`, so `x + width` for `x = u32::MAX` and
        // `width = 1` is `0` in the binary that ships, and `0 <= of_width`
        // passed this guard — the one guard there is — on the exact input it
        // exists to refuse. A window that cannot state its own far edge in a
        // `u32` is outside every grid a `u32` can describe, so the answer is
        // no, not a wrapped yes.
        let (Some(right), Some(bottom)) = (
            self.x.checked_add(self.width),
            self.y.checked_add(self.height),
        ) else {
            return false;
        };
        self.width > 0
            && self.height > 0
            && self.of_width > 0
            && self.of_height > 0
            && right <= self.of_width
            && bottom <= self.of_height
    }

    /// The whole of a `width` x `height` grid.
    pub const fn whole(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
            of_width: width,
            of_height: height,
        }
    }

    /// The ground this window covers, given the ground and grid of the picture
    /// it is a window into.
    ///
    /// **Linear in Mercator Y and in longitude, which is what the grid is.**
    /// `MercatorBounds::project` maps longitude linearly across `width` and
    /// Mercator Y linearly down `height`, so a texel-aligned window's ground is
    /// the same linear map read backwards. Taken through `MercatorBounds` and
    /// not through latitude arithmetic: the projection clamps to
    /// `MERCATOR_LAT_LIMIT_DEG` before it maps, and a second spelling that
    /// forgot the clamp would place a polar picture wrong by degrees.
    ///
    /// **Nothing calls this, in production or in a test**, and the sentence
    /// that used to stand here said a gate did. The draw takes the whole
    /// picture's screen rect and reads the window out of it as a fraction,
    /// which cannot disagree with the placement of the picture it is inside,
    /// so no caller needs the window's ground and none has appeared. It is
    /// kept because the arithmetic is the inverse of the one placement
    /// already trusts and a future caller should not re-derive it — but it is
    /// unexercised, and a first caller should gate it before believing it.
    pub fn ground(&self, bounds: &GeoBounds) -> GeoBounds {
        let (width, height) = (self.of_width, self.of_height);
        let mb = MercatorBounds::from_geo(bounds);
        let lon_at = |x: u32| {
            mb.min_lon + (mb.max_lon - mb.min_lon) * (f64::from(x) / f64::from(width.max(1)))
        };
        let lat_at = |y: u32| {
            let frac = f64::from(y) / f64::from(height.max(1));
            let merc_y = mb.merc_y_max - (mb.merc_y_max - mb.merc_y_min) * frac;
            merc_y_to_lat(merc_y)
        };
        GeoBounds {
            min_lon: lon_at(self.x),
            // Saturating on both far edges: this is the same wire-decoded
            // pair [`Self::fits`] checks, and a wrapped far edge here would
            // answer ground *north-west* of the window's own origin rather
            // than refusing.
            max_lon: lon_at(self.x.saturating_add(self.width)),
            // Y is inverted: the window's top row is its NORTH edge.
            max_lat: lat_at(self.y),
            min_lat: lat_at(self.y.saturating_add(self.height)),
        }
    }
}

/// **The texel extent a rasterizer is about to paint into**, accumulated as it
/// locates its items and spent once, before the pixmap exists.
///
/// Every `add_*` takes a reach in **texels** and the reach is the caller's
/// claim, not this type's: a window narrower than what the painter goes on to
/// draw clips it, and a clipped overlay is a visible defect that no counter
/// reports. So each caller states the reach from the same figure its own cull
/// states, and [`ContentExtent::unbounded`] exists for the cases where no
/// figure can be stated at all — a feature carrying no `geo_bounds` is the
/// live one — which fall back to the whole picture rather than guess.
///
/// The three degenerate cases, each explicit:
///
/// * **Content spanning the viewport.** The window clamps to the grid, so the
///   answer is the whole picture and the pipeline is exactly what it was. This
///   is the case that must not get *worse*, and it cannot: the clamp is two
///   `min`s and the pixmap that follows is the one it always allocated.
/// * **No content at all.** [`Self::crop`] answers a 1x1 window rather than a
///   whole picture: the raster is going to settle blank either way, and a 4 B
///   buffer is what a blank costs when the door has already let the dispatch
///   through. `SourceHandler::paints_in` short-circuits most of these one
///   field earlier, before a job is built at all — that path is untouched, and
///   this is the residue it cannot see (a page of storm reports all later than
///   the depicted instant, every METAR off the texture).
/// * **Content in several distant clusters.** One window is what this
///   computes, so two clusters at opposite corners give a window close to the
///   viewport and close to no saving. That is deliberate and is not a defect:
///   a picture is one texture at one offset, and splitting it into several
///   would multiply the upload, the texture handle and the draw call per
///   layer. The fires counter reports the window's own bytes, so a scene whose
///   clusters spread reports itself as a small saving rather than as a claim.
#[derive(Debug, Clone, Copy)]
pub struct ContentExtent {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    any: bool,
    unbounded: bool,
}

impl Default for ContentExtent {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentExtent {
    pub const fn new() -> Self {
        Self {
            min_x: f32::INFINITY,
            min_y: f32::INFINITY,
            max_x: f32::NEG_INFINITY,
            max_y: f32::NEG_INFINITY,
            any: false,
            unbounded: false,
        }
    }

    /// **This rasterizer cannot state where it paints.** Every later `add_*` is
    /// still recorded and [`Self::crop`] still answers, but it answers `None` —
    /// the whole picture — because a window computed from a partial extent is
    /// a clip.
    pub fn unbounded(&mut self) {
        self.unbounded = true;
    }

    /// An item at `(x, y)` reaching `reach` texels in every direction.
    pub fn add_point(&mut self, x: f32, y: f32, reach: f32) {
        self.add_rect(x - reach, y - reach, x + reach, y + reach);
    }

    /// A box in texels, already grown by whatever the painter adds to it.
    pub fn add_rect(&mut self, min_x: f32, min_y: f32, max_x: f32, max_y: f32) {
        if !(min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()) {
            // A non-finite coordinate is a projection that failed, and a
            // window grown to `NaN` swallows every comparison silently. The
            // safe direction is the whole picture.
            self.unbounded();
            return;
        }
        self.min_x = self.min_x.min(min_x);
        self.min_y = self.min_y.min(min_y);
        self.max_x = self.max_x.max(max_x);
        self.max_y = self.max_y.max(max_y);
        self.any = true;
    }

    /// Every projected point of a ring, grown by `reach`.
    pub fn add_points(&mut self, pts: &[(f32, f32)], reach: f32) {
        for &(x, y) in pts {
            self.add_point(x, y, reach);
        }
    }

    /// The window to allocate, or `None` for the whole `width` x `height`
    /// picture.
    ///
    /// The edges are taken **outward** — `floor` on the near side, `ceil` on
    /// the far — so the window can only be wider than the extent and never
    /// narrower, and then grown by [`CROP_GUARD_TEXELS`] on every side.
    pub fn crop(&self, width: u32, height: u32) -> Option<PictureCrop> {
        if width == 0 || height == 0 || self.unbounded {
            return None;
        }
        if !self.any {
            // Nothing to paint. The raster settles blank; this is the smallest
            // buffer that can carry that answer.
            return Some(PictureCrop {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                of_width: width,
                of_height: height,
            });
        }
        let guard = CROP_GUARD_TEXELS as f32;
        let x0 = (self.min_x - guard).floor().max(0.0) as u32;
        let y0 = (self.min_y - guard).floor().max(0.0) as u32;
        // `saturating_add` and not `+`: `as u32` saturates a huge-but-finite
        // extent at `u32::MAX` (`add_rect` refuses only the non-finite ones),
        // and `u32::MAX + 1` wraps to `0` under the shipped profile — which
        // `.min(width)` then reads as a far edge of zero and the window below
        // collapses to one texel. That is a *clipped* overlay, the defect this
        // whole rounding-outward exists to avoid, arrived at by wrapping.
        let x1 = (self.max_x + guard).ceil().max(0.0) as u32;
        let y1 = (self.max_y + guard).ceil().max(0.0) as u32;
        let x1 = x1.saturating_add(1).min(width);
        let y1 = y1.saturating_add(1).min(height);
        let x0 = x0.min(width.saturating_sub(1));
        let y0 = y0.min(height.saturating_sub(1));
        Some(PictureCrop {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0).max(1),
            height: y1.saturating_sub(y0).max(1),
            of_width: width,
            of_height: height,
        })
    }
}

/// The transparent margin every window keeps around its content, in texels.
///
/// **It is about the composite, not about the raster.** The picture is drawn
/// with `TextureOptions::LINEAR` and its own screen rect, so the sampler reads
/// half a texel outside the outermost texel row at each edge and clamps. In the
/// whole-viewport picture what lies there is the transparent texel the content
/// stops short of; in a window cut flush to the content it would be the content
/// itself, smeared one half-texel outward along the seam. One transparent row
/// on every side makes the two the same read.
///
/// One and not two: the guard is a floor under the rounding above, which
/// already takes both edges outward, and every texel of it is a texel
/// allocated. A second guard texel on a 40 x 40 window is 9.8 % more picture
/// for nothing — 44² against 42².
pub const CROP_GUARD_TEXELS: u32 = 1;

/// **Whether a rasterizer may cut its picture down to its content.**
///
/// It is an argument and not a `cfg` or a hidden switch, because the whole
/// claim this feature rests on is that the two answers are the *same picture* —
/// and a claim like that is worth only as much as the thing that can ask for
/// both and compare them. `crop_identity_tests` does exactly that, over every
/// scene it can build, and it could not exist if `Whole` were unreachable.
///
/// Production passes [`Self::Content`] everywhere; the four-argument
/// `rasterize_*` names below are that call, kept so nothing else in the tree
/// has to say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropPolicy {
    /// Allocate the bounding box of what will be painted.
    Content,
    /// Allocate the whole dispatched grid, as every row did before windows
    /// existed. The reference arm of the identity gate.
    Whole,
}

impl CropPolicy {
    /// The window this policy allows, given the one the content asks for.
    fn allow(self, crop: Option<PictureCrop>) -> Option<PictureCrop> {
        match self {
            Self::Content => crop,
            Self::Whole => None,
        }
    }
}

/// The pixmap dimensions and the drawing origin a window implies — and the
/// whole picture's when there is none, which is what makes `None` cost every
/// caller a subtraction of zero and nothing else.
///
/// The origin comes back as `f32` because that is what it is subtracted from.
/// It is a whole number of texels, and that is the load-bearing part: `px - ox`
/// for an integral `ox` no larger in magnitude than `px` is **exact** in binary
/// floating point, so the coordinate the window is painted at is the same
/// coordinate the whole picture would have painted at, to the bit. A
/// fractional origin would resample every path and the two pictures would
/// differ in the anti-aliased fringe of every edge.
fn crop_dims(crop: Option<PictureCrop>, width: u32, height: u32) -> (u32, u32, f32, f32) {
    match crop {
        Some(c) => (c.width, c.height, c.x as f32, c.y as f32),
        None => (width, height, 0.0, 0.0),
    }
}

/// **Why a raster painted nothing.**
///
/// A blank is a *clear*: `OverlayTextureCache::show_blank` takes the picture
/// off the glass. So the difference between "the view left this layer's
/// ground" and "the window collapsed while the layer still covered the view"
/// is the difference between correct behaviour and a user watching data
/// disappear — and until this enum existed the always-on counters recorded
/// that a picture was blank and nothing whatever about which of the two it
/// was. A high blank rate was readable only as waste.
///
/// **This is also what keeps two different defects apart.** A blank that names
/// its reason cannot be confused with a picture that was never dispatched: the
/// first is a pane cleared, the second a pane never filled, and both end in
/// "nothing on screen". They have already been attributed to each other once.
/// [`crate::render::rasterize::BlankReason`] counts only the first, because
/// only the first reaches this type at all.
///
/// **Armed where the decision is made, never inferred downstream.** Each
/// variant is written at the branch that took it, and
/// [`RasterizeOutput::settle_blank`] spends what it finds. A reason
/// recomputed from the window's shape after the fact is precisely the failure
/// this exists to make visible: it would agree with the code that produced it
/// by construction, and so could never disagree with it. Same posture as
/// `squallar_egui::overlay_cache::RerenderReason`, which splits the dispatch
/// count the same way.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BlankReason {
    /// There was nothing to draw *from* or nothing to draw *onto*: no values,
    /// a zero-sized grid, or a zero-sized texture.
    EmptyInput,
    /// The field is one this build registers no colours for — a refusal, not
    /// an absence. Painting it through some other field's scale would be a
    /// silent misread, so the raster declines and says so here.
    UnknownField,
    /// The projection window collapsed: no grid index range to walk.
    WindowEmpty,
    /// The window held grid cells and **not one carried a paintable value** —
    /// every one non-finite or fully transparent. A gap in the data, over
    /// ground the grid does cover.
    NoDataInWindow,
    /// The window held paintable values and **not one of them landed on this
    /// texture**. The view is off this layer's ground: a geostationary
    /// composite panned past the top of its own latitude axis, say. **This is
    /// the one blank that is correct.**
    ///
    /// **Far narrower in practice than that sentence reads, and measured so.**
    /// Over a 32-leg browser arm on 2026-09-08 it fired **24 times in 3,294
    /// blanks — 0.7 %**, and the reason is upstream of it: `projection_window`
    /// carries the box into the grid's own longitude frame and
    /// `GridCoords::index_bounds` *clamps to an empty window* when the box
    /// falls off the grid, so the ordinary "the view left this regional grid"
    /// case returns [`Self::WindowEmpty`] before the cell walk that could
    /// reach here ever starts. What is left for this variant is the case where
    /// the window is **not** empty and its `interior` ring is — a window
    /// pinned against the grid's own edge, which is the latitude-edge case
    /// `rasterize_gridded` names.
    ///
    /// So the excluded set is small and
    /// [`crate::render::rasterize::BlankReason::clears_covered_ground`] is
    /// **more** conservative than reading its own doc suggests: most correct
    /// off-grid clears are counted against covered ground under
    /// `window-empty`. That is the safe direction, and it is why a `covered`
    /// figure is a ceiling on the defect and never a measurement of it.
    OutsideCoverage,
    /// **No raster ran at all**: the handler's own `paints_in` answered that
    /// this layer cannot put a pixel in the bounds being rendered, so the
    /// dispatch was short-circuited to a blank on the page and no paint input,
    /// hit list, wire message or pixmap was ever built.
    ///
    /// **Its own variant rather than [`Self::OutsideCoverage`], which it looks
    /// like.** The two are different claims taken on different sides by
    /// different predicates: this one is the handler *declaring* it has no ink
    /// here, before anything is built; `OutsideCoverage` is the cell walk
    /// *observing* that nothing landed. Folding them would hide a wrong
    /// `paints_in` inside a count of correct blanks, which is the exact shape
    /// of confusion this enum exists to prevent — and it is the one variant
    /// whose truth is a claim and not a reading, because nothing downstream
    /// checks the handler's answer.
    ///
    /// **What the tree actually does, which is better than "unchecked".** All
    /// three implementors of `SourceHandler::paints_in` — alerts, SPC outlooks
    /// and fire weather — route through
    /// [`any_feature_paints_in`](crate::render::rasterize::any_feature_paints_in),
    /// which is `feature_survives_cull` over the features, and
    /// `feature_survives_cull` is the **identical predicate `draw_feature`
    /// culls with**. One function, not two spellings, and that is deliberate
    /// (see `any_feature_paints_in`). So on these three handlers the refusal
    /// is provably what the draw loop would have done, and the blank is a
    /// correct clear for the same reason [`Self::OutsideView`] is.
    ///
    /// **The subtotal still counts it, and that is not an oversight.** What
    /// the paragraph above establishes is a property of *today's three
    /// implementors*, not of the trait: `paints_in` is the extension point a
    /// new source overrides, and a fourth implementor answering from its own
    /// arithmetic would land here with nothing to check it. The variant is a
    /// *claim slot*, so it is the claim and not today's occupants that decides
    /// which side of the subtotal it sits on. A reader who has read the three
    /// implementations can subtract this count; the line prints it beside the
    /// subtotal for exactly that.
    ExtentDeclaredEmpty,
    /// **Nothing survived a filter that is not about where the view is**: an
    /// alert whose category the pane has switched off or whose id is hidden, a
    /// storm report or a lightning flash later than the depicted instant, a
    /// flash older than the pane's own time window. The layer holds data and
    /// the view is over it; the pane's own settings, or the depicted time,
    /// removed every row.
    ///
    /// **Its own variant rather than [`Self::EmptyInput`], which it arrives
    /// looking like.** `EmptyInput` says the raster was handed nothing;
    /// this says it was handed rows and threw them all away, and the two are a
    /// missing fetch against a working filter. It is also the shape of the one
    /// blank a scrubbing user produces on purpose — every flash in the window
    /// is in the future — so folding it into a geographic reason would file a
    /// correct clear as a pan defect and a pan defect as a correct clear on
    /// alternate frames.
    FilteredOut,
    /// **Every item was resolved and not one of them reached this texture.**
    /// The point rasterizers' analogue of [`Self::OutsideCoverage`]: each
    /// station, report, flash or coverage disc was projected and tested
    /// against the texture rect, each feature's extent was carried into the
    /// texture's frame and intersected with it, and every one missed.
    ///
    /// **Deliberately NOT folded into [`Self::OutsideCoverage`], and it counts
    /// against covered ground.** For the four point rows the observation is as
    /// strong as the cell walk's — a real latitude and longitude went through
    /// `MercatorBounds::project` and the answer landed off the texture. For
    /// the two feature rows it is not: `feature_survives_cull` tests
    /// [`OverlayFeature::geo_bounds`], a bounding box the source computed, so a
    /// wrong extent files itself here. One variant spanning both is kept on the
    /// conservative side of that split rather than two, because a wrong extent
    /// hidden inside a "correct clear" is the exact failure
    /// [`Self::ExtentDeclaredEmpty`] is documented against. Split it the day a
    /// reading makes the difference worth two counters.
    OutsideView,
    /// **Items reached this texture and none of them put a pixel down.** Not
    /// an absence and not a pan: the raster had rows, they were neither
    /// filtered out nor found off the texture, and the pixmap came back with
    /// no ink in it. A fill and a stroke that are both fully transparent, a
    /// ring that degenerates to fewer than three points, a coverage disc that
    /// projects below a texel — and, when none of those explain it, a painter
    /// that has stopped painting.
    ///
    /// The item rasterizers' analogue of [`Self::NoDataInWindow`], kept apart
    /// from it because the mechanisms do not resemble each other: that one is
    /// a grid cell carrying no finite value, this one is a draw call that
    /// changed nothing. **This is the residue arm** — a rasterizer that can
    /// place its items and still paints nothing lands here rather than in
    /// [`Self::Unattributed`], so a reader knows the geometry was in range.
    DrewNoInk,
    /// **The pixmap could not be created at a non-zero size.** `Pixmap::new`
    /// answered `None` for a width and height that are both positive, which is
    /// an allocation this target refused; the raster gives back a zeroed
    /// buffer and this reason. A zero-sized texture is [`Self::EmptyInput`]
    /// and never this, because that one is an input that says nothing to draw
    /// onto and this one is a request the allocator would not serve.
    AllocationFailed,
    /// A raster settled blank with no reason armed.
    ///
    /// **Not zero in a healthy tree, unlike its `RerenderReason` counterpart**
    /// — and the set that lands here is now much smaller than it was. Until
    /// 2026-09-09 only the gridded row armed, so the seven rasterizers behind
    /// the other eight overlay handlers all fell here: `Unattributed` was
    /// **67.5 % of all blanks** pooled over a 32-leg browser arm, and 100 % of
    /// blanks in every leg whose camera stayed over the data. All seven arm
    /// now. What is left is a rasterizer that reaches
    /// [`RasterizeOutput::settle_blank`] having armed nothing at all, which
    /// today means only a rasterizer nobody has been through — radar's plan
    /// view is not in this funnel and never reaches here.
    ///
    /// Counted as its own variant rather than folded into a neighbour, so the
    /// hole is visible instead of silently inflating whichever reason it was
    /// merged with. **It must stay a real counted variant**: the day it is
    /// quietly mapped onto a plausible name is the day the next gap is
    /// invisible.
    Unattributed,
}

impl BlankReason {
    /// Every variant, in the order [`Self::index`] assigns.
    pub const ALL: [Self; Self::COUNT] = [
        Self::EmptyInput,
        Self::UnknownField,
        Self::WindowEmpty,
        Self::NoDataInWindow,
        Self::OutsideCoverage,
        Self::ExtentDeclaredEmpty,
        Self::FilteredOut,
        Self::OutsideView,
        Self::DrewNoInk,
        Self::AllocationFailed,
        Self::Unattributed,
    ];

    /// How many variants there are — the width of the ledger's counter array.
    pub const COUNT: usize = 11;

    /// This variant's slot in the ledger's array.
    ///
    /// **Not [`Self::wire_code`].** The two agreed until `ExtentDeclaredEmpty`
    /// was appended: the wire's numbers may never move, so that variant took
    /// code 6 and sits at index 5 of a 7-wide array. Spelling the ledger's
    /// index as the wire's code would leave a permanent hole at index 5 and
    /// put a variant past the end of the array the day another is appended.
    pub const fn index(self) -> usize {
        match self {
            Self::EmptyInput => 0,
            Self::UnknownField => 1,
            Self::WindowEmpty => 2,
            Self::NoDataInWindow => 3,
            Self::OutsideCoverage => 4,
            Self::ExtentDeclaredEmpty => 5,
            Self::FilteredOut => 6,
            Self::OutsideView => 7,
            Self::DrewNoInk => 8,
            Self::AllocationFailed => 9,
            Self::Unattributed => 10,
        }
    }

    /// A short name for a log line. Stable — the Tier-2 rig reads these.
    pub const fn name(self) -> &'static str {
        match self {
            Self::EmptyInput => "empty-input",
            Self::UnknownField => "unknown-field",
            Self::WindowEmpty => "window-empty",
            Self::NoDataInWindow => "no-data",
            Self::OutsideCoverage => "outside-coverage",
            Self::ExtentDeclaredEmpty => "extent-declared-empty",
            Self::FilteredOut => "filtered-out",
            Self::OutsideView => "outside-view",
            Self::DrewNoInk => "drew-no-ink",
            Self::AllocationFailed => "allocation-failed",
            Self::Unattributed => "unattributed",
        }
    }

    /// **Whether a blank for this reason has NOT been shown to be correct.**
    ///
    /// [`Self::OutsideCoverage`] is the one reason a clear is proven right,
    /// and it is proven by an *observation*: the cell walk resolved paintable
    /// values and watched none of them land on the texture. Every other reason
    /// counts here, and the counted set is deliberately wider than "known
    /// wrong".
    ///
    /// **[`Self::ExtentDeclaredEmpty`] counts, and it is the one that looks
    /// like it should not.** It says the layer is absent here, which if true
    /// is a correct clear — but it is the handler's own *claim*, taken before
    /// anything was built and checked by nothing downstream. Excluding it
    /// would file every wrong `paints_in` as a correct blank, and a leg whose
    /// blanks were all wrong `paints_in` refusals would report **zero** over
    /// covered ground: the counter would read perfect at the exact moment it
    /// was needed. `SourceHandler::paints_in`'s own doc names that direction
    /// as the one it may not be wrong in — "a wrong `false` clears a pane that
    /// should have had ink".
    ///
    /// So the subtotal is **conservative by construction**: blanks not shown
    /// to be correct, not blanks shown to be wrong. A reader who trusts a
    /// handler can subtract its `extent-declared-empty` count, which the
    /// `overlay blanks:` line prints beside the subtotal for exactly that.
    /// The reverse — recovering a hidden refusal from a subtotal that already
    /// absorbed it — is not possible at all.
    ///
    /// **Measured, and more conservative than the paragraphs above admit.**
    /// The excluded variant is nearly unreachable: 24 of 3,294 blanks over a
    /// 32-leg browser arm on 2026-09-08, because the ordinary off-grid case
    /// exits at [`Self::WindowEmpty`] a step earlier — see
    /// [`Self::OutsideCoverage`], which carries the mechanism. So this
    /// predicate answers `true` for very nearly every blank a real run
    /// produces, and the subtotal it feeds is a **ceiling** on the pictures a
    /// user lost, never a count of them. Read a fall in it as progress and a
    /// figure in it as an upper bound; do not read it as a defect count.
    ///
    /// **All four reasons added on 2026-09-09 count here**, including
    /// [`Self::OutsideView`], which for the four point rasterizers is as
    /// well-observed as `OutsideCoverage`. That variant's own doc says why it
    /// was not admitted to the correct set instead: the two feature rows share
    /// it and rest on an extent the source computed.
    pub const fn clears_covered_ground(self) -> bool {
        !matches!(self, Self::OutsideCoverage)
    }

    /// The wire's spelling, and the ledger's index. Explicit rather than
    /// `as u8` so a reordering of the variants cannot silently renumber a byte
    /// two builds exchange.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::EmptyInput => 0,
            Self::UnknownField => 1,
            Self::WindowEmpty => 2,
            Self::NoDataInWindow => 3,
            Self::OutsideCoverage => 4,
            // **Appended, not inserted.** Codes 0..=5 keep the numbers they
            // shipped with, so a reply written by a build that predates this
            // variant decodes to the reason it meant.
            Self::ExtentDeclaredEmpty => 6,
            Self::FilteredOut => 7,
            Self::OutsideView => 8,
            Self::DrewNoInk => 9,
            Self::AllocationFailed => 10,
            Self::Unattributed => 5,
        }
    }

    /// The inverse of [`Self::wire_code`]; `None` for a code this build does
    /// not know, which a decoder must refuse rather than default.
    pub const fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::EmptyInput),
            1 => Some(Self::UnknownField),
            2 => Some(Self::WindowEmpty),
            3 => Some(Self::NoDataInWindow),
            4 => Some(Self::OutsideCoverage),
            5 => Some(Self::Unattributed),
            6 => Some(Self::ExtentDeclaredEmpty),
            7 => Some(Self::FilteredOut),
            8 => Some(Self::OutsideView),
            9 => Some(Self::DrewNoInk),
            10 => Some(Self::AllocationFailed),
            _ => None,
        }
    }
}

pub struct RasterizeOutput {
    /// The picture's premultiplied bytes — **empty when `blank` is `Some`**.
    ///
    /// A [`RasterBuf`] rather than a `Vec<u8>` because which of the two
    /// layouts the bytes are in is the producer's to decide and no consumer's
    /// to care about: it derefs to `[u8]`, so every reader below and every
    /// reader downstream is unchanged, and the one consumer that wants pixels
    /// takes them by move. See [`RasterBuf`] for why the layout cannot be
    /// changed after the fact.
    pub rgba: RasterBuf,
    pub hit_cells: Option<HitCells>,
    pub alpha: AlphaMode,
    /// `Some(len)` when this raster has been judged and had no ink in it:
    /// `rgba` is then empty and `len` is the byte length the picture *would*
    /// have had.
    ///
    /// **Written in exactly one place**, [`Self::settle_blank`], which the job
    /// funnel's output stage calls once per reply after the premultiply. Every
    /// other producer leaves it `None`, which says "the pixels are in `rgba`"
    /// and is what an unjudged raster means; nothing downstream re-decides.
    ///
    /// The length rather than a bare flag, because it is the term the arrival
    /// checks the answer's size by: a handler answering the wrong size is a
    /// failed render, and a blank has to be separable from one.
    pub blank: Option<u32>,
    /// **Why**, when this raster turns out to have painted nothing — armed by
    /// the branch that decided it and spent by [`Self::settle_blank`].
    ///
    /// Independent of `blank`, and deliberately so. `blank` is *whether* the
    /// buffer was given up, written in one place after the premultiply; this
    /// is *why there was nothing in it*, which only the rasterizer knows and
    /// only at the moment it stopped. A rasterizer that leaves this `None` and
    /// then paints nothing is counted [`BlankReason::Unattributed`] rather
    /// than guessed at.
    pub blank_reason: Option<BlankReason>,
    /// **The texel window of the dispatched grid these pixels are**, or `None`
    /// for the whole picture.
    ///
    /// Written by the rasterizer that allocated the pixmap and read by
    /// `App::overlay_job_deliver`, which sizes its answer check, its
    /// `ColorImage` and the pane's placement off it. `None` is what every row
    /// that has not adopted a window answers and what every row answers when
    /// its content spans the viewport, and it means exactly what it always
    /// meant: `width` x `height` texels at the origin.
    ///
    /// **`blank` is stated in the window's own terms.** A raster that settles
    /// blank gives up a buffer of `crop.bytes()`, not of the plan's, because
    /// `blank` is the length the *picture* would have had and the picture is
    /// the window. The arrival's size check reads both off this same field, so
    /// the two cannot disagree.
    ///
    /// See [`PictureCrop`].
    pub crop: Option<PictureCrop>,
}

impl std::fmt::Debug for RasterizeOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterizeOutput")
            .field("rgba_len", &self.rgba.len())
            .field(
                "hit_cells_occupied",
                &self.hit_cells.as_ref().map(|cells| cells.cells.len()),
            )
            .field("alpha", &self.alpha)
            .field("blank", &self.blank)
            .field("blank_reason", &self.blank_reason)
            .field("crop", &self.crop)
            .finish()
    }
}

impl RasterizeOutput {
    /// Give up a buffer that cannot change the frame it would be drawn on,
    /// keeping the length it would have had.
    ///
    /// **Its precondition is premultiplied bytes**, which is why the funnel
    /// calls it after the premultiply and not at the end of a rasterizer: a
    /// straight buffer may carry non-zero colour under a zero alpha, and
    /// [`has_ink`] would call that ink.
    ///
    /// Idempotent, and it never un-settles: a raster already judged blank is
    /// left alone, and one with ink keeps every byte.
    pub fn settle_blank(&mut self) {
        if self.blank.is_some() || has_ink(&self.rgba) {
            return;
        }
        if let Ok(len) = u32::try_from(self.rgba.len()) {
            self.blank = Some(len);
            self.rgba = RasterBuf::empty();
            // **Spent here, never decided here.** A rasterizer that armed a
            // reason has it recorded; one that did not is counted
            // [`BlankReason::Unattributed`] rather than given a plausible
            // reason this function would be inventing. Filling it in from the
            // buffer's shape is the failure mode the whole enum exists to
            // avoid — see [`BlankReason`].
            self.blank_reason.get_or_insert(BlankReason::Unattributed);
        }
    }

    /// Why this raster painted nothing, for a raster that has been settled
    /// blank; `None` for one that has ink in it.
    ///
    /// Reading it off a raster that is *not* blank would answer with the
    /// reason its producer armed against the possibility, which is a
    /// prediction and not a reading — so the blank is what gates the answer.
    pub fn blank_reason(&self) -> Option<BlankReason> {
        self.blank.and(self.blank_reason)
    }
}

impl squallar_source::job::JobOut for RasterizeOutput {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
        match self.alpha {
            AlphaMode::Premultiplied => Vec::new(),
            AlphaMode::Straight => {
                self.alpha = AlphaMode::Premultiplied;
                vec![self.rgba.as_mut_bytes()]
            }
        }
    }

    fn discard_blank_rasters(&mut self) {
        self.settle_blank();
    }
}

/// Web Mercator's own limit, from [`squallar_geo`].
const MAX_MERCATOR_LAT: f64 = squallar_geo::MERCATOR_LAT_LIMIT_DEG;

#[derive(Debug, Clone, Copy)]
pub(crate) struct MercatorBounds {
    min_lon: f64,
    max_lon: f64,
    merc_y_min: f64, // south edge
    merc_y_max: f64, // north edge
}

impl MercatorBounds {
    pub(crate) fn from_geo(bounds: &GeoBounds) -> Self {
        let clamped_min = bounds.min_lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
        let clamped_max = bounds.max_lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
        Self {
            min_lon: bounds.min_lon,
            max_lon: bounds.max_lon,
            merc_y_min: lat_rad_to_mercator_y(clamped_min.to_radians()),
            merc_y_max: lat_rad_to_mercator_y(clamped_max.to_radians()),
        }
    }

    /// Bring a longitude into this box's own frame: `[min_lon, min_lon + 360)`.
    #[inline]
    pub(crate) fn wrap_lon(&self, lon: f64) -> f64 {
        self.min_lon + (lon - self.min_lon).rem_euclid(360.0)
    }

    /// The whole multiple of 360° carrying a datum spanning `[min_lon, max_lon]`
    /// to its representation *nearest* this box — a rigid translation, only defined
    /// for a datum inside a half-turn.
    #[inline]
    pub(crate) fn lon_shift(&self, min_lon: f64, max_lon: f64) -> f64 {
        crate::render::geo::lon_shift(min_lon, max_lon, self.min_lon, self.max_lon)
    }

    #[inline]
    pub(crate) fn nearest_lon(&self, lon: f64) -> f64 {
        lon + self.lon_shift(lon, lon)
    }

    /// To texture pixel coordinates. Longitude is mapped linearly and no shift
    /// is applied here, so each caller states its own frame.
    #[inline]
    pub(crate) fn project(&self, lat: f64, lon: f64, w: f32, h: f32) -> (f32, f32) {
        let lon_frac = (lon - self.min_lon) / (self.max_lon - self.min_lon);
        let merc_y = lat_rad_to_mercator_y(lat.to_radians());
        let merc_frac = (merc_y - self.merc_y_min) / (self.merc_y_max - self.merc_y_min);
        let px = (lon_frac * w as f64) as f32;
        // Y is inverted: top of texture = north = max Mercator Y.
        let py = ((1.0 - merc_frac) * h as f64) as f32;
        (px, py)
    }
}

/// **What an item rasterizer counted while it drew, so it can say why it
/// painted nothing.**
///
/// The seven non-gridded rasterizers all have the same shape — a list of
/// items, a filter or two, a cull, a draw — and until 2026-09-09 not one of
/// them armed a [`BlankReason`], so every blank they produced was counted
/// `Unattributed`: **67.5 % of all blanks** over a 32-leg browser arm, and
/// 100 % of them in every leg whose camera stayed over the data.
///
/// **One tally and one classifier rather than seven**, and that is the point
/// of the type: seven hand-written `if` ladders would drift, and a reason is
/// only worth counting while every row spells it the same way. The counters
/// are three `u64`s kept in registers across the item loop and read once at
/// the end — the same shape `rasterize_gridded` keeps `drawn_cells` in.
///
/// **Counted at the branch that decides, never recomputed after.** Each
/// increment sits on the arm the loop actually took: the `continue` that a
/// filter took, the `continue` that a cull took, the fall-through that
/// reached a draw call. [`Self::reason`] does no geometry and reads no
/// pixels — it only names which arm ran out of items — so it cannot agree
/// with the loop by construction the way a reason re-derived from the
/// window's shape would.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ItemTally {
    /// Items dropped by a test that is **not about where the view is**: a
    /// category the pane switched off, a hidden id, a timestamp past the
    /// depicted instant, an age past the pane's window.
    filtered: u64,
    /// Items whose position or extent was resolved and found **off this
    /// texture** — projected past the rect and its slack, or an extent carried
    /// into the texture's frame that failed to intersect it.
    off_texture: u64,
    /// Items that survived everything and **reached a draw call on this
    /// texture**. Not "put ink down": whether the draw changed a pixel is
    /// `has_ink`'s answer later, and the difference between the two is exactly
    /// what [`BlankReason::DrewNoInk`] names.
    on_texture: u64,
}

impl ItemTally {
    /// One item removed by a non-geographic filter.
    fn filtered(&mut self) {
        self.filtered += 1;
    }

    /// One item resolved and found off this texture.
    fn off_texture(&mut self) {
        self.off_texture += 1;
    }

    /// One item that reached a draw call on this texture.
    fn on_texture(&mut self) {
        self.on_texture += 1;
    }

    /// **Why a raster with this tally would have painted nothing.**
    ///
    /// `items` is the length of the list the rasterizer was handed, which is
    /// the only term the tally cannot carry: a raster that counted nothing at
    /// all is either an empty list or a list of items that reached no branch,
    /// and those are different answers.
    ///
    /// The order of the arms is the order of the questions, and it is
    /// deliberate:
    ///
    /// * **Nothing to draw from** comes first, because an empty list makes
    ///   every other count zero and nothing else could be said.
    /// * **Something reached the texture** comes next, and it wins over both
    ///   culls: once one item was in range, "the view moved off the data" is
    ///   false whatever the other rows did, and what is left to explain is a
    ///   draw call that changed no pixel.
    /// * **Something was off the texture** before **everything was filtered**,
    ///   so a mixed raster — some rows in the future, some rows off screen —
    ///   is named by position rather than by time. Both count against covered
    ///   ground, so this ordering moves no figure across the subtotal; it
    ///   picks which of two true sentences is printed.
    /// * The fall-through is [`BlankReason::DrewNoInk`] and not
    ///   [`BlankReason::Unattributed`]: a list that is not empty and whose
    ///   items reached no branch at all is a degenerate row — a discussion
    ///   with no rings, a ring of two points — which is a painter that drew
    ///   nothing, not a rasterizer that armed nothing.
    fn reason(self, items: usize) -> BlankReason {
        if items == 0 {
            BlankReason::EmptyInput
        } else if self.on_texture == 0 && self.off_texture > 0 {
            BlankReason::OutsideView
        } else if self.on_texture == 0 && self.off_texture == 0 && self.filtered > 0 {
            BlankReason::FilteredOut
        } else {
            BlankReason::DrewNoInk
        }
    }
}

/// The reason a rasterizer gives back when `Pixmap::new` answers `None`.
///
/// **Two causes behind one `None`, and they are not the same fault.**
/// `tiny_skia` refuses a zero width or height, which is an input with nothing
/// to draw onto — [`BlankReason::EmptyInput`], exactly as the gridded row
/// spells a zero-sized texture. It also refuses a size whose buffer it cannot
/// take, which is [`BlankReason::AllocationFailed`]: the request was
/// well-formed and the allocator would not serve it. Reading the second as the
/// first would file a target running out of memory as an empty layer.
pub(crate) fn no_pixmap_reason(width: u32, height: u32) -> BlankReason {
    if width == 0 || height == 0 {
        BlankReason::EmptyInput
    } else {
        BlankReason::AllocationFailed
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutlooksInput {
    pub features: Vec<OverlayFeature>,
    pub hatch_color: [u8; 4],
    pub device_scale: f32,
}

squallar_source::impl_job_input!(OutlooksInput);

pub fn rasterize_spc_outlooks(
    input: &OutlooksInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let OutlooksInput {
        features,
        hatch_color,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_spc_outlooks ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            crop: None,
            rgba: vec![0u8; (width * height * 4) as usize].into(),
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
            blank_reason: Some(no_pixmap_reason(width, height)),
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // Two passes: hatching must go over every fill, including later features.
    let mut tally = ItemTally::default();
    for feature in features {
        if draw_feature(&mut pixmap, feature, &mb, w, h, scale) {
            tally.on_texture();
        } else {
            tally.off_texture();
        }
    }
    crate::render::hatch::draw_hatch_pass(&mut pixmap, features, &mb, w, h, *hatch_color);

    RasterizeOutput {
        crop: None,
        rgba: pixmap.take().into(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
        blank: None,
        // **The hatch pass is not tallied, and does not need to be.** It walks
        // the same `features` and applies no cull of its own, but
        // `OverlayFeature::geo_bounds` is computed from the very polygons it
        // projects — so a feature `draw_feature` culled cannot put hatching on
        // this texture either, and `off_texture` stays true of it. See
        // `feature_survives_cull`, which carries that argument and the note to
        // re-check it if the hatch pass ever paints something not derived from
        // `feature.polygons`.
        blank_reason: Some(tally.reason(features.len())),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionPaint {
    pub md_type: crate::spc::discussion::MdType,
    pub polygon: squallar_geo::GeoPolygon,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionsInput {
    pub discussions: Vec<DiscussionPaint>,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(DiscussionsInput);

pub fn rasterize_spc_discussions(
    input: &DiscussionsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let DiscussionsInput {
        discussions,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_spc_discussions ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            crop: None,
            rgba: vec![0u8; (width * height * 4) as usize].into(),
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
            blank_reason: Some(no_pixmap_reason(width, height)),
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let mut tally = ItemTally::default();
    for md in discussions {
        let fill_rgba = md_fill_color(&md.md_type);
        let stroke_rgba = md_stroke_color(&md.md_type);

        for ring in &md.polygon {
            if ring.len() < 3 {
                continue;
            }
            // No longitude shift here: `spc::discussion::parse_coord_token`
            // drops any point outside `(-140.0..=-50.0)`.
            let pts: Vec<(f32, f32)> = ring
                .iter()
                .map(|&(lat, lon)| mb.project(lat, lon, w, h))
                .collect();
            if let Some(path) = build_polygon_path(&pts) {
                // **This row has no cull of its own**, so the tally's position
                // question is asked here, off the path already in hand: every
                // other rasterizer answers it with the `continue` it takes,
                // and this one projects every ring and lets `tiny_skia` clip.
                // `Path::bounds` is the projected extent, so this is the same
                // observation the clipper is about to make and not a
                // re-derivation of it after the fact — which is why it sits
                // before the fill rather than after the pixmap is read.
                if path_reaches_texture(&path, w, h) {
                    tally.on_texture();
                } else {
                    tally.off_texture();
                }
                fill_path(&mut pixmap, &path, fill_rgba, FillRule::Winding);
                let sw = scaled_stroke_width(&path, 2.0, scale);
                stroke_path(&mut pixmap, &path, stroke_rgba, sw);
            }
        }
    }

    RasterizeOutput {
        crop: None,
        rgba: pixmap.take().into(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
        blank: None,
        // **The item count is discussions, not rings.** A discussion carrying
        // no ring the loop could build a path from counts on neither arm of the
        // tally, so a page of them lands on `reason`'s fall-through — which is
        // `DrewNoInk`, and is what a degenerate polygon is.
        blank_reason: Some(tally.reason(discussions.len())),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertPaint {
    pub id: String,
    pub category: AlertCategory,
    pub features: Arc<Vec<OverlayFeature>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertsInput {
    pub alerts: Vec<AlertPaint>,
    pub enabled_categories: Vec<AlertCategory>,
    pub hidden_ids: HashSet<String>,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(AlertsInput);

pub fn rasterize_nws_alerts(
    input: &AlertsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    rasterize_nws_alerts_windowed(input, bounds, width, height, CropPolicy::Content)
}

/// [`rasterize_nws_alerts`] with the window policy stated. See [`CropPolicy`].
pub fn rasterize_nws_alerts_windowed(
    input: &AlertsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    policy: CropPolicy,
) -> RasterizeOutput {
    let AlertsInput {
        alerts,
        enabled_categories,
        hidden_ids,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // **Tallied per alert and not per feature**, so the denominator below is
    // the `alerts` list the caller handed over. A multi-polygon alert with one
    // feature on the texture is one item in range, which is what
    // `reason`'s "something reached the texture" question means.
    let mut tally = ItemTally::default();

    // **Projected first, painted second, and the window cut between them.**
    //
    // The window is taken from the polygons' own **projected** points and not
    // from `OverlayFeature::geo_bounds`, and the difference is a real one
    // rather than a matter of taste: `feature_survives_cull` shifts a feature's
    // geo box by `lon_shift` of the whole *feature*, while `project_polygon`
    // shifts each polygon by `ring_lon_shift` of its own *exterior ring*. On a
    // multi-polygon feature straddling the antimeridian those are two different
    // whole turns, so a window cut from the feature's box can sit 360° from the
    // polygon that gets drawn into it — and what that looks like on screen is a
    // county-shaped hole where an alert should be. Cutting the window from the
    // points that are about to be painted cannot make that mistake, because
    // there is only one projection and both passes read it.
    //
    // The projected rings are kept rather than re-projected. That is a real
    // transient — a national alert page is on the order of a megabyte of
    // `(f32, f32)` — but it is a fraction of the 17,971,200 B picture it is
    // buying back, and it replaces a per-polygon transient the old loop already
    // paid one polygon at a time.
    let mut painted: Vec<([u8; 4], [u8; 4], ProjectedPolygon)> = Vec::new();
    let mut extent = ContentExtent::new();
    for alert in alerts {
        if !enabled_categories.contains(&alert.category) || hidden_ids.contains(&alert.id) {
            // A category the pane switched off or an id the user hid: the
            // layer holds this alert and the view may well be over it.
            tally.filtered();
            continue;
        }
        let mut reached = false;
        for feature in alert.features.iter() {
            // **The cull's verdict, exactly as `draw_feature` returns it** —
            // `true` means the feature survived and its polygons were handed
            // over, and says nothing about whether a pixel changed. Splitting
            // the draw in two may not move that line; see `draw_feature`.
            if !feature_survives_cull(feature, &mb) {
                continue;
            }
            reached = true;
            for polygon in &feature.polygons {
                let Some(projected) = project_polygon(polygon, &mb, w, h) else {
                    continue;
                };
                extent.add_points(&projected.exterior, stroke_reach(feature, scale));
                painted.push((feature.fill_rgba, feature.stroke_rgba, projected));
            }
        }
        if reached {
            tally.on_texture();
        } else if !alert.features.is_empty() {
            tally.off_texture();
        }
        // An alert carrying no feature at all counts on neither arm: nothing
        // was filtered and nothing was located, so a page of them falls
        // through to `DrewNoInk` rather than claiming the view moved off
        // geometry that was never there.
    }

    let crop = policy.allow(extent.crop(width, height));
    let (cw, ch, ox, oy) = crop_dims(crop, width, height);
    let Some(mut pixmap) = Pixmap::new(cw, ch) else {
        log::error!(
            "Pixmap allocation failed in rasterize_nws_alerts ({}×{})",
            cw,
            ch
        );
        return RasterizeOutput {
            crop,
            rgba: vec![0u8; (cw as usize) * (ch as usize) * 4].into(),
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
            blank_reason: Some(no_pixmap_reason(cw, ch)),
        };
    };

    for (fill_rgba, stroke_rgba, mut projected) in painted {
        // Into the window's frame, in place. A whole number of texels off every
        // coordinate, which is exact — see [`crop_dims`] — so the path this
        // builds is the whole picture's path translated and not a resampling of
        // it. `build_filled_polygon_path`'s hole tests are areas and perimeters
        // and `scaled_stroke_width` reads a bounding box's width and height, all
        // of which a translation leaves alone.
        shift_polygon(&mut projected, ox, oy);
        if let Some((path, rule)) = build_filled_polygon_path(&projected.exterior, &projected.holes)
        {
            fill_path(&mut pixmap, &path, fill_rgba, rule);
            if stroke_rgba[3] > 0 {
                let sw = scaled_stroke_width(&path, 1.5, scale);
                stroke_path(&mut pixmap, &path, stroke_rgba, sw);
            }
        }
    }

    RasterizeOutput {
        crop,
        rgba: pixmap.take().into(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
        blank: None,
        blank_reason: Some(tally.reason(alerts.len())),
    }
}

/// How far past its own vertices a feature's outline can reach, in texels.
///
/// `scaled_stroke_width` caps the width at `base * scale` — `base` is 1.5 at
/// every call site here — and the stroke is centred on the path, so half of it
/// lies outside. **The factor of four is the miter**: `Stroke::default()` joins
/// with `LineJoin::Miter` at the default `miter_limit` of 4.0, and a sharp
/// corner in a county boundary extends the join up to that multiple of the
/// half-width beyond the vertex itself. A polygon layer is full of sharp
/// corners, so this is the term that actually decides the window's margin, and
/// leaving it out would shave a few texels off the point of every spike.
///
/// A feature with a transparent outline strokes nothing and reaches only its
/// own anti-aliased fringe.
fn stroke_reach(feature: &OverlayFeature, scale: f32) -> f32 {
    if feature.stroke_rgba[3] == 0 {
        return AA_FRINGE_TEXELS;
    }
    1.5 * scale * 0.5 * 4.0 + AA_FRINGE_TEXELS
}

/// Move a projected polygon into a window's frame, in place.
fn shift_polygon(projected: &mut ProjectedPolygon, ox: f32, oy: f32) {
    if ox == 0.0 && oy == 0.0 {
        return;
    }
    for point in projected.exterior.iter_mut() {
        point.0 -= ox;
        point.1 -= oy;
    }
    for hole in projected.holes.iter_mut() {
        for point in hole.iter_mut() {
            point.0 -= ox;
            point.1 -= oy;
        }
    }
}

/// One station, as the coverage wash needs it: a position and nothing else.
///
/// **No name and no role.** The wash is the network's, not any one station's,
/// so which radar the pane is on cannot change a texel of it — which is what
/// lets two panes at the same viewport share one raster, and what keeps the
/// input free of the pane read the old sites job needed.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageSite {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoverageInput {
    pub sites: Vec<CoverageSite>,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(CoverageInput);

/// A WSR-88D's nominal coverage radius, 230 km, in degrees of latitude.
///
/// 230 km rather than the 460 km the long-range reflectivity sweep reaches:
/// 230 is the range every base product shares, so it is the distance at which
/// "is this storm inside a radar's coverage" has one answer instead of one per
/// product.
/// **Public because there is exactly one of it.** The selected station's ring
/// is painted per frame by `squallar_egui::site_marker`, in points off the live
/// projector, while the network-wide coverage wash below is ground in a raster.
/// Two painters, one radius: a second spelling in the frontend is what
/// `geodesy_one_definition` exists to refuse, and a 230/111.32 written from
/// memory is ~250 m wrong on this ring.
pub const COVERAGE_RADIUS_DEG_LAT: f64 = 230.0 / squallar_geo::KM_PER_DEGREE_LAT;

/// The coverage wash's outline width, in texels before density. A hairline: it
/// is the edge of the covered region, not a ring around a station.
const COVERAGE_EDGE_WIDTH: f32 = 1.0;

/// **Where the radar network can see, as ground.**
///
/// Every station's 230 km disc, filled **as one path under the non-zero winding
/// rule**, so the overlaps merge into a single region instead of stacking into
/// 160 outlines. That is the difference between this and what it replaced: the
/// old raster stroked each station's ring separately, and at continental zoom
/// the result was a mesh of intersecting circles with the map invisible under
/// it. One filled region has one edge — the boundary of national coverage —
/// which is the thing the 230 km figure was chosen to answer.
///
/// **Nothing about the pane reaches this.** No station is coloured for being
/// current or loading, because the wash is the network's; the markers say which
/// radar the pane is on, in screen space, and the selected station's own ring is
/// painted there too.
pub fn rasterize_radar_coverage(
    input: &CoverageInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    rasterize_radar_coverage_windowed(input, bounds, width, height, CropPolicy::Content)
}

/// [`rasterize_radar_coverage`] with the window policy stated. See [`CropPolicy`].
pub fn rasterize_radar_coverage_windowed(
    input: &CoverageInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    policy: CropPolicy,
) -> RasterizeOutput {
    let CoverageInput {
        sites,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // **Located before anything is allocated.** The wash is one filled path
    // over every station's disc, so where it paints is settled once the discs
    // are projected and before a texel exists; the pixmap below is allocated at
    // that window rather than at the viewport. See [`PictureCrop`].
    //
    // The discs are kept rather than re-projected for the paint pass. At 208
    // stations that is 2,496 B against a second walk of `project` and
    // `lat_rad_to_mercator_y` — and, the part that matters, it is the *same*
    // `f32` triple both passes read, so the window cannot be computed off one
    // projection and painted off another.
    let mut discs: Vec<(f32, f32, f32)> = Vec::new();
    let mut extent = ContentExtent::new();
    let mut tally = ItemTally::default();
    for site in sites {
        // Into the viewport's frame first: the catalogue folds longitude into
        // [-180, 180] while `bounds` is unfolded; 4 of 208 stations are east.
        let lon = mb.nearest_lon(site.lon);
        let (px, py) = mb.project(site.lat, lon, w, h);

        // The radius in texels, taken by projecting a point one coverage radius
        // due north of the station and measuring. Web Mercator is conformal, so
        // a circle this small comes back a circle rather than an ellipse, and
        // the north offset is a faithful radius in every direction. Latitude
        // scaling is therefore handled for free: the same 230 km is more texels
        // at Nome than at Key West, which is what the ground looks like on this
        // projection.
        let (_, py_north) = mb.project(site.lat + COVERAGE_RADIUS_DEG_LAT, lon, w, h);
        let radius = (py - py_north).abs();

        // Cull on the disc, not on the station: a radar whose antenna is off the
        // texture still covers ground that is on it.
        if px < -radius || px > w + radius || py < -radius || py > h + radius {
            tally.off_texture();
            continue;
        }
        // A sub-texel disc contributes nothing a reader can see and `push_circle`
        // is happy to build a degenerate one, so it is dropped here rather than
        // left for the rasterizer to round away.
        if !radius.is_finite() || radius < 1.0 {
            // **On the texture and unpaintable**, which is the tally's
            // fall-through and not either cull: the station's disc covers this
            // view and rounds away, so a raster of nothing but sub-texel discs
            // reads `drew-no-ink` rather than `outside-view`. Zoomed far out is
            // exactly when that happens.
            tally.on_texture();
            continue;
        }

        tally.on_texture();
        // The disc, plus the outline that is stroked **centred on its edge** so
        // half the width lies outside the radius, plus one texel for the
        // anti-aliased rim. `COVERAGE_EDGE_WIDTH` is a hairline and the path is
        // conics with no corners, so there is no miter to allow for.
        extent.add_point(px, py, radius + COVERAGE_EDGE_WIDTH * scale * 0.5 + 1.0);
        discs.push((px, py, radius));
    }

    let crop = policy.allow(extent.crop(width, height));
    let (cw, ch, ox, oy) = crop_dims(crop, width, height);
    let Some(mut pixmap) = Pixmap::new(cw, ch) else {
        log::error!(
            "Pixmap allocation failed in rasterize_radar_coverage ({}×{})",
            cw,
            ch
        );
        return RasterizeOutput {
            crop,
            rgba: vec![0u8; (cw as usize) * (ch as usize) * 4].into(),
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
            blank_reason: Some(no_pixmap_reason(cw, ch)),
        };
    };

    let mut pb = PathBuilder::new();
    for (px, py, radius) in discs {
        pb.push_circle(px - ox, py - oy, radius);
    }

    if let Some(path) = pb.finish() {
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };

        // The wash. Faint on purpose: this draws over the whole eastern half of
        // the country at continental zoom, and anything a reader has to see
        // *through* has failed at being a basemap annotation.
        paint.set_color(Color::from_rgba8(100, 150, 255, 38));
        // **Winding and not even-odd**, and this is the whole construction: under
        // even-odd two overlapping discs cancel to a hole where coverage is
        // doubled, which says the opposite of the truth. Winding merges them.
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );

        paint.set_color(Color::from_rgba8(100, 150, 255, 160));
        let stroke = Stroke {
            width: COVERAGE_EDGE_WIDTH * scale,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    RasterizeOutput {
        crop,
        rgba: pixmap.take().into(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
        blank: None,
        blank_reason: Some(tally.reason(sites.len())),
    }
}

/// Tornado: funnel, i.e. an inverted triangle.
fn draw_tornado_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.6; // symbol half-size
    let mut pb = PathBuilder::new();
    pb.move_to(px - s, py - s * 0.7);
    pb.line_to(px + s, py - s * 0.7);
    pb.line_to(px, py + s * 0.9);
    pb.close();
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Hail: filled core with four radiating ticks.
fn draw_hail_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let core = r * 0.3;
    let tick_inner = r * 0.35;
    let tick_outer = r * 0.65;
    let stroke_w = (r * 0.18).clamp(0.5, 1.5);

    let mut pb = PathBuilder::new();
    pb.push_circle(px, py, core);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: stroke_w,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    let diag = std::f32::consts::FRAC_1_SQRT_2;
    let offsets: [(f32, f32); 4] = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)];
    for (dx, dy) in offsets {
        let (adx, ady) = if dx.abs() > 0.5 {
            (dx, diag * dy.signum().max(0.3))
        } else {
            (diag * dx.signum().max(0.3), dy)
        };
        let _ = ady;
        let _ = adx;
        let mut pb = PathBuilder::new();
        pb.move_to(px + dx * tick_inner, py + dy * tick_inner);
        pb.line_to(px + dx * tick_outer, py + dy * tick_outer);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

/// Wind: right-pointing chevron.
fn draw_wind_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.55;
    let stroke_w = (r * 0.22).clamp(0.5, 2.0);

    let mut pb = PathBuilder::new();
    pb.move_to(px - s * 0.5, py - s);
    pb.line_to(px + s * 0.5, py);
    pb.line_to(px - s * 0.5, py + s);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width: stroke_w,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReportPaint {
    pub kind: StormReportKind,
    pub lat: f64,
    pub lon: f64,
    /// The instant the report happened ([`StormReport::valid`]), for the
    /// as-of cull below; `None` draws at every instant — a report is never
    /// dropped for want of a readable time.
    ///
    /// [`StormReport::valid`]: crate::spc::reports::StormReport::valid
    pub valid: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportsInput {
    /// Behind an `Arc` so the page can hold one built row set across the
    /// dispatches whose only moving terms are the scalars below; the wire
    /// carries the rows, never the sharing.
    pub reports: Arc<Vec<ReportPaint>>,
    pub zoom: f64,
    pub is_dark: bool,
    pub device_scale: f32,
    /// The instant the picture **depicts**, captured at dispatch
    /// (`RasterizeContext::as_of`) and carried on the wire — the worker culls
    /// against this and never against a clock of its own, the same rule as
    /// [`GlmStrikesInput::now`].
    pub as_of: chrono::NaiveDateTime,
}

squallar_source::impl_job_input!(ReportsInput);

#[derive(Debug, Clone, PartialEq)]
pub struct MetarInput {
    /// The observations, in the handler's own order — **row `i` is the
    /// station the handler's `per_frame_points()[i]` carries as `MapPoint::id`**,
    /// so the drawn model and its click target are one index apart.
    /// Behind an `Arc` so the page can hold one built row set across the
    /// dispatches whose only moving terms are the scalars below; the wire
    /// carries the rows, never the sharing.
    pub obs: Arc<Vec<crate::metar::types::MetarOb>>,
    pub zoom: f64,
    pub is_dark: bool,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(MetarInput);

/// The station models, drawn into a picture instead of onto the frame thread.
///
/// Everything except the text: see [`PixmapPointPainter`] for why the five
/// texts a station draws stay behind, and what the 41 shapes that move were
/// costing.
///
/// A [`PointPainter`](crate::render::draw::PointPainter) that **measures a
/// station model instead of drawing it**: the texel box the shapes would cover,
/// relative to the station's own position.
///
/// **Why this and not a constant.** The METAR row's cull allows 60 points of
/// slack around a station because "a station model reaches well past its
/// centre", and that figure is a *cull* bound — deliberately generous, and
/// nobody's claim about how far the shapes actually go. A window cut from a
/// number like that is a number that has to stay true as the model changes, and
/// the failure when it stops being true is a station clipped at the edge of its
/// own picture, which nothing counts and nothing gates. So the reach is
/// measured through the same `draw_metar_station` call that paints it: one
/// model, walked twice.
///
/// **`text` is a no-op here for the same reason it is one in
/// [`PixmapPointPainter`]** — the picture carries no text at all, the frame
/// thread draws it — and that is not a conservative approximation but the
/// matching one: measuring text this painter never draws would grow every
/// window by a string's worth of texels that stay transparent.
///
/// Every pad below mirrors what `PixmapPointPainter` hands tiny-skia one method
/// at a time: `radius * scale` for a fill, plus half of `width * scale` for a
/// stroke centred on its path, and one texel everywhere for the anti-aliased
/// fringe. Both stroke sites use `LineCap::Round` and single-segment paths, so
/// a cap is a half-width disc at each end and there is no miter join anywhere
/// in the model.
#[derive(Debug, Clone, Copy)]
struct PointExtentPainter {
    scale: f32,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    any: bool,
}

impl PointExtentPainter {
    fn new(scale: f32) -> Self {
        Self {
            scale,
            min_x: f32::INFINITY,
            min_y: f32::INFINITY,
            max_x: f32::NEG_INFINITY,
            max_y: f32::NEG_INFINITY,
            any: false,
        }
    }

    fn at(&self, offset: [f32; 2]) -> (f32, f32) {
        (offset[0] * self.scale, offset[1] * self.scale)
    }

    fn grow(&mut self, x: f32, y: f32, pad: f32) {
        self.min_x = self.min_x.min(x - pad);
        self.min_y = self.min_y.min(y - pad);
        self.max_x = self.max_x.max(x + pad);
        self.max_y = self.max_y.max(y + pad);
        self.any = true;
    }

    /// The box, relative to the station, or `None` for a model that emitted no
    /// geometry at all.
    fn box_of(&self) -> Option<(f32, f32, f32, f32)> {
        self.any
            .then_some((self.min_x, self.min_y, self.max_x, self.max_y))
    }
}

/// One texel of anti-aliased fringe, allowed on every measured edge.
const AA_FRINGE_TEXELS: f32 = 1.0;

impl crate::render::draw::PointPainter for PointExtentPainter {
    /// **The same answer [`PixmapPointPainter`] gives, and it has to be.**
    ///
    /// `wants_geometry` is a capability a point model asks *before* it builds a
    /// symbol, so a painter answering `false` is handed less geometry than one
    /// answering `true`. This painter exists to measure the box the pixmap
    /// painter will draw into; if the two disagreed here, the window would be
    /// cut from one set of shapes and painted with another — the station model
    /// would be clipped at the edge of its own picture, and nothing in the tree
    /// counts that. Written out rather than defaulted so the coupling is at
    /// least visible; `a_measuring_painter_is_handed_what_the_pixmap_painter_is`
    /// is what actually holds the two together.
    fn wants_geometry(&self) -> bool {
        true
    }

    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, _color: [u8; 4]) {
        let (x, y) = self.at(offset);
        self.grow(x, y, radius * self.scale + AA_FRINGE_TEXELS);
    }

    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, _color: [u8; 4], width: f32) {
        let (x, y) = self.at(offset);
        self.grow(
            x,
            y,
            radius * self.scale + width * self.scale * 0.5 + AA_FRINGE_TEXELS,
        );
    }

    fn text(
        &mut self,
        _offset: [f32; 2],
        _text: &str,
        _color: [u8; 4],
        _size: f32,
        _anchor: crate::render::draw::TextAnchor,
    ) {
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], _color: [u8; 4], width: f32) {
        let pad = width * self.scale * 0.5 + AA_FRINGE_TEXELS;
        let (x0, y0) = self.at(from);
        let (x1, y1) = self.at(to);
        self.grow(x0, y0, pad);
        self.grow(x1, y1, pad);
    }

    fn filled_polygon(&mut self, points: &[[f32; 2]], _color: [u8; 4]) {
        for point in points {
            let (x, y) = self.at(*point);
            self.grow(x, y, AA_FRINGE_TEXELS);
        }
    }
}

/// **No hit map, deliberately** — the one texture layer that builds none. A
/// METAR click is resolved by the pane's point pass against live projected
/// positions ([`crate::render::handlers`]' METAR `per_frame_points`), which
/// tests the same stations at the same
/// [`station_model::hit_radius_for_zoom`](crate::render::station_model::hit_radius_for_zoom)
/// radius; a second answer off the picture only duplicated it. It was not
/// cheap duplication: at 799 stations on a 1568x1424 picture the disc stamp
/// was 122,354 occupied cells — one `Vec` allocation each, 1.47 MB of the
/// reply (14.7%), and about 8 MB held for as long as the texture was cached.
/// Handlers whose clicks have no other answer — storm reports, GLM — still
/// build one, and for them it is the only click path there is.
pub fn rasterize_metar_stations(
    input: &MetarInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    rasterize_metar_stations_windowed(input, bounds, width, height, CropPolicy::Content)
}

/// [`rasterize_metar_stations`] with the window policy stated. See [`CropPolicy`].
pub fn rasterize_metar_stations_windowed(
    input: &MetarInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    policy: CropPolicy,
) -> RasterizeOutput {
    let MetarInput {
        obs,
        zoom,
        is_dark,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let mb = MercatorBounds::from_geo(bounds);
    let (w, h) = (width as f32, height as f32);
    let ctx = crate::render::draw::DrawPointContext {
        zoom: *zoom as f32,
        is_dark: *is_dark,
    };
    let mut tally = ItemTally::default();

    // **Measured before a texel is allocated**, through the same model the
    // paint pass draws — see [`PointExtentPainter`]. The stations that survive
    // the cull are kept with the box each one measured, so the second walk
    // draws exactly the list the window was cut from.
    let mut placed: Vec<(usize, f32, f32)> = Vec::new();
    let mut extent = ContentExtent::new();
    for (idx, ob) in obs.iter().enumerate() {
        // Into the viewport's frame first, as every point row does.
        let lon = mb.nearest_lon(ob.lon);
        let (px, py) = mb.project(ob.lat, lon, w, h);
        // A station model reaches well past its centre — wind barbs, cloud
        // cover and the pressure group all sit off to one side — so the slack
        // is generous. A symbol clipped at the edge is a drawing artifact; a
        // symbol culled at the edge is a missing station.
        let slack = 60.0 * scale;
        if px < -slack || px > w + slack || py < -slack || py > h + slack {
            tally.off_texture();
            continue;
        }
        tally.on_texture();
        {
            let mut measure = PointExtentPainter::new(scale);
            let text = crate::render::station_model::StationText::of(ob);
            crate::render::station_model::draw_metar_station(ob, &text, &mut measure, &ctx);
            // A model that emitted no geometry occupies nothing, so it widens
            // no window — and it is not `unbounded` either: nothing about it is
            // unknown.
            if let Some((min_x, min_y, max_x, max_y)) = measure.box_of() {
                extent.add_rect(px + min_x, py + min_y, px + max_x, py + max_y);
            }
        }
        placed.push((idx, px, py));
    }

    let crop = policy.allow(extent.crop(width, height));
    let (cw, ch, ox, oy) = crop_dims(crop, width, height);
    let Some(mut pixmap) = Pixmap::new(cw, ch) else {
        log::error!(
            "Pixmap allocation failed in rasterize_metar_stations ({}x{})",
            cw,
            ch
        );
        return RasterizeOutput {
            crop,
            rgba: vec![0u8; (cw as usize) * (ch as usize) * 4].into(),
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
            blank_reason: Some(no_pixmap_reason(cw, ch)),
        };
    };

    for (idx, px, py) in placed {
        let ob = &obs[idx];
        let mut painter = PixmapPointPainter {
            pixmap: &mut pixmap,
            center: (px - ox, py - oy),
            scale,
        };
        // Text is a no-op in this painter, but the draw still asks for it;
        // building it here is per station per PICTURE, in the worker.
        let text = crate::render::station_model::StationText::of(ob);
        crate::render::station_model::draw_metar_station(ob, &text, &mut painter, &ctx);
    }

    RasterizeOutput {
        crop,
        rgba: pixmap.take().into(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
        blank: None,
        blank_reason: Some(tally.reason(obs.len())),
    }
}

/// A [`PointPainter`](crate::render::draw::PointPainter) that draws into a
/// `tiny_skia` pixmap, so a station model can be rasterized OFF the frame
/// thread.
///
/// **Why this exists.** The METAR station model emits 46 shapes per station —
/// 28 lines, 7 stroked circles, 6 filled circles, 4 polygons, 5 texts, which
/// sum to 50 rather than 46; the census that settles which is unrun — and a
/// scene D leg carries 799 observations. Drawn through `egui` that is ~36,750
/// shapes per frame of which ~28,000 are STROKED, and a stroked path is the
/// tessellator's expensive case: it feathers geometry along both edges. The
/// measured cost was 98,815 vertices and 401,072 indices staged EVERY FRAME,
/// with `epaint::tessellator::stroke_and_fill_path` the single largest symbol
/// in the app at 11.75 %, for observations that change every twenty minutes.
///
/// **`text` is deliberately a no-op here.** `tiny_skia` has no font support and
/// nothing in the worker can lay out a galley, so the five texts a station
/// draws stay on the frame thread where the font atlas is. That is the split:
/// the 41 geometric shapes — every stroked one — become a picture, and only
/// the text remains. Moving the text as well needs a font rasterizer in the
/// worker and is a separate question with its own dependency decision.
struct PixmapPointPainter<'a> {
    pixmap: &'a mut Pixmap,
    /// The station's own position in texels; every offset is relative to it.
    center: (f32, f32),
    /// Texels per point — [`sane_device_scale`] of the plan's density.
    ///
    /// **The station model speaks points and the picture is texels.** Every
    /// offset, radius and stroke width the model hands over is a length on
    /// the display, the same lengths the frame thread draws its text at; a
    /// picture at three texels per point that painted them as texels drew a
    /// third-size circle, barb and weather symbol beside full-size numbers.
    /// The hit radius and the cull slack the caller computes were already
    /// scaled; the shapes were not.
    scale: f32,
}

impl PixmapPointPainter<'_> {
    fn at(&self, offset: [f32; 2]) -> (f32, f32) {
        (
            self.center.0 + offset[0] * self.scale,
            self.center.1 + offset[1] * self.scale,
        )
    }

    /// `PointPainter` speaks straight (unmultiplied) RGBA, which is what
    /// `from_rgba8` takes.
    fn paint_of(color: [u8; 4]) -> Paint<'static> {
        Paint {
            shader: tiny_skia::Shader::SolidColor(Color::from_rgba8(
                color[0], color[1], color[2], color[3],
            )),
            anti_alias: true,
            ..Default::default()
        }
    }
}

impl crate::render::draw::PointPainter for PixmapPointPainter<'_> {
    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4]) {
        let (x, y) = self.at(offset);
        let mut pb = PathBuilder::new();
        pb.push_circle(x, y, radius * self.scale);
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(
                &path,
                &Self::paint_of(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4], width: f32) {
        let (x, y) = self.at(offset);
        let mut pb = PathBuilder::new();
        pb.push_circle(x, y, radius * self.scale);
        if let Some(path) = pb.finish() {
            self.pixmap.stroke_path(
                &path,
                &Self::paint_of(color),
                &Stroke {
                    width: width * self.scale,
                    line_cap: LineCap::Round,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
    }

    /// No-op: see the type's own note. The frame thread still draws the text.
    fn text(
        &mut self,
        _offset: [f32; 2],
        _text: &str,
        _color: [u8; 4],
        _size: f32,
        _anchor: crate::render::draw::TextAnchor,
    ) {
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4], width: f32) {
        let (x0, y0) = self.at(from);
        let (x1, y1) = self.at(to);
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        pb.line_to(x1, y1);
        if let Some(path) = pb.finish() {
            self.pixmap.stroke_path(
                &path,
                &Self::paint_of(color),
                &Stroke {
                    width: width * self.scale,
                    line_cap: LineCap::Round,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
    }

    fn filled_polygon(&mut self, points: &[[f32; 2]], color: [u8; 4]) {
        let Some((first, rest)) = points.split_first() else {
            return;
        };
        let (x0, y0) = self.at(*first);
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        for p in rest {
            let (x, y) = self.at(*p);
            pb.line_to(x, y);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(
                &path,
                &Self::paint_of(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

/// Tornado = red, hail = green, wind = blue. Below a 5 px radius the symbols
/// fall back to filled dots.
pub fn rasterize_storm_reports(
    input: &ReportsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    rasterize_storm_reports_windowed(input, bounds, width, height, CropPolicy::Content)
}

/// [`rasterize_storm_reports`] with the window policy stated. See [`CropPolicy`].
pub fn rasterize_storm_reports_windowed(
    input: &ReportsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    policy: CropPolicy,
) -> RasterizeOutput {
    let ReportsInput {
        reports,
        zoom,
        is_dark,
        device_scale,
        as_of,
    } = input;
    let (zoom, is_dark) = (*zoom, *is_dark);
    let scale = sane_device_scale(*device_scale);
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let zoom_f32 = zoom as f32;
    let radius = (3.0 + zoom_f32 * 0.5).clamp(3.0, 10.0) * scale;
    let stroke_w = (radius * 0.3).clamp(0.5 * scale, 2.0 * scale);
    let hit_radius = radius + stroke_w;

    let outline = if is_dark {
        Color::from_rgba8(255, 255, 255, 220)
    } else {
        Color::from_rgba8(40, 40, 40, 220)
    };

    let mut hit_cells = HitCells::new(width, height);
    let mut tally = ItemTally::default();

    // **Where the marks land, before a texel is allocated.** This is the row
    // the DHAT reading singled out: four whole-viewport pictures, 71,884,800 B
    // allocated and **96,384 B written — 0.13 %**. A storm report is a disc of
    // at most ten texels' radius and a page of them is a handful of counties,
    // so the window below is a fraction of the viewport on every scene that has
    // any reports at all. See [`PictureCrop`].
    //
    // The filter and the cull keep their order and their tallies exactly: this
    // walk is the old loop with the drawing lifted out of it, and the second
    // walk below draws the list it leaves. Both read the same `(px, py)`, so the
    // window and the paint cannot come from two projections.
    let mut placed: Vec<(usize, f32, f32)> = Vec::new();
    let mut extent = ContentExtent::new();
    for (idx, report) in reports.iter().enumerate() {
        // **A report later than the depicted instant has not happened yet**
        // (`TimeAxis::EventLifetime`): the picture at `as_of` is which of
        // today's reports have already happened. The cull lives HERE and not
        // in `paint_input`, because a row's position is its hit-map id
        // (`HitMap::from_cells`): dropping rows at the handler would renumber
        // every row after the gap and hand hovers to the wrong reports. A
        // culled row travels, draws nothing and records no cells — absent
        // from the picture, aligned in the map. `None` passes: a report is
        // never dropped for want of a readable time.
        if report.valid.is_some_and(|valid| valid > *as_of) {
            // A report the depicted instant has not reached: the layer holds
            // it and the view may be right over where it will happen.
            tally.filtered();
            continue;
        }
        // Into the viewport's frame first — see `rasterize_radar_sites`.
        let lon = mb.nearest_lon(report.lon);
        let (px, py) = mb.project(report.lat, lon, w, h);
        let slack = 20.0 * scale;
        if px < -slack || px > w + slack || py < -slack || py > h + slack {
            tally.off_texture();
            continue;
        }
        tally.on_texture();
        // The disc, its outline stroked centred so half the width lies outside
        // the radius, and one texel for the anti-aliased rim — the whole stroke
        // width allowed rather than half, because the symbols inside
        // (`draw_tornado_symbol` and its two siblings) are filled paths within
        // `radius` and splitting hairs at this size buys nothing. `hit_radius`
        // is `radius + stroke_w` and so sits inside it; the hit cells are
        // recorded in the WHOLE picture's grid regardless (see below), so
        // nothing about the window constrains them.
        extent.add_point(px, py, radius + stroke_w + 2.0);
        placed.push((idx, px, py));
    }

    let crop = policy.allow(extent.crop(width, height));
    let (cw, ch, ox, oy) = crop_dims(crop, width, height);
    let Some(mut pixmap) = Pixmap::new(cw, ch) else {
        log::error!(
            "Pixmap allocation failed in rasterize_storm_reports ({}×{})",
            cw,
            ch
        );
        return RasterizeOutput {
            crop,
            rgba: vec![0u8; (cw as usize) * (ch as usize) * 4].into(),
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
            blank_reason: Some(no_pixmap_reason(cw, ch)),
        };
    };

    for (idx, ppx, ppy) in placed {
        let report = &reports[idx];
        // **Two coordinates from here down, and they are not interchangeable.**
        // `(px, py)` is where the mark is drawn: the window's frame, which is
        // the whole picture's coordinate minus a whole number of texels.
        // `(ppx, ppy)` is the whole picture's own, and it is what the hit cells
        // are recorded in — the hit map is a quarter-resolution grid over the
        // WHOLE dispatched texture and the pane's click test reads it through
        // the picture's full screen rect (`ui_map_overlays`), so recording it in
        // the window's frame would hand every hover to the wrong report.
        let (px, py) = (ppx - ox, ppy - oy);

        let fill = match report.kind {
            StormReportKind::Tornado => Color::from_rgba8(220, 40, 40, 220),
            StormReportKind::Hail => Color::from_rgba8(40, 180, 40, 220),
            StormReportKind::Wind => Color::from_rgba8(40, 80, 220, 220),
        };

        let use_symbol = radius >= 5.0;

        let mut pb = PathBuilder::new();
        pb.push_circle(px, py, radius);
        if let Some(path) = pb.finish() {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };

            if use_symbol {
                paint.set_color(fill);
                let stroke = Stroke {
                    width: stroke_w,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);

                match report.kind {
                    StormReportKind::Tornado => {
                        draw_tornado_symbol(&mut pixmap, px, py, radius, fill)
                    }
                    StormReportKind::Hail => draw_hail_symbol(&mut pixmap, px, py, radius, fill),
                    StormReportKind::Wind => draw_wind_symbol(&mut pixmap, px, py, radius, fill),
                }
            } else {
                paint.set_color(fill);
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );

                paint.set_color(outline);
                let stroke = Stroke {
                    width: stroke_w,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }

        // The report's position in the input list **is** its id.
        let item_id = idx as u32;
        let min_x = (ppx - hit_radius).max(0.0) as i32;
        let max_x = ((ppx + hit_radius) as i32).min(width as i32 - 1);
        let min_y = (ppy - hit_radius).max(0.0) as i32;
        let max_y = ((ppy + hit_radius) as i32).min(height as i32 - 1);
        let r2 = hit_radius * hit_radius;
        let mut sy = min_y;
        while sy <= max_y {
            let mut sx = min_x;
            while sx <= max_x {
                let dx = sx as f32 - ppx;
                let dy = sy as f32 - ppy;
                if dx * dx + dy * dy <= r2 {
                    hit_cells.record(sx as f32, sy as f32, item_id);
                }
                sx += 4;
            }
            sy += 4;
        }
    }

    RasterizeOutput {
        crop,
        rgba: pixmap.take().into(),
        hit_cells: Some(hit_cells),
        alpha: AlphaMode::Premultiplied,
        blank: None,
        blank_reason: Some(tally.reason(reports.len())),
    }
}

/// One storm report as the **dispatch door** reads it: where it is, and when it
/// happened. The kind is absent because the cull below does not look at it —
/// every kind draws the same radius.
pub struct ReportPlace {
    pub lat: f64,
    pub lon: f64,
    /// [`ReportPaint::valid`]: `None` is never culled.
    pub valid: Option<chrono::NaiveDateTime>,
}

/// How far outside the texture [`rasterize_storm_reports`] admits a report,
/// **in texels** — its own `slack`, `20.0 * scale`, with `scale` the device
/// scale the symbols are drawn at.
const REPORT_SLACK_TEXELS: f64 = 20.0;

/// The factor [`report_pad`] multiplies the exact texel→ground conversion by.
///
/// The conversion goes through the map zoom because the texture's texel count
/// is **not** on [`RasterizeContext`] (54 sites construct one; the pixel plan
/// reaches only the worker), and the zoom arrives quantized: the dispatch
/// rounds to 1/32 of a zoom level (`ZOOM_QUANTIZATION_FACTOR`), so the ground
/// it names is out by at most 2^(1/64), 1.1 %, and the planned side is
/// truncated to a whole texel besides. A factor of 2 is far more than either
/// needs, which is the point — this is the term that is cheap to be generous
/// with, since what it buys back is a raster and what it costs is a walk that
/// was going to run anyway.
const REPORT_PAD_HEADROOM: f64 = 2.0;

/// The floor under the pad, as a fraction of the box's own span per side.
///
/// It is not a second opinion about the slack — it is what keeps a *wrong*
/// zoom from producing a wrong `false`. The zoom on a dispatch describes the
/// viewport the bounds were taken from, so the two cannot disagree by more
/// than the quantization above; if a later change ever lets them, a zoom that
/// names a box far smaller than the one it is handed would shrink the pad
/// towards nothing, and this term holds it at a twentieth of the box instead.
/// It is **under** the zoom term at every picture this tree plans — the
/// smallest, a 120-point pane, spends a third of its own box on the pad — so
/// it changes no answer today and is there for the day the relationship
/// moves.
const REPORT_PAD_FLOOR_FRACTION: f64 = 0.05;

/// The ground [`rasterize_storm_reports`]'s pixel slack covers, as a longitude
/// pad and a Mercator-`y` pad for [`grow_bounds`].
///
/// # The texel→ground conversion, which is exact
///
/// One logical point of the map is `360 / (2^zoom * 256)` degrees of longitude
/// — the Web Mercator scale `walkers::Projector` lays the pane out at, and the
/// pane's bounds are its two unprojected corners. A texel is that divided by
/// `device_scale`: `OverlayTexturePlan::pixels_per_point` is exactly the
/// density the picture's texels were sized at, whether that came from the
/// display alone or from the resolution the coverage widening gave up
/// (`MIN_COVERAGE_SCALE`). The picture's own oversampling cancels — it widens
/// the ground and the texel count by one factor — which is why neither the
/// overdraw nor the pane's size appears here.
///
/// `sane_device_scale` is applied for the same reason the rasterizer applies
/// it: the symbols, and so the slack, are drawn at the clamped scale, while
/// the texels are laid out at the raw one.
fn report_pad(bounds: &GeoBounds, zoom: f64, device_scale: f32) -> Option<(f64, f64)> {
    symbol_pad(
        bounds,
        zoom,
        device_scale,
        REPORT_SLACK_TEXELS * f64::from(sane_device_scale(device_scale)),
    )
}

/// The ground a rasterizer's pixel slack covers, for a slack already stated
/// **in texels at the scale the symbols are drawn at** — which is what
/// `sane_device_scale` names in both callers and why it is applied by them
/// rather than here.
///
/// The conversion and both margins are [`report_pad`]'s, whose doc states
/// them; this is that body with the one term that differs between the two
/// symbol layers — how far outside the texture each admits an item — passed
/// in. **One spelling, on purpose:** a second copy that disagreed would refuse
/// a raster its own rasterizer would have painted, which is the one direction
/// `SourceHandler::paints_in` may not be wrong in.
fn symbol_pad(
    bounds: &GeoBounds,
    zoom: f64,
    device_scale: f32,
    slack_texels: f64,
) -> Option<(f64, f64)> {
    if !zoom.is_finite() || !device_scale.is_finite() || device_scale <= 0.0 {
        return None;
    }
    if !slack_texels.is_finite() || slack_texels < 0.0 {
        return None;
    }
    let deg_per_point = 360.0 / (2f64.powf(zoom) * 256.0);
    if !deg_per_point.is_finite() || deg_per_point <= 0.0 {
        return None;
    }
    let texels_per_point = f64::from(device_scale);
    let lon_pad = slack_texels / texels_per_point * deg_per_point * REPORT_PAD_HEADROOM;

    let mb = MercatorBounds::from_geo(bounds);
    let lon_floor = (bounds.max_lon - bounds.min_lon).abs() * REPORT_PAD_FLOOR_FRACTION;
    let merc_floor = (mb.merc_y_max - mb.merc_y_min).abs() * REPORT_PAD_FLOOR_FRACTION;
    // Mercator `y` is in radians here (`lat_rad_to_mercator_y`), and one
    // logical point is the same fraction of the world on both axes.
    let merc_pad = lon_pad.to_radians();
    Some((lon_pad.max(lon_floor), merc_pad.max(merc_floor)))
}

/// Whether **any** report could put ink in a texture over `bounds` — the
/// predicate `StormReportsHandler::paints_in` is built from, and the mirror of
/// the two `continue`s at the head of [`rasterize_storm_reports`]'s loop.
///
/// # What it mirrors, term by term
///
/// * The as-of cull is that loop's, spelled the same way down to the
///   `is_some_and`: a report with no readable time is never culled, and a
///   report later than the depicted instant has not happened yet.
/// * The geographic cull is that loop's `slack` rectangle, carried into ground
///   by [`report_pad`] and applied by [`grow_bounds`] — Mercator on the axis
///   the texture's rows are linear in, degrees on the other. `nearest_lon` is
///   the same shift the loop applies before it projects, so a dateline
///   viewport asks about the same representation of a report either way.
///
/// **It admits a superset, on both terms.** The pad is the rasterizer's
/// tolerance with a factor of two on it and a floor under it, so the ring of
/// ground where the two disagree is ground the rasterizer accepts and this
/// accepts too. `None` from [`report_pad`] — a zoom or a density that does not
/// describe a picture — is `true`: the geometry is unknown, and the one
/// direction this may not be wrong in is `false`.
///
/// **Unmemoized and per dispatch**, on the same terms as the alert layer's:
/// the walk short-circuits on the first report that survives, so the scene it
/// walks furthest on is the one where it is about to answer `true`.
pub fn any_report_paints_in(
    reports: impl IntoIterator<Item = ReportPlace>,
    bounds: &GeoBounds,
    zoom: f64,
    device_scale: f32,
    as_of: chrono::NaiveDateTime,
) -> bool {
    let Some((lon_pad, merc_pad)) = report_pad(bounds, zoom, device_scale) else {
        return true;
    };
    let mb = MercatorBounds::from_geo(bounds);
    let grown = grow_bounds(bounds, lon_pad, merc_pad);
    reports.into_iter().any(|report| {
        if report.valid.is_some_and(|valid| valid > as_of) {
            return false;
        }
        grown.contains_point(report.lat, mb.nearest_lon(report.lon))
    })
}

fn draw_lightning_bolt(pixmap: &mut Pixmap, cx: f32, cy: f32, size: f32, rgba: [u8; 4]) {
    let s = size * 0.5;
    let mut pb = PathBuilder::new();
    pb.move_to(cx - s * 0.1, cy - s);
    pb.line_to(cx + s * 0.35, cy - s);
    pb.line_to(cx + s * 0.05, cy - s * 0.15);
    pb.line_to(cx + s * 0.35, cy - s * 0.15);
    pb.line_to(cx - s * 0.15, cy + s);
    pb.line_to(cx + s * 0.05, cy + s * 0.15);
    pb.line_to(cx - s * 0.25, cy + s * 0.15);
    pb.close();

    let Some(path) = pb.finish() else { return };
    let paint = tiny_skia::Paint {
        shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(
            rgba[0], rgba[1], rgba[2], rgba[3],
        )),
        anti_alias: true,
        ..Default::default()
    };
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
}

/// Age ramp: white → yellow → orange → red, in thirds of `window_secs`.
fn time_decay_color(age_secs: f64, window_secs: f64, is_dark: bool) -> [u8; 4] {
    let t = (age_secs / window_secs).clamp(0.0, 1.0) as f32;
    let (r, g, b) = if t < 0.33 {
        let f = t / 0.33;
        (255, (255.0 - f * 30.0) as u8, (255.0 - f * 200.0) as u8)
    } else if t < 0.66 {
        let f = (t - 0.33) / 0.33;
        (255, (225.0 - f * 90.0) as u8, (55.0 - f * 55.0) as u8)
    } else {
        let f = (t - 0.66) / 0.34;
        ((255.0 - f * 55.0) as u8, (135.0 - f * 85.0) as u8, 0)
    };
    let alpha = if is_dark { 230 } else { 200 };
    [r, g, b, alpha]
}

/// GLM radiant energy → 0…1 bolt-size channel.
///
/// The 1e-16…1e-12 J clamp window covers event and group energies but not all
/// flash energies. Input must be CF-unpacked. `None` draws at the midpoint and
/// must not collapse to either end.
fn energy_size_scale(energy: Option<f32>) -> f32 {
    match energy {
        Some(e) => (e.log10().clamp(-16.0, -12.0) + 16.0) / 4.0,
        None => 0.5,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlashPaint {
    pub lat: f64,
    pub lon: f64,
    /// UTC. Aged against [`GlmStrikesInput::now`] for the fade ramp and cull.
    pub time: chrono::NaiveDateTime,
    /// Radiant energy in joules; sizes the bolt. `None` means unknown.
    pub energy: Option<f32>,
}

/// A flash's position in `flashes` is its hit-map id.
#[derive(Debug, Clone, PartialEq)]
pub struct GlmStrikesInput {
    /// **Shared, not copied per dispatch.** The rows are a function of the
    /// resident granule alone — every other field of this struct is a scalar
    /// the dispatch supplies — so the handler builds them once per poll and
    /// hands out a refcount clone. At the ~125,000 flashes a busy 20 s poll
    /// delivers, a `Vec` here was a 5 MB deep copy on the frame thread per
    /// overlay render. The worker side decodes into a fresh `Arc`, which is
    /// one block for the whole run.
    pub flashes: Arc<Vec<FlashPaint>>,
    pub zoom: f64,
    pub is_dark: bool,
    /// Flashes older than this many seconds are dropped; younger ones fade
    /// through [`time_decay_color`]'s ramp over it.
    pub time_window_secs: f64,
    /// **The page's clock at dispatch, never the worker's** — flash age is
    /// `now - flash.time`, so a worker's own clock would render another picture.
    pub now: chrono::NaiveDateTime,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(GlmStrikesInput);

/// **How far outside the texture [`rasterize_glm_strikes`] admits a flash, in
/// texels** — its own `base_size`, which is both the bolt's drawn size and the
/// reach of its projection cull, so there is exactly one number and one
/// spelling of it.
///
/// ~12 points at zoom 6, clamped to 6-20 points, then taken into texels at the
/// clamped device scale the symbols are drawn at.
fn glm_base_size(zoom: f64, device_scale: f32) -> f32 {
    (zoom as f32 * 2.0).clamp(6.0, 20.0) * sane_device_scale(device_scale)
}

/// The ground [`rasterize_glm_strikes`]'s pixel slack covers — [`symbol_pad`]
/// with this layer's own slack, which unlike the storm reports' is a function
/// of the zoom.
fn glm_pad(bounds: &GeoBounds, zoom: f64, device_scale: f32) -> Option<(f64, f64)> {
    symbol_pad(
        bounds,
        zoom,
        device_scale,
        f64::from(glm_base_size(zoom, device_scale)),
    )
}

/// 1° latitude bands, index `lat + 90` floored into `0..180`.
const OCC_LAT_BANDS: usize = 180;
/// 1° longitude cells, index `lon.rem_euclid(360)` floored into `0..360`.
const OCC_LON_CELLS: usize = 360;
/// The words one band's longitude bitmap takes.
const OCC_LON_WORDS: usize = OCC_LON_CELLS.div_ceil(64);
/// **How many cells of grid the query dilates its box by, per side.**
///
/// Not a second opinion about the pad, which is stated in ground and is
/// [`symbol_pad`]'s. This is about the *discretization*: the index places a
/// flash by `lon.rem_euclid(360.0)` and a query places the box's edges by the
/// same call on a number that reached it through
/// [`MercatorBounds::wrap_lon`] and [`grow_bounds`]'s Mercator round trip. The
/// two spellings agree exactly except within an ulp of a cell boundary, where
/// they can name adjacent cells; one cell of dilation is more ground than any
/// such disagreement can cross.
///
/// **It is margin and not a correction**, and the tamper that shows it is the
/// pair `a_refused_flash_could_not_have_painted_ink` was run under: this at
/// zero with `REPORT_PAD_HEADROOM` at 1.0 and `REPORT_PAD_FLOOR_FRACTION` at
/// 0.0 — the exact conversion, every margin gone — is GREEN over all 144
/// scenes, and one cell NARROWER than the exact box is RED. So what the suite
/// needs is the cell arithmetic, and this is headroom over it.
const OCC_DILATION_CELLS: i64 = 1;

/// **Where the retained flashes are, coarsely, so the dispatch door can refuse
/// a raster without walking them.**
///
/// [`GlmHandler::paints_in`] cannot afford the shape the storm reports' door
/// takes. That predicate is a linear scan over a population of hundreds; this
/// layer's population is the 250,000-row retention ceiling — measured
/// 244,877-247,919 rows resident — and the same scan at 247,000 rows measures
/// **0.518 ms on the frame thread** (release, best of five, 2.10 ns a row),
/// which is disqualifying for a mechanism whose whole purpose is to save an
/// offloaded job.
///
/// So the walk happens once per data generation instead, inside the one
/// [`crate::render::handlers::glm`] already runs to build its paint rows, and
/// what a dispatch reads is this: a 1°×1° occupancy bitmap of the globe with
/// the time extent of each of its words beside it. **0.614-0.627 ms added to
/// that once-per-generation walk** — which is a 20 s poll apart and lands on a
/// frame already paying 0.795-0.842 ms for the rows, so the frame it lands on
/// stays inside the 4 ms budget — and a query is `bands × OCC_LON_WORDS`
/// masked word tests: **114-125 ns refused, 114 ns admitted, 352 ns for a view
/// of the whole world**, which is the bound and is an admission anyway.
///
/// # It answers a superset of what the rasterizer inks
///
/// A cell is marked when a flash is anywhere inside it and a word's extent
/// spans every flash in the word, so "occupied and in time" is implied by, and
/// strictly weaker than, "a flash is here". Three things make that hold rather
/// than merely sound:
///
/// * **A flash the index cannot place sets [`Self::unplaced`]**, and every
///   query then admits. A non-finite latitude or longitude has no cell — and
///   `as usize` would saturate it into cell 0, which is not a superset of
///   anywhere else — so the index stops claiming to describe the rows.
/// * **The band range is unioned with the raw box.** `grow_bounds` pads in
///   Mercator, and `MercatorBounds::from_geo` clamps to Web Mercator's ±85.05°,
///   so a box reaching past that limit comes back *narrower* than it went in.
/// * **The time interval is widened a second at each end**, because the
///   rasterizer's own window test is `f64` seconds off an integer millisecond
///   subtraction and this one is integer milliseconds.
///
/// # What it is not
///
/// It is not a spatial *filter* — nothing here decides which flashes are
/// drawn, and the rasterizer's loop is untouched. It answers one bit for a
/// whole picture, and the only use of that bit is to skip a job that would
/// have come back blank.
#[derive(Debug, Clone)]
pub struct FlashOccupancy {
    /// Bit `c % 64` of `cells[band * OCC_LON_WORDS + c / 64]` is "a flash sits
    /// in 1° band `band`, 1° cell `c`".
    cells: Box<[u64]>,
    /// `(earliest, latest)` flash time in the matching `cells` word,
    /// `(MAX, MIN)` while the word is empty. **Per word and not per cell**:
    /// 1,080 slots rather than 64,800, and a word's extent spans a superset of
    /// any one of its cells'.
    ///
    /// `NaiveDateTime` and not milliseconds, because `timestamp_millis` is a
    /// multiply chain and this is compared, not arithmetic — so the conversion
    /// belongs once per query and not once per retained flash.
    times: Box<[(chrono::NaiveDateTime, chrono::NaiveDateTime)]>,
    /// A flash with a non-finite position, which this index does not describe.
    unplaced: bool,
    /// How many flashes were placed — zero only for an empty row set.
    placed: usize,
}

impl Default for FlashOccupancy {
    fn default() -> Self {
        Self {
            cells: vec![0u64; OCC_LAT_BANDS * OCC_LON_WORDS].into_boxed_slice(),
            times: vec![
                (chrono::NaiveDateTime::MAX, chrono::NaiveDateTime::MIN);
                OCC_LAT_BANDS * OCC_LON_WORDS
            ]
            .into_boxed_slice(),
            unplaced: false,
            placed: 0,
        }
    }
}

/// Which 1° band a latitude falls in — **monotone and total**, which is what
/// makes a query's band range cover every flash inside the query's latitudes.
///
/// `as i64` and not `floor()`: the cast truncates toward zero, which differs
/// from a floor only below `-90°`, and there the clamp answers `0` either way.
#[inline]
fn occ_band(lat: f64) -> usize {
    ((lat + 90.0) as i64).clamp(0, OCC_LAT_BANDS as i64 - 1) as usize
}

/// Which 1° cell a longitude falls in, in the `[0, 360)` frame the index is
/// laid out in.
///
/// **The two comparisons are there instead of a division.** `rem_euclid` on
/// `f64` is a real `fdiv`, this runs once per retained flash, and every
/// longitude a GLM granule carries is already inside one of the two shifted
/// ranges — the fallback is for a row no product produces and is kept because
/// a wrong cell is not a slow cell, it is a missing overlay.
#[inline]
fn occ_cell(lon: f64) -> usize {
    let normalized = if (0.0..360.0).contains(&lon) {
        lon
    } else if (-360.0..0.0).contains(&lon) {
        lon + 360.0
    } else {
        lon.rem_euclid(360.0)
    };
    (normalized as i64).clamp(0, OCC_LON_CELLS as i64 - 1) as usize
}

impl FlashOccupancy {
    /// Bytes this index owns on the heap — its two fixed tables, whose size is
    /// a function of the grid alone and not of the row count.
    pub fn heap_bytes(&self) -> u64 {
        (std::mem::size_of_val(&*self.cells) + std::mem::size_of_val(&*self.times)) as u64
    }

    /// Record one flash. Called once per row of the generation, inside the walk
    /// that builds the paint rows.
    #[inline]
    pub fn insert(&mut self, lat: f64, lon: f64, time: chrono::NaiveDateTime) {
        if !lat.is_finite() || !lon.is_finite() {
            self.unplaced = true;
            return;
        }
        let cell = occ_cell(lon);
        let idx = occ_band(lat) * OCC_LON_WORDS + cell / 64;
        self.cells[idx] |= 1u64 << (cell % 64);
        let slot = &mut self.times[idx];
        if time < slot.0 {
            slot.0 = time;
        }
        if time > slot.1 {
            slot.1 = time;
        }
        self.placed += 1;
    }

    /// Whether **any** retained flash could put ink in a texture over
    /// `bounds` — the predicate `GlmHandler::paints_in` is built from, and the
    /// mirror of the three `continue`s at the head of
    /// [`rasterize_glm_strikes`]'s loop.
    ///
    /// # What it mirrors, term by term
    ///
    /// * The geographic cull is that loop's: latitude inside the box, and a
    ///   longitude whose `wrap_lon` representation is inside it. Both are
    ///   carried out to the reach of the loop's *projection* cull by
    ///   [`glm_pad`] and [`grow_bounds`], which is looser than the loop's own
    ///   pre-projection test on both axes and is therefore the safe one to
    ///   mirror.
    /// * **The as-of cull and the fade window are one interval.** The loop
    ///   drops a flash later than the depicted instant and a flash older than
    ///   `time_window_secs`, so what can ink is `[as_of - window, as_of]` and
    ///   nothing else. This is the finest-grained as-of dependence any layer
    ///   has — the fade ramp makes a flash's *age* reach the pixels — and it
    ///   is exactly why this layer cannot use `as_of_signature`. It does not
    ///   stop the interval from being the whole of the loop's time test: a
    ///   flash inside the interval may be drawn dim, but "dim" is ink and
    ///   `time_decay_color` never returns a zero alpha.
    ///
    /// # Fail-open, by enumeration
    ///
    /// `true` — dispatch — for a slack the zoom or the density cannot describe
    /// ([`glm_pad`] `None`), a non-finite or negative window, a box whose own
    /// edges are inverted or non-finite, and a row set holding a flash this
    /// index could not place. A wrong `true` costs one raster that comes back
    /// blank, which is the behaviour without this method; a wrong `false`
    /// **clears a pane that should have had ink**.
    pub fn any_paints_in(
        &self,
        bounds: &GeoBounds,
        zoom: f64,
        device_scale: f32,
        as_of: chrono::NaiveDateTime,
        time_window_secs: f64,
    ) -> bool {
        if self.unplaced {
            return true;
        }
        if self.placed == 0 {
            return false;
        }
        if !time_window_secs.is_finite() || time_window_secs < 0.0 {
            return true;
        }
        let Some((lon_pad, merc_pad)) = glm_pad(bounds, zoom, device_scale) else {
            return true;
        };
        let grown = grow_bounds(bounds, lon_pad, merc_pad);
        // The union, not `grown` alone: see the type's note on Web Mercator's
        // latitude clamp.
        let min_lat = grown.min_lat.min(bounds.min_lat);
        let max_lat = grown.max_lat.max(bounds.max_lat);
        let lon_lo = bounds.min_lon - lon_pad;
        let lon_hi = bounds.max_lon + lon_pad;
        if !min_lat.is_finite()
            || !max_lat.is_finite()
            || !lon_lo.is_finite()
            || !lon_hi.is_finite()
            || max_lat < min_lat
            || lon_hi < lon_lo
        {
            return true;
        }

        // The interval the rasterizer's two time culls admit, **widened a
        // second at each end** for the `f64`-seconds-versus-integer-
        // milliseconds seam described on the type. A window or an `as_of` that
        // walks off the calendar saturates to the far end of it, which is the
        // open direction and therefore the safe one.
        let slack = chrono::TimeDelta::seconds(1);
        let hi = as_of
            .checked_add_signed(slack)
            .unwrap_or(chrono::NaiveDateTime::MAX);
        let lo = chrono::TimeDelta::try_milliseconds((time_window_secs * 1000.0) as i64)
            .and_then(|window| as_of.checked_sub_signed(window))
            .and_then(|edge| edge.checked_sub_signed(slack))
            .unwrap_or(chrono::NaiveDateTime::MIN);

        let mut mask = [0u64; OCC_LON_WORDS];
        if lon_hi - lon_lo >= 360.0 {
            // Every longitude is inside the box, so `wrap_lon` can carry any
            // flash into it.
            for word in mask.iter_mut() {
                *word = u64::MAX;
            }
        } else {
            let a0 = lon_lo.rem_euclid(360.0);
            let c0 = a0.floor() as i64 - OCC_DILATION_CELLS;
            let c1 = (a0 + (lon_hi - lon_lo)).floor() as i64 + OCC_DILATION_CELLS;
            for c in c0..=c1 {
                let cell = c.rem_euclid(OCC_LON_CELLS as i64) as usize;
                mask[cell / 64] |= 1u64 << (cell % 64);
            }
        }

        let band_lo = occ_band(min_lat).saturating_sub(OCC_DILATION_CELLS as usize);
        let band_hi = (occ_band(max_lat) + OCC_DILATION_CELLS as usize).min(OCC_LAT_BANDS - 1);
        for band in band_lo..=band_hi {
            let base = band * OCC_LON_WORDS;
            let row = &self.cells[base..base + OCC_LON_WORDS];
            for (word, (occupied, admitted)) in row.iter().zip(mask.iter()).enumerate() {
                if occupied & admitted == 0 {
                    continue;
                }
                let (first, last) = self.times[base + word];
                if last >= lo && first <= hi {
                    return true;
                }
            }
        }
        false
    }
}

pub fn rasterize_glm_strikes(
    input: &GlmStrikesInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let GlmStrikesInput {
        flashes,
        zoom,
        is_dark,
        time_window_secs,
        now,
        device_scale,
    } = input;
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_glm_strikes ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            crop: None,
            rgba: vec![0u8; (width * height * 4) as usize].into(),
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
            blank_reason: Some(no_pixmap_reason(width, height)),
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;
    let mut hit_cells = HitCells::new(width, height);

    let base_size = glm_base_size(*zoom, *device_scale);

    let mut tally = ItemTally::default();

    // Before the loop, and counting the rows offered rather than the rows
    // drawn: the question this answers is whether the delivered block was read
    // by an instruction at all, and every row is loaded to be culled.
    crate::glm::fetch::gauge::rastered(flashes.len());

    for (i, flash) in flashes.iter().enumerate() {
        // Into the viewport's frame before either test: the flash carries a
        // folded longitude and `bounds` carries an unfolded one.
        let lon = mb.wrap_lon(flash.lon);
        if flash.lat < bounds.min_lat || flash.lat > bounds.max_lat || lon > bounds.max_lon {
            tally.off_texture();
            continue;
        }

        // **A flash later than the depicted instant has not happened yet.**
        // A cull and not a clamp. The clamp this replaces - `.max(0)` on the
        // age - read a future flash as age *zero*, which is the peak of
        // `time_decay_color`'s ramp, so a scrubbed pane drew tomorrow's
        // strikes at full brightness beside today's. Nothing survives the
        // clamp to guard: this subtraction is exact integer arithmetic on two
        // `NaiveDateTime`s, so past the cull it cannot be negative.
        if flash.time > *now {
            tally.filtered();
            continue;
        }
        let age_secs = (*now - flash.time).num_milliseconds() as f64 / 1000.0;
        if age_secs > *time_window_secs {
            tally.filtered();
            continue;
        }

        let (px, py) = mb.project(flash.lat, lon, w, h);
        if px < -base_size || px > w + base_size || py < -base_size || py > h + base_size {
            tally.off_texture();
            continue;
        }
        tally.on_texture();

        let bolt_size = base_size * (0.8 + energy_size_scale(flash.energy) * 0.4);

        let rgba = time_decay_color(age_secs, *time_window_secs, *is_dark);
        draw_lightning_bolt(&mut pixmap, px, py, bolt_size, rgba);

        let item_id = i as u32;
        let r = bolt_size * 0.6;
        let r2 = r * r;
        // **Clamped into the texture, as the station and report loops are.**
        // Unclamped, `(px - r) as i32` truncates *toward zero*, so a bolt
        // within `r` of the west or north edge sampled at negative texel
        // coordinates — which `HitCells::record` then saturated back into
        // column or row 0. The answer came out nearly right by that accident
        // and not by the arithmetic: over a 5400-position sweep of the four
        // edges, 52 positions recorded a cell that the clamped sampling
        // reaches directly and the unclamped one only reached through the
        // cast. Sampling inside the texture is what makes the cast's
        // behaviour stop mattering.
        let min_x = (px - r).max(0.0) as i32;
        let max_x = ((px + r) as i32).min(width as i32 - 1);
        let min_y = (py - r).max(0.0) as i32;
        let max_y = ((py + r) as i32).min(height as i32 - 1);
        let mut sy = min_y;
        let sy_end = max_y;
        while sy <= sy_end {
            let mut sx = min_x;
            let sx_end = max_x;
            while sx <= sx_end {
                let dx = sx as f32 - px;
                let dy = sy as f32 - py;
                if dx * dx + dy * dy <= r2 {
                    hit_cells.record(sx as f32, sy as f32, item_id);
                }
                sx += 4;
            }
            sy += 4;
        }
    }

    RasterizeOutput {
        crop: None,
        rgba: pixmap.take().into(),
        hit_cells: Some(hit_cells),
        alpha: AlphaMode::Premultiplied,
        blank: None,
        // **The geographic test runs before the two time tests here**, and
        // that ordering is the loop's, not the tally's. A flash outside the
        // box never reaches the age cull, so `filtered` undercounts on a
        // raster whose flashes are both stale and off screen — which is
        // exactly why `ItemTally::reason` asks the position question first and
        // reads `filtered` only when nothing was located at all. Both answers
        // count against covered ground, so the ordering names a sentence and
        // moves no figure across the subtotal.
        blank_reason: Some(tally.reason(flashes.len())),
    }
}

/// The geo-AABB cull [`draw_feature`] applies before any projection work.
///
/// `false` is "this feature cannot put a pixel in this texture", and it is the
/// whole of what `draw_feature` decides before it starts projecting. A feature
/// carrying no [`OverlayFeature::geo_bounds`] is not culled at all — the extent
/// is unknown, so the safe answer is that it paints.
///
/// **Shifted into the texture's frame first**, because
/// `OverlayFeature::geo_bounds` is the raw GeoJSON extent while a dateline
/// viewport arrives as e.g. -195..-165. That is the whole reason this is not a
/// bare `intersects`, and the whole reason it is one function rather than two
/// spellings: the dispatch door asks the same question one field earlier (see
/// [`any_feature_paints_in`]) and a second spelling that disagreed with this
/// one would refuse a raster the rasterizer would have painted.
/// Whether a projected path's extent overlaps the texture at all.
///
/// **The tally's position question for a row with no cull of its own.** Only
/// `rasterize_spc_discussions` needs it: every other item rasterizer already
/// takes a `continue` on a point outside the rect, and that `continue` is
/// where its tally is written. `tiny_skia` clips a path to the pixmap on its
/// own, so this changes no pixel — it names, at the moment the path exists,
/// the thing the clipper is about to do silently.
///
/// Half-open on neither side: a path that touches the edge is on the texture.
fn path_reaches_texture(path: &tiny_skia::Path, w: f32, h: f32) -> bool {
    let b = path.bounds();
    b.right() >= 0.0 && b.left() <= w && b.bottom() >= 0.0 && b.top() <= h
}

fn feature_survives_cull(feature: &OverlayFeature, mb: &MercatorBounds) -> bool {
    let Some(ref fb) = feature.geo_bounds else {
        return true;
    };
    let tb = GeoBounds {
        min_lat: merc_y_to_lat(mb.merc_y_min),
        max_lat: merc_y_to_lat(mb.merc_y_max),
        min_lon: mb.min_lon,
        max_lon: mb.max_lon,
    };
    let shift = mb.lon_shift(fb.min_lon, fb.max_lon);
    let shifted = GeoBounds {
        min_lon: fb.min_lon + shift,
        max_lon: fb.max_lon + shift,
        ..*fb
    };
    shifted.intersects(&tb)
}

/// Whether **any** of `features` survives [`feature_survives_cull`] against
/// `bounds` — the predicate a handler's `SourceHandler::paints_in` is built
/// from.
///
/// The Mercator frame is built once for the whole walk rather than once per
/// feature, which is the only difference between this and calling the cull in a
/// loop: `MercatorBounds::from_geo` clamps and transforms two latitudes, and a
/// national alert list would pay that per item.
///
/// # What a `false` here is allowed to conclude, per rasterizer
///
/// [`rasterize_nws_alerts`] paints through [`draw_feature`] and nothing else,
/// so "every feature was culled" and "the pixmap stays empty" are the *same
/// statement* about it, reached through the same code.
///
/// **[`rasterize_spc_outlooks`] is NOT that shape**, and the difference is
/// worth stating rather than assuming, because the safe-looking reading is that
/// two polygon layers behave alike. It runs a second painter,
/// `hatch::draw_hatch_pass`, which walks the features again and **does not
/// apply this cull at all** — it projects every hatched polygon and relies on
/// tiny-skia clipping to the pixmap for anything off-texture. The predicate is
/// still sound there, but by an *argument* and not by a shared implementation:
/// [`OverlayFeature::geo_bounds`] is computed by `OverlayFeature::new` from the
/// very polygons the hatch pass projects, so a feature this cull rejects cannot
/// project inside the texture either. **Re-check that if the hatch pass ever
/// paints something not derived from `feature.polygons`.**
///
/// A rasterizer with a painter of any other shape — a marker whose radius
/// reaches outside its own centre's box, a wash grown from a station by a fixed
/// ground distance — is not covered by this at all and must not be gated on it.
pub fn any_feature_paints_in<'a>(
    features: impl IntoIterator<Item = &'a OverlayFeature>,
    bounds: &GeoBounds,
) -> bool {
    let mb = MercatorBounds::from_geo(bounds);
    features
        .into_iter()
        .any(|feature| feature_survives_cull(feature, &mb))
}

/// Draw one feature, and say whether its extent reached this texture.
///
/// **The answer is the cull's, not the painter's**: `true` means the feature
/// survived [`feature_survives_cull`] and its polygons were handed to the
/// rasterizer, and says nothing about whether a pixel changed. That is the
/// distinction [`BlankReason::OutsideView`] and [`BlankReason::DrewNoInk`] are
/// kept apart by, and it is why this returns the cull's verdict rather than
/// "did any `fill_path` run": a feature whose every polygon fails to project
/// is still a feature the view was over.
fn draw_feature(
    pixmap: &mut Pixmap,
    feature: &OverlayFeature,
    mb: &MercatorBounds,
    w: f32,
    h: f32,
    scale: f32,
) -> bool {
    if !feature_survives_cull(feature, mb) {
        return false;
    }

    for polygon in &feature.polygons {
        let Some(projected) = project_polygon(polygon, mb, w, h) else {
            continue;
        };
        if let Some((path, rule)) = build_filled_polygon_path(&projected.exterior, &projected.holes)
        {
            fill_path(pixmap, &path, feature.fill_rgba, rule);
            if feature.stroke_rgba[3] > 0 {
                let sw = scaled_stroke_width(&path, 1.5, scale);
                stroke_path(pixmap, &path, feature.stroke_rgba, sw);
            }
        }
    }
    true
}

/// Thins the stroke below a 40-point minimum dimension, so a small polygon is
/// not swallowed by its own outline. Points in, texels per point as `scale`.
fn scaled_stroke_width(path: &tiny_skia::Path, base: f32, scale: f32) -> f32 {
    let b = path.bounds();
    let min_dim = b.width().min(b.height());
    (min_dim / 40.0 * base).clamp(0.5 * scale, base * scale)
}

/// A device scale that can be multiplied by: a zero draws nothing and a `NaN`
/// makes `Rect::from_xywh` return `None`, so the floor is one texel per point.
fn sane_device_scale(device_scale: f32) -> f32 {
    if device_scale.is_finite() {
        device_scale.max(1.0)
    } else {
        1.0
    }
}

/// Interior rings below this projected area (square pixels) are dropped: such a
/// ring encloses nothing, but the stroke would draw it as a hairline scratch.
const MIN_HOLE_AREA_PX: f32 = 0.25;

/// Interior rings thinner than this *on average* — twice the area over the
/// perimeter — are dropped as well: the worst measured spreads 0.39 px² over
/// 31.1 px of rim. A quarter of a pixel drops 78 of the 2,040 rings above the
/// area floor.
const MIN_HOLE_WIDTH_PX: f32 = 0.25;

fn ring_area_px(pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut twice = 0.0;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        twice += x1 * y2 - x2 * y1;
    }
    (twice * 0.5).abs()
}

fn ring_perimeter_px(pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 2 {
        return 0.0;
    }
    let mut total = 0.0;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        total += ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
    }
    total
}

pub(crate) fn hole_is_drawable(pts: &[(f32, f32)]) -> bool {
    let area = ring_area_px(pts);
    let perimeter = ring_perimeter_px(pts);
    // `2 * area >= width * perimeter` rather than a division, so a ring of zero
    // perimeter cannot produce a NaN.
    area >= MIN_HOLE_AREA_PX && 2.0 * area >= MIN_HOLE_WIDTH_PX * perimeter
}

/// A GeoJSON polygon projected to texture pixels: the exterior ring and the
/// interior rings [`hole_is_drawable`] keeps — one definition of the interior,
/// so the fill and the hatch mask cannot disagree.
pub(crate) struct ProjectedPolygon {
    pub(crate) exterior: Vec<(f32, f32)>,
    pub(crate) holes: Vec<Vec<(f32, f32)>>,
}

/// The rigid longitude shift that carries `ring` into `mb`'s frame, from the
/// ring's own longitude extent. Zero for an empty ring.
fn ring_lon_shift(ring: &[(f64, f64)], mb: &MercatorBounds) -> f64 {
    match crate::render::geo::ring_lon_extent(ring) {
        Some((min_lon, max_lon)) => mb.lon_shift(min_lon, max_lon),
        None => 0.0,
    }
}

/// `None` when the exterior ring is too short to enclose anything. The whole
/// polygon is translated by one shared multiple of 360°, taken from the
/// *exterior* ring: sharing it is what makes the move rigid.
pub(crate) fn project_polygon(
    polygon: &[GeoPolygonRing],
    mb: &MercatorBounds,
    w: f32,
    h: f32,
) -> Option<ProjectedPolygon> {
    let exterior_ring = polygon.first()?;
    if exterior_ring.len() < 3 {
        return None;
    }
    let shift = ring_lon_shift(exterior_ring, mb);
    let project = |ring: &[(f64, f64)]| -> Vec<(f32, f32)> {
        strip_closing_dup(ring)
            .iter()
            .map(|&(lat, lon)| mb.project(lat, lon + shift, w, h))
            .collect()
    };
    // `polygon[1..]` are interior rings — holes.
    let holes = polygon[1..]
        .iter()
        .filter(|ring| ring.len() >= 3)
        .map(|ring| project(ring))
        .filter(|pts| hole_is_drawable(pts))
        .collect();
    Some(ProjectedPolygon {
        exterior: project(exterior_ring),
        holes,
    })
}

fn push_ring(pb: &mut PathBuilder, pts: &[(f32, f32)]) {
    pb.move_to(pts[0].0, pts[0].1);
    for &(x, y) in &pts[1..] {
        pb.line_to(x, y);
    }
    pb.close();
}

pub(crate) fn build_polygon_path(pts: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    if pts.len() < 3 {
        return None;
    }
    let mut pb = PathBuilder::new();
    push_ring(&mut pb, pts);
    let path = pb.finish()?;
    let b = path.bounds();
    if b.width() < 0.1 || b.height() < 0.1 {
        return None;
    }
    Some(path)
}

/// A whole GeoJSON polygon — exterior ring plus interior rings — as one path,
/// with the fill rule it must be filled under.
///
/// **Even-odd, because that is what the hit test computes.**
/// `geo_point_in_feature` counts ray crossings per ring and never looks at
/// orientation. Measured over a 7,015-zone NWS cache, two of 2,064 interior
/// rings wind *with* their exterior; another 2,515 have zero area, over a ring
/// population this reading does not name — it exceeds the 2,064. The
/// exterior alone keeps `Winding`, since 4.7% of rings self-intersect.
pub(crate) fn build_filled_polygon_path(
    exterior: &[(f32, f32)],
    holes: &[Vec<(f32, f32)>],
) -> Option<(tiny_skia::Path, FillRule)> {
    let exterior_path = build_polygon_path(exterior)?;
    if holes.is_empty() {
        return Some((exterior_path, FillRule::Winding));
    }
    let mut pb = PathBuilder::new();
    pb.push_path(&exterior_path);
    for hole in holes {
        if hole.len() >= 3 {
            push_ring(&mut pb, hole);
        }
    }
    let path = pb.finish()?;
    Some((path, FillRule::EvenOdd))
}

fn fill_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4], rule: FillRule) {
    if rgba[3] == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, rule, Transform::identity(), None);
}

fn stroke_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4], width: f32) {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), None);
}

pub(crate) fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

use crate::hrrr::HrrrGridData;
use crate::render::gridded::ValuesRef;

fn merc_y_to_lat(merc_y: f64) -> f64 {
    squallar_geo::mercator_y_to_lat_rad(merc_y).to_degrees()
}

/// Half-open `(i, j)` ranges of the grid the rasterizer touches.
///
/// Carried on the wire rather than recomputed on the far side: the window math
/// runs through libm, and a re-derivation could land one index off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexWindow {
    pub i0: usize,
    pub i1: usize,
    pub j0: usize,
    pub j1: usize,
}

impl IndexWindow {
    pub fn is_empty(&self) -> bool {
        self.i0 >= self.i1 || self.j0 >= self.j1
    }

    pub fn area(&self) -> usize {
        if self.is_empty() {
            0
        } else {
            (self.i1 - self.i0) * (self.j1 - self.j0)
        }
    }

    /// This window cut down to the grid it indexes, so a window off a message
    /// port can never name a point past the grid that arrived beside it.
    fn clamped(&self, ni: usize, nj: usize) -> Self {
        Self {
            i0: self.i0.min(ni),
            i1: self.i1.min(ni),
            j0: self.j0.min(nj),
            j1: self.j1.min(nj),
        }
    }

    /// The cells this window can *draw*: one ring in from the edge, because
    /// sizing a cell reads its four neighbours, and only a neighbour inside the
    /// window has been projected.
    fn interior(&self, ni: usize, nj: usize) -> Self {
        Self {
            i0: self.i0 + usize::from(self.i0 > 0),
            i1: self.i1 - usize::from(self.i1 < ni),
            j0: self.j0 + usize::from(self.j0 > 0),
            j1: self.j1 - usize::from(self.j1 < nj),
        }
    }
}

/// How far outside the texture a grid point can be and still paint into it.
/// `0.55` cells is the overlap the cell loop applies; the rest is headroom.
const CELL_REACH: f64 = 0.75;

/// The same reach in pixels; the half-extent has a `0.5` px floor. Up from 1.5.
const PIXEL_REACH: f64 = 2.0;

/// `bounds` widened by `lon_pad` degrees and `merc_pad` of Mercator `y`.
///
/// Latitude is padded in Mercator, the axis the texture's pixels are linear in.
fn grow_bounds(bounds: &GeoBounds, lon_pad: f64, merc_pad: f64) -> GeoBounds {
    let mb = MercatorBounds::from_geo(bounds);
    GeoBounds {
        min_lon: bounds.min_lon - lon_pad,
        max_lon: bounds.max_lon + lon_pad,
        min_lat: merc_y_to_lat(mb.merc_y_min - merc_pad),
        max_lat: merc_y_to_lat(mb.merc_y_max + merc_pad),
    }
}

/// Which grid points [`rasterize_gridded`] must project: the whole grid
/// unless the coordinates can name a narrower window — see
/// [`crate::hrrr::GridCoords::index_bounds`].
///
/// A point *outside* the texture still paints into it, so the **box** is grown
/// before the index range is taken — a Lambert row is not a parallel. The pad is
/// **absolute**, not a fraction of the box.
fn projection_window(
    coords: &crate::hrrr::GridCoords,
    ni: usize,
    nj: usize,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> IndexWindow {
    let full = IndexWindow {
        i0: 0,
        i1: ni,
        j0: 0,
        j1: nj,
    };

    // A grid with a longitude discontinuity — anti-meridian or the cone's own
    // seam — has an `i` neighbour most of a turn away, so the "0.55 of a cell"
    // reach stops describing it. On a Lambert grid that costs the `j` axis too:
    // a row is not a parallel, both indices are axes of the projection plane,
    // and `detect_longitude_wrap` reports a break found stepping along *either*
    // of them, so neither axis has a window left. Measured, not argued: with
    // only `i` widened and `j` narrowed, both wrapping-Lambert suites below
    // still fail.
    //
    // Where the rows *are* parallels the wrap reaches one axis only, and
    // `index_bounds` answers each on its own terms: the latitude bracket
    // always, and the column pair when the box does not walk over the axis's
    // own end. GMGSI is 15,000,000 points and this is the whole of its window.
    if coords.wraps_longitude() && !coords.rows_are_parallels() {
        return full;
    }

    // **Into the grid's own longitude frame, before an edge of the box is
    // read.** The box arrives in the app's one continuous frame —
    // `walkers::Projector::unproject` folds nothing — so past the antimeridian
    // the ground at `-130..-60` is written `230..300` (`seam_frame_tests`).
    // Each *point* is carried to the representation nearest the box as it is
    // projected (`rasterize_gridded`, `nearest_lon`), but which points are
    // projected is decided here, and `index_bounds` reads the box's *edges*
    // against the grid's stored longitudes: `(min_lon - lon0) / dlon` on a
    // regular grid, which puts `230` at column 36,000 of MRMS's 7,000 and
    // clamps to an empty window. So the box is the datum, the grid's frame is
    // the target, and the box moves by the whole turn that brings it nearest
    // — the identity for a box already in frame. Not gated on
    // `wraps_longitude`: the Lambert arm folds an edge on its own in `theta`
    // and the wrapping separable arm locates each edge angularly, and a fold
    // that is a no-op there is what keeps one geometry from projecting two
    // ways depending on which arm holds it. `regional_window_frame_tests`.
    let shift = coords.lon_frame().map_or(0.0, |(west, east)| {
        crate::render::geo::box_lon_shift(bounds.min_lon, bounds.max_lon, west, east)
    });
    let carried = GeoBounds {
        min_lon: bounds.min_lon + shift,
        max_lon: bounds.max_lon + shift,
        ..*bounds
    };
    let bounds = &carried;

    // `cos` at the box's own extreme latitude: the only cells that can reach
    // the texture sit within a cell of the box.
    let edge_lat = bounds.min_lat.abs().max(bounds.max_lat.abs());
    let Some(cell_deg) = coords.cell_span_degrees(edge_lat) else {
        return full;
    };
    let mb = MercatorBounds::from_geo(bounds);
    let lon_pad = CELL_REACH * cell_deg
        + PIXEL_REACH * (bounds.max_lon - bounds.min_lon) / width.max(1) as f64;
    // A cell `cell_deg` **degrees** tall is `cell_deg / cos(lat)` of Mercator
    // **y**, and the two agree only at the equator. Spending the pad as though
    // they were the same reaches `cos(lat)` of the way it means to: 0.71 at
    // 45 N, 0.31 at 72 N — GMGSI's own top row — and `low`/`high` below only
    // hand back one index of slack, so past about 65 N the shortfall is more
    // than a whole cell and a row that paints is left unprojected.
    let merc_scale = edge_lat
        .min(MAX_MERCATOR_LAT)
        .to_radians()
        .cos()
        .max(f64::MIN_POSITIVE);
    let merc_pad = CELL_REACH * cell_deg.to_radians() / merc_scale
        + PIXEL_REACH * (mb.merc_y_max - mb.merc_y_min) / height.max(1) as f64;

    let grown = grow_bounds(bounds, lon_pad, merc_pad);
    let Some((fi0, fi1, fj0, fj1)) = coords.index_bounds(&grown, ni, nj) else {
        return full;
    };

    // One more cell each way so every drawn cell's four neighbours are
    // projected too — an unprojected neighbour resizes the cell.
    let low = |f: f64, n: usize| (f.floor() - 1.0).max(0.0).min(n as f64) as usize;
    let high = |f: f64, n: usize| (f.ceil() + 1.0).max(0.0).min(n as f64) as usize;

    IndexWindow {
        i0: low(fi0, ni),
        i1: high(fi1, ni),
        j0: low(fj0, nj),
        j1: high(fj1, nj),
    }
}

/// What [`rasterize_gridded`] reads. The HRRR values vector is 1,905,141
/// `f32` — **7.62 MB** — and the raster only reads the points inside its own
/// [`projection_window`], so this is an enum over how much of the grid is in
/// hand: [`Self::Whole`] carries it by `Arc`, [`Self::Window`] carries the
/// window and exactly its values (the wire).
// `Window` is ~240 B against `Whole`'s 8 (it carries `GridCoords` and the
// tagged `GridValues`, not a `Vec<f32>`). It is built once per job and handed
// to `DescribedJob::new`, which boxes it; a `Box` here would be a second
// pointer to chase on every raster read for nothing saved.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum GriddedInput {
    Whole(std::sync::Arc<HrrrGridData>),
    /// A whole grid held by a source that carries **no source-specific enum**
    /// into the raster — the shape a second gridded source registers in. Same
    /// posture as [`Self::Whole`] (hold it all, cut at encode), without the
    /// model's own type in the signature.
    Resident(std::sync::Arc<crate::render::gridded::ResidentGrid>),
    Window(GridWindow),
}

squallar_source::impl_job_input!(GriddedInput);

/// The wire form of a gridded raster: the field's identity, the grid's shape
/// and coordinates, the [`IndexWindow`] its values were cut to, and those
/// values alone.
#[derive(Debug, Clone, PartialEq)]
pub struct GridWindow {
    /// **A field identity, not a source's own enum.** The raster resolves it
    /// through [`crate::render::gridded::field_paint`] and refuses what that
    /// does not answer, so a second gridded source needs no arm here.
    pub field: squallar_source::product::FieldId,
    /// The **full grid's** shape, which `win` and `coords` index against.
    pub ni: usize,
    pub nj: usize,
    pub coords: crate::hrrr::GridCoords,
    /// The window `values` covers, computed at the dispatch and carried.
    pub win: IndexWindow,
    /// Row-major within `win`: point `(i, j)` of the grid is
    /// `values[(j - win.j0) * (win.i1 - win.i0) + (i - win.i0)]`.
    ///
    /// **In the sending grid's own storage width**, which is what keeps the
    /// worker's copy the same size as the page's: a mosaic that is 16-bit
    /// codes resident travels as codes and is held as codes here, and the
    /// widening happens one value at a time at [`GriddedInput::value_at`].
    pub values: crate::render::gridded::GridValues,
}

impl GriddedInput {
    /// The field being drawn. The whole-grid arm reads it off the model's own
    /// registration, which is where the persisted spelling already lives.
    pub fn field(&self) -> &squallar_source::product::FieldId {
        match self {
            Self::Whole(grid) => &crate::hrrr::fields::spec(grid.parameter).id,
            Self::Resident(grid) => &grid.field,
            Self::Window(window) => &window.field,
        }
    }

    pub fn shape(&self) -> (usize, usize) {
        match self {
            Self::Whole(grid) => (grid.ni, grid.nj),
            Self::Resident(grid) => (grid.ni, grid.nj),
            Self::Window(window) => (window.ni, window.nj),
        }
    }

    pub fn coords(&self) -> &crate::hrrr::GridCoords {
        match self {
            Self::Whole(grid) => &grid.coords,
            Self::Resident(grid) => &grid.coords,
            Self::Window(window) => &window.coords,
        }
    }

    /// The values of a grid held **whole**, with the row stride they are
    /// indexed by — `None` for the windowed arm, which carries only a cut.
    ///
    /// The two whole arms differ in nothing the raster reads, so the readers
    /// below take this rather than repeating an arm each. A
    /// [`ValuesRef`] rather than a `&[f32]`: the storage width is the grid's
    /// business and none of the raster's.
    fn whole_values(&self) -> Option<(ValuesRef<'_>, usize)> {
        match self {
            Self::Whole(grid) => Some((grid.values.view(), grid.ni)),
            Self::Resident(grid) => Some((grid.values.view(), grid.ni)),
            Self::Window(_) => None,
        }
    }

    /// How this input's values are stored, whichever arm holds them.
    ///
    /// What the wire tags itself with, and what the encoders dispatch on.
    pub fn values_ref(&self) -> ValuesRef<'_> {
        match self {
            Self::Whole(grid) => grid.values.view(),
            Self::Resident(grid) => grid.values.view(),
            Self::Window(window) => window.values.view(),
        }
    }

    /// The index window a raster of `bounds` may draw from: computed for a whole
    /// grid, **carried** for a window.
    pub fn window_for(&self, bounds: &GeoBounds, width: u32, height: u32) -> IndexWindow {
        let (ni, nj) = self.shape();
        match self {
            Self::Window(window) => window.win.clamped(ni, nj),
            _ => projection_window(self.coords(), ni, nj, bounds, width, height),
        }
    }

    /// The value at grid point `(i, j)`, or `None` where there is none to read
    /// — past a short values vector, or outside the carried window.
    ///
    /// **The one place a stored value becomes an `f32`.** The whole rest of
    /// `rasterize_gridded` reads coordinates, so a narrower store costs exactly
    /// this call: a bounds-checked index plus, on the scaled arm, two flops and
    /// a walk of at most `MAX_NAN_CODES` reserved codes — strictly less than
    /// the `partition_point` the `color_for_value` on the next line already
    /// pays. Nothing here allocates.
    pub(crate) fn value_at(&self, i: usize, j: usize) -> Option<f32> {
        if let Some((values, stride)) = self.whole_values() {
            return values.grid_value(i, j, stride);
        }
        let Self::Window(window) = self else {
            return None;
        };
        let win = &window.win;
        if i < win.i0 || i >= win.i1 || j < win.j0 || j >= win.j1 {
            return None;
        }
        // **Through the store**, not through one flat index computed here: a
        // tiled band is cut in whole tile rows and keeps the grid's own
        // numbering, where the flat stores are cut column by column and are
        // numbered from the window's corner. See `ValuesRef::window_value`.
        window.values.view().window_value(i, j, win)
    }

    /// One row of `win`'s values as `f32`; the whole arm pads with NaN where
    /// its vector runs short, which paints nothing. A callback per row so the
    /// encoder writes straight from the grid's storage.
    ///
    /// **The `f32`-store reader.** A grid whose values are stored narrower has
    /// no `&[f32]` to lend and is written by
    /// [`Self::for_each_window_row_raw`] instead; this yields nothing for one,
    /// and the only production caller — `GriddedJob::encode` — dispatches on
    /// [`Self::values_ref`] rather than calling this blind. Expanding here
    /// instead would put a mosaic-row buffer on the **frame thread**, which is
    /// where `JobRequest::to_bytes` runs.
    pub fn for_each_window_row(&self, win: &IndexWindow, mut f: impl FnMut(&[f32])) {
        if win.is_empty() {
            return;
        }
        if let Some((values, stride)) = self.whole_values() {
            let ValuesRef::F32(values) = values else {
                return;
            };
            let mut padded: Vec<f32> = Vec::new();
            for j in win.j0..win.j1 {
                let start = j * stride + win.i0;
                let end = j * stride + win.i1;
                if end <= values.len() {
                    f(&values[start..end]);
                } else {
                    padded.clear();
                    padded.extend((start..end).map(|k| values.get(k).copied().unwrap_or(f32::NAN)));
                    f(&padded);
                }
            }
            return;
        }
        let Self::Window(window) = self else {
            return;
        };
        let ValuesRef::F32(values) = window.values.view() else {
            return;
        };
        let carried = &window.win;
        let row_w = carried.i1 - carried.i0;
        for j in win.j0..win.j1 {
            let row = (j - carried.j0) * row_w;
            f(&values[row + (win.i0 - carried.i0)..row + (win.i1 - carried.i0)]);
        }
    }

    /// **The points inside `win` that carry no reading**, as indices into the
    /// window the encoder is about to write — `win.area()`-space, row-major,
    /// the same space the payload's values are in.
    ///
    /// Empty for every store but the byte one, which is the only one whose
    /// missing points are a property of *where* a value sits rather than of
    /// the code it carries: `ScaledU16` reserves codes, and an `f32` says NaN
    /// in the value itself. See `gridded::ByteCodes`.
    ///
    /// **Bounded work on the frame thread.** `JobRequest::to_bytes` runs at the
    /// dispatch site, so this is at most `MAX_ABSENT_POINTS` divisions and
    /// comparisons — no walk over the window, and nothing proportional to the
    /// grid. A per-point representation of the same fact could not be cut to a
    /// strided window without one.
    ///
    /// The order survives the cut: the indices arrive ascending, and both
    /// spaces are row-major over the same rows, so the mapping is monotone —
    /// which is what `ByteCodes::new` demands at the far end.
    pub fn absent_in_window(&self, win: &IndexWindow) -> Vec<u32> {
        // Where this store's index space starts, and how wide its rows are.
        // A resident grid is indexed from the grid's own origin; a window that
        // has already been cut is indexed from its own.
        let (absent, i0, j0, stride) = match self {
            Self::Resident(grid) => match grid.values.view() {
                ValuesRef::Bytes(b) => (b.absent(), 0, 0, grid.ni),
                ValuesRef::F32(_) | ValuesRef::Scaled(_) | ValuesRef::Tiled(_) => {
                    return Vec::new();
                }
            },
            Self::Window(window) => match window.values.view() {
                ValuesRef::Bytes(b) => (
                    b.absent(),
                    window.win.i0,
                    window.win.j0,
                    window.win.i1.saturating_sub(window.win.i0),
                ),
                ValuesRef::F32(_) | ValuesRef::Scaled(_) | ValuesRef::Tiled(_) => {
                    return Vec::new();
                }
            },
            Self::Whole(_) => return Vec::new(),
        };
        if absent.is_empty() || win.is_empty() || stride == 0 {
            return Vec::new();
        }
        let row_w = win.i1 - win.i0;
        absent
            .iter()
            .filter_map(|&k| {
                let k = k as usize;
                let j = j0 + k / stride;
                let i = i0 + k % stride;
                (i >= win.i0 && i < win.i1 && j >= win.j0 && j < win.j1)
                    .then(|| ((j - win.j0) * row_w + (i - win.i0)) as u32)
            })
            .collect()
    }

    /// One row of `win`'s values as **stored bytes**, in the storage's own
    /// width — the shape the wire carries and the transport lends.
    ///
    /// **No expansion, no scratch, no allocation.** That is the point rather
    /// than a detail: this runs inside `JobRequest::to_bytes` on the frame
    /// thread, so a per-row widening buffer here would trade the footprint win
    /// for frame time. Each row is handed out as a borrow of the grid's own
    /// allocation.
    ///
    /// A row that runs past the end yields **nothing at all** rather than a
    /// short run or a fabricated pad. The far end checks the payload length
    /// against the window the head names, so a short write is refused as a
    /// length mismatch — which is the honest answer for a values vector that
    /// does not match the shape beside it, and is unreachable for a decoded
    /// grid anyway (`parse_grib2_raw_in` refuses any other count).
    pub fn for_each_window_row_raw(&self, win: &IndexWindow, mut f: impl FnMut(&[u8])) {
        if win.is_empty() {
            return;
        }
        if let Some((values, stride)) = self.whole_values() {
            for j in win.j0..win.j1 {
                let Some(bytes) = values.sample_bytes(j * stride + win.i0..j * stride + win.i1)
                else {
                    return;
                };
                f(bytes);
            }
            return;
        }
        let Self::Window(window) = self else {
            return;
        };
        let carried = &window.win;
        let row_w = carried.i1 - carried.i0;
        let values = window.values.view();
        for j in win.j0..win.j1 {
            let row = (j - carried.j0) * row_w;
            let Some(bytes) =
                values.sample_bytes(row + (win.i0 - carried.i0)..row + (win.i1 - carried.i0))
            else {
                return;
            };
            f(bytes);
        }
    }
}

/// What a **half-turn** of longitude spans in a texture `width` px wide over
/// `bounds`. Longitude is mapped linearly, so this is the whole of the
/// conversion; [`rasterize_gridded`]'s cell loop uses it to tell a neighbour
/// from the same meridian a turn away. `inf` for a degenerate box, which
/// refuses nothing and leaves the sizing as it was.
///
/// **Computed in the app's one continuous longitude frame, and it has to be.**
/// `walkers::Projector::unproject` is linear in pixel x and folds nothing, so
/// a viewport straddling the seam arrives as e.g. `179.25..180.75`,
/// `squallar_egui::overlay_cache::viewport_geo_bounds` takes its two corners as
/// they come, and `OverlayTexturePlan::coverage` grows the result without
/// folding: the denominator here is the box's true width. Fold the box into
/// ±180 anywhere upstream and that 1.5-degree view (2.25 with its overdraw)
/// measures 357.75: a half-turn shrinks from 80 textures to half of one —
/// 345 901 px to 2 172 px on a 4317 px picture — and the box now claims the
/// ground it does not show. The refusal keeps refusing (the seam pair is 4334
/// px apart against that 2172), so the smear does not come back; what comes
/// instead is the whole world drawn into a pane a degree and a half wide —
/// 4978 of GMGSI's 5000 columns, 100 % of the picture, the seam columns
/// themselves off both ends of the texture (measured once on the seam probe's
/// fixture, 2026-09-06; not gated). `gmgsi_seam_probe_tests` cannot see any
/// of it: it pins one viewport's output, and the frame is decided upstream of
/// this crate. `seam_frame_tests` builds the box the way the app does, asserts
/// it is continuous, and refuses the folded spelling with both denominators
/// named.
pub(crate) fn half_turn_px(bounds: &GeoBounds, width: u32) -> f32 {
    (180.0 / (bounds.max_lon - bounds.min_lon) * f64::from(width as f32)) as f32
}

/// One grid cell's filled rect, as [`rasterize_gridded`] sized it.
///
/// Cells are held a row at a time rather than filled where they are computed,
/// because a cell cannot know what it is allowed to skip until the cells drawn
/// **after** it are known — see [`Self::clip_x`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellRect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    /// Straight (unmultiplied) RGBA, as this rasterizer writes — in the
    /// picture's own element type, so a cell's colour reaches a pixel as one
    /// word store rather than four byte stores. `Color32` is a 4-byte
    /// container here and states nothing about alpha; the convention is
    /// [`RasterizeOutput::alpha`], which this rasterizer answers
    /// [`AlphaMode::Straight`] until the funnel's premultiply.
    color: Color32,
}

impl CellRect {
    /// Nothing left to write. Reachable two ways: a cell clamped entirely off
    /// the texture, and a cell every pixel of which a later cell covers.
    fn is_empty(&self) -> bool {
        self.x1 < self.x0 || self.y1 < self.y0
    }

    /// Give up the columns a **strictly later** cell's rect `l` covers.
    ///
    /// **The output cannot move, and that is the whole construction.** The fill
    /// below overwrites — "no blending between adjacent grid cells" — so a
    /// pixel's colour is the colour of the *last* cell to cover it. Removing
    /// from this cell only pixels that `l` covers therefore changes neither
    /// which pixels end up painted (`l` paints every one of them) nor what
    /// colour any of them ends up (`l` is later, so it already won them). What
    /// it removes is the store this cell was making into a pixel it was about
    /// to lose.
    ///
    /// `l` is the later cell's rect **before** `l`'s own clipping, which is what
    /// makes the argument close over a whole row: whatever `l` gives up, it
    /// gives up only to a cell later still, so the union of everything from `l`
    /// onward still covers `l`'s full rect.
    ///
    /// **Two conditions, both load-bearing.** `l` must span every row this cell
    /// writes, or the part given up is only covered on some of them; and `l`
    /// must reach one of this cell's own ends, or what it covers is an interior
    /// band and giving it up would split one rect into two. Where either fails
    /// nothing is given up, which costs a store and cannot cost a pixel — the
    /// posture a Lambert grid lands in, where a row is not a parallel and two
    /// cells beside each other can sit a pixel apart in `y`.
    fn clip_x(&mut self, l: &CellRect) {
        if self.is_empty() || l.is_empty() {
            return;
        }
        if l.y0 > self.y0 || l.y1 < self.y1 {
            return;
        }
        if l.x1 < self.x0 || l.x0 > self.x1 {
            return;
        }
        if l.x0 <= self.x0 && l.x1 >= self.x1 {
            self.x1 = self.x0 - 1;
        } else if l.x1 >= self.x1 {
            self.x1 = l.x0 - 1;
        } else if l.x0 <= self.x0 {
            self.x0 = l.x1 + 1;
        }
    }

    /// [`Self::clip_x`] on the other axis: the rows a later cell covers.
    ///
    /// Applied **after** `clip_x`, so the columns it tests against are the ones
    /// this cell will really write. That is not merely an optimisation: a cell
    /// narrowed in `x` is one a later row's cell is more likely to span, so the
    /// two clips compose into the full partition rather than half of it.
    fn clip_y(&mut self, l: &CellRect) {
        if self.is_empty() || l.is_empty() {
            return;
        }
        if l.x0 > self.x0 || l.x1 < self.x1 {
            return;
        }
        if l.y1 < self.y0 || l.y0 > self.y1 {
            return;
        }
        if l.y0 <= self.y0 && l.y1 >= self.y1 {
            self.y1 = self.y0 - 1;
        } else if l.y1 >= self.y1 {
            self.y1 = l.y0 - 1;
        } else if l.y0 <= self.y0 {
            self.y0 = l.y1 + 1;
        }
    }

    /// Store this cell's colour into every pixel it owns, and count the stores.
    fn fill(&self, px: &mut [Color32], width: u32, written_px: &mut u64) {
        if self.is_empty() {
            return;
        }
        *written_px += (self.x1 - self.x0 + 1) as u64 * (self.y1 - self.y0 + 1) as u64;
        for y in self.y0..=self.y1 {
            let row_start = (y as u32 * width) as usize;
            for x in self.x0..=self.x1 {
                // Overwrite — no blending between adjacent grid cells.
                px[row_start + x as usize] = self.color;
            }
        }
    }
}

/// Emit one row of cells, each shrunk to the pixels no later cell takes from it.
///
/// `row` is clipped in place and then filled **left to right**, and `next` — the
/// row drawn after it, unclipped — is what its cells give their bottom rows up
/// to. The two together are the whole of the saving: at sub-pixel spacing a
/// cell's `0.5` px half-extent floor makes its rect two pixels by two whatever
/// the cell's real size is, and its right column and bottom row are then the
/// left column and top row of cells that overwrite them a moment later.
fn emit_cell_row(
    px: &mut [Color32],
    width: u32,
    row: &mut [Option<CellRect>],
    next: Option<&[Option<CellRect>]>,
    written_px: &mut u64,
) {
    // Right to left, so `ahead` is the next drawn cell in the row — and it is
    // the rect that cell was *sized* to, never the one it was clipped to.
    let mut ahead: Option<CellRect> = None;
    for idx in (0..row.len()).rev() {
        let Some(mut cell) = row[idx] else {
            continue;
        };
        let sized = cell;
        if let Some(l) = ahead {
            cell.clip_x(&l);
        }
        if let Some(l) = next.and_then(|n| n[idx]) {
            cell.clip_y(&l);
        }
        row[idx] = Some(cell);
        ahead = Some(sized);
    }
    for cell in row.iter().flatten() {
        cell.fill(px, width, written_px);
    }
}

/// Writes pixels directly rather than through tiny-skia: one filled rectangle
/// per grid point, sized from its neighbour spacing.
pub fn rasterize_gridded(
    input: &GriddedInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    // **Pixels, not bytes, and that is the whole of the allocation saving.**
    // A `Vec<u8>` can never be handed to a consumer that wants `Vec<Color32>`
    // — the alignment a block is freed with is the one it was taken with — so
    // a byte picture is copied into a second buffer its own size at the
    // arrival, 40.79 MiB at the 4317 x 2477 case. Written as pixels here, the
    // one buffer is the one the texture upload takes.
    //
    // `zeroed_vec`, not `vec![Color32::TRANSPARENT; n]`: the latter has no
    // `IsZero` specialisation for a foreign element type, so it would take an
    // uninitialised block and write the zeros the kernel hands over already
    // zeroed. This is `alloc_zeroed`, which is what `vec![0u8; size]` was.
    let size = (width * height * 4) as usize;
    let mut px: Vec<Color32> = bytemuck::zeroed_vec(size / 4);
    let (ni, nj) = input.shape();
    let coords = input.coords();

    let empty = input.whole_values().is_some_and(|(v, _)| v.is_empty());
    if empty || width == 0 || height == 0 || ni == 0 || nj == 0 {
        return RasterizeOutput {
            crop: None,
            rgba: px.into(),
            hit_cells: None,
            alpha: AlphaMode::Straight,
            blank: None,
            blank_reason: Some(BlankReason::EmptyInput),
        };
    }

    // Resolved once, outside the cell loop, and **refused** rather than
    // defaulted: a field this build does not register is a newer build's, and
    // painting it through some other field's colours would be a silent misread.
    let Some(paint) = crate::render::gridded::field_paint(input.field()) else {
        return RasterizeOutput {
            crop: None,
            rgba: px.into(),
            hit_cells: None,
            alpha: AlphaMode::Straight,
            blank: None,
            blank_reason: Some(BlankReason::UnknownField),
        };
    };

    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // Only points that can influence a pixel of *this* texture are projected;
    // `coords.at` over all 1.9 M points was two thirds of this function's cost.
    let win = input.window_for(bounds, width, height);
    if win.is_empty() {
        return RasterizeOutput {
            crop: None,
            rgba: px.into(),
            hit_cells: None,
            alpha: AlphaMode::Straight,
            blank: None,
            blank_reason: Some(BlankReason::WindowEmpty),
        };
    }
    let win_w = win.i1 - win.i0;

    // One row of the window, projected. The cell loop reads each neighbour
    // several times, so the row it is on and the two beside it are pre-projected
    // rather than recomputed per read.
    let project_row = |j: usize, row: &mut Vec<(f32, f32)>| {
        row.clear();
        row.reserve(win_w);
        for i in win.i0..win.i1 {
            match coords.at(j * ni + i) {
                // **Into this box's own frame, point by point.** `project` maps
                // longitude linearly and states no frame, so a point is drawn
                // where its *stored* number falls — and a grid that closes the
                // globe stores the two columns either side of its seam a whole
                // turn apart (GMGSI: `+179.99961` and `-179.92838`, one
                // 0.0720089-degree cell apart on the ground). `nearest_lon`
                // carries each to the representation nearest this box, which is
                // what puts the seam's two columns beside each other and lets a
                // view centred on the anti-meridian hold columns from both ends
                // of the axis at once.
                //
                // Not gated on `coords.wraps_longitude()`: `GridCoords::Explicit`
                // answers that `false` by construction rather than by
                // measurement, so a gate there would make the same geometry
                // project two different ways depending on which arm holds it —
                // which is exactly what `wrapping_window_tests` compares.
                Some((lat, lon)) => row.push(mb.project(lat, mb.nearest_lon(lon), w, h)),
                None => row.push((f32::NAN, f32::NAN)),
            }
        }
    };

    // **Three rows, never the window.** Sizing a cell reads `(i±1, j)` and
    // `(i, j±1)` and nothing else, so row `j` needs `j-1` and `j+1` beside it
    // and no more. Holding the whole window instead cost a `(f32, f32)` per
    // grid point, and the window is what *zoom* moves: a tight view over one
    // state projects a few thousand points, a view zoomed out until a CONUS
    // mosaic fits projects all 24 500 000 of them — 196 MB in one infallible
    // `Vec::with_capacity`, which on wasm32 is `handle_alloc_error` against the
    // 1 GiB module ceiling and a trap that nothing unwinds. The band is 168 KB
    // at that same width. Gated by `tests/gridded_projection_band.rs`.
    //
    // `band[j % 3]` holds grid row `j`: the loop advances one row at a time and
    // the three live rows are consecutive, so their residues never collide.
    let mut band: [Vec<(f32, f32)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    // Cost, in the two terms `gridded_ledger` reads it in. Kept in registers
    // across the cell loop and posted once, below.
    let mut drawn_cells: u64 = 0;
    let mut written_px: u64 = 0;
    let mut projected_to: Option<usize> = None;

    // **Two rows of rects, never the window** — the same bound the projection
    // band is held to, and for the same reason. `prev` is the row about to be
    // filled and `cur` the row after it, which is what `prev`'s cells give
    // their bottom rows up to. One `Option<CellRect>` per column of the window
    // is 24 bytes: 336 KB at MRMS's 7000, against the 588 MB a rect per grid
    // point would be at the zoomed-out window that holds all 24 500 000 of
    // them. Columns outside the drawn range are `None` for the life of the
    // raster and are never assigned, so no row pays to clear them.
    let mut prev: Vec<Option<CellRect>> = vec![None; win_w];
    let mut cur: Vec<Option<CellRect>> = vec![None; win_w];
    let mut have_prev = false;

    let half_turn_px = half_turn_px(bounds, width);

    let draw = win.interior(ni, nj);
    for j in draw.j0..draw.j1 {
        // Every read below is inside `win`; `interior` is what guarantees it,
        // and it is what bounds this range to rows the window really carries.
        let lo = j.saturating_sub(1).max(win.j0);
        let hi = (j + 1).min(win.j1 - 1);
        let first = match projected_to {
            Some(done) if done + 1 > lo => done + 1,
            _ => lo,
        };
        for row in first..=hi {
            project_row(row, &mut band[row % 3]);
        }
        projected_to = Some(hi);

        let here = &band[j % 3];
        // `j - 1` and `j + 1` modulo three, read behind the same `j > 0` and
        // `j + 1 < nj` guards the whole-window buffer was read behind.
        let above = &band[(j + 2) % 3];
        let below = &band[(j + 1) % 3];
        let at = |i: usize| here[i - win.i0];

        for i in draw.i0..draw.i1 {
            // Assigned before any guard below can skip the cell: the buffer is
            // reused row after row, and a `Some` surviving from the row before
            // would be filled a second time at the wrong `y`.
            cur[i - win.i0] = None;
            let Some(value) = input.value_at(i, j) else {
                continue;
            };
            let color = paint.color_for_value(value);
            if color[3] == 0 {
                continue;
            }
            // The four bytes verbatim: `from_rgba_premultiplied` is
            // `ecolor`'s raw constructor and does no arithmetic, so this
            // packs the straight channels the paint answered into the
            // picture's element type and changes no byte. The premultiply is
            // still the funnel's, and still runs over these same bytes.
            let color = Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);

            let (cx, cy) = at(i);
            if cx.is_nan() || cy.is_nan() {
                continue;
            }

            // Half-extents from neighbour spacing. 0.55, not 0.50: a slight
            // overlap hides seams between adjacent cells.
            //
            // **A neighbour a whole turn away is not a neighbour.** A grid that
            // closes the globe stores one of the two columns either side of its
            // seam wrapped: GMGSI's column 0 is `+179.99961` and its column 1
            // `-179.92838` — one ordinary 0.0720089-degree cell apart on the
            // ground, 359.928 degrees apart as numbers. `MercatorBounds::project`
            // maps longitude linearly and applies no shift (each caller states
            // its own frame), so those two land most of a world apart in the
            // texture and the spacing below reads that distance as the size of
            // one cell. Measured at a whole-world viewport (2878 x 1651 pane,
            // zoom 3.3268, a 605.05-degree box on a 4317 px picture): columns 0
            // and 1 filled rects **1668 px and 1412 px** wide and held
            // **4 582 934 pixels, 42.88 % of the picture**, where the cell they
            // describe is 0.51 px. `refused` is what declines to size a cell
            // from a spacing no cell can have, and the other side answers
            // instead — the fallback this arm already had for the grid's own
            // edge. Gated by `gmgsi_seam_probe_tests`. The half-turn it is
            // measured against is `half_turn_px`, which is only right in the
            // frame the box arrives in — see there, and `seam_frame_tests`.
            let spacing = |a: f32, b: f32| {
                let d = (b - a).abs();
                (d <= half_turn_px).then(|| (d * 0.55).max(0.5))
            };
            let left = (i > 0).then(|| spacing(at(i - 1).0, cx)).flatten();
            let right = (i + 1 < ni).then(|| spacing(cx, at(i + 1).0)).flatten();
            // Unchanged where nothing is refused: `left` for the left half and
            // `right` for the right, each falling back to the other side and
            // then to one pixel.
            let dx_left = left.or(right).unwrap_or(1.0);
            let dx_right = right.or(left).unwrap_or(1.0);
            let dy_up = if j > 0 {
                let (_, ny) = above[i - win.i0];
                ((cy - ny).abs() * 0.55).max(0.5)
            } else if j + 1 < nj {
                let (_, ny) = below[i - win.i0];
                ((ny - cy).abs() * 0.55).max(0.5)
            } else {
                1.0
            };
            let dy_down = if j + 1 < nj {
                let (_, ny) = below[i - win.i0];
                ((ny - cy).abs() * 0.55).max(0.5)
            } else {
                dy_up
            };

            let x0 = ((cx - dx_left) as i32).max(0);
            let y0 = ((cy - dy_up) as i32).max(0);
            let x1 = ((cx + dx_right) as i32).min(width as i32 - 1);
            let y1 = ((cy + dy_down) as i32).min(height as i32 - 1);

            drawn_cells += 1;
            cur[i - win.i0] = Some(CellRect {
                x0,
                y0,
                x1,
                y1,
                color,
            });
        }

        // Row `j - 1` is filled here rather than where it was sized, because
        // what it may skip is decided by row `j`. The order the picture is
        // written in is unchanged: rows still ascend and, inside a row,
        // columns still ascend.
        if have_prev {
            emit_cell_row(&mut px, width, &mut prev, Some(&cur), &mut written_px);
        }
        std::mem::swap(&mut prev, &mut cur);
        have_prev = true;
    }

    // The last row has no row after it, so it gives up nothing downward.
    if have_prev {
        emit_cell_row(&mut px, width, &mut prev, None, &mut written_px);
    }

    gridded_ledger::record(
        drawn_cells,
        written_px,
        u64::from(width) * u64::from(height),
    );
    // The same picture, keyed by whose grid it was. A resident plane is
    // written once at decode and read by this loop or by nothing, so a field
    // absent from `by_field` after a leg is a decode nothing consumed.
    gridded_ledger::record_field(input.field(), drawn_cells);

    RasterizeOutput {
        crop: None,
        rgba: px.into(),
        hit_cells: None,
        alpha: AlphaMode::Straight,
        blank: None,
        // **Armed whether or not it is spent.** Whether this raster is blank is
        // settled once, later, after the premultiply
        // ([`RasterizeOutput::settle_blank`]); what only this call can say is
        // *why* there would be nothing in it, and it says it now while the
        // count is in hand. `settle_blank` spends this on a raster that turns
        // out blank and ignores it on one that painted.
        //
        // Three outcomes, and `drawn_cells` is what keeps the first two apart:
        // it counts the cells that resolved a value AND a finite place to put
        // it, which is exactly "the window held something paintable".
        //
        // `interior` gives back the ring the loop may draw from, since sizing a
        // cell reads its four neighbours; a window pinned against the grid's
        // own edge — `j 0..1` on a box entirely north of the top row — has **no
        // interior at all** and the loop above never ran. That is the view
        // being off this grid's ground, and reading it as "no data" would file
        // GMGSI's correct polar blanks as a defect.
        //
        // A window away from the edges is at least two cells each way, because
        // `window_for` widens by one index either side; so an empty interior
        // means an edge clamp, which means the box reached past the grid. With
        // an interior to walk, `drawn_cells == 0` is a real data gap over
        // ground the grid does cover, and any other count means the values were
        // there and none of them landed on this texture.
        blank_reason: Some(if draw.area() > 0 && drawn_cells == 0 {
            BlankReason::NoDataInWindow
        } else {
            BlankReason::OutsideCoverage
        }),
    }
}

pub mod gridded_ledger;

#[cfg(test)]
mod alpha_tests;

#[cfg(test)]
mod dateline_tests;

#[cfg(test)]
mod glm_energy_tests;

#[cfg(test)]
mod glm_time_tests;

#[cfg(test)]
mod has_ink_tests;

#[cfg(test)]
mod hole_tests;

#[cfg(test)]
mod item_blank_reason_tests;

/// **A picture cut to a bounding box of its content is the same picture.**
/// The identity gate behind [`PictureCrop`], its anti-vacuity floor, and the
/// tamper that shows the comparison is live.
#[cfg(test)]
mod crop_identity_tests;

/// **The window's arithmetic on wire-decoded values.** [`PictureCrop::fits`]
/// is the only guard on a window that arrives from an offloaded rasterizer,
/// and every far edge it computes has to exist before it can be compared.
#[cfg(test)]
mod crop_bounds_tests;

#[cfg(test)]
pub(crate) mod lambert_fixture;

#[cfg(test)]
mod model_nan_tests;

#[cfg(test)]
mod model_window_tests;

#[cfg(test)]
mod projection_window_tests;

#[cfg(test)]
mod wrapping_window_tests;

#[cfg(test)]
mod device_scale_tests;

#[cfg(test)]
mod sites_marker_tests;

#[cfg(test)]
mod hit_cells_tests;

#[cfg(test)]
mod hit_map_bytes_tests;

#[cfg(test)]
mod gmgsi_seam_probe_tests;

#[cfg(test)]
mod gridded_overdraw_tests;

#[cfg(test)]
mod seam_frame_tests;

#[cfg(test)]
mod regional_window_frame_tests;

#[cfg(test)]
mod blank_reason_tests;

#[cfg(test)]
mod gmgsi_blank_probe_tests;
