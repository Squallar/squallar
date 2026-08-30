//! Footprints in, prisms out.
//!
//! One building becomes a cap at [`RENDER_HEIGHT`](crate::RENDER_HEIGHT), a
//! wall quad per footprint edge running down to
//! [`RENDER_MIN_HEIGHT`](crate::RENDER_MIN_HEIGHT), and — only for the
//! buildings that do not start on the ground — a second cap at the bottom.
//!
//! # Nothing here draws
//!
//! This is the last of the worker's work. The mesh it answers is positions,
//! normals and indices in the volume box's own kilometres; putting it on the
//! ground, giving it a colour and getting it into the occluder pass is the
//! next unit's, and none of it is here.
//!
//! # Where the vertices go, and why there are so many of them
//!
//! A prism's faces are flat and meet at hard edges, so a vertex on the corner
//! of a building belongs to three surfaces with three different normals and
//! cannot be shared between them. A ring of `n` edges therefore gives `4n`
//! wall vertices — four per quad, not two per corner — plus a cap of about
//! `n`. Sharing them would round the corners of every building in the scene,
//! which is the one thing a city silhouette cannot afford.
//!
//! # The z the mesh is authored in
//!
//! **Height above the ground, in kilometres**, so that one vertex carries one
//! unit. The tile says metres and the box is kilometres; converting here
//! rather than in the renderer keeps the `/ 1000` off the frame thread and out
//! of a shader where it would be a second place for the unit to be wrong.
//!
//! Above the *ground* and not above mean sea level, because this crate has
//! never seen the terrain: what stands under a building is the height field,
//! which is another crate's answer and arrives on another job. A prism sitting
//! at `z = 0` is a prism whose ground has not been added yet, not a prism at
//! sea level.

use lyon_tessellation::geom::{Point, point};
use lyon_tessellation::path::{Path, Polygon};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers,
};

use crate::budget::{PrismBudget, shed_order};
use crate::footprint::{BuildingFootprint, Ring};

/// Metres to the kilometre. Written once here so the two conversions in this
/// module cannot disagree.
const M_PER_KM: f64 = 1000.0;

/// The tessellator's flattening tolerance, metres.
///
/// A footprint is line segments and carries no curves for this to flatten, so
/// it is not doing the job its name describes. It is set explicitly anyway,
/// and in metres, because lyon's default is `0.1` in whatever unit the path
/// happens to be in — and the path this module builds would be in kilometres
/// if it were not deliberately translated into a local metre frame first, at
/// which point `0.1` would be a hundred metres and would eat every building it
/// touched.
const TOLERANCE_M: f32 = 0.01;

/// Positions, normals and indices, plus what the budget did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildingMesh {
    /// Box kilometres: east, north, and height above the ground beneath.
    pub positions: Vec<[f32; 3]>,
    /// One unit normal per position.
    pub normals: Vec<[f32; 3]>,
    /// Triangles, counter-clockwise seen from outside the solid.
    pub indices: Vec<u32>,
    /// Footprints the budget accepted, tallest first.
    ///
    /// A footprint that tessellated to nothing is counted here rather than in
    /// [`shed`](Self::shed): the budget did not refuse it. `kept + shed` is
    /// therefore always the number of footprints handed in.
    pub kept: u32,
    /// Footprints the budget refused, shortest first.
    pub shed: u32,
    /// Tiles whose bytes did not decode.
    ///
    /// **Carried so a partial answer is not a silent one.** A round that
    /// parses ninety tiles and refuses ten still returns a mesh, because
    /// ninety tiles of city is the right thing to draw; but a mesh that said
    /// only `kept` and `shed` would report that round exactly as it reports a
    /// clean one. [`extrude`] never sets this — it is [`crate::jobs`]' to fill
    /// in, since tiles are what it holds.
    pub refused_tiles: u32,
}

impl BuildingMesh {
    /// Bytes this mesh would occupy in VRAM, priced the way
    /// [`crate::budget`] prices a rung.
    pub fn bytes(&self) -> u64 {
        self.positions.len() as u64 * crate::budget::PRISM_VERTEX_BYTES
            + self.indices.len() as u64 * crate::budget::PRISM_INDEX_BYTES
    }

