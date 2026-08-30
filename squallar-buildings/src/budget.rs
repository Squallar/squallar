//! What the prisms are allowed to cost, and who goes when they cost too much.
//!
//! # Its own VRAM row
//!
//! The terrain plan's budget table carries **"VRAM, geometry: 0"**, and that
//! row is about the ground: a procedural grid whose positions come from
//! `@builtin(vertex_index)` really does allocate no vertex and no index
//! buffer. Buildings do allocate both, so they get a row of their own rather
//! than hiding under a zero that was never about them:
//!
//! | | at [`FINEST_VERTEX_CEILING`] | at the [`DEFAULT_PRISM_VRAM_BYTES`] fit |
//! |---|---:|---:|
//! | positions + normals | 25.17 MB | 6.29 MB |
//! | indices, at the [`INDICES_PER_VERTEX_CEILING`] | 12.58 MB | 3.15 MB |
//! | **total** | **37.75 MB** | **9.44 MB** |
//!
//! Both columns are arithmetic over [`PRISM_VERTEX_BYTES`],
//! [`PRISM_INDEX_BYTES`] and the two ceilings, not measurements of a running
//! app — nothing has drawn a building yet. What *is* measured is the shape the
//! arithmetic stands on: see [`INDICES_PER_VERTEX_CEILING`].
//!
//! # The ladder, and what it is actually searching
//!
//! Written in the mould of `squallar_elevation::plan::HeightPlan::fit`: a rung
//! with a [`PrismRung::next_coarser`] and a loop, **total by construction**,
//! because the coarsest rung is returned rather than refused. What differs is
//! which ceiling is searched and which is solved.
//!
//! The height fit solves three ceilings in closed form and searches the tile
//! count, because where a footprint falls against the tile grid has no closed
//! form. Here the closed-form quantity is the byte cost of a rung, and the
//! quantity with no closed form is **how many indices a set of footprints
//! tessellates into**, which is not knowable from the input the caller has.
//! So the ladder prices indices at a ceiling rather than at their true ratio,
//! and [`shed`](crate::prism::extrude) enforces the vertex and index counts
//! *separately* as the mesh is built — a rung that turns out to be
//! index-heavy sheds a building earlier rather than overrunning the row.
//!
//! # And what it is not
//!
//! There is no `cfg` here and there must not be one. `vram_bytes` and
//! `max_buffer_bytes` are runtime figures the caller reads off its adapter,
//! for the reason the height plan reads `max_texture_dimension_2d` at runtime:
//! a beefcake desktop on Chrome and a cheap Android phone on the PWA are the
//! same `wasm32` and must not get the same answer from a compile-time
//! cascade.

use crate::footprint::BuildingFootprint;

/// Bytes one prism vertex occupies: three `f32` of position and three of
/// normal.
///
/// The normal is carried rather than derived in the fragment shader because
/// a prism's faces are flat and its vertices are already unshared across
/// them — a wall quad cannot share a vertex with the roof it meets, so the
/// face split has already paid for the duplication that per-face normals
/// need.
pub const PRISM_VERTEX_BYTES: u64 = 24;

/// Bytes one index occupies. `u32` and not `u16`, because
/// [`FINEST_VERTEX_CEILING`] is sixteen times past what a `u16` addresses.
pub const PRISM_INDEX_BYTES: u64 = 4;

/// Indices this budget prices each vertex at.
///
/// **Headroom over a measured ratio, not a guess.** The topology
/// `crate::prism` emits is fixed: a ring of `n` edges gives `4n` wall
/// vertices and `6n` wall indices, plus a cap of about `n` vertices and about
/// `3n` indices, plus the same again for a floor cap when the building does
/// not start on the ground. That is `5n` vertices against `9n` indices
/// without a floor and `6n` against `12n` with one — 1.8 and 2.0. Three is
/// where a tessellation that needed Steiner points for a self-intersecting
/// footprint still fits, and the mesh builder holds the real index count under
/// its own ceiling anyway.
pub const INDICES_PER_VERTEX_CEILING: u64 = 3;

/// The most vertices any rung allows: 1,048,576.
///
/// **A refusal ceiling that the fit starts from, not a target.** At
/// [`PRISM_VERTEX_BYTES`] this is 25.17 MB of positions and normals before a
/// single index, which no shipped `vram_bytes` reaches; it is the top of the
/// ladder so that a caller handing over a generous row gets a bounded answer
/// rather than an unbounded one.
pub const FINEST_VERTEX_CEILING: u32 = 1 << 20;

