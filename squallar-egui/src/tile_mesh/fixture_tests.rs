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

/// The fixture tile's decoded geometry, or `None` when the archive will not
/// open.
///
/// The **parse**, not the styling: [`walkers::mvt::styled`] evaluates any
/// style at any zoom over this without re-reading the bytes, which is what
/// lets the zoom sweep below cost one decode rather than fifteen.
fn monaco_parsed() -> Option<walkers::mvt::ParsedTile> {
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
    Some(walkers::mvt::parse(&bytes).expect("the tile decodes"))
}

/// The fixture's shape list under the committed dark style at its own zoom.
fn monaco_shapes() -> Option<Vec<ShapeOrText>> {
    let parsed = monaco_parsed()?;
    Some(walkers::mvt::styled(
        &parsed,
        &crate::basemap_style::committed(true),
        TILE.0,
    ))
}

/// epaint's tessellator, configured the way egui configures it for a frame at
/// `pixels_per_point`.
fn tessellator_at(pixels_per_point: f32) -> egui::epaint::Tessellator {
    egui::epaint::Tessellator::new(
        pixels_per_point,
        egui::epaint::TessellationOptions::default(),
        [1, 1],
        Vec::new(),
    )
}

/// The feathering [`tessellator_at`] will use, read the way the shipped code
/// reads it rather than restated.
fn feathering_at(pixels_per_point: f32) -> f32 {
    let options = egui::epaint::TessellationOptions::default();
    assert!(options.feathering, "the default is feathered");
    options.feathering_size_in_pixels / pixels_per_point
}

/// The feathering at [`PIXELS_PER_POINT`], which is what most of this file
/// sweeps at.
fn feathering() -> f32 {
    feathering_at(PIXELS_PER_POINT)
}

/// The `pixels_per_point` that forces most of the committed styles' widths
/// onto epaint's **hairline** branch.
///
/// The thinnest evaluated width is 0.3 points and the thickest 22.0, so no
/// single value puts every stroke on one branch; 0.25 gives a feathering of 4
/// and a hairline threshold of 3.6 points, which is above the great majority
/// of them. It is not a display anybody has — it is a lever for reaching the
/// branch, and `the_offsets_reproduce_epaints_own_tessellation` asserts that
/// both branches were actually reached rather than assuming this worked.
const HAIRLINE_PIXELS_PER_POINT: f32 = 0.25;

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
/// [`the_offsets_reproduce_epaints_own_tessellation`] at one
/// `pixels_per_point`, answering how many paths it compared, how many of them
/// took each of epaint's two branches, and the worst position disagreement.
fn offsets_against_epaint(
    shapes: &[ShapeOrText],
    pixels_per_point: f32,
) -> (usize, usize, usize, f32) {
    let feathering = feathering_at(pixels_per_point);
    let flat = flatten(shapes, feathering);
    let rect = draw_rect();
    let place = Placement::of(rect);
    let placement = walkers::mvt::placement(rect);
    let mut tess = tessellator_at(pixels_per_point);

    let mut compared = 0usize;
    let mut thick = 0usize;
    let mut hairline = 0usize;
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

            if stroke::vertices_per_path_point(placed.stroke.width, feathering)
                == stroke::HAIRLINE_VERTICES_PER_PATH_POINT
            {
                hairline += 1;
            } else {
                thick += 1;
            }

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

    (compared, thick, hairline, worst)
}