    /// Whether the mesh has a triangle in it.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Whether positions, normals and indices agree with each other: one
    /// normal per position, a whole number of triangles, and no index off the
    /// end.
    ///
    /// Spelled as a method rather than left to the tests because
    /// `crate::jobs`' reply decoder needs exactly this check against bytes it
    /// did not build.
    pub fn is_coherent(&self) -> bool {
        self.positions.len() == self.normals.len()
            && self.indices.len().is_multiple_of(3)
            && self
                .indices
                .iter()
                .all(|&i| (i as usize) < self.positions.len())
    }
}

/// Triangulate and extrude, tallest building first, stopping when the budget
/// is full.
///
/// The order and the prefix rule are [`shed_order`]'s; this function's part is
/// that a building is **all or nothing**. A prism is built into a scratch and
/// appended only if both of its counts fit, so the mesh never carries a
/// building with some of its walls missing — which would be a hole through the
/// side of a tower rather than a smaller tower.
pub fn extrude(footprints: &[BuildingFootprint], budget: &PrismBudget) -> BuildingMesh {
    let order = shed_order(footprints);
    let mut mesh = BuildingMesh::default();
    let mut scratch = Scratch::default();
    let max_vertices = budget.max_vertices as usize;
    let max_indices = budget.max_indices as usize;

    for (rank, &index) in order.iter().enumerate() {
        let prism = tessellate(&footprints[index], &mut scratch);
        if mesh.positions.len() + prism.positions.len() > max_vertices
            || mesh.indices.len() + prism.indices.len() > max_indices
        {
            mesh.shed = (order.len() - rank) as u32;
            return mesh;
        }
        let base = mesh.positions.len() as u32;
        mesh.positions.extend_from_slice(&prism.positions);
        mesh.normals.extend_from_slice(&prism.normals);
        mesh.indices.extend(prism.indices.iter().map(|&i| i + base));
        mesh.kept += 1;
    }
    mesh
}

