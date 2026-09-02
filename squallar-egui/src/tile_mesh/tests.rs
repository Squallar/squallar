use super::*;

const EXTENT: f32 = 4096.0;

/// Feathering off, which is what every fill-only case here wants: it is the
/// value `stroke::is_open_stroke` refuses at, so a `Shape::Path` in the
/// shape list stays the CPU path's exactly as it did before strokes were
/// flattened at all.
const NO_FEATHERING: f32 = 0.0;

/// One physical pixel at `pixels_per_point` 1 — egui's default
/// `feathering_size_in_pixels`, undivided.
const FEATHERING: f32 = 1.0;

/// A mesh of `count` quads, each a distinct colour, at ascending positions.
fn mesh(count: u32) -> ShapeOrText {
    let mut mesh = egui::epaint::Mesh::default();
    for quad in 0..count {
        let at = quad as f32 * 10.0;
        mesh.add_rect_with_uv(
            egui::Rect::from_min_size(egui::pos2(at, at), egui::vec2(8.0, 8.0)),
            egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
            egui::Color32::from_rgb((quad % 255) as u8, 7, 9),
        );
    }
    ShapeOrText::Shape(egui::Shape::Mesh(mesh.into()))
}

fn a_path() -> ShapeOrText {
    ShapeOrText::Shape(egui::Shape::line(
        vec![egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)],
        egui::Stroke::new(2.0, egui::Color32::RED),
    ))
}

fn a_background() -> ShapeOrText {
    ShapeOrText::Shape(egui::Shape::rect_filled(
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(EXTENT, EXTENT)),
        0.0,
        egui::Color32::BLUE,
    ))
}

/// **A run keeps its place in the style's order.** The fills draw where the
/// style put them, between the strokes, so a callback emitted at the wrong
/// index would put a park on top of the road through it.
#[test]
fn runs_carry_the_shape_index_they_came_from() {
    let shapes = vec![a_background(), a_path(), mesh(2), a_path(), mesh(1)];
    let flat = flatten(&shapes, NO_FEATHERING);

    assert_eq!(
        flat.runs()
            .iter()
            .map(|run| run.shape_index)
            .collect::<Vec<_>>(),
        vec![2, 4],
        "the runs are not the positions of the meshes in the shape list"
    );
}

/// Index ranges are disjoint, in order, and rebased into the shared vertex
/// buffer — so a run draws with a zero base vertex over its own range.
#[test]
fn every_run_owns_a_distinct_index_range_rebased_into_one_buffer() {
    let shapes = vec![mesh(2), a_path(), mesh(3)];
    let flat = flatten(&shapes, NO_FEATHERING);

    assert_eq!(flat.runs().len(), 2);
    let first = flat.runs()[0];
    let second = flat.runs()[1];
    assert_eq!(first.first_index, 0);
    assert_eq!(first.index_count, 2 * 6, "two quads are twelve indices");
    assert_eq!(
        second.first_index, first.index_count,
        "the second run does not begin where the first ends"
    );
    assert_eq!(second.index_count, 3 * 6);

    // The rebase: the second run's smallest index must be at or past the
    // first run's vertex count, or it would draw the first mesh's geometry.
    let first_vertices = 2 * 4;
    let smallest = (second.first_index..second.first_index + second.index_count)
        .filter_map(|i| flat.index(i as usize))
        .min()
        .expect("the second run has indices");
    assert!(
        smallest >= first_vertices,
        "the second run's indices were not rebased: smallest is {smallest}, \
         the first run holds {first_vertices} vertices"
    );

    assert_eq!(flat.vertex_count(), (2 + 3) * 4);
    assert_eq!(flat.index_count(), (2 + 3) * 6);
}

