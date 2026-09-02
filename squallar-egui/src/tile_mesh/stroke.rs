//! epaint's thick-open stroke tessellation, run once in extent space.
//!
//! # Why this can be pre-computed at all
//!
//! A style's `line-width` is in **screen points** and the geometry beside it
//! is in **MVT extent units**, so a road is the width the style asked for
//! whatever side the tile is drawn at (`mvt::render_line`). That is what kept
//! strokes on the CPU: pre-tessellating in extent space would scale the width
//! with the tile.
//!
//! It does not have to. Every vertex epaint emits for a thick open path is
//! `point + normal * radius` — plus, at the two ends, a back-extrude along
//! `normal.rot90()` — and **the normal is computed from the path's own points
//! alone**. The map's placement is `scale * p + translation` with `scale > 0`
//! and no rotation, and a normalised direction is invariant under that. So the
//! offset a vertex takes from its point is the same number of *screen points*
//! whatever side the tile is drawn at: it can be computed once, in extent
//! space, and carried as a second vertex attribute the shader adds after the
//! placement.
//!
//! ```wgsl
//! r_locals.scale * a_pos + r_locals.translation + a_offset
//! ```
//!
//! No join or cap arithmetic happens in the shader. This is epaint's own
//! output, split into the part the placement scales and the part it does not.
//!
//! # Not bit-identical, and which way the error goes
//!
//! In `f32`, `(s·p₁ + t) − (s·p₀ + t) ≠ s·(p₁ − p₀)`: epaint takes the
//! difference of two *placed* points, this takes the difference of two extent
//! points, so the direction — and with it the normal — differs in the last few
//! ulps. The error is in this side's favour, because extent coordinates are
//! small integers and that subtraction is exact while epaint's suffers
//! cancellation at large translations. "Exact" is still not the claim:
//! `tile_mesh::tests::the_offsets_reproduce_epaints_own_tessellation` measures
//! the gap against epaint's real tessellator over every stroke of the
//! committed fixture and pins it.
//!
//! # What is refused
//!
//! Everything outside epaint's thick-open branch, and everything the packed
//! vertex cannot hold exactly. A refusal is not a fallback path of its own:
//! the shape stays a `Shape::Path` and takes the CPU placement it always took.

use egui::epaint::{Color32, PathShape, Pos2, Vec2, color::ColorMode};

/// Bytes one [`StrokeVertex`] occupies: `Sint16x2`, `Float32x2`, `Uint32`.
///
/// Positions are `i16` and that is **exact, not a quantisation**: MVT geometry
/// arrives as integer varints over the tile's extent, and a point that is not
/// an integer in range is refused rather than rounded.
pub const STROKE_VERTEX_BYTES: u64 = 16;

/// Bytes one stroke index occupies. `u16`, which is what bounds
/// [`STROKE_RUN_VERTICES`].
pub const STROKE_INDEX_BYTES: u64 = 2;

/// Vertices one stroke run may hold before the next path starts a new one.
///
/// The whole `u16` index space: indices are rebased onto the run's own first
/// vertex and the run binds the vertex buffer at that offset, so index 65,535
/// is addressable. A run is split at a path boundary, never inside one.
pub const STROKE_RUN_VERTICES: u32 = 65_536;

/// One vertex of a tile's pre-tessellated strokes.
///
/// `pos` is in **MVT extent units** and `offset` is in **screen points**: the
/// shader places the first and adds the second. `color` is egui's own packed
/// byte quadruple, moved across unchanged.
///
/// The layout description, not the storage — [`append`] writes bytes in
/// exactly this shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeVertex {
    pub pos: [i16; 2],
    pub offset: [f32; 2],
    pub color: u32,
}

/// What [`append`] did with one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Appended {
    /// Vertices and indices were written. Carries the index count.
    Wrote(u32),
    /// The path does not fit the run's `u16` index space. Nothing was
    /// written; close the run, open a new one and offer the path again.
    RunFull,
    /// Not a path this module can hold. Nothing was written and nothing will
    /// be: it stays on the CPU placement path.
    Refused,
}

/// One point of the path epaint's `Path::add_open_points` builds.
///
/// A copy of epaint's `PathPoint`, which is private to its tessellator, with
/// the position already narrowed to the pair the vertex carries.
#[derive(Clone, Copy, Debug)]
pub(super) struct PathPoint {
    pos: [i16; 2],
    normal: Vec2,
}

/// The per-tile working set, so a tile's flatten allocates once rather than
/// once per path.
#[derive(Default)]
pub(super) struct Scratch {
    points: Vec<PathPoint>,
}

