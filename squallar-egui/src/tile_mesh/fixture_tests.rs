//! The stroke flatten against a real tile, and against epaint itself.
//!
//! Native-only: the fixture is a PMTiles archive read through
//! [`crate::basemap_archive`], which needs `tokio` and the filesystem.
//!
//! # What is proved here
//!
//! * **Fidelity.** Every covered path is tessellated a second time by
//!   epaint's own `Tessellator`, on the *placed* shape, and the two outputs
//!   are compared vertex for vertex and index for index. Indices and colours
//!   must match exactly; positions are allowed the ulps the two orders of
//!   arithmetic differ by, and the worst one measured is asserted against a
//!   bound far under what the rasteriser can resolve.
//! * **Coverage.** Every `Shape::Path` of the tile is inside exactly one
//!   run's span, or inside none. A path in two spans would draw twice; a path
//!   in none draws on the CPU, which is correct but is the thing this work
//!   exists to avoid, so the count is pinned.
//! * **Bytes.** What one tile's strokes cost the GPU, against the fill-only
//!   figure this workspace shipped before them.

use std::path::PathBuf;

use walkers::ShapeOrText;

use super::*;
use crate::basemap_archive::{BasemapArchive, FileRangeSource};

/// `pixels_per_point` 1, where egui's default `feathering_size_in_pixels` of
/// 1.0 makes the feathering 1.0 points.
const PIXELS_PER_POINT: f32 = 1.0;

/// The tile every figure in this file is measured on: Monaco's own z14 tile,
/// the densest the committed fixture holds.
const TILE: (u8, u32, u32) = (14, 8529, 5974);

/// Where the tile is drawn for the fidelity comparison — 256 points across,
/// which is what a whole zoom at bias 0 gives, offset well away from the
/// origin so the placement's translation is large enough for the `f32`
/// cancellation this file is measuring to actually happen.
fn draw_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(1731.0, 909.0), egui::vec2(256.0, 256.0))
}

/// The fixture's shape list under the committed dark style, or `None` when
/// the archive will not open.
fn monaco_shapes() -> Option<Vec<ShapeOrText>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/monaco.pmtiles");
    if FileRangeSource::open(&path).is_err() {
        eprintln!(
            "SKIPPED: {} would not open. It is committed; `git status` on it.",
            path.display()
        );
        return None;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let bytes = runtime.block_on(async {
        let archive =
            BasemapArchive::open(FileRangeSource::open(&path).expect("the fixture opens"))
                .await
                .expect("the fixture is a PMTiles archive");
        archive
            .tile(TILE.0, TILE.1, TILE.2)
            .await
            .expect("the tile reads")
            .into_bytes()
            .expect("the fixture holds Monaco's own z14 tile")
    });
    Some(
        walkers::mvt::render(&bytes, &crate::basemap_style::committed(true), TILE.0)
            .expect("the tile renders"),
    )
}

/// epaint's tessellator, configured the way egui configures it for a frame at
/// [`PIXELS_PER_POINT`].
fn tessellator() -> egui::epaint::Tessellator {
    egui::epaint::Tessellator::new(
        PIXELS_PER_POINT,
        egui::epaint::TessellationOptions::default(),
        [1, 1],
        Vec::new(),
    )
}

/// The feathering [`tessellator`] will use, read the way the shipped code
/// reads it rather than restated.
fn feathering() -> f32 {
    let options = egui::epaint::TessellationOptions::default();
    assert!(options.feathering, "the default is feathered");
    options.feathering_size_in_pixels / PIXELS_PER_POINT
}