/// The coarsest rung: 4,096 vertices, roughly forty buildings.
///
/// The ladder stops here rather than at zero. A pane that can afford forty
/// towers should draw forty towers; the alternative is a rung ladder that
/// walks all the way to an empty mesh, which is a blank pane dressed up as a
/// fitted one.
pub const MIN_VERTEX_CEILING: u32 = 1 << 12;

/// The VRAM row this crate asks for when the caller has no measured figure of
/// its own: 16 MiB.
///
/// **Chosen against the arithmetic above, and not measured** — the same
/// posture `squallar_elevation::jobs`' `MAX_TILE_PX` records for itself.
/// It fits the ladder at 262,144 vertices, which is 9.44 MB of buffers and of
/// order 2,600 buildings at the ~100 vertices per building the confirmation
/// archive's footprints average. A real figure should replace it the day
/// something measures a frame.
pub const DEFAULT_PRISM_VRAM_BYTES: u64 = 16 << 20;

/// The ceilings a budget is fitted inside. Both are **runtime** figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PrismCeilings {
    /// Bytes of VRAM the building geometry may occupy, positions, normals and
    /// indices together.
    pub vram_bytes: u64,
    /// The adapter's largest single buffer, read off the adapter before device
    /// creation. Priced against each buffer on its own, because it is a limit
    /// on one allocation rather than on their sum.
    pub max_buffer_bytes: u64,
}

impl PrismCeilings {
    /// The row this crate asks for against an adapter that imposes no limit of
    /// its own worth speaking of.
    pub const DEFAULT: Self = Self {
        vram_bytes: DEFAULT_PRISM_VRAM_BYTES,
        max_buffer_bytes: u64::MAX,
    };
}

/// How many times the vertex ceiling has been halved below
/// [`FINEST_VERTEX_CEILING`].
///
/// A halving and not a fixed list of counts, for the reason
/// `squallar_elevation::plan::PostRung` is a `zooms_below` and not an enum: a
/// fixed arity is the wrong shape for a search whose floor is set by a
/// constant rather than by the number of variants somebody wrote down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrismRung {
    halvings: u8,
}

impl PrismRung {
    /// Where a fit starts: [`FINEST_VERTEX_CEILING`] entire.
    pub const FINEST: Self = Self { halvings: 0 };

    /// A rung `halvings` steps below the finest.
    pub fn from_halvings(halvings: u8) -> Self {
        Self { halvings }
    }

    /// How many halvings below the finest rung this one is.
    pub fn halvings(self) -> u8 {
        self.halvings
    }

    /// The vertex ceiling at this rung, never below [`MIN_VERTEX_CEILING`].
    ///
    /// Saturating rather than shifting: past 31 halvings the shift is wider
    /// than the counter, and a saturating answer is total where a shift is a
    /// panic in debug and a wrap in release.
    pub fn vertex_ceiling(self) -> u32 {
        FINEST_VERTEX_CEILING
            .checked_shr(u32::from(self.halvings))
            .unwrap_or(0)
            .max(MIN_VERTEX_CEILING)
    }

    /// The index ceiling that goes with it.
    pub fn index_ceiling(self) -> u32 {
        // Saturating, though it cannot fire: the finest rung times three is
        // 3,145,728, well inside a `u32`.
        u32::try_from(u64::from(self.vertex_ceiling()) * INDICES_PER_VERTEX_CEILING)
            .unwrap_or(u32::MAX)
    }

    /// Bytes a full mesh at this rung would occupy: vertices and their priced
    /// share of the index buffer.
    pub fn budgeted_bytes(self) -> u64 {
        u64::from(self.vertex_ceiling()) * PRISM_VERTEX_BYTES
            + u64::from(self.index_ceiling()) * PRISM_INDEX_BYTES
    }

    /// The next rung down, or `None` at the floor.
    ///
    /// The floor is [`MIN_VERTEX_CEILING`] and not the width of the counter:
    /// once a rung's ceiling has clamped there, halving again answers the same
    /// number and the loop would spin.
    pub fn next_coarser(self) -> Option<Self> {
        if self.vertex_ceiling() <= MIN_VERTEX_CEILING {
            return None;
        }
        self.halvings.checked_add(1).map(Self::from_halvings)
    }
}