/// Whether a `Shape::Path` is one this module tessellates.
///
/// `feathering` is the tessellator's, in points: egui's
/// `feathering_size_in_pixels / pixels_per_point`.
///
/// **Both of epaint's feathered branches are covered.** The thick one emits
/// four vertices per path point and the hairline one three, but every vertex
/// of either is `point + normal * scalar` with the scalar in screen points —
/// so both are invariant under the placement and both pre-compute. Which
/// branch a path takes depends on the feathering, and the feathering is a
/// flatten input ([`super::TileMeshes::feathering`]), so the choice is made
/// here rather than deferred to a shader that could not make it.
pub(super) fn is_open_stroke(path: &PathShape, feathering: f32) -> bool {
    let ColorMode::Solid(color) = path.stroke.color else {
        // `ColorMode::UV` colours a vertex from its position, which the
        // placement moves. `mvt::render_line` emits none.
        return false;
    };
    !path.closed
        && path.fill == Color32::TRANSPARENT
        && path.points.len() >= 2
        && path.stroke.width > 0.0
        && color != Color32::TRANSPARENT
        && feathering > 0.0
        // What `From<Stroke> for PathStroke` produces, and the only kind that
        // leaves the centreline where it is.
        && path.stroke.kind == egui::StrokeKind::Middle
}

/// Whether epaint paints this width as a **hairline**: a three-edge ridge two
/// feather-widths wide, with the thinness carried by the opacity rather than
/// by the geometry, because a line painted narrower than a pixel would alias.
///
/// epaint's own comparison, epsilon included. It is not a rare branch — over
/// both committed styles at every zoom `mvt::styled` can be asked for, **11.4%
/// of stroke points land here at `pixels_per_point` 1**, and none at 3.34 or
/// above; `fixture_tests::no_stroke_of_either_committed_style_falls_back_to_the_cpu`
/// is where those figures come from.
fn is_hairline(width: f32, feathering: f32) -> bool {
    width <= 0.9 * feathering
}

/// Vertices one path point contributes on epaint's thick branch: outer,
/// inner, inner, outer.
pub const THICK_VERTICES_PER_PATH_POINT: u32 = 4;

/// Vertices one path point contributes on the hairline branch: outer, middle,
/// outer.
pub const HAIRLINE_VERTICES_PER_PATH_POINT: u32 = 3;

/// Vertices one path point of this stroke contributes, which is a function of
/// the feathering and not of the geometry.
pub fn vertices_per_path_point(width: f32, feathering: f32) -> u32 {
    if is_hairline(width, feathering) {
        HAIRLINE_VERTICES_PER_PATH_POINT
    } else {
        THICK_VERTICES_PER_PATH_POINT
    }
}

/// One vertex, in the byte layout [`StrokeVertex`] declares.
fn write_vertex(vertices: &mut Vec<u8>, pos: [i16; 2], offset: Vec2, color: [u8; 4]) {
    vertices.extend_from_slice(&pos[0].to_ne_bytes());
    vertices.extend_from_slice(&pos[1].to_ne_bytes());
    vertices.extend_from_slice(&offset.x.to_ne_bytes());
    vertices.extend_from_slice(&offset.y.to_ne_bytes());
    vertices.extend_from_slice(&color);
}

/// Append one open stroked path to a run's buffers.
///
/// `first_vertex` is how many vertices the run already holds; the indices
/// appended are rebased onto it, so a run draws through a `u16` index buffer
/// and a vertex-buffer offset rather than a base vertex, which WebGL2 has no
/// draw call for.
///
/// A run may mix the two branches freely: both write the same
/// [`StrokeVertex`] into the same buffer and are drawn by the same pipeline.
/// What differs is only how many vertices a point contributes and which
/// triangles join them.
///
/// On anything but [`Appended::Wrote`] **both buffers are exactly as they
/// were**.
pub(super) fn append(
    path: &PathShape,
    feathering: f32,
    scratch: &mut Scratch,
    vertices: &mut Vec<u8>,
    indices: &mut Vec<u8>,
    first_vertex: u32,
) -> Appended {
    if !is_open_stroke(path, feathering) {
        return Appended::Refused;
    }
    let ColorMode::Solid(color) = path.stroke.color else {
        return Appended::Refused;
    };
    if !add_open_points(&path.points, &mut scratch.points) {
        return Appended::Refused;
    }

    let Ok(n) = u32::try_from(scratch.points.len()) else {
        return Appended::Refused;
    };
    if n < 2 {
        return Appended::Refused;
    }

    let width = path.stroke.width;
    let hairline = is_hairline(width, feathering);
    let per_point = vertices_per_path_point(width, feathering);
    if first_vertex + per_point * n > STROKE_RUN_VERTICES {
        return Appended::RunFull;
    }

    vertices.reserve(per_point as usize * n as usize * STROKE_VERTEX_BYTES as usize);
    if hairline {
        append_hairline_vertices(&scratch.points, width, feathering, color, vertices);
        Appended::Wrote(hairline_triangles(n, first_vertex, indices))
    } else {
        append_thick_vertices(&scratch.points, width, feathering, color, vertices);
        Appended::Wrote(thick_triangles(n, first_vertex, indices))
    }
}

