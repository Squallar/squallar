use egui::{Color32, Pos2, Rect, Vec2, pos2, vec2};

#[derive(Debug, Clone)]
pub struct Text {
    /// **Refcounted, not owned bytes.** A label is placed once per tile per
    /// frame ([`crate::mvt::ShapeOrText::placed`]) and probed against the
    /// galley memo once more ([`Self::galley_cached`]), and both spellings
    /// clone this field. Owned, that was two `malloc`s and two copies of the
    /// name per label per frame on a map that had not moved -- measured as
    /// 26% of the pane walk on a 1920x1080 basemap frame, of which ~40% was
    /// `memcpy` and `malloc`/`free` under `place_one`. `Arc<str>` makes both
    /// clones a refcount bump; the bytes are allocated once, where the tile
    /// is styled.
    pub text: std::sync::Arc<str>,
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
        text: impl Into<std::sync::Arc<str>>,
        font_size: f32,
        text_color: Color32,
        angle: f32,
    ) -> Self {
        let text = text.into();
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
    ///
    /// **`pixels_per_point` is the caller's to read, once, and that is not a
    /// convenience.** `Context::pixels_per_point` is `Context::input`, which is
    /// `Context::write`: an exclusive lock on the whole context plus a probe of
    /// its viewport table. Read here it was one of those per label, for a value
    /// that is fixed for the pass and identical for every label in it — a
    /// basemap pane's label solve took it 459 times a solve. The table still
    /// settles on it exactly as before; only who reads it moved.
    pub fn galley_cached(
        &self,
        ctx: &egui::Context,
        cache: &mut GalleyCache,
        pixels_per_point: f32,
    ) -> std::sync::Arc<egui::Galley> {
        cache.settle(pixels_per_point);

        let style = LabelStyle {
            font_size: self.font_size.to_bits(),
            text_color: self.text_color,
            max_width_ems: self.max_width_ems.map(f32::to_bits),
            line_height_ems: self.line_height_ems.map(f32::to_bits),
        };

        // Read out before anything is counted, so the two miss reasons can be
        // told apart: the borrow of `cache.entries` ends with this statement
        // and what survives it is an owned answer.
        //
        // `&self.text` and not `&*self.text`. Both borrow, and both hash
        // identically -- an `Arc<str>` hashes as the `str` it points at -- but
        // the `Arc` spelling also *compares* as an `Arc`, and std answers a
        // pointer match without reading the bytes. A pane probing the name its
        // tile is still holding therefore settles on a pointer compare; the
        // `&str` spelling sent 92,580 more calls into `memcmp` over the
        // measured fixture for the identical answer.
        let found = cache
            .entries
            .get(&style)
            .map(|galleys| galleys.get(&self.text).cloned());

        match found {
            Some(Some(hit)) => {
                cache.hits += 1;
                return hit;
            }
            Some(None) => cache.text_misses += 1,
            None => cache.style_misses += 1,
        }

        let galley = self.galley(ctx);
        cache.layouts += 1;
        // The `Arc` is cloned here and only here: once per name the memo has
        // not seen, never once per probe.
        if cache
            .entries
            .entry(style)
            .or_default()
            .insert(self.text.clone(), galley.clone())
            .is_none()
        {
            cache.entries_len += 1;
        }
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
        let half = size * 0.5;

        // **Nearly every label is upright**, and a rotation by zero is four
        // multiplies by one, four by zero and a `sincosf` to learn that -- per
        // label, per solve, per pane. Spelled out, the corners of an upright
        // box are its own extents.
        //
        // The two arms agree BIT FOR BIT, which is what
        // `an_upright_box_is_built_exactly_as_a_rotation_by_zero_builds_it`
        // holds: with `sin_cos(0.0)` the rotated spelling adds a signed zero
        // to each coordinate, and `v + 0.0` is `v` for every `v` except `-0.0`
        // -- so the guard is a half-extent strictly above zero, which is what
        // makes a coordinate of `center - half` a signed zero impossible. The
        // finiteness conjunct is there for the same reason: `half * 0.0` is
        // `NaN` when the half-extent is infinite, and the rotated spelling
        // produces one where this arm would not.
        if angle == 0.0
            && half.x > 0.0
            && half.y > 0.0
            && (center.x + center.y + half.x + half.y).is_finite()
        {
            let min = center - half;
            let max = center + half;
            return Self {
                corners: [min, pos2(max.x, min.y), max, pos2(min.x, max.y)],
                // The same box `Rect::from_points` produces: a half-extent
                // above zero orders the corners, and no coordinate here can be
                // the `NaN` that would make a component-wise `min` pick the
                // other operand.
                bbox: Rect::from_min_max(min, max),
            };
        }

        let (s, c) = angle.sin_cos();

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

    /// The rotated spelling, always. The only caller is the gate that holds
    /// [`Self::new`]'s upright arm to it.
    #[cfg(test)]
    fn rotated(center: Pos2, angle: f32, size: Vec2) -> Self {
        let (s, c) = angle.sin_cos();
        let half = size * 0.5;

        let ux = vec2(half.x * c, half.x * s);
        let uy = vec2(-half.y * s, half.y * c);

        let corners = [
            center - ux - uy,
            center + ux - uy,
            center + ux + uy,
            center - ux + uy,
        ];

        Self {
            corners,
            bbox: Rect::from_points(&corners),
        }
    }

    pub fn top_left(&self) -> Pos2 {
        self.corners[0]
    }

    /// Whether these two claims overlap.
    ///
    /// **Every call is counted**, in [`INTERSECT_TESTS`]. The count is the
    /// figure any collision-broad-phase change is judged by: the test itself
    /// is a bounding-box compare plus a separating-axis test over eight
    /// corners, and how often it runs is a property of the *search*, not of
    /// the geometry. See [`OccupiedAreas`].
    pub fn intersects(&self, other: &OrientedRect) -> bool {
        INTERSECT_TESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Checking bbox first gives huge performance boost.
        self.bbox.intersects(other.bbox) && !separated(&self.corners, &other.corners)
    }
}

/// Every [`OrientedRect::intersects`] this process has run.
///
/// Product telemetry rather than a campaign instrument: always on, no feature
/// gate, one `Relaxed` `fetch_add` per test. It exists because the label
/// collision phase is a *search* whose cost is the number of tests it makes,
/// and a search's cost is invisible to every timing an ordinary run takes --
/// the per-test work never changed, only how many times it was asked for.
static INTERSECT_TESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`OrientedRect::intersects`] calls since the process started.
pub fn intersect_tests() -> u64 {
    INTERSECT_TESTS.load(std::sync::atomic::Ordering::Relaxed)
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

/// The side of one broad-phase bucket, in points.
///
/// **Published because it is what makes the search's cost derivable.** A
/// caller that knows this can compute, from its own claims alone, exactly
/// which of them can be candidates for which query -- two claims are
/// candidates for each other precisely when their bounding boxes touch a
/// common bucket -- and so state a bound on the number of
/// [`OrientedRect::intersects`] calls a run may make without restating the
/// search. `squallar-egui/tests/label_collision_is_bucketed.rs` is that
/// caller.
///
/// A label is the thing this holds: a wrapped place name at a basemap text
/// size measures on the order of 100 x 30 points, and a station name about
/// 40 x 14. 64 puts the ordinary label in one or two buckets on each axis --
/// small enough that a query looks at its own neighbourhood rather than the
/// pane, large enough that inserting one claim is a handful of bucket writes
/// rather than a walk over a fine grid.
pub const BUCKET_POINTS: f32 = 64.0;

/// The most buckets one claim may be filed in before it is filed in none.
///
/// A claim wider than this is not a label; it is a rectangle produced by a
/// degenerate projector or an enormous galley, and spreading it over thousands
/// of buckets would cost more than testing it against every query. Such a
/// claim goes on [`OccupiedAreas::unbucketed`] instead, which every query
/// tests in full -- so the answer is the same and only the search changes.
const MAX_BUCKETS_PER_AREA: i64 = 256;

/// The buckets a bounding box touches, as a column range and a row range,
/// both **exclusive** at the high end -- or `None` when it touches too many to
/// be worth filing (see [`MAX_BUCKETS_PER_AREA`]) or is not finite.
///
/// **One comparison chain decides all of that.** A `NaN` coordinate fails
/// every compare, an infinite one fails a bound, an inverted box fails the
/// ordering, and an oversized one fails a span cap -- so the four separate
/// finite-and-in-range gates this replaces are one `&&` chain over values
/// already in registers. It is worth spelling that way because it is reached
/// once per claim on the frame thread and its result is *handed* to
/// [`OccupiedAreas::file`] rather than recomputed there, which is the other
/// half of the same cut: filing used to derive the span a second time from the
/// same box.
///
/// Exclusive at the high end so the walk over it is a `Range` and not a
/// `RangeInclusive`, which carries an exhausted flag and tests it on every
/// step. The `+ 1.0` is done in the float, where a bucket index at the top of
/// `i32` cannot overflow into the bottom of it.
#[inline]
fn bucket_span(bbox: Rect) -> Option<(i32, i32, i32, i32)> {
    const INV: f32 = 1.0 / BUCKET_POINTS;
    const CELL_MIN: f32 = i32::MIN as f32;
    const CELL_MAX: f32 = i32::MAX as f32;
    let x0 = (bbox.min.x * INV).floor();
    let y0 = (bbox.min.y * INV).floor();
    let x1 = (bbox.max.x * INV).floor();
    let y1 = (bbox.max.y * INV).floor();
    let across = x1 - x0;
    let down = y1 - y0;
    let ok = x0 >= CELL_MIN
        && y0 >= CELL_MIN
        && x1 <= CELL_MAX
        && y1 <= CELL_MAX
        && across >= 0.0
        && down >= 0.0
        && (across + 1.0) * (down + 1.0) <= MAX_BUCKETS_PER_AREA as f32;
    ok.then(|| (x0 as i32, y0 as i32, (x1 + 1.0) as i32, (y1 + 1.0) as i32))
}

/// The side of the cell grid, in buckets: the search's whole index is
/// `GRID * GRID` chain heads in one flat array.
///
/// **A cell is a bucket folded onto the grid, not a bucket.** Two buckets
/// [`GRID_POINTS`] apart on an axis share a cell, and a query that lands on
/// one of them looks at the other's claims too. That is safe for the same
/// reason the buckets themselves are: a candidate is only ever a *candidate*,
/// and every one of them is put through [`OrientedRect::intersects`] before it
/// refuses anything. Folding costs the extra tests and nothing else -- and it
/// costs nothing at all on a pane narrower than [`GRID_POINTS`], which every
/// pane this draws on is.
///
/// **Why a flat array and not the hash table it replaces.** A query touches
/// two or three cells and a placement writes two or three more, so a pane's
/// solve was making about five table operations per label over a table of
/// screen coordinates -- and the table is what the search *is*, so its per-key
/// cost is the search's cost. Folded onto a fixed grid the key arithmetic is
/// two masks and a multiply-add, the lookup is one load, and the insertion is
/// one store. Measured on a 1920x1080 fixture of 600 place names (535 reaching
/// a claim, 396 placed, warm galley memo, callgrind, marginal instructions
/// between a 100-pass and a 300-pass run): the claim phase fell **36.0 %**,
/// and the whole label solve **25.3 %**, with the count of
/// [`OrientedRect::intersects`] calls identical to the test.
const GRID: i32 = 64;

/// The distance on one axis after which two claims share a cell. Published
/// for the reason [`BUCKET_POINTS`] is: it is what makes the folding a
/// property a caller can state and a gate can hold.
pub const GRID_POINTS: f32 = BUCKET_POINTS * GRID as f32;

const GRID_MASK: i32 = GRID - 1;
const CELLS: usize = (GRID as usize) * (GRID as usize);

/// Where a bucket's chain head lives in [`OccupiedAreas::grid`].
#[inline]
fn cell_of(cx: i32, cy: i32) -> usize {
    ((cy & GRID_MASK) as usize) * (GRID as usize) + ((cx & GRID_MASK) as usize)
}

/// Tracks areas occupied by texts to avoid overlapping them.
///
/// **The rule is unchanged and the search is not.** First claim to ask for a
/// piece of screen keeps it; a later claim that touches it is refused. What
/// changed is how the claims already made are found: they are filed by the
/// buckets their bounding boxes touch ([`BUCKET_POINTS`]), and a query tests
/// only the claims sharing a bucket with it.
///
/// **That is exact, not approximate.** Two [`OrientedRect`]s can only
/// intersect if their bounding boxes overlap, and two overlapping boxes always
/// share at least one bucket -- each is filed in *every* bucket it touches --
/// so no claim that would have been hit can be missed. Every candidate the
/// buckets produce is then put through the same [`OrientedRect::intersects`]
/// as before, so nothing is accepted that the full scan would have refused.
/// The accept/reject sequence is identical for any input.
///
/// **Why it was worth doing.** The scan it replaces was linear in the claims
/// already made, so a pane laying out `n` labels ran `n(n-1)/2` intersection
/// tests. Measured on the native rig's scene D (one 1920x1080 pane, KTLX,
/// every layer on, the `ui-sweep` script, RTX 3090 / Vulkan on Xvfb), the
/// ground phase placed 282-343 label anchors per frame across two legs, and
/// `try_occupy` was the largest single symbol of this crate's or the app's own
/// code on the frame thread -- 0.34% of all on-CPU samples on the leg that
/// counted 343, ~78% of them inside the scan loop itself.
pub struct OccupiedAreas {
    areas: Vec<OrientedRect>,
    /// The head of each cell's chain -- an index into [`Self::filed`], or
    /// [`END`] -- for all [`CELLS`] cells at once. See [`GRID`] for why the
    /// index is a flat array and not the table of screen coordinates it
    /// replaces.
    ///
    /// Empty until the first claim is filed, so an [`OccupiedAreas`] that
    /// never places a label pays nothing for it: walkers' own per-tile draw
    /// builds one of these per tile.
    grid: Vec<u32>,
    /// The cells of [`Self::grid`] whose head is not [`END`], so
    /// [`Self::clear`] can reset the cells a solve used instead of the whole
    /// grid -- a couple of hundred stores rather than [`CELLS`] of them.
    touched: Vec<u32>,
    /// The chains themselves, `(claim, next link)`.
    ///
    /// **One arena rather than a `Vec` per cell**, because a fresh
    /// `OccupiedAreas` is built for every pane on every frame: a map of
    /// per-cell vectors would have allocated once per occupied cell per
    /// frame — around two hundred on a 1920x1080 pane of labels — to save a
    /// scan that costs less than that.
    filed: Vec<(u32, u32)>,
    /// Claims with no usable bucket span, tested by every query.
    unbucketed: Vec<u32>,
    /// Per-claim stamp of the query that last considered it, so a claim filed
    /// in several of the query's buckets is tested once rather than once per
    /// shared bucket.
    seen: Vec<u64>,
    /// The query counter [`Self::seen`] is stamped with. Never reset, so a
    /// stamp can never collide with a live one.
    query: u64,
    /// Candidate claims for the query in flight. A field rather than a local
    /// so the allocation is made once per pane rather than once per label.
    candidates: Vec<u32>,
}

/// The end of a cell's chain.
const END: u32 = u32::MAX;

impl Default for OccupiedAreas {
    fn default() -> Self {
        Self::new()
    }
}

impl OccupiedAreas {
    pub fn new() -> Self {
        Self {
            areas: Vec::new(),
            grid: Vec::new(),
            touched: Vec::new(),
            filed: Vec::new(),
            unbucketed: Vec::new(),
            seen: Vec::new(),
            query: 0,
            candidates: Vec::new(),
        }
    }

    /// Drop every claim, keeping every buffer this has already grown.
    ///
    /// **A pane builds one of these per solve, and its size is a property of
    /// the pane rather than of the frame.** Built with [`Self::new`] each
    /// time, a solve grows `areas`, `filed`, `seen` and the bucket table from
    /// empty and hands the lot back to the allocator one frame later — for a
    /// claim count that is the same few hundred every frame a pan re-solves
    /// on. This empties them instead, so the second solve writes into memory
    /// the first already has.
    ///
    /// The result is the same as a fresh one, claim for claim: every field
    /// that decides an answer is emptied, and the only thing kept besides
    /// capacity is [`Self::query`], which is deliberately NOT reset — it
    /// stamps [`Self::seen`], and a counter that only goes up cannot collide
    /// with a stamp left behind, whether or not the stamps were cleared.
    pub fn clear(&mut self) {
        self.areas.clear();
        for &cell in &self.touched {
            self.grid[cell as usize] = END;
        }
        self.touched.clear();
        self.filed.clear();
        self.unbucketed.clear();
        self.seen.clear();
        self.candidates.clear();
    }

    /// Whether `rect`'s screen is free, and claim it when it is.
    ///
    /// **The whole cost of this is finding the claims already made**, not
    /// deciding against them: on the 1920x1080 fixture above, a claim is put
    /// through [`OrientedRect::intersects`] 1.42 times on average, and the
    /// rest of the call is the index. So the index is where the work is spent
    /// and where it has been taken from -- a flat cell grid, one span
    /// computation handed on to [`Self::file`] rather than made twice, and the
    /// buffers borrowed field by field rather than moved out and back.
    pub fn try_occupy(&mut self, rect: OrientedRect) -> bool {
        // Field by field rather than `&mut self`, so the gather can read the
        // grid and the arena while it writes the stamps without taking the
        // buffers out and putting them back.
        let Self {
            areas,
            grid,
            filed,
            unbucketed,
            seen,
            query,
            candidates,
            ..
        } = self;
        candidates.clear();
        *query += 1;
        let query = *query;

        let span = bucket_span(rect.bbox);
        match span {
            Some((x0, y0, x_end, y_end)) => {
                if !grid.is_empty() {
                    for cy in y0..y_end {
                        for cx in x0..x_end {
                            let mut link = grid[cell_of(cx, cy)];
                            while link != END {
                                let (at, next) = filed[link as usize];
                                if seen[at as usize] != query {
                                    seen[at as usize] = query;
                                    candidates.push(at);
                                }
                                link = next;
                            }
                        }
                    }
                }
                for &at in unbucketed.iter() {
                    if seen[at as usize] != query {
                        seen[at as usize] = query;
                        candidates.push(at);
                    }
                }
            }
            None => candidates.extend(0..areas.len() as u32),
        }

        let free = !candidates
            .iter()
            .any(|&at| areas[at as usize].intersects(&rect));

        if free {
            self.file(rect, span);
        }
        free
    }

    /// File an accepted claim in every cell its bounding box touches.
    ///
    /// The span is the caller's, already computed to answer the query. It is a
    /// parameter and not a second [`bucket_span`] call because deriving it
    /// again from the same box is the same arithmetic on the same bytes, and
    /// filing is on the accepting path -- which most claims take.
    fn file(&mut self, rect: OrientedRect, span: Option<(i32, i32, i32, i32)>) {
        let at = self.areas.len() as u32;
        match span {
            Some((x0, y0, x_end, y_end)) => {
                if self.grid.is_empty() {
                    self.grid.resize(CELLS, END);
                }
                for cy in y0..y_end {
                    for cx in x0..x_end {
                        let cell = cell_of(cx, cy);
                        let link = self.filed.len() as u32;
                        let head = std::mem::replace(&mut self.grid[cell], link);
                        if head == END {
                            self.touched.push(cell as u32);
                        }
                        self.filed.push((at, head));
                    }
                }
            }
            None => self.unbucketed.push(at),
        }
        self.areas.push(rect);
        self.seen.push(0);
    }
}

/// The hash both galley memos and the solve's repeat-name index are probed
/// through: a multiply-xorshift fold, not SipHash.
///
/// **Not a taste preference, and measured on the shipped path.** A pane's
/// label solve probes three tables per name — the galley memo, the
/// repeat-name index, and the index again when the name draws — and with
/// `RandomState` those three probes were **45.9 % of the whole solve's
/// instructions** (203 solves x 600 names, release+LTO, callgrind, memo warm
/// so every probe was a hit). The galley probe alone cost **651 instructions**
/// of hashing per name, because SipHash charges a full round per `write` call
/// and a struct key spends one per field.
///
/// The same argument [`BucketHasher`] makes, on keys that are place names
/// rather than screen coordinates: what a table like this can lose to a poor
/// hash is comparisons inside a table [`GalleyCache::MAX_ENTRIES`] already
/// caps, and both tables are private memos of what the map is drawing, not a
/// public index. A tile server that chose colliding names would slow its own
/// labels down and reach nothing else.
///
/// Bytes are folded eight at a time and the length folded in after them, so
/// two writes cannot be confused for one longer write. [`Hasher::finish`]
/// applies a murmur3 finalizer because `hashbrown` reads the top seven bits
/// for its control byte and the low bits for the bucket index, and a bare
/// multiply leaves the top bits of a short key doing most of the work.
#[derive(Default)]
pub struct NameHasher(u64);

impl NameHasher {
    /// The 64-bit constant `rustc-hash` folds with.
    const SEED: u64 = 0x517c_c1b7_2722_0a95;

    #[inline]
    fn fold(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(Self::SEED);
    }
}

impl std::hash::Hasher for NameHasher {
    #[inline]
    fn finish(&self) -> u64 {
        // murmur3's finalizer: both halves of the word carry the whole key.
        let mut hash = self.0;
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
        hash ^= hash >> 33;
        hash
    }

    /// **Every read here has a length the compiler knows.** The obvious
    /// spelling of the tail — `word[..tail.len()].copy_from_slice(tail)` —
    /// has a runtime length, and LLVM emits a call to `memcpy` for it: 4.29 M
    /// instructions of `__memcpy_avx_unaligned_erms` over the measured fixture,
    /// 2.7 % of the whole solve, to move at most seven bytes at a time.
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some((word, tail)) = rest.split_first_chunk::<8>() {
            self.fold(u64::from_ne_bytes(*word));
            rest = tail;
        }
        if !rest.is_empty() {
            let mut word = 0u64;
            if let Some((chunk, tail)) = rest.split_first_chunk::<4>() {
                word = u64::from(u32::from_ne_bytes(*chunk));
                rest = tail;
            }
            if let Some((chunk, tail)) = rest.split_first_chunk::<2>() {
                word = (word << 16) | u64::from(u16::from_ne_bytes(*chunk));
                rest = tail;
            }
            if let Some((byte, _)) = rest.split_first() {
                word = (word << 8) | u64::from(*byte);
            }
            self.fold(word);
        }
        // Length after the bytes: `write("ab") + write("c")` must not be
        // `write("abc")`, or two fields of a struct key could trade bytes.
        self.fold(bytes.len() as u64);
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.fold(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.fold(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.fold(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.fold(value);
    }

    #[inline]
    fn write_u128(&mut self, value: u128) {
        self.fold(value as u64);
        self.fold((value >> 64) as u64);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.fold(value as u64);
    }

    #[inline]
    fn write_i8(&mut self, value: i8) {
        self.fold(value as u64);
    }

    #[inline]
    fn write_i16(&mut self, value: i16) {
        self.fold(value as u64);
    }

    #[inline]
    fn write_i32(&mut self, value: i32) {
        self.fold(value as u64);
    }

    #[inline]
    fn write_i64(&mut self, value: i64) {
        self.fold(value as u64);
    }

    #[inline]
    fn write_isize(&mut self, value: isize) {
        self.fold(value as u64);
    }
}

/// [`NameHasher`] as a `HashMap` parameter.
pub type NameHash = std::hash::BuildHasherDefault<NameHasher>;

/// The inner table of one style: laid-out galleys by the text they were laid
/// out from. `K` is what that style's caller can hand the table without
/// allocating -- an `Arc<str>` a tile is holding, or a `Box<str>` copied once
/// from a `&str` the caller owns.
type Galleys<K> = std::collections::HashMap<K, std::sync::Arc<egui::Galley>, NameHash>;

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
    /// egui's pass number at that check, so the next one can tell whether it
    /// is the *consecutive* reading [`AtlasStamp::invalidates`] requires.
    /// `None` until the first check.
    observed_pass: Option<u64>,
    /// How many times the atlas has been seen to move under this table.
    /// See [`Self::generation`].
    generation: u64,
    /// The place-label table, keyed in two levels for the reason `points`
    /// below is: **so a probe can borrow the name instead of cloning it.**
    ///
    /// A single-level map keyed by a struct holding the `Arc<str>` cannot be
    /// probed without building that struct, and building it clones the `Arc`:
    /// two atomic read-modify-writes per name per frame, given straight back
    /// one statement later. Split, the name is the inner key and a probe
    /// borrows it.
    ///
    /// **The refcount was the small half and the hash was the large one.**
    /// Those two atomics were 227,360 instructions of a 247,165,098-instruction
    /// solve — 0.09 % — while hashing the key they were part of was 29.9 %.
    /// They are worth removing anyway because their *cost* is not their
    /// instruction count: a clone-and-drop pair measures 4.09 ns on a line no
    /// other core is touching and 22.78 ns while one is, and nothing in a
    /// refcount tells a reader which it will be.
    ///
    /// The style is what a whole pass has about four distinct values of, so
    /// the outer probe is a hash of sixteen bytes that hits the same slot for
    /// hundreds of consecutive names.
    entries: std::collections::HashMap<LabelStyle, Galleys<std::sync::Arc<str>>, NameHash>,
    /// Entries across every inner map of `entries`, kept as a running count so
    /// the ceiling check stays O(1).
    entries_len: usize,
    /// The point-label table, keyed in two levels so a lookup can borrow.
    ///
    /// **Two levels because the hot path holds a `&str`, not a `String`.**
    /// `EguiPointPainter` draws a station model from text it already owns, and
    /// a single-level map keyed by a struct containing `String` cannot be
    /// probed without building that `String` first — which is the very
    /// allocation `Painter::text`'s `to_string()` was making. Splitting the
    /// style out leaves an inner map keyed by `Box<str>`, and `Box<str>:
    /// Borrow<str>`, so a hit costs a hash of the text and no allocation.
    points: std::collections::HashMap<PointStyle, Galleys<Box<str>>, NameHash>,
    /// Entries across every inner map of `points`, kept as a running count so
    /// the ceiling check stays O(1).
    points_len: usize,
    layouts: u64,
    hits: u64,
    /// Probes that found no table for their style at all.
    style_misses: u64,
    /// Probes that found their style's table and not their text in it.
    text_misses: u64,
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
    ///
    /// `pixels_per_point` is the caller's, for [`Text::galley_cached`]'s
    /// reason: a station model draws several strings and every one of them was
    /// taking `Context::write` to read the same number.
    pub fn galley_for_point(
        &mut self,
        ctx: &egui::Context,
        text: &str,
        font: egui::FontId,
        color: Color32,
        pixels_per_point: f32,
    ) -> std::sync::Arc<egui::Galley> {
        self.settle(pixels_per_point);
        let style = PointStyle {
            font_size: font.size.to_bits(),
            family: font.family.clone(),
            color,
        };
        let found = self
            .points
            .get(&style)
            .map(|inner| inner.get(text).cloned());
        match found {
            Some(Some(hit)) => {
                self.hits += 1;
                return hit;
            }
            Some(None) => self.text_misses += 1,
            None => self.style_misses += 1,
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
        self.entries_len + self.points_len
    }

    /// Why the probes that missed missed: how many found no table for their
    /// style, and how many found the style and not the text.
    ///
    /// **The figure a two-level table has to be watched by.** A memo that
    /// never hits still paints correctly, so nothing on the glass says whether
    /// a key spelling reaches the entry it stored a frame ago; a style term
    /// that is minted fresh each pass reads here as a style miss per name and
    /// nowhere else.
    pub fn misses_by_reason(&self) -> (u64, u64) {
        (self.style_misses, self.text_misses)
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

    /// Check the table against egui's font atlas — **once per pass, from one
    /// owner, at the head of the pass before anything has laid text out.**
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
    /// The three words in the first sentence are each load-bearing, and the
    /// argument for all three is [`AtlasStamp::invalidates`]': the check is a
    /// difference of two levels, so it answers only for consecutive readings
    /// taken at the same point in a pass, and only at a point where a repack
    /// has not yet been papered over by the pass's own text. Called behind a
    /// gate — the shape this had until a map's place names started drawing as
    /// the right words in the wrong letters — the readings straddle a repack
    /// and a regrowth and say nothing moved. A caller that cannot promise all
    /// three should read [`Self::generation`] instead of holding geometry
    /// across this. A pass this table can see it *missed* drops the table
    /// outright rather than reasoning from a reading it did not take.
    ///
    /// Once per pass and not per lookup, because `Context::fonts` is a write
    /// lock on the whole context.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        let pass = ctx.cumulative_pass_nr();
        // **A pass this table did not watch is a pass it cannot vouch for.**
        // The stamp is a level, not an event: a repack is visible only as the
        // fill *falling*, and the fill climbs back. Two readings either side
        // of a gap can therefore bracket a repack and a regrowth to the same
        // size at a higher fill, and `invalidates` will say nothing moved --
        // which is exactly what happened while every caller of this called it
        // behind its own gate. Skipping is not a state this table can reason
        // from, so it drops.
        let skipped = self.observed_pass.is_some_and(|last| pass > last + 1);
        let stamp = AtlasStamp::read(ctx);
        if skipped || stamp.invalidates(self.atlas) {
            self.drop_all();
            self.generation = self.generation.wrapping_add(1);
        }
        self.atlas = stamp;
        self.observed_pass = Some(pass);
    }

    /// How many times the glyph raster has moved under this table.
    ///
    /// **The number a caller stamps its own kept geometry with.** Anything
    /// holding baked atlas coordinates -- a galley, a tessellated mesh, a
    /// solved list of label shapes -- is valid only for the generation it was
    /// built in, and comparing this against the generation it was built under
    /// is a `u64` compare rather than a second `Context::fonts` read.
    ///
    /// It is a counter and not a level for the reason [`Self::begin_frame`]
    /// gives: a level can return to a value it held before a repack, and a
    /// cache that missed the frames in between would read that as "unmoved".
    /// A counter only goes up, so a holder that skipped a hundred frames
    /// still compares correctly.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn drop_all(&mut self) {
        self.entries.clear();
        self.entries_len = 0;
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
    ///
    /// **Only answers for the pass immediately after `earlier` was taken, and
    /// only if both were taken before that pass laid any text out.** This is a
    /// difference of two levels, and the level it watches is not monotone
    /// across a repack -- it collapses and climbs back. Between two readings
    /// far enough apart it has climbed back past `earlier` under a size that
    /// is once again a power of two, and this returns `false` over a raster
    /// that moved entirely. [`GalleyCache::begin_frame`] is what holds the
    /// precondition; [`GalleyCache::generation`] is what a cache that cannot
    /// watch every pass should compare instead.
    ///
    /// Held that way it is sound rather than likely: a repack fires only above
    /// a fill of 0.8, a fill above 0.8 is reachable only once the atlas height
    /// has doubled up to its width, and the atlas the repack leaves behind is
    /// the constructor's -- 32 rows tall. So the reading taken at the head of
    /// the pass after a repack differs from its predecessor in `size`, always.
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
/// as [`LabelStyle`]: a spurious miss costs one layout, a spurious hit draws the
/// wrong text.
#[derive(PartialEq, Eq, Hash, Clone)]
struct PointStyle {
    font_size: u32,
    family: egui::FontFamily,
    color: Color32,
}

/// Every field [`Text::galley`] reads **except the text**, in a form that is
/// `Hash` and `Eq`.
///
/// The style half of a place label's key, split out for the reason
/// [`PointStyle`] is: the text is then the inner table's key and can be probed
/// as a borrowed `&str`. See [`GalleyCache::entries`].
///
/// The three `f32`s are keyed by their bits rather than by value because `f32`
/// is not `Eq`. That is stricter than equality — `-0.0` and `0.0` are two keys
/// — and stricter is the safe direction for a memo: a spurious miss costs one
/// layout, a spurious hit draws the wrong text.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
struct LabelStyle {
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
                ctx.pixels_per_point(),
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
            text: text.into(),
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
            let cached = text.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
            // Second time through is the one that comes off the table.
            let hit = text.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());

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
            let _ = label(name).galley_cached(&ctx, &mut kept, ctx.pixels_per_point());
        }
        let after_first = kept.layouts();
        assert_eq!(after_first, names.len() as u64);

        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut kept, ctx.pixels_per_point());
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
            let _ = label(name).galley_cached(&ctx, &mut dropped, ctx.pixels_per_point());
        }
        dropped.clear();
        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut dropped, ctx.pixels_per_point());
        }
        assert_eq!(dropped.layouts(), 2 * names.len() as u64);
    }

    /// **The borrowed probe and the stored key must hash to the same word.**
    ///
    /// A two-level table is probed with one spelling of the name and filled
    /// with another. If those two hash differently the entry is unreachable
    /// for ever: the memo pays the probe, lays the text out again, and stores
    /// a second copy no probe will ever find — and nothing on the glass says
    /// so, because a memo that never hits still paints correctly.
    ///
    /// The second half is the floor under the first: a hasher that answered a
    /// constant would pass the agreement check and be useless.
    #[test]
    fn a_borrowed_probe_and_a_stored_key_hash_alike() {
        use std::hash::{BuildHasher, Hash, Hasher};

        fn hash_of(value: &impl Hash) -> u64 {
            let mut hasher = super::NameHash::default().build_hasher();
            value.hash(&mut hasher);
            hasher.finish()
        }

        for name in [
            "",
            "a",
            "Norman",
            "Washita River",
            "North Canadian River",
            "Ciudad Juárez",
            "a name of exactly thirty-two ch",
            "a name of exactly thirty-three c",
        ] {
            let owned: std::sync::Arc<str> = std::sync::Arc::from(name);
            let boxed: Box<str> = Box::from(name);
            let borrowed: &str = name;
            assert_eq!(
                hash_of(&owned),
                hash_of(&borrowed),
                "`Arc<str>` and `&str` must hash alike or an entry is unreachable: {name:?}"
            );
            assert_eq!(
                hash_of(&boxed),
                hash_of(&borrowed),
                "`Box<str>` and `&str` must hash alike or an entry is unreachable: {name:?}"
            );
            // And they compare equal, which is the other half of a probe.
            assert_eq!(&*owned, borrowed);
            assert_eq!(&*boxed, borrowed);
        }

        // The floor: names that differ must hash apart. One collision here is
        // allowed by any hash; a hasher that answered a constant would give
        // every one of these.
        let names = [
            "Norman",
            "Noble",
            "Moore",
            "Edmond",
            "Yukon",
            "Mustang",
            "Bethany",
            "Choctaw",
            "Harrah",
            "Purcell",
            "Blanchard",
            "Newcastle",
        ];
        let mut seen = std::collections::HashSet::new();
        for name in names {
            assert!(
                seen.insert(hash_of(&name)),
                "{name} collided with an earlier name"
            );
        }
        assert_eq!(seen.len(), names.len());
    }

    /// **A second pass over the same names misses nothing, of either kind.**
    ///
    /// The figure this cut has to be watched by: a key spelling that mints a
    /// fresh style or a fresh text every pass reads here as a miss per name
    /// and nowhere else. Both tables are asked, because they are keyed by two
    /// different pairs of types.
    #[test]
    fn a_warm_pass_over_the_same_names_misses_nothing() {
        let ctx = ctx_with_fonts();
        let names = ["Norman", "Washita River", "Ciudad Juárez", "东京", ""];
        let mut cache = GalleyCache::default();
        let font = egui::FontId::proportional(11.0);

        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
            let _ = cache.galley_for_point(
                &ctx,
                name,
                font.clone(),
                Color32::WHITE,
                ctx.pixels_per_point(),
            );
        }

        let hits = cache.hits();
        let layouts = cache.layouts();
        let (style_misses, text_misses) = cache.misses_by_reason();

        for name in names {
            let _ = label(name).galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
            let _ = cache.galley_for_point(
                &ctx,
                name,
                font.clone(),
                Color32::WHITE,
                ctx.pixels_per_point(),
            );
        }

        assert_eq!(
            cache.hits() - hits,
            2 * names.len() as u64,
            "every probe of the second pass was answered from the memo"
        );
        assert_eq!(
            cache.layouts(),
            layouts,
            "the second pass laid nothing out again"
        );
        assert_eq!(
            cache.misses_by_reason(),
            (style_misses, text_misses),
            "the second pass missed neither on the style nor on the text"
        );
    }

    /// **A miss says which half missed.** The two are different defects: a
    /// style nobody has drawn before is the memo working, and a style the
    /// table already holds whose text is absent is too — but a style miss per
    /// name, pass after pass, is a key being minted fresh, which is the way a
    /// two-level memo dies silently.
    #[test]
    fn a_miss_says_which_half_of_the_key_missed() {
        let ctx = ctx_with_fonts();
        let mut cache = GalleyCache::default();

        let _ = label("Norman").galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
        assert_eq!(
            cache.misses_by_reason(),
            (1, 0),
            "the first name of a fresh table has no style table to be absent from"
        );

        // Same style, a name the table has not seen.
        let _ = label("Moore").galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
        assert_eq!(
            cache.misses_by_reason(),
            (1, 1),
            "a new name under a style already held is a text miss"
        );

        // A style the table has not seen, under a name it has.
        let recoloured = Text {
            text_color: Color32::RED,
            ..label("Norman")
        };
        let _ = recoloured.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
        assert_eq!(
            cache.misses_by_reason(),
            (2, 1),
            "a new style is a style miss even for a name the table holds"
        );
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
                    text: "Canadian River".into(),
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
            let _ = base.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
            assert_eq!(cache.layouts(), 1, "{field}: setup");
            let _ = variant.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
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
        let _ = base.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());

        for (x, y) in [(11.0, 20.0), (400.0, 300.0), (-50.0, 900.0)] {
            let moved = Text {
                position: pos2(x, y),
                angle: FRAC_PI_4,
                ..base.clone()
            };
            let _ = moved.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
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

        let _ = text.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
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

        let _ = text.galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
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
            let first = cache.galley_for_point(
                &ctx,
                body,
                font.clone(),
                Color32::WHITE,
                ctx.pixels_per_point(),
            );
            let second = cache.galley_for_point(
                &ctx,
                body,
                font.clone(),
                Color32::WHITE,
                ctx.pixels_per_point(),
            );

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
                let _ = cache.galley_for_point(
                    &ctx,
                    body,
                    font.clone(),
                    Color32::WHITE,
                    ctx.pixels_per_point(),
                );
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

        let _ = cache.galley_for_point(
            &ctx,
            "24",
            base.clone(),
            Color32::WHITE,
            ctx.pixels_per_point(),
        );
        assert_eq!(cache.layouts(), 1);
        let _ = cache.galley_for_point(
            &ctx,
            "24",
            egui::FontId::proportional(14.0),
            Color32::WHITE,
            ctx.pixels_per_point(),
        );
        assert_eq!(cache.layouts(), 2, "font size was not keyed");
        let _ = cache.galley_for_point(
            &ctx,
            "24",
            base.clone(),
            Color32::RED,
            ctx.pixels_per_point(),
        );
        assert_eq!(cache.layouts(), 3, "colour was not keyed");
        let _ = cache.galley_for_point(
            &ctx,
            "25",
            base.clone(),
            Color32::WHITE,
            ctx.pixels_per_point(),
        );
        assert_eq!(cache.layouts(), 4, "text was not keyed");

        // **Without this the test is vacuous.** The four assertions above all
        // count misses, and a memo that cached nothing at all would satisfy
        // every one of them. Re-asking for the first key proves the table is
        // actually answering, so "these are four keys" and "nothing is stored"
        // stop being indistinguishable.
        let _ = cache.galley_for_point(&ctx, "24", base, Color32::WHITE, ctx.pixels_per_point());
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
        let _ = label("Washita River").galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
        for i in 0..GalleyCache::MAX_ENTRIES {
            let _ = cache.galley_for_point(
                &ctx,
                &format!("r{i}"),
                font.clone(),
                Color32::WHITE,
                ctx.pixels_per_point(),
            );
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
            let _ =
                label(&format!("name {i}")).galley_cached(&ctx, &mut cache, ctx.pixels_per_point());
        }
        assert!(cache.len() <= GalleyCache::MAX_ENTRIES);
    }

    fn rect(cx: f32, cy: f32, angle: f32, w: f32, h: f32) -> OrientedRect {
        OrientedRect::new(pos2(cx, cy), angle, vec2(w, h))
    }

    /// **The upright arm of `OrientedRect::new` is the rotated one, to the
    /// bit.**
    ///
    /// A label's collision box decides which names the map draws and where
    /// their glyphs land, so an arm that is *nearly* the rotated spelling is
    /// not a cut, it is a rendering change nobody would attribute. The
    /// comparison is over the raw bits and not over `==`, because the two
    /// things that can differ here compare equal: `-0.0 == 0.0`, and no `NaN`
    /// equals itself.
    ///
    /// The input set is chosen for the two ways they *can* differ. The
    /// rotated spelling adds a signed zero to every coordinate — `sin(0.0)`
    /// is `0.0` and the `uy` axis negates it — and `v + 0.0` is `v` for every
    /// `v` but `-0.0`; and it multiplies a half-extent by that zero, which is
    /// `NaN` when the half-extent is infinite. So the fixture carries zero and
    /// negative-zero centres, zero and subnormal sizes, and infinities, and
    /// `an_upright_box_with_a_zero_extent_takes_the_rotated_arm` is the arm
    /// that says those cases really are reached.
    #[test]
    fn an_upright_box_is_built_exactly_as_a_rotation_by_zero_builds_it() {
        let coords = [
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            0.5,
            -320.0,
            1920.0,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            1.0e-45,
            3.0e38,
            -3.0e38,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let sizes = [
            0.0f32,
            1.0e-45,
            f32::MIN_POSITIVE,
            1.0,
            96.0,
            3.0e38,
            f32::INFINITY,
        ];
        let bits = |r: &OrientedRect| {
            let mut out = Vec::with_capacity(12);
            for c in &r.corners {
                out.push(c.x.to_bits());
                out.push(c.y.to_bits());
            }
            out.extend([
                r.bbox.min.x.to_bits(),
                r.bbox.min.y.to_bits(),
                r.bbox.max.x.to_bits(),
                r.bbox.max.y.to_bits(),
            ]);
            out
        };

        let mut cases = 0usize;
        for &cx in &coords {
            for &cy in &coords {
                for &w in &sizes {
                    for &h in &sizes {
                        let (at, size) = (pos2(cx, cy), vec2(w, h));
                        let built = OrientedRect::new(at, 0.0, size);
                        let rotated = OrientedRect::rotated(at, 0.0, size);
                        assert_eq!(
                            bits(&built),
                            bits(&rotated),
                            "centre ({cx}, {cy}) size ({w}, {h}) came out differently"
                        );
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(
            cases,
            coords.len() * coords.len() * sizes.len() * sizes.len()
        );
    }

    /// **The guard is load-bearing, and these are the inputs that say so.**
    ///
    /// Without it the equality arm above could pass on a fixture that never
    /// reaches a zero half-extent or an infinite one, and the guard could be
    /// deleted with the arm still green. So this asserts the opposite of an
    /// equality: on these inputs the *unguarded* upright arithmetic —
    /// `center - half` and `center + half`, spelled here — gives an answer the
    /// rotated spelling does not, which is exactly why they are sent to the
    /// rotated one.
    #[test]
    fn the_upright_arm_is_refused_where_it_would_answer_differently() {
        // A zero half-extent is what lets `center - half` be a negative zero,
        // where the rotated spelling's `- (-0.0)` makes a positive one.
        let (at, size) = (pos2(-0.0, 4.0), vec2(0.0, 8.0));
        let unguarded = at - size * 0.5;
        let rotated = OrientedRect::rotated(at, 0.0, size);
        assert_ne!(
            unguarded.x.to_bits(),
            rotated.corners[0].x.to_bits(),
            "fixture: the upright arithmetic already agrees here, so the \
             guard turning this input away proves nothing",
        );
        assert_eq!(
            OrientedRect::new(at, 0.0, size).corners[0].x.to_bits(),
            rotated.corners[0].x.to_bits(),
            "the guard let a zero half-extent through",
        );

        // An infinite half-extent is what makes `half * sin(0.0)` a `NaN`.
        let (at, size) = (pos2(0.0, 0.0), vec2(f32::INFINITY, 1.0));
        let unguarded = at - size * 0.5;
        let rotated = OrientedRect::rotated(at, 0.0, size);
        assert!(
            unguarded.y.is_finite() && rotated.corners[0].y.is_nan(),
            "fixture: expected the upright arithmetic to be finite where the \
             rotated one is NaN; got {unguarded:?} and {:?}",
            rotated.corners[0],
        );
        assert!(
            OrientedRect::new(at, 0.0, size).corners[0].y.is_nan(),
            "the guard let an infinite half-extent through",
        );
    }

    /// **Two claims a grid period apart are two claims.**
    ///
    /// The cell grid folds bucket coordinates onto [`GRID`] columns and rows,
    /// so claims [`GRID_POINTS`] apart on an axis land in the same cell and
    /// become candidates for each other. Candidacy is not refusal: every
    /// candidate goes through [`OrientedRect::intersects`], which knows where
    /// the claims really are. A fold that refused them would silence a label
    /// four thousand points away from the one that beat it.
    ///
    /// Both axes and both directions, because a fold that dropped the mask on
    /// one of them would still pass on the others.
    #[test]
    fn claims_a_grid_period_apart_are_not_mistaken_for_each_other() {
        for (dx, dy) in [
            (GRID_POINTS, 0.0),
            (-GRID_POINTS, 0.0),
            (0.0, GRID_POINTS),
            (0.0, -GRID_POINTS),
            (GRID_POINTS, GRID_POINTS),
            (GRID_POINTS * 3.0, -GRID_POINTS * 2.0),
        ] {
            let mut areas = OccupiedAreas::new();
            let at = pos2(600.0, 400.0);
            assert!(areas.try_occupy(OrientedRect::new(at, 0.0, vec2(96.0, 28.0))));
            let away = pos2(at.x + dx, at.y + dy);
            assert!(
                areas.try_occupy(OrientedRect::new(away, 0.0, vec2(96.0, 28.0))),
                "a claim at {away:?} was refused by one at {at:?}, {dx} by {dy} away",
            );
            // And the fold really did put them together: a claim that landed
            // in a cell of its own would prove nothing about the mask.
            let (first, second) = (
                bucket_span(OrientedRect::new(at, 0.0, vec2(96.0, 28.0)).bbox).unwrap(),
                bucket_span(OrientedRect::new(away, 0.0, vec2(96.0, 28.0)).bbox).unwrap(),
            );
            assert_eq!(
                cell_of(first.0, first.1),
                cell_of(second.0, second.1),
                "fixture: {dx} by {dy} did not fold onto the same cell",
            );
        }
    }

    /// **A claim wide enough to fold onto its own cells is still filed
    /// everywhere it reaches.**
    ///
    /// A box spanning more than [`GRID`] columns is filed in the same cell
    /// several times over -- the fold is what makes that possible, and the
    /// per-query stamp is what keeps it from being tested several times. What
    /// must not happen is the other thing: a cell the box reaches that it was
    /// never filed in, which would hand the same screen out twice.
    ///
    /// One bucket tall on purpose, so the box passes
    /// [`MAX_BUCKETS_PER_AREA`] and really is filed rather than landing on
    /// `unbucketed`, which every query tests in full and which would make the
    /// arm prove nothing about the fold.
    #[test]
    fn a_claim_that_folds_onto_its_own_cells_is_filed_everywhere_it_reaches() {
        let across = GRID_POINTS * 4.0 - BUCKET_POINTS;
        // Offset half a bucket, so the box covers part of every bucket its
        // span names rather than meeting the last one at a single edge.
        let left = BUCKET_POINTS * 0.5;
        let wide = OrientedRect::new(pos2(left + across * 0.5, 300.0), 0.0, vec2(across, 20.0));
        let span = bucket_span(wide.bbox);
        let (x0, y0, x_end, y_end) = span.expect(
            "fixture: the claim was refused a span, so it lands on `unbucketed` \
             and never exercises the fold",
        );
        assert!(
            x_end - x0 > GRID,
            "fixture: {} columns does not fold onto {GRID}",
            x_end - x0,
        );
        assert_eq!(
            y_end - y0,
            1,
            "fixture: the claim is more than one row tall"
        );

        let wide_bbox = wide.bbox;
        let mut areas = OccupiedAreas::new();
        assert!(areas.try_occupy(wide));
        // Every bucket the box covers, asked in the middle of its row. A
        // filing that stopped early leaves one of these free.
        for column in x0..x_end {
            let at = pos2(
                ((column as f32 + 0.5) * BUCKET_POINTS)
                    .clamp(wide_bbox.min.x + 4.0, wide_bbox.max.x - 4.0),
                300.0,
            );
            let probe = OrientedRect::new(at, 0.0, vec2(6.0, 10.0));
            let (probe_x0, _, probe_x_end, _) = bucket_span(probe.bbox).unwrap();
            assert_eq!(
                (probe_x0, probe_x_end),
                (column, column + 1),
                "fixture: the probe for column {column} sits in another bucket",
            );
            assert!(
                !areas.try_occupy(probe),
                "column {column} at {at:?} was handed out inside a claim that covers it",
            );
        }
        // And a row the box does not reach stays free, so the arm is not
        // simply refusing everything.
        assert!(areas.try_occupy(OrientedRect::new(
            pos2(GRID_POINTS * 1.5, 900.0),
            0.0,
            vec2(96.0, 28.0)
        )));
    }

    /// **A cleared `OccupiedAreas` answers exactly as a fresh one, and keeps
    /// what it grew.**
    ///
    /// The caller builds one of these per solve and empties it rather than
    /// dropping it, so both halves matter and neither is visible by eye: a
    /// claim left behind would refuse a label the map should draw, and a
    /// buffer released would put the allocator back in the solve it was taken
    /// out of.
    ///
    /// The sequence is asked twice over the same claims, so "answers the
    /// same" covers acceptance AND refusal rather than only the first claim.
    #[test]
    fn a_cleared_claim_set_answers_as_a_fresh_one_and_keeps_its_buffers() {
        let claim = |i: usize| {
            {
                rect(
                    20.0 + (i % 8) as f32 * 30.0,
                    20.0 + (i / 8) as f32 * 25.0,
                    if i % 3 == 0 { 0.4 } else { 0.0 },
                    70.0,
                    22.0,
                )
            }
        };
        // **One claim with no usable bucket span**, so `unbucketed` is not an
        // empty list nothing can leave a stale index in. It is far enough
        // away that it never refuses one of the sixty-four, so what it tests
        // is the filing and not the rule.
        let sprawl = || rect(200_000.0, 200_000.0, 0.0, 40_000.0, 40_000.0);
        let answers = |areas: &mut OccupiedAreas| -> Vec<bool> {
            let mut out: Vec<bool> = (0..32).map(|i| areas.try_occupy(claim(i))).collect();
            out.push(areas.try_occupy(sprawl()));
            out.extend((32..64).map(|i| areas.try_occupy(claim(i))));
            out
        };

        let mut fresh = OccupiedAreas::new();
        let expected = answers(&mut fresh);
        assert!(
            expected.iter().any(|a| *a) && expected.iter().any(|a| !*a),
            "fixture: every claim got the same answer, so a stale claim set \
             could not change one: {expected:?}"
        );

        let mut reused = OccupiedAreas::new();
        assert_eq!(answers(&mut reused), expected);
        let grown = (
            reused.areas.capacity(),
            reused.filed.capacity(),
            reused.seen.capacity(),
            reused.candidates.capacity(),
            reused.grid.capacity(),
            reused.touched.capacity(),
        );
        assert!(grown.0 > 0 && grown.1 > 0 && grown.2 > 0 && grown.4 > 0 && grown.5 > 0);

        reused.clear();
        assert_eq!(
            (
                reused.areas.len(),
                reused.filed.len(),
                reused.seen.len(),
                reused.unbucketed.len(),
                reused.candidates.len(),
                reused.touched.len(),
            ),
            (0, 0, 0, 0, 0, 0),
            "`clear` left a claim behind"
        );
        // **The cell grid keeps its allocation and loses its content**, and
        // those are different fields now: `clear` empties the list of cells a
        // solve wrote and resets exactly those, so a cell it forgot to list
        // would still point at a retired claim. Read off the grid itself
        // rather than off the list that drives the reset, or the assertion
        // would be the implementation restated.
        assert!(
            reused.grid.iter().all(|&head| head == END),
            "`clear` left {} of {CELLS} cells pointing at a retired claim",
            reused.grid.iter().filter(|&&head| head != END).count(),
        );
        assert_eq!(
            (
                reused.areas.capacity(),
                reused.filed.capacity(),
                reused.seen.capacity(),
                reused.candidates.capacity(),
                reused.grid.capacity(),
                reused.touched.capacity(),
            ),
            grown,
            "`clear` released the buffers instead of emptying them"
        );
        assert_eq!(
            answers(&mut reused),
            expected,
            "a cleared claim set answered differently from a fresh one"
        );
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