/// **The pre-computed offsets reproduce epaint's own tessellation, on both of
/// its branches.**
///
/// The claim this whole mechanism rests on. Run twice: once at
/// [`PIXELS_PER_POINT`], where the committed styles are mostly thick, and once
/// at [`HAIRLINE_PIXELS_PER_POINT`], where most widths cross onto the ridge.
/// **Both arms are asserted to have been reached**, so neither pass can be a
/// silent repeat of the other branch.
#[test]
fn the_offsets_reproduce_epaints_own_tessellation() {
    let Some(shapes) = monaco_shapes() else {
        return;
    };

    let mut reached_thick = 0usize;
    let mut reached_hairline = 0usize;

    for pixels_per_point in [PIXELS_PER_POINT, HAIRLINE_PIXELS_PER_POINT] {
        let (compared, thick, hairline, worst) = offsets_against_epaint(&shapes, pixels_per_point);
        reached_thick += thick;
        reached_hairline += hairline;

        assert!(
            compared > 400,
            "only {compared} paths were compared against epaint at \
             pixels_per_point {pixels_per_point}; the fixture's densest tile \
             holds hundreds and a comparison over a handful proves nothing"
        );

        // **The bound is one ulp of the placed coordinate**, not a tolerance
        // somebody picked. Nothing can be closer: the two sides land on
        // adjacent representable `f32`s at worst, which is the floor of what
        // the format can express and is far under the 1/256 of a point the
        // rasteriser resolves. Stating it this way also makes the bound
        // independent of the style and of where the tile is drawn.
        let rect = draw_rect();
        let ulp = f32::EPSILON * rect.max.x.max(rect.max.y);
        println!(
            "  pixels_per_point {pixels_per_point}: {compared} paths \
             ({thick} thick, {hairline} hairline), worst disagreement \
             {worst:e} points against one ulp = {ulp:e}"
        );
        assert!(
            worst <= ulp,
            "at pixels_per_point {pixels_per_point} the pre-computed offsets \
             put a vertex {worst} points from where epaint's own tessellator \
             put it, over {compared} paths — more than the {ulp} that is one \
             ulp of the placed coordinate, so the two are no longer differing \
             only in the last bit"
        );
    }

    // Neither branch may be untested: a pass over thick strokes alone would
    // say nothing about the hairline arm, and vice versa.
    assert!(
        reached_thick > 0 && reached_hairline > 0,
        "the comparison reached {reached_thick} thick paths and \
         {reached_hairline} hairline ones; both of epaint's feathered \
         branches have to be exercised or one of the two arms is unproven"
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
    let mut tess = tessellator_at(PIXELS_PER_POINT);

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

        // Which arithmetic applies is a property of the width against the
        // feathering, so it is asked rather than assumed: 4 vertices and
        // `18n - 6` indices on the thick branch, 3 and `12(n - 1)` on the
        // hairline one.
        let per_point = stroke::vertices_per_path_point(path.stroke.width, feathering);
        let n = oracle.vertices.len() as u32 / per_point;
        let expected = if per_point == stroke::HAIRLINE_VERTICES_PER_PATH_POINT {
            12 * (n - 1)
        } else {
            18 * n - 6
        };
        assert_eq!(
            oracle.indices.len() as u32,
            expected,
            "epaint's own output does not match the arithmetic this file \
             documents for the branch a {}-point line takes at feathering \
             {feathering}",
            path.stroke.width
        );
    }

    assert_eq!(flat.stroke_vertex_count(), vertices);
    assert_eq!(flat.stroke_index_count(), indices);
}

/// Which shapes of `shapes` a stroke run of `flat` has taken to the GPU.
///
/// A `Text` inside a span is marked too and simply never counted: the caller
/// only ever asks about paths.
fn covered_by_a_stroke_run(shapes: &[ShapeOrText], flat: &TileMeshes) -> Vec<bool> {
    let mut covered = vec![false; shapes.len()];
    for run in flat.runs() {
        if run.kind != RunKind::Stroke {
            continue;
        }
        for at in run.shape_index..run.shape_index + run.shape_span {
            covered[at as usize] = true;
        }
    }
    covered
}