/// epaint's thick branch: four vertices per point at `±outer_rad` and
/// `±inner_rad`, with the two outer ones at each end extruded back along the
/// line to anti-alias the cap.
fn append_thick_vertices(
    points: &[PathPoint],
    width: f32,
    feathering: f32,
    color: Color32,
    vertices: &mut Vec<u8>,
) {
    let inner_rad = 0.5 * (width - feathering);
    let outer_rad = 0.5 * (width + feathering);
    let outer = Color32::TRANSPARENT.to_array();
    let middle = color.to_array();
    let last = points.len() - 1;

    for (i, point) in points.iter().enumerate() {
        let normal = point.normal;
        // epaint anti-aliases an open line's two ends by extruding the outer
        // edge one feathering along the line, away from the body.
        let extrude = if i == 0 {
            normal.rot90() * feathering
        } else if i == last {
            -normal.rot90() * feathering
        } else {
            Vec2::ZERO
        };
        // Lane order is epaint's vertex order for one path point, and the
        // extrude is on the two outer lanes only.
        for (radius, color, extruded) in [
            (outer_rad, outer, true),
            (inner_rad, middle, false),
            (-inner_rad, middle, false),
            (-outer_rad, outer, true),
        ] {
            let offset = normal * radius + if extruded { extrude } else { Vec2::ZERO };
            write_vertex(vertices, point.pos, offset, color);
        }
    }
}

/// epaint's hairline branch: three vertices per point at `+n·feathering`, the
/// centreline, and `−n·feathering`.
///
/// The ridge is two feather-widths wide whatever the stroke asked for, and the
/// thinness is carried by the **opacity** instead — which is why a hairline
/// cannot be drawn as a thin version of the thick branch and needs its own
/// arm. `mul_color` is `Color32::gamma_multiply`, so that is called here
/// rather than reproduced and the byte is epaint's own.
///
/// **No caps.** epaint's hairline branch carries a standing `TODO` for them
/// and emits none, so neither does this — a cap here would be geometry the
/// path it replaces does not draw.
fn append_hairline_vertices(
    points: &[PathPoint],
    width: f32,
    feathering: f32,
    color: Color32,
    vertices: &mut Vec<u8>,
) {
    let middle = color.gamma_multiply(width / feathering).to_array();
    // Both outer edges: `color_outer` above the line and `color_fill` below
    // it, and an open stroke's fill is transparent.
    let outer = Color32::TRANSPARENT.to_array();

    for point in points {
        let normal = point.normal;
        write_vertex(vertices, point.pos, normal * feathering, outer);
        write_vertex(vertices, point.pos, Vec2::ZERO, middle);
        write_vertex(vertices, point.pos, -normal * feathering, outer);
    }
}

/// epaint's thick-branch triangles, in epaint's order because a moved index is
/// a moved picture: the start cap, six per segment, then the end extension.
/// `18n − 6` indices for `n` path points.
fn thick_triangles(n: u32, first_vertex: u32, indices: &mut Vec<u8>) -> u32 {
    indices.reserve(3 * (6 * n as usize - 2));
    let mut written = 0u32;
    let mut triangle = |a: u32, b: u32, c: u32| {
        for corner in [a, b, c] {
            indices.extend_from_slice(&((first_vertex + corner) as u16).to_ne_bytes());
        }
        written += 3;
    };
    triangle(0, 1, 2);
    triangle(0, 2, 3);
    for i1 in 1..n {
        let i0 = 4 * (i1 - 1);
        let i1 = 4 * i1;
        triangle(i0, i0 + 1, i1);
        triangle(i0 + 1, i1, i1 + 1);
        triangle(i0 + 1, i0 + 2, i1 + 1);
        triangle(i0 + 2, i1 + 1, i1 + 2);
        triangle(i0 + 2, i0 + 3, i1 + 2);
        triangle(i0 + 3, i1 + 2, i1 + 3);
    }
    let last = THICK_VERTICES_PER_PATH_POINT * (n - 1);
    triangle(last, last + 1, last + 2);
    triangle(last, last + 2, last + 3);

    debug_assert_eq!(written, 18 * n - 6, "epaint's index count for {n} points");
    written
}