/// What stopped the budget from being larger.
///
/// **There is no "nothing bound it" arm.** The fit starts at
/// [`FINEST_VERTEX_CEILING`] and comes down, so one of these is always the
/// answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrismLimit {
    /// [`FINEST_VERTEX_CEILING`] itself: neither runtime ceiling cut into it.
    /// Where the fit starts, and the answer whenever the caller's row is
    /// generous.
    VertexCeiling,
    /// The building geometry's own VRAM row.
    Vram,
    /// The adapter's largest single buffer.
    BufferSize,
}

/// A fitted budget: how many vertices and indices the mesh may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PrismBudget {
    pub max_vertices: u32,
    pub max_indices: u32,
    /// The rung the ladder stopped on.
    pub rung: PrismRung,
    /// What stopped it.
    pub limit: PrismLimit,
}

impl PrismBudget {
    /// Step the ladder down until a rung's worst-case bytes fit both runtime
    /// ceilings.
    ///
    /// **Total.** The coarsest rung is returned whether or not it cleared the
    /// ceilings, with `limit` naming what was still unmet — a pane that can
    /// afford almost nothing gets `MIN_VERTEX_CEILING` and a shed that empties
    /// it, rather than no answer at all.
    pub fn fit(ceilings: PrismCeilings) -> Self {
        let mut rung = PrismRung::FINEST;
        loop {
            let unmet = Self::unmet_at(rung, ceilings);
            match (unmet, rung.next_coarser()) {
                (None, _) => {
                    return Self::at(
                        rung,
                        if rung == PrismRung::FINEST {
                            PrismLimit::VertexCeiling
                        } else {
                            // The ceiling that was unmet one rung up is the one
                            // that forced this step; re-reading it there is
                            // what makes the answer say *why* rather than only
                            // *what*.
                            Self::unmet_at(PrismRung::from_halvings(rung.halvings() - 1), ceilings)
                                .unwrap_or(PrismLimit::VertexCeiling)
                        },
                    );
                }
                (Some(limit), None) => return Self::at(rung, limit),
                (Some(_), Some(next)) => rung = next,
            }
        }
    }

    /// Which ceiling `rung` fails, if either does. VRAM is judged first
    /// because it is the row this crate owns; a caller whose adapter is the
    /// binding one wants to be told that instead.
    fn unmet_at(rung: PrismRung, ceilings: PrismCeilings) -> Option<PrismLimit> {
        if rung.budgeted_bytes() > ceilings.vram_bytes {
            return Some(PrismLimit::Vram);
        }
        let vertex_bytes = u64::from(rung.vertex_ceiling()) * PRISM_VERTEX_BYTES;
        let index_bytes = u64::from(rung.index_ceiling()) * PRISM_INDEX_BYTES;
        if vertex_bytes > ceilings.max_buffer_bytes || index_bytes > ceilings.max_buffer_bytes {
            return Some(PrismLimit::BufferSize);
        }
        None
    }

    fn at(rung: PrismRung, limit: PrismLimit) -> Self {
        Self {
            max_vertices: rung.vertex_ceiling(),
            max_indices: rung.index_ceiling(),
            rung,
            limit,
        }
    }

    /// Bytes a mesh filling this budget would occupy.
    pub fn budgeted_bytes(&self) -> u64 {
        u64::from(self.max_vertices) * PRISM_VERTEX_BYTES
            + u64::from(self.max_indices) * PRISM_INDEX_BYTES
    }
}

/// The order buildings are kept in: **tallest first**, ties in the order they
/// arrived.
///
/// Returns indices into `footprints` rather than reordering them, so the
/// caller keeps its own numbering.
///
/// # Why tallest first, and why a prefix
///
/// The shed keeps a **prefix** of this order and drops the rest, so a building
/// that is present implies every taller building is present. The greedier
/// alternative — walk past a building that does not fit and pick up smaller
/// ones behind it — packs the budget fuller, and it was refused: it makes the
/// kept set depend on the tessellated size of each footprint rather than on
/// its height, so the skyline gains and loses a tower as the camera moves and
/// the footprint set changes underneath it. A slightly under-full budget reads
/// as a smaller city; a non-monotone one reads as a flickering city.
///
/// A stable sort and not an unstable one, for the same reason: 16 of the
/// confirmation archive's 126 buildings share `render_height = 5`, so ties are
/// the common case rather than the corner, and an unstable sort would let the
/// kept set at a given ceiling depend on the sort's internals.
pub fn shed_order(footprints: &[BuildingFootprint]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..footprints.len()).collect();
    order.sort_by(|&a, &b| {
        footprints[b]
            .height_m
            .partial_cmp(&footprints[a].height_m)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order
}

#[cfg(test)]
mod tests;