/// What one sweep of both committed themes over every reachable zoom found.
struct Sweep {
    /// Stroke points the styles produced, over every (theme, zoom) pair.
    points: usize,
    /// Of those, points on a path the GPU path refused.
    refused: usize,
    /// Points on a path epaint paints as a **hairline** — its three-edge
    /// ridge branch, a different topology with no caps. Counted over *every*
    /// stroke point and not only the refused ones, because since the hairline
    /// arm landed these are drawn rather than refused, and the figure is now
    /// the coverage of that arm rather than the size of a gap.
    hairline: usize,
    pairs: usize,
    pairs_with_strokes: usize,
    /// The (theme, zoom, refused, hairline) that refused most.
    worst: Option<(&'static str, u8, usize, usize)>,
}

/// Style the fixture at every zoom of both committed themes, flatten each
/// result at `feathering`, and count what the GPU path refused.
///
/// **Integer zooms are the whole population, not a sample.** `mvt::styled`
/// takes `zoom: u8` and every call site rounds
/// (`ui_map_overlays::draw_tile_layer`'s `zoom.round() as u8`), so a style is
/// never evaluated between two stops and an interpolated width has no
/// continuum to dip in. `0..=TILE.0` is the reachable range for this archive:
/// `tile_zoom` is `.min(source_max_zoom)`-capped and the fixture declares 14.
///
/// What this does **not** vary is the geometry: one tile's features, styled at
/// every zoom. A real z6 frame draws a z6 tile. The widths are the variable
/// under test and the geometry is here to carry them.
fn sweep(parsed: &walkers::mvt::ParsedTile, feathering: f32) -> Sweep {
    let mut out = Sweep {
        points: 0,
        refused: 0,
        hairline: 0,
        pairs: 0,
        pairs_with_strokes: 0,
        worst: None,
    };

    for (theme, dark) in [("dark", true), ("light", false)] {
        let style = crate::basemap_style::committed(dark);
        for zoom in 0..=TILE.0 {
            let shapes = walkers::mvt::styled(parsed, &style, zoom);
            let flat = flatten(&shapes, feathering);
            let covered = covered_by_a_stroke_run(&shapes, &flat);

            let (mut points, mut refused, mut hairline) = (0usize, 0usize, 0usize);
            for (index, shape) in shapes.iter().enumerate() {
                let ShapeOrText::Shape(egui::Shape::Path(path)) = shape else {
                    continue;
                };
                points += path.points.len();
                // Before the coverage test: this is which epaint branch the
                // width takes, not whether this code handled it.
                if path.stroke.width <= 0.9 * feathering {
                    hairline += path.points.len();
                }
                if covered[index] {
                    continue;
                }
                refused += path.points.len();
            }

            out.pairs += 1;
            out.pairs_with_strokes += usize::from(points > 0);
            out.points += points;
            out.refused += refused;
            out.hairline += hairline;
            if refused > 0 && out.worst.is_none_or(|(_, _, most, _)| refused > most) {
                out.worst = Some((theme, zoom, refused, hairline));
            }
        }
    }
    out
}

/// Non-vacuity for a sweep, so a zero refusal count is a result and not an
/// absence of strokes to refuse.
fn assert_sweep_is_populated(s: &Sweep) {
    println!(
        "  swept {} (theme, zoom) pairs, {} with strokes, {} stroke points",
        s.pairs, s.pairs_with_strokes, s.points
    );
    assert!(
        s.points > 10_000,
        "the sweep saw only {} stroke points across {} (theme, zoom) pairs, \
         so a refusal count over it says nothing",
        s.points,
        s.pairs
    );
    assert!(
        s.pairs_with_strokes * 2 >= s.pairs,
        "only {} of {} (theme, zoom) pairs produced any stroke at all, so the \
         count is mostly the absence of strokes rather than a reading of them",
        s.pairs_with_strokes,
        s.pairs
    );
}

/// **No stroke of either committed style falls back to the CPU**, at any zoom
/// `mvt::styled` can be asked for.
///
/// Every reason [`stroke::append`] can refuse a path is either impossible over
/// an MVT tile — a non-integer coordinate, one outside `i16`, a closed or
/// filled path — or was the hairline branch, which now has its own arm. So the
/// gate is a flat zero rather than a share, and a non-zero here is a defect in
/// this code and not a property of the styles.
///
/// **The non-vacuity conjunct is the hairline count.** A zero refusal count
/// would also be what a sweep that never reached a hairline produced, and that
/// sweep would say nothing about the arm this test exists to cover — so the
/// hairline share is asserted positive beside the refusal zero. It was
/// **11.4% of stroke points** when the arm landed, which is the figure that
/// justified building it.
///
/// Swept at [`PIXELS_PER_POINT`] 1, the **largest** feathering any
/// `pixels_per_point >= 1` produces and so the case where the hairline branch
/// is easiest to reach.
#[test]
fn no_stroke_of_either_committed_style_falls_back_to_the_cpu() {
    let Some(parsed) = monaco_parsed() else {
        return;
    };
    let feathering = feathering();
    let swept = sweep(&parsed, feathering);
    assert_sweep_is_populated(&swept);

    let share = 100.0 * swept.hairline as f64 / swept.points as f64;
    println!(
        "  at feathering {feathering} (pixels_per_point {PIXELS_PER_POINT}): \
         {} of {} stroke points refused; {} on epaint's hairline branch \
         ({share:.1}% of all stroke points)",
        swept.refused, swept.points, swept.hairline
    );

    assert!(
        swept.hairline > 0,
        "the sweep never reached epaint's hairline branch, so the zero below \
         is a sweep of thick strokes only and says nothing about the arm that \
         carries the other {share:.1}%"
    );
    assert_eq!(
        swept.refused, 0,
        "{} of {} stroke points still fall back to the CPU. Every remaining \
         refusal reason is impossible over an MVT tile, so this is a defect \
         in tile_mesh::stroke. Worst pair: {:?}",
        swept.refused, swept.points, swept.worst
    );
}

/// **Where the hairline branch begins**, derived analytically and confirmed by
/// the sweep.
///
/// epaint's hairline branch fires at `width <= 0.9 * feathering`, and
/// feathering is `feathering_size_in_pixels / pixels_per_point`. So no width
/// is a hairline once `feathering <= min_width`, and the `pixels_per_point`
/// that gives that is `feathering_size_in_pixels / min_width`. Above it every
/// stroke takes the thick branch; below it some take the ridge.
///
/// **Two spellings that must agree**, and this is the whole point of the
/// test: the boundary is derived from the minimum evaluated width, and then
/// the sweep is re-run on both sides of it. An arithmetic slip in the
/// derivation shows up as a count that disagrees rather than as a plausible
/// number in a log. It is not a claim that either side is better — both are
/// drawn now — only that this code and epaint agree about which is which.
///
/// # The population the minimum is over, exactly
///
/// **Evaluated widths, not declared literals.** The widths come off shapes
/// `walkers::mvt::styled` produced, and `mvt::render_line` computes each one
/// as `width.evaluate(context)` with the zoom in the context — so an
/// `["interpolate", ["linear"], ["zoom"], …]` width is evaluated here, at
/// every zoom, rather than read as its source text. This matters more than it
/// looks: **55 of the 56 line layers in each committed style are expressions**
/// and only `boundary_country_outline` is a literal, so a test that read
/// source text would be reading one layer in fifty-six.
#[test]
fn the_hairline_branch_begins_where_the_arithmetic_says_it_does() {
    let Some(parsed) = monaco_parsed() else {
        return;
    };
    let feathering_size_in_pixels =
        egui::epaint::TessellationOptions::default().feathering_size_in_pixels;

    let mut thinnest = f32::INFINITY;
    let mut at = (String::new(), 0u8);
    let mut seen = 0usize;
    for (theme, dark) in [("dark", true), ("light", false)] {
        let style = crate::basemap_style::committed(dark);
        for zoom in 0..=TILE.0 {
            for shape in walkers::mvt::styled(&parsed, &style, zoom) {
                let ShapeOrText::Shape(egui::Shape::Path(path)) = shape else {
                    continue;
                };
                seen += 1;
                // A zero-width stroke draws nothing in epaint either way, so
                // it is not the thinnest *drawn* line and would drag this to
                // zero.
                if path.stroke.width > 0.0 && path.stroke.width < thinnest {
                    thinnest = path.stroke.width;
                    at = (theme.to_owned(), zoom);
                }
            }
        }
    }

    assert!(
        seen > 1_000,
        "only {seen} stroked paths across the sweep, so the minimum below is \
         not a minimum over anything"
    );
    // **Without this the derivation divides by infinity and the sweep below
    // runs at feathering 0, where `is_thick_open_stroke` refuses everything
    // for a different reason entirely.**
    assert!(
        thinnest.is_finite() && thinnest > 0.0,
        "no stroked path in the sweep had a positive width, so there is no \
         thinnest line to derive a threshold from"
    );

    let threshold = feathering_size_in_pixels / thinnest;
    println!(
        "  thinnest evaluated line-width: {thinnest} points ({} z{}); every \
         stroke is thick at pixels_per_point >= {threshold}",
        at.0, at.1
    );

    // Above the boundary. At exactly this feathering every width satisfies
    // `width > 0.9 * feathering`, since the smallest is `feathering` itself.
    let above = sweep(&parsed, thinnest);
    assert_sweep_is_populated(&above);
    assert_eq!(
        above.hairline, 0,
        "the derivation says pixels_per_point {threshold} puts every stroke on \
         the thick branch, but sweeping at that feathering found {} hairline \
         stroke points. The arithmetic and the predicate disagree",
        above.hairline
    );

    // Below it. Without this the equality above is satisfied by a feathering
    // so small that nothing is a hairline anywhere, which proves no boundary.
    let below = sweep(&parsed, feathering());
    assert!(
        below.hairline > 0,
        "no width is a hairline even at feathering {}, so there is no \
         boundary here to have located",
        feathering()
    );
    assert_eq!(
        below.refused,
        0,
        "{} stroke points fell back at feathering {}",
        below.refused,
        feathering()
    );
}