/// epaint's hairline triangles: four per segment and none at the ends.
/// `12(n − 1)` indices for `n` path points.
///
/// epaint spells this as a loop over every point with `connect_with_previous`
/// false at the first one for an open path; this is that loop with the dead
/// first iteration removed, which emits the same triangles in the same order.
fn hairline_triangles(n: u32, first_vertex: u32, indices: &mut Vec<u8>) -> u32 {
    indices.reserve(3 * 4 * (n as usize - 1));
    let mut written = 0u32;
    let mut triangle = |a: u32, b: u32, c: u32| {
        for corner in [a, b, c] {
            indices.extend_from_slice(&((first_vertex + corner) as u16).to_ne_bytes());
        }
        written += 3;
    };
    for i1 in 1..n {
        let i0 = 3 * (i1 - 1);
        let i1 = 3 * i1;
        triangle(i0, i0 + 1, i1);
        triangle(i0 + 1, i1, i1 + 1);
        triangle(i0 + 1, i0 + 2, i1 + 1);
        triangle(i0 + 2, i1 + 1, i1 + 2);
    }

    debug_assert_eq!(written, 12 * (n - 1), "epaint's index count for {n} points");
    written
}

/// Build the path points epaint's `Path::add_open_points` would build.
///
/// A line-for-line port, including the sharp-corner split that makes the
/// result longer than `points`. Answers `false`, leaving `out` cleared, when
/// any coordinate is not an integer in `i16` range — the check that makes the
/// packed position exact rather than rounded.
fn add_open_points(points: &[Pos2], out: &mut Vec<PathPoint>) -> bool {
    out.clear();

    let n = points.len();
    debug_assert!(n >= 2, "checked by is_open_stroke");

    let mut narrowed: Vec<[i16; 2]> = Vec::with_capacity(n);
    for point in points {
        let Some(pair) = exact_i16(*point) else {
            return false;
        };
        narrowed.push(pair);
    }

    if n == 2 {
        // epaint's `add_line_segment`: one normal for both points.
        let normal = (points[1] - points[0]).normalized().rot90();
        out.push(PathPoint {
            pos: narrowed[0],
            normal,
        });
        out.push(PathPoint {
            pos: narrowed[1],
            normal,
        });
        return true;
    }

    out.reserve(n);
    out.push(PathPoint {
        pos: narrowed[0],
        normal: (points[1] - points[0]).normalized().rot90(),
    });
    let mut n0 = (points[1] - points[0]).normalized().rot90();
    for i in 1..n - 1 {
        let mut n1 = (points[i + 1] - points[i]).normalized().rot90();

        // Duplicated points (but not triplicated), as upstream.
        if n0 == Vec2::ZERO {
            n0 = n1;
        } else if n1 == Vec2::ZERO {
            n1 = n0;
        }

        let normal = (n0 + n1) / 2.0;
        let length_sq = normal.length_sq();
        let right_angle_length_sq = 0.5;
        if length_sq < right_angle_length_sq {
            // Sharper than a right angle: cut the corner off with two path
            // points at one position.
            let center_normal = normal.normalized();
            let n0c = (n0 + center_normal) / 2.0;
            let n1c = (n1 + center_normal) / 2.0;
            out.push(PathPoint {
                pos: narrowed[i],
                normal: n0c / n0c.length_sq(),
            });
            out.push(PathPoint {
                pos: narrowed[i],
                normal: n1c / n1c.length_sq(),
            });
        } else {
            out.push(PathPoint {
                pos: narrowed[i],
                normal: normal / length_sq,
            });
        }

        n0 = n1;
    }
    out.push(PathPoint {
        pos: narrowed[n - 1],
        normal: (points[n - 1] - points[n - 2]).normalized().rot90(),
    });

    true
}

/// `pos` as the `i16` pair a vertex carries, or `None` when it is not that
/// pair exactly.
fn exact_i16(pos: Pos2) -> Option<[i16; 2]> {
    Some([lane_i16(pos.x)?, lane_i16(pos.y)?])
}

/// One coordinate as an `i16`, refusing anything that is not that integer.
///
/// `as i32` truncates toward zero, so the round trip back through `f32` is
/// what rejects a fractional coordinate; the conversion is what rejects one a
/// wide tile buffer put outside `i16`.
fn lane_i16(value: f32) -> Option<i16> {
    let rounded = value as i32;
    if rounded as f32 != value {
        return None;
    }
    i16::try_from(rounded).ok()
}