/// Positions cross in **extent units**, unplaced. Placing them here would be
/// the very copy this mechanism exists to stop making.
#[test]
fn vertices_cross_in_extent_units_with_egui_s_own_packed_colour() {
    let colour = egui::Color32::from_rgba_premultiplied(1, 2, 3, 4);
    let mut mesh = egui::epaint::Mesh::default();
    mesh.add_rect_with_uv(
        egui::Rect::from_min_size(egui::pos2(2048.0, 1024.0), egui::vec2(1.0, 1.0)),
        egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
        colour,
    );
    let flat = flatten(
        &[ShapeOrText::Shape(egui::Shape::Mesh(mesh.into()))],
        NO_FEATHERING,
    );

    let first = flat.vertex(0).expect("a vertex was flattened");
    assert_eq!(first.pos, [2048.0, 1024.0], "the position was placed");
    assert_eq!(
        first.color.to_ne_bytes(),
        colour.to_array(),
        "the colour is not egui's own byte quadruple in its own order"
    );
}

/// **The texture is a constant in the shader, so a mesh that needs one is
/// refused here.** `mvt::render` emits none; this is the branch that keeps
/// that a checked property, and its non-triviality half is the identical
/// mesh with the default texture being accepted.
#[test]
fn a_mesh_that_needs_a_texture_is_left_for_the_cpu() {
    let mut textured = egui::epaint::Mesh::with_texture(egui::TextureId::User(7));
    textured.add_rect_with_uv(
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(4.0, 4.0)),
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    let refused = flatten(
        &[ShapeOrText::Shape(egui::Shape::Mesh(textured.into()))],
        NO_FEATHERING,
    );
    assert!(
        refused.is_empty(),
        "a mesh carrying a user texture was flattened into a run the shader \
         would draw untextured"
    );

    // Non-triviality: the same geometry with egui's own atlas and WHITE_UV is
    // taken, so the refusal above is the texture and not the shape.
    assert_eq!(flatten(&[mesh(1)], NO_FEATHERING).runs().len(), 1);
}

/// A mesh whose uv is not the atlas's white texel is refused for the same
/// reason: the shader multiplies by one.
#[test]
fn a_mesh_sampling_off_the_white_texel_is_left_for_the_cpu() {
    let mut sampled = egui::epaint::Mesh::default();
    sampled.add_rect_with_uv(
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(4.0, 4.0)),
        egui::Rect::from_min_max(egui::pos2(0.1, 0.1), egui::pos2(0.2, 0.2)),
        egui::Color32::WHITE,
    );
    assert!(
        flatten(
            &[ShapeOrText::Shape(egui::Shape::Mesh(sampled.into()))],
            NO_FEATHERING
        )
        .is_empty(),
        "a mesh reading real texels was flattened into a run drawn as flat colour"
    );
}

/// With feathering off, nothing but a mesh is flattened: the background rect,
/// the strokes and the labels are the CPU path's, and a run claiming one of
/// them would draw it twice.
#[test]
fn only_meshes_are_flattened() {
    let shapes = vec![
        a_background(),
        a_path(),
        ShapeOrText::Text(walkers::Text::new(
            egui::pos2(1.0, 1.0),
            "Monaco".to_owned(),
            12.0,
            egui::Color32::WHITE,
            0.0,
        )),
    ];
    let flat = flatten(&shapes, NO_FEATHERING);
    assert!(flat.is_empty());
    assert_eq!(flat.vertex_count(), 0);
    assert_eq!(flat.bytes(), 0);
}

/// **An empty mesh is not a run.** It would draw nothing and ask the renderer
/// for a zero-length buffer, which wgpu refuses rather than treating as an
/// empty draw. The non-triviality half is the same call with one quad in it.
#[test]
fn a_mesh_with_no_triangles_is_not_a_run() {
    let empty = egui::epaint::Mesh::default();
    assert!(
        flatten(
            &[ShapeOrText::Shape(egui::Shape::Mesh(empty.into()))],
            NO_FEATHERING
        )
        .is_empty(),
        "an empty mesh became a run with zero-length buffers behind it"
    );
    assert_eq!(flatten(&[mesh(1)], NO_FEATHERING).runs().len(), 1);
}

/// Identities are minted, never derived: the renderer keys residency on them,
/// and two tiles sharing a key would draw each other's geography.
#[test]
fn every_flatten_mints_its_own_identity() {
    let a = flatten(&[mesh(1)], NO_FEATHERING);
    let b = flatten(&[mesh(1)], NO_FEATHERING);
    assert_ne!(a.id(), b.id());
}