/// One building's own vertices, indexed from zero.
#[derive(Default)]
struct Prism {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

/// The allocations the loop reuses.
///
/// `VertexBuffers::new()` is `with_capacity(512, 1024)` — 10,240 and 4,096
/// bytes whichever polygon it is handed — and a city is thousands of small
/// polygons. Holding one set and clearing it is the difference between paying
/// that once and paying it per building.
#[derive(Default)]
struct Scratch {
    tessellator: FillTessellator,
    caps: VertexBuffers<[f32; 2], u32>,
    ring_points: Vec<Point<f32>>,
}

fn tessellate(footprint: &BuildingFootprint, scratch: &mut Scratch) -> Prism {
    let mut prism = Prism::default();
    let Some(origin) = footprint
        .rings
        .first()
        .and_then(|ring| ring.points.first())
        .copied()
    else {
        return prism;
    };
    // Kilometres are the wrong frame to tessellate in twice over: a building
    // is 0.02 to 0.1 of a unit across there, and the whole footprint sits at
    // an offset of up to a thousand units from the box origin, which is 6 cm
    // of `f32` resolution spent before the tessellator sees anything. Its own
    // first vertex, in metres, puts every coordinate inside a few hundred.
    let local = |p: [f64; 2]| {
        point(
            ((p[0] - origin[0]) * M_PER_KM) as f32,
            ((p[1] - origin[1]) * M_PER_KM) as f32,
        )
    };

    let mut builder = Path::builder();
    for ring in &footprint.rings {
        scratch.ring_points.clear();
        scratch
            .ring_points
            .extend(ring.points.iter().map(|&p| local(p)));
        builder.add_polygon(Polygon {
            points: &scratch.ring_points,
            closed: true,
        });
    }

    scratch.caps.vertices.clear();
    scratch.caps.indices.clear();
    let options = FillOptions::default()
        .with_fill_rule(FillRule::NonZero)
        .with_tolerance(TOLERANCE_M);
    let result = scratch.tessellator.tessellate_path(
        &builder.build(),
        &options,
        &mut BuffersBuilder::new(&mut scratch.caps, |vertex: FillVertex| {
            let p = vertex.position();
            [p.x, p.y]
        }),
    );
    if let Err(e) = result {
        // A footprint the tessellator refuses is one building missing, and
        // saying so is the whole reason this crate declares `log`: an empty
        // pane and a refused parse are otherwise the same observable.
        log::warn!(
            "buildings: a {}-ring footprint did not tessellate: {e:?}",
            footprint.rings.len(),
        );
        return prism;
    }

    let top_km = (footprint.height_m / M_PER_KM) as f32;
    let base_km = (footprint.base_m / M_PER_KM) as f32;
    let to_world = |p: [f32; 2], z: f32| {
        [
            (origin[0] + f64::from(p[0]) / M_PER_KM) as f32,
            (origin[1] + f64::from(p[1]) / M_PER_KM) as f32,
            z,
        ]
    };

    // The roof. Its winding is fixed here rather than trusted: lyon's output
    // order is its own business, and a cap facing the wrong way is a hole in
    // the top of every building in the scene.
    let roof_base = prism.positions.len() as u32;
    for &p in &scratch.caps.vertices {
        prism.positions.push(to_world(p, top_km));
        prism.normals.push([0.0, 0.0, 1.0]);
    }
    push_cap_indices(
        &mut prism.indices,
        roof_base,
        &scratch.caps,
        CapFacing::Upward,
    );

    // The floor, and only for a building that does not start on the ground.
    // Emitting it unconditionally would be a cap nothing can ever see, at the
    // price of a third of the cap vertices in the scene.
    if footprint.base_m > 0.0 {
        let floor_base = prism.positions.len() as u32;
        for &p in &scratch.caps.vertices {
            prism.positions.push(to_world(p, base_km));
            prism.normals.push([0.0, 0.0, -1.0]);
        }
        push_cap_indices(
            &mut prism.indices,
            floor_base,
            &scratch.caps,
            CapFacing::Downward,
        );
    }

    for ring in &footprint.rings {
        push_walls(&mut prism, ring, base_km, top_km);
    }
    prism
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapFacing {
    Upward,
    Downward,
}

/// A cap's triangles, wound so their front face points the way the cap does.
///
/// The signed area is taken in the tessellator's own local metre frame, where
/// `x` is east and `y` is north — so a positive area is counter-clockwise seen
/// from above, which is front-facing for an upward cap.
fn push_cap_indices(
    out: &mut Vec<u32>,
    base: u32,
    caps: &VertexBuffers<[f32; 2], u32>,
    facing: CapFacing,
) {
    for triangle in caps.indices.chunks_exact(3) {
        let (a, b, c) = (
            caps.vertices[triangle[0] as usize],
            caps.vertices[triangle[1] as usize],
            caps.vertices[triangle[2] as usize],
        );
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
        let counter_clockwise = area >= 0.0;
        let wanted = facing == CapFacing::Upward;
        if counter_clockwise == wanted {
            out.extend_from_slice(&[base + triangle[0], base + triangle[1], base + triangle[2]]);
        } else {
            out.extend_from_slice(&[base + triangle[0], base + triangle[2], base + triangle[1]]);
        }
    }
}

/// One quad per ring edge, with the ring's own outward normal.
///
/// **One rule serves both kinds of ring**, and that is what
/// [`Ring`]'s canonical winding buys. Whether a ring is a building's outline
/// wound counter-clockwise or a courtyard wound clockwise, the solid lies to
/// the **left** of travel; so the outward normal of the edge `a -> b` is
/// always its right-hand perpendicular, `(dy, -dx)`. Getting this wrong on
/// holes alone would light the inside of every courtyard in the scene from the
/// wrong side, which is invisible until somebody flies into one.
fn push_walls(prism: &mut Prism, ring: &Ring, base_km: f32, top_km: f32) {
    let n = ring.points.len();
    for i in 0..n {
        let a = ring.points[i];
        let b = ring.points[(i + 1) % n];
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let length = dx.hypot(dy);
        if length == 0.0 || !length.is_finite() {
            continue;
        }
        let normal = [(dy / length) as f32, (-dx / length) as f32, 0.0];
        let base = prism.positions.len() as u32;
        for (p, z) in [(a, base_km), (b, base_km), (b, top_km), (a, top_km)] {
            prism.positions.push([p[0] as f32, p[1] as f32, z]);
            prism.normals.push(normal);
        }
        // Counter-clockwise seen from the direction the normal points.
        prism
            .indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

#[cfg(test)]
mod tests;