/// **The pre-computed offsets reproduce epaint's own tessellation.**
///
/// The claim this whole mechanism rests on: the offset a stroke vertex takes
/// from its point is invariant under the placement, so it can be computed
/// once in extent space. This runs epaint's real tessellator over every
/// covered path of a real tile, *placed*, and compares.
///
/// Indices and colours are exact. Positions are not, and cannot be: epaint
/// computes its normals from the differences of **placed** points, which in
/// `f32` is not the placement of the difference of extent points. The error
/// runs the other way from what a reader expects — extent coordinates are
/// small integers, so this side's subtraction is exact while epaint's loses
/// bits to cancellation — but it is measured rather than argued.
#[test]
fn the_offsets_reproduce_epaints_own_tessellation() {
    let Some(shapes) = monaco_shapes() else {
        return;
    };
    let feathering = feathering();
    let flat = flatten(&shapes, feathering);
    let rect = draw_rect();
    let place = Placement::of(rect);
    let placement = walkers::mvt::placement(rect);
    let mut tess = tessellator();

    let mut compared = 0usize;
    let mut worst = 0.0f32;

    for run in flat.runs() {
        if run.kind != RunKind::Stroke {
            continue;
        }
        // Where this run's vertices and indices start. Indices are rebased
        // onto the run's own first vertex, exactly as the shader reads them.
        let mut vertex = run.first_vertex as usize;
        let mut index = run.first_index as usize;

        for shape_index in run.shape_index..run.shape_index + run.shape_span {
            if !matches!(
                &shapes[shape_index as usize],
                ShapeOrText::Shape(egui::Shape::Path(_))
            ) {
                continue;
            }
            let ShapeOrText::Shape(egui::Shape::Path(placed)) =
                shapes[shape_index as usize].placed(placement)
            else {
                panic!("the path arm did not answer a path");
            };

            let mut oracle = egui::epaint::Mesh::default();
            tess.tessellate_path(&placed, &mut oracle);
            assert!(
                !oracle.vertices.is_empty(),
                "epaint tessellated shape {shape_index} to nothing, so the \
                 comparison below is vacuous"
            );

            for (lane, expected) in oracle.vertices.iter().enumerate() {
                let ours = flat
                    .stroke_vertex(vertex + lane)
                    .expect("the stroke vertex is in range");
                let here = [
                    place.scale * f32::from(ours.pos[0]) + place.translation[0] + ours.offset[0],
                    place.scale * f32::from(ours.pos[1]) + place.translation[1] + ours.offset[1],
                ];
                assert_eq!(
                    ours.color.to_ne_bytes(),
                    expected.color.to_array(),
                    "shape {shape_index} vertex {lane}'s colour is not epaint's"
                );
                worst = worst
                    .max((here[0] - expected.pos.x).abs())
                    .max((here[1] - expected.pos.y).abs());
            }

            for (lane, expected) in oracle.indices.iter().enumerate() {
                let ours = flat
                    .stroke_index(index + lane)
                    .expect("the stroke index is in range");
                // epaint's indices are rebased into `oracle`, which starts
                // empty per shape; ours are rebased onto the run's first
                // vertex and then onto this path's place in the run.
                assert_eq!(
                    u32::from(ours) - (vertex as u32 - run.first_vertex),
                    *expected,
                    "shape {shape_index} index {lane} is not epaint's"
                );
            }

            vertex += oracle.vertices.len();
            index += oracle.indices.len();
            compared += 1;
        }
    }

    assert!(
        compared > 400,
        "only {compared} paths were compared against epaint; the fixture's \
         densest tile holds hundreds and a comparison over a handful proves \
         nothing"
    );

    // **The bound is one ulp of the placed coordinate**, not a tolerance
    // somebody picked. Nothing can be closer: the two sides land on adjacent
    // representable `f32`s at worst, which is the floor of what the format can
    // express and is 32x under the 1/256 of a point the rasteriser resolves.
    // Stating it this way also makes the bound independent of the style and of
    // where the tile is drawn, so a style edit cannot quietly loosen it.
    let ulp = f32::EPSILON * rect.max.x.max(rect.max.y);
    println!(
        "worst position disagreement with epaint: {worst:e} points over \
         {compared} paths, against one ulp of the placed coordinate = {ulp:e}"
    );
    assert!(
        worst <= ulp,
        "the pre-computed offsets put a vertex {worst} points from where \
         epaint's own tessellator put it, over {compared} paths — more than \
         the {ulp} that is one ulp of the placed coordinate, so the two are \
         no longer differing only in the last bit"
    );
}

/// **Every path is drawn once.** A path inside two runs would draw twice; the
/// runs must therefore be disjoint, in shape order, and cover only paths.
#[test]
fn runs_are_disjoint_in_shape_order_and_cover_only_paths() {
    let Some(shapes) = monaco_shapes() else {
        return;
    };
    let flat = flatten(&shapes, feathering());

    let mut reach = 0u32;
    let mut covered = 0usize;
    for run in flat.runs() {
        assert!(
            run.shape_index >= reach,
            "run at shape {} overlaps the one before it, which reached {reach}",
            run.shape_index
        );
        assert!(run.shape_span >= 1);
        if run.kind == RunKind::Fill {
            assert_eq!(run.shape_span, 1, "a fill run covers one mesh");
            assert!(matches!(
                shapes[run.shape_index as usize],
                ShapeOrText::Shape(egui::Shape::Mesh(_))
            ));
        } else {
            for at in run.shape_index..run.shape_index + run.shape_span {
                match &shapes[at as usize] {
                    // A label inside a span is fine: the ground phase defers
                    // every `Text` and none of them draws between two of the
                    // span's paths.
                    ShapeOrText::Text(_) => {}
                    ShapeOrText::Shape(egui::Shape::Path(_)) => covered += 1,
                    other => panic!(
                        "shape {at} is inside a stroke run's span but is a \
                         {other:?}, which draws in the ground phase and would \
                         be drawn under the run's geometry"
                    ),
                }
            }
        }
        reach = run.shape_index + run.shape_span;
    }

    let paths = shapes
        .iter()
        .filter(|shape| matches!(shape, ShapeOrText::Shape(egui::Shape::Path(_))))
        .count();
    assert!(paths > 400, "the fixture holds {paths} stroked paths");
    assert_eq!(
        covered,
        paths,
        "{} of the tile's {paths} stroked paths were refused and stay on the \
         CPU. Every coordinate in an MVT tile is an integer, so a refusal \
         here is a defect and not a property of the data",
        paths - covered
    );
}