/// The bytes figure is the four buffers and nothing else — the number the
/// renderer's residency is reported by.
#[test]
fn the_byte_figure_is_the_four_buffers() {
    let flat = flatten(&[mesh(3), a_path()], FEATHERING);
    assert_eq!(
        flat.bytes(),
        u64::from(flat.vertex_count()) * TILE_VERTEX_BYTES
            + u64::from(flat.index_count()) * TILE_INDEX_BYTES
            + u64::from(flat.stroke_vertex_count()) * stroke::STROKE_VERTEX_BYTES
            + u64::from(flat.stroke_index_count()) * stroke::STROKE_INDEX_BYTES
    );
    assert_eq!(
        flat.vertex_bytes().len() as u64,
        u64::from(flat.vertex_count()) * TILE_VERTEX_BYTES
    );
    assert_eq!(
        flat.stroke_vertex_bytes().len() as u64,
        u64::from(flat.stroke_vertex_count()) * stroke::STROKE_VERTEX_BYTES
    );
    // Non-vacuity: both halves are populated here, so neither term is zero.
    assert!(flat.vertex_count() > 0 && flat.stroke_vertex_count() > 0);
}

/// **The flat buffers placed by hand are exactly what `placed` answers.**
///
/// The renderer's GPU parity gate (`squallar-gpu/tests/tile_mesh_gpu.rs`)
/// builds its CPU arm this way — it must not depend on `walkers`, so it
/// applies `scale * p + translation` to the flattened vertices itself. That
/// arm is only the CPU path if this holds, so the equivalence is pinned here,
/// in the crate that owns both halves, rather than assumed there.
#[test]
fn the_flat_buffers_placed_by_hand_are_what_placed_answers() {
    let shapes = vec![mesh(5)];
    let flat = flatten(&shapes, NO_FEATHERING);
    let rect = egui::Rect::from_min_size(egui::pos2(64.0, 128.0), egui::vec2(256.0, 256.0));
    let place = Placement::of(rect);

    let ShapeOrText::Shape(egui::Shape::Mesh(placed)) =
        shapes[0].placed(walkers::mvt::placement(rect))
    else {
        panic!("the mesh arm did not answer a mesh");
    };

    assert_eq!(placed.vertices.len(), flat.vertex_count() as usize);
    for (i, expected) in placed.vertices.iter().enumerate() {
        let flat_vertex = flat.vertex(i).expect("the vertex is in range");
        let by_hand = [
            place.scale * flat_vertex.pos[0] + place.translation[0],
            place.scale * flat_vertex.pos[1] + place.translation[1],
        ];
        assert_eq!(
            by_hand,
            [expected.pos.x, expected.pos.y],
            "vertex {i} placed by hand is not where `placed` put it"
        );
        assert_eq!(
            flat_vertex.color.to_ne_bytes(),
            expected.color.to_array(),
            "vertex {i}'s colour did not survive the flatten"
        );
        assert_eq!(expected.uv, egui::epaint::WHITE_UV);
    }
    assert_eq!(placed.indices, {
        (0..flat.index_count() as usize)
            .map(|i| flat.index(i).expect("the index is in range"))
            .collect::<Vec<_>>()
    });
}

/// [`Placement`] is read off `mvt::placement` and not re-derived, so the
/// uniform and `ShapeOrText::placed` cannot drift apart.
#[test]
fn the_placement_is_the_one_the_cpu_path_places_by() {
    let rect = egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(256.0, 256.0));
    let transform = walkers::mvt::placement(rect);
    let place = Placement::of(rect);
    assert_eq!(place.scale, transform.scaling);
    assert_eq!(
        place.translation,
        [transform.translation.x, transform.translation.y]
    );
    assert_eq!(place.scale, 256.0 / EXTENT);
}

// ---------------------------------------------------------------------------
// Stroke runs
// ---------------------------------------------------------------------------

/// A stroked line from `a` to `b`, thick enough to take epaint's thick-open
/// branch at [`FEATHERING`].
fn line(a: (f32, f32), b: (f32, f32)) -> ShapeOrText {
    ShapeOrText::Shape(egui::Shape::line(
        vec![egui::pos2(a.0, a.1), egui::pos2(b.0, b.1)],
        egui::Stroke::new(8.0, egui::Color32::RED),
    ))
}

fn a_label() -> ShapeOrText {
    ShapeOrText::Text(walkers::Text::new(
        egui::pos2(1.0, 1.0),
        "Monaco".to_owned(),
        12.0,
        egui::Color32::WHITE,
        0.0,
    ))
}

/// **Consecutive paths are one run, and a fill closes it.** A stroke run is a
/// span rather than a shape, so a tile of hundreds of roads is a handful of
/// draws — but only as far as the next thing that draws, or the run would
/// paint over the fill styled above it.
#[test]
fn consecutive_paths_are_one_run_and_a_fill_closes_it() {
    let shapes = vec![
        line((0.0, 0.0), (100.0, 100.0)),
        line((0.0, 100.0), (100.0, 0.0)),
        mesh(1),
        line((10.0, 10.0), (90.0, 90.0)),
    ];
    let flat = flatten(&shapes, FEATHERING);
    let runs: Vec<_> = flat
        .runs()
        .iter()
        .map(|run| (run.shape_index, run.shape_span, run.kind))
        .collect();
    assert_eq!(
        runs,
        vec![
            (0, 2, RunKind::Stroke),
            (2, 1, RunKind::Fill),
            (3, 1, RunKind::Stroke),
        ]
    );
}

/// **A label does not close a run.** Every `Text` is deferred to the label
/// phase and drawn above the whole ground, so nothing of it can land between
/// two of a span's paths — and closing the run on one would split a tile's
/// roads at every place name in it.
#[test]
fn a_label_between_two_paths_does_not_close_the_run() {
    let shapes = vec![
        line((0.0, 0.0), (100.0, 100.0)),
        a_label(),
        line((0.0, 100.0), (100.0, 0.0)),
    ];
    let flat = flatten(&shapes, FEATHERING);
    assert_eq!(flat.runs().len(), 1);
    assert_eq!(flat.runs()[0].shape_index, 0);
    assert_eq!(flat.runs()[0].shape_span, 3);
}

/// **The background rectangle closes a run.** It draws, unlike a label, so a
/// span reaching across it would put the roads under the background.
#[test]
fn the_background_rect_closes_a_run() {
    let shapes = vec![
        line((0.0, 0.0), (100.0, 100.0)),
        a_background(),
        line((0.0, 100.0), (100.0, 0.0)),
    ];
    let flat = flatten(&shapes, FEATHERING);
    let spans: Vec<_> = flat
        .runs()
        .iter()
        .map(|run| (run.shape_index, run.shape_span))
        .collect();
    assert_eq!(spans, vec![(0, 1), (2, 1)]);
}

/// **A fractional coordinate is refused, not rounded**, and the refusal
/// closes the run so the path draws on the CPU at its own place in the order.
///
/// MVT geometry is integer varints, so nothing in a real tile reaches this;
/// it is the branch that keeps the `i16` position exact rather than a
/// quantisation nobody measured.
#[test]
fn a_fractional_coordinate_is_refused_and_closes_the_run() {
    let shapes = vec![
        line((0.0, 0.0), (100.0, 100.0)),
        line((0.5, 0.0), (100.0, 100.0)),
        line((0.0, 100.0), (100.0, 0.0)),
    ];
    let flat = flatten(&shapes, FEATHERING);
    let spans: Vec<_> = flat
        .runs()
        .iter()
        .map(|run| (run.shape_index, run.shape_span))
        .collect();
    assert_eq!(
        spans,
        vec![(0, 1), (2, 1)],
        "the fractional path was folded into a run instead of being left to \
         the CPU, or it did not close the run it interrupted"
    );

    // The control: the same three paths on integer coordinates are one run.
    let integral = vec![
        line((0.0, 0.0), (100.0, 100.0)),
        line((1.0, 0.0), (100.0, 100.0)),
        line((0.0, 100.0), (100.0, 0.0)),
    ];
    assert_eq!(flatten(&integral, FEATHERING).runs().len(), 1);
}