/// **What one tile's strokes cost the GPU**, against the fill-only figure
/// this workspace shipped before them, and against what epaint's own vertex
/// format would have cost for the same geometry.
///
/// A **report with a floor**, not a ceiling on growth: the exact figures move
/// with the committed style, and pinning them would make a style edit a
/// suite failure. What is asserted is the property the format was chosen for
/// — that the packed stroke vertex is smaller than epaint's — and the numbers
/// are printed so a reader has the denominators.
#[test]
fn the_stroke_buffers_are_smaller_than_epaints_own_format() {
    let Some(shapes) = monaco_shapes() else {
        return;
    };
    let flat = flatten(&shapes, feathering());

    let fills = u64::from(flat.vertex_count()) * TILE_VERTEX_BYTES
        + u64::from(flat.index_count()) * TILE_INDEX_BYTES;
    let strokes = u64::from(flat.stroke_vertex_count()) * stroke::STROKE_VERTEX_BYTES
        + u64::from(flat.stroke_index_count()) * stroke::STROKE_INDEX_BYTES;

    // What the same geometry costs in `epaint::Vertex` (pos, uv, colour = 20
    // bytes) with `u32` indices — the format the frame thread stages today,
    // every frame, for every visible tile.
    let epaint =
        u64::from(flat.stroke_vertex_count()) * 20 + u64::from(flat.stroke_index_count()) * 4;

    let path_points: usize = shapes
        .iter()
        .map(|shape| match shape {
            ShapeOrText::Shape(egui::Shape::Path(path)) => path.points.len(),
            _ => 0,
        })
        .sum();

    let fill_runs = flat
        .runs()
        .iter()
        .filter(|run| run.kind == RunKind::Fill)
        .count();
    let stroke_runs = flat.runs().len() - fill_runs;

    println!(
        "monaco z14/{}/{}: {} shapes, {path_points} stroke points, \
         {fill_runs} fill runs + {stroke_runs} stroke runs\n  \
         fills   {fills} B ({} vertices, {} indices)\n  \
         strokes {strokes} B ({} vertices, {} indices) = {:.3}x the fills\n  \
         same strokes in epaint's format: {epaint} B ({:.3}x the fills)\n  \
         whole tile {} B = {:.3}x the fill-only baseline\n  \
         per centreline point: {:.1} B here, {:.1} B in epaint's format",
        TILE.1,
        TILE.2,
        shapes.len(),
        flat.vertex_count(),
        flat.index_count(),
        flat.stroke_vertex_count(),
        flat.stroke_index_count(),
        strokes as f64 / fills as f64,
        epaint as f64 / fills as f64,
        fills + strokes,
        (fills + strokes) as f64 / fills as f64,
        strokes as f64 / path_points as f64,
        epaint as f64 / path_points as f64,
    );

    assert!(
        strokes < epaint,
        "the packed stroke vertex ({strokes} B) is no smaller than epaint's \
         own ({epaint} B), which is the whole reason for the i16 position and \
         the u16 index"
    );
    assert!(
        flat.stroke_vertex_count() > 0 && flat.vertex_count() > 0,
        "non-vacuity: one of the two halves is empty, so the ratio above is \
         not a ratio"
    );
}

/// **The vertex count is epaint's arithmetic, exactly.**
///
/// Four vertices and `18n − 6` indices per path point, summed over the paths
/// a run covers. A count that drifts from this is a tessellation that has
/// stopped being epaint's, whatever the pictures look like.
#[test]
fn the_counts_are_epaints_arithmetic() {
    let Some(shapes) = monaco_shapes() else {
        return;
    };
    let feathering = feathering();
    let flat = flatten(&shapes, feathering);
    let mut tess = tessellator();

    let mut vertices = 0u32;
    let mut indices = 0u32;
    for shape in &shapes {
        let ShapeOrText::Shape(egui::Shape::Path(path)) = shape else {
            continue;
        };
        let mut oracle = egui::epaint::Mesh::default();
        tess.tessellate_path(path, &mut oracle);
        vertices += oracle.vertices.len() as u32;
        indices += oracle.indices.len() as u32;
        assert_eq!(
            oracle.indices.len() as u32,
            18 * (oracle.vertices.len() as u32 / stroke::VERTICES_PER_PATH_POINT) - 6,
            "epaint's own output is not 4 vertices and 18n-6 indices per path \
             point, so the arithmetic this file documents is wrong"
        );
    }

    assert_eq!(flat.stroke_vertex_count(), vertices);
    assert_eq!(flat.stroke_index_count(), indices);
}