/// **A hairline takes epaint's other branch, and is drawn rather than
/// refused.** A line thinner than a pixel becomes a three-edge ridge — three
/// vertices per path point instead of four, four triangles per segment
/// instead of six, and no caps — which is a different topology but the same
/// `point + normal * scalar` shape, so it pre-computes exactly as the thick
/// branch does.
///
/// Both arms are here because the *counts* are what tell them apart, and a
/// tree that quietly drew one as the other would still produce a picture.
#[test]
fn a_hairline_is_tessellated_on_epaints_ridge_branch_not_refused() {
    let two_points = vec![egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)];

    let hairline = ShapeOrText::Shape(egui::Shape::line(
        two_points.clone(),
        egui::Stroke::new(0.5 * FEATHERING, egui::Color32::RED),
    ));
    let flat = flatten(&[hairline], FEATHERING);
    assert_eq!(flat.runs().len(), 1, "a hairline is a run, not a refusal");
    // Two path points: 3 vertices each, 4 triangles for the one segment.
    assert_eq!(flat.stroke_vertex_count(), 6);
    assert_eq!(flat.stroke_index_count(), 12);

    let thick = ShapeOrText::Shape(egui::Shape::line(
        two_points,
        egui::Stroke::new(1.5 * FEATHERING, egui::Color32::RED),
    ));
    let flat = flatten(&[thick], FEATHERING);
    assert_eq!(flat.runs().len(), 1);
    // Two path points: 4 vertices each, and 18n - 6 indices.
    assert_eq!(flat.stroke_vertex_count(), 8);
    assert_eq!(flat.stroke_index_count(), 30);
}

/// Runs are in shape order, which is what lets the ground phase walk them in
/// step with the shapes and never search.
#[test]
fn runs_are_in_shape_order() {
    let shapes = vec![
        a_background(),
        line((0.0, 0.0), (100.0, 100.0)),
        mesh(2),
        line((0.0, 100.0), (100.0, 0.0)),
        line((5.0, 5.0), (95.0, 95.0)),
        mesh(1),
    ];
    let flat = flatten(&shapes, FEATHERING);
    assert!(
        flat.runs()
            .windows(2)
            .all(|pair| pair[0].shape_index + pair[0].shape_span <= pair[1].shape_index),
        "the runs {:?} are not disjoint and ascending",
        flat.runs()
    );
    assert_eq!(flat.runs().len(), 4);
}

/// **A run's indices address the run's own vertices from zero**, because they
/// are `u16` and the vertex buffer is bound at the run's first vertex — WebGL2
/// has no base-vertex draw call. So no index may reach past the run's own
/// vertex count, and the first run's first index must be zero.
#[test]
fn stroke_indices_are_rebased_onto_each_runs_own_first_vertex() {
    let shapes = vec![
        line((0.0, 0.0), (100.0, 100.0)),
        mesh(1),
        line((0.0, 100.0), (100.0, 0.0)),
    ];
    let flat = flatten(&shapes, FEATHERING);
    let strokes: Vec<_> = flat
        .runs()
        .iter()
        .filter(|run| run.kind == RunKind::Stroke)
        .collect();
    assert_eq!(strokes.len(), 2);
    assert_eq!(strokes[0].first_vertex, 0);
    assert!(
        strokes[1].first_vertex > 0,
        "the second run shares a buffer"
    );

    for run in strokes {
        // **The run's own vertex span, not a count derived from its index
        // count.** Vertices per path point is 4 on epaint's thick branch and 3
        // on its hairline one, so deriving one from the other would silently
        // assume a branch; the span is what the vertex-buffer binding offset
        // actually makes addressable either way.
        let vertices = flat
            .runs()
            .iter()
            .filter(|other| other.first_vertex > run.first_vertex)
            .map(|other| other.first_vertex)
            .min()
            .unwrap_or(flat.stroke_vertex_count())
            - run.first_vertex;
        for i in 0..run.index_count as usize {
            let index = flat
                .stroke_index(run.first_index as usize + i)
                .expect("the index is in range");
            assert!(
                u32::from(index) < vertices,
                "index {i} of the run at shape {} reads vertex {index} of \
                 {vertices}, past the end of what the binding offset makes \
                 addressable",
                run.shape_index
            );
        }
    }
}
