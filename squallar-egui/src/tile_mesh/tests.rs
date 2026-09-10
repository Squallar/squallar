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
        flat.with_fill_bytes(|vertices, _| vertices.len() as u64)
            .expect("nothing has taken the fill buffers yet"),
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

/// **The fill bytes leave the host at the upload, and a tile that has given
/// them up stops offering its fill runs to a painter that never saw them.**
///
/// The take is the whole memory cut — a flattened fill pair was a second host
/// copy of a device allocation, held for the life of every cached tile — and
/// the epoch is the only thing standing between it and a hole. A store is
/// built with the painter that reaches it (`App::install_volume_bridge`), so a
/// surface lost and rebuilt puts a *new* store over a tile cache that
/// survived; that store cannot make an already-taken tile's fills resident and
/// its draw skips the run rather than falling back. Nothing else in the tree
/// notices: the tile draws, the runs are there, and the fills are simply
/// missing from the picture.
///
/// Three states, because only the middle one is obvious: bytes still here,
/// bytes taken by the painter now installed, bytes taken by an earlier one.
#[test]
fn a_tile_that_gave_its_fills_to_an_earlier_store_draws_them_on_the_cpu() {
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(egui::pos2(0.0, 0.0), egui::Color32::RED);
    mesh.colored_vertex(egui::pos2(64.0, 0.0), egui::Color32::RED);
    mesh.colored_vertex(egui::pos2(0.0, 64.0), egui::Color32::RED);
    mesh.add_triangle(0, 1, 2);
    let flat = flatten(
        &[ShapeOrText::Shape(egui::Shape::Mesh(mesh.into()))],
        NO_FEATHERING,
    );

    assert!(
        flat.fill_bytes_len() > 0,
        "the fixture flattened no fills, so nothing below is about fills"
    );
    assert!(
        flat.fill_runs_drawable(),
        "a tile still holding its fill bytes must let the store upload them"
    );

    let taken = flat.take_fill_bytes().expect("the fill bytes are here");
    assert_eq!(
        taken.vertices.len() as u64 + taken.indices.len() as u64,
        flat.fill_bytes_len(),
        "the take handed over something other than the fill pair it was priced at"
    );
    assert!(
        flat.take_fill_bytes().is_none(),
        "the take is one-way: a second store must not be told it has bytes to upload"
    );
    assert_eq!(
        flat.host_bytes(),
        flat.resident_host_bytes(),
        "the host is still holding the fill pair after the take"
    );
    assert!(
        flat.fill_runs_drawable(),
        "the store that took the bytes cannot draw what it uploaded"
    );

    // The rebuilt surface. The bytes are in a store that no longer exists.
    super::note_painter_installed();
    assert!(
        !flat.fill_runs_drawable(),
        "a painter that never saw these bytes was offered the run anyway, and its \
         store draws nothing for a run it has no buffers for"
    );
}

/// A label anchored `at` in extent units.
fn label_at(at: (f32, f32), name: &str) -> ShapeOrText {
    ShapeOrText::Text(walkers::Text::new(
        egui::pos2(at.0, at.1),
        name.to_owned(),
        12.0,
        egui::Color32::WHITE,
        0.0,
    ))
}

/// The `Place` steps of `shapes`' plan, as shape indices.
fn placed_indices(shapes: &[ShapeOrText]) -> Vec<u32> {
    let flat = flatten(shapes, FEATHERING);
    flat.plan()
        .expect("`flatten` builds a plan")
        .steps()
        .iter()
        .filter_map(|step| match step {
            PlanStep::Place(index) => Some(*index),
            PlanStep::Runs { .. } => None,
        })
        .collect()
}

/// **A label no pass of this tile can draw gets no step at all.**
///
/// The frame's anchor test is the same answer every frame — it reads the
/// label's own position and the `uv` window, and a `uv` is always inside the
/// unit square — so an anchor outside the tile's own extent fails it under
/// every camera there is. `build_plan` settles those once, off the frame
/// thread, instead of the frame rediscovering it: on the native rig's scene A
/// that is 86.7% of every text step the frame walked.
///
/// The survivors keep their order and their indices, because a label's place
/// among the shapes is what decides which of two colliding names wins in
/// `solve_labels`.
#[test]
fn a_label_anchored_off_the_tile_gets_no_plan_step() {
    let shapes = vec![
        a_background(),
        label_at((EXTENT / 2.0, EXTENT / 2.0), "Monaco"),
        // West of the tile's own origin, which is what a neighbour's place
        // looks like in this tile's buffer.
        label_at((-EXTENT * 0.01, EXTENT / 2.0), "Nice"),
        label_at((EXTENT / 4.0, EXTENT * 1.01), "Menton"),
        label_at((EXTENT / 4.0, EXTENT / 4.0), "Fontvieille"),
    ];

    assert_eq!(
        placed_indices(&shapes),
        vec![0, 1, 4],
        "the plan did not drop exactly the two labels anchored off the tile"
    );

    // And the guard still describes the list it was built for, so the frame
    // does not fall back to the un-planned walk over a plan that is now
    // shorter than the shapes.
    let flat = flatten(&shapes, FEATHERING);
    assert!(
        flat.plan().expect("a plan").matches(shapes.len()),
        "the shape-count guard no longer matches the list the plan was built for"
    );
}

/// **A label inside the extent keeps its step whatever the `uv` is.**
///
/// The cull is `uv`-free on purpose: a stretched ancestor draws a *window* of
/// its tile and the frame decides that window per piece. What is dropped is
/// only what no window could show, so an anchor on the tile is still the
/// frame's to answer — and so is one a hair outside it, because the margin
/// leans that way rather than the other.
#[test]
fn a_label_on_the_tile_or_a_hair_outside_it_keeps_its_step() {
    for at in [
        (0.0, 0.0),
        (EXTENT, EXTENT),
        (EXTENT * 0.999, EXTENT * 0.001),
        // Inside the one-extent-unit margin: droppable in exact arithmetic,
        // kept because the frame's own `f32` answer for it can round either
        // way.
        (-0.5, EXTENT / 2.0),
        (EXTENT + 0.5, EXTENT / 2.0),
    ] {
        let shapes = vec![a_background(), label_at(at, "Monaco")];
        assert_eq!(
            placed_indices(&shapes),
            vec![0, 1],
            "the label at {at:?} lost its step"
        );
    }
}

/// **The two facts the off-tile cull rests on**, asserted rather than
/// believed.
///
/// `place_one`'s test is `rect.contains(placement * anchor)` with `rect` the
/// tile's piece and `placement` the whole tile's. Every `rect` term cancels
/// out of it — leaving `uv.contains(anchor / extent)` — only because a piece
/// is square, and `unit.contains(..)` is a necessary condition for that only
/// because a `uv` is inside the unit square. Either fact changing is what
/// would make [`anchor_is_off_the_tile`] start dropping labels a frame would
/// have drawn, and neither is this crate's to keep.
#[test]
fn the_two_facts_the_off_tile_cull_rests_on() {
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(9.0).expect("nine is inside the zoom range");
    let projector = walkers::Projector::new(
        egui::Rect::from_min_size(egui::pos2(13.0, 29.0), egui::vec2(1920.0, 1040.0)),
        &memory,
        walkers::lat_lon(35.33, -97.28),
    );

    for (x, y, zoom) in [(0i64, 0u32, 0u8), (8529, 5974, 14), (-3, 12, 5), (60, 3, 6)] {
        let rect = projector.tile_rect_at(x, y, zoom);
        assert_eq!(
            rect.width(),
            rect.height(),
            "a piece at {x}/{y}/z{zoom} is not square, so the anchor test is not \
             independent of where the tile is drawn"
        );
    }

    let unit = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    for zoom in 0..=6u8 {
        for available in 0..=zoom {
            for (x, y) in [(0u32, 0u32), (1, 1), ((1 << zoom) - 1, (1 << zoom) - 1)] {
                let id = walkers::TileId { x, y, zoom };
                let Some((_, uv)) = walkers::interpolate_from_lower_zoom(id, available) else {
                    continue;
                };
                assert!(
                    unit.contains(uv.min) && unit.contains(uv.max) && uv.min.x <= uv.max.x,
                    "the uv window {uv:?} for {x}/{y}/z{zoom} from z{available} \
                     reaches outside the tile, so an anchor outside the extent \
                     could still be drawn"
                );
            }
        }
    }
}

/// A tessellator configured the way egui configures one for a frame, so a
/// batched run is compared against the per-tile run under the options the app
/// really tessellates at — `coarse_tessellation_culling` included, which is
/// the whole reason [`HoistedBackgrounds`] makes a cull test of its own.
fn frame_tessellator(pixels_per_point: f32) -> egui::epaint::Tessellator {
    egui::epaint::Tessellator::new(
        pixels_per_point,
        egui::epaint::TessellationOptions::default(),
        [1, 1],
        Vec::new(),
    )
}

/// Every byte a tessellated run puts in front of the renderer, in order.
fn primitive_digest(prims: &[egui::epaint::ClippedPrimitive]) -> Vec<String> {
    prims
        .iter()
        .map(|p| match &p.primitive {
            egui::epaint::Primitive::Mesh(mesh) => format!(
                "clip=({:?},{:?},{:?},{:?}) tex={:?} indices={:?} vertices={:?}",
                p.clip_rect.min.x.to_bits(),
                p.clip_rect.min.y.to_bits(),
                p.clip_rect.max.x.to_bits(),
                p.clip_rect.max.y.to_bits(),
                mesh.texture_id,
                mesh.indices,
                mesh.vertices
                    .iter()
                    .map(|v| (
                        v.pos.x.to_bits(),
                        v.pos.y.to_bits(),
                        v.uv.x.to_bits(),
                        v.uv.y.to_bits(),
                        v.color.to_array(),
                    ))
                    .collect::<Vec<_>>(),
            ),
            egui::epaint::Primitive::Callback(_) => "callback".to_owned(),
        })
        .collect()
}

/// A tile background as `mvt::render` emits it, already placed at `piece`.
fn placed_background(piece: egui::Rect, fill: egui::Color32) -> egui::epaint::RectShape {
    egui::epaint::RectShape::filled(piece, egui::CornerRadius::ZERO, fill)
}

/// **The batch draws the tiles the one-shape-per-tile spelling drew, byte for
/// byte.**
///
/// The span carries the three cases a real pass has: pieces wholly inside the
/// pane, a piece straddling its edge, and a piece off the pane entirely —
/// which `tile_span` produces at the edges of the grid and which epaint culls
/// today off the shape's own bounds. A batched mesh is bounded by the union
/// of its quads, so if [`HoistedBackgrounds`] did not make that cull itself
/// the off-pane quad's vertices would appear in the stream and this would
/// read them.
#[test]
fn a_batched_background_run_tessellates_to_the_same_bytes_as_one_shape_per_tile() {
    const PPP: f32 = 1.0;
    let clip = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, 384.0));
    let fill = egui::Color32::from_rgb(0x10, 0x20, 0x30);
    // Four pieces of a 256-point grid plus one a whole tile off the pane.
    let pieces = [
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0)),
        egui::Rect::from_min_size(egui::pos2(256.0, 0.0), egui::vec2(256.0, 256.0)),
        egui::Rect::from_min_size(egui::pos2(0.0, 256.0), egui::vec2(256.0, 256.0)),
        egui::Rect::from_min_size(egui::pos2(256.0, 256.0), egui::vec2(256.0, 256.0)),
        egui::Rect::from_min_size(egui::pos2(-512.0, -512.0), egui::vec2(256.0, 256.0)),
    ];

    let per_tile: Vec<egui::epaint::ClippedShape> = pieces
        .iter()
        .filter_map(|piece| {
            background_within(&placed_background(*piece, fill), *piece, PPP).map(|shape| {
                egui::epaint::ClippedShape {
                    clip_rect: clip,
                    shape,
                }
            })
        })
        .collect();
    assert_eq!(
        per_tile.len(),
        pieces.len(),
        "fixture: every piece must produce a background shape, including the off-pane one"
    );

    let mut batched = HoistedBackgrounds::default();
    let hoisted = pieces
        .iter()
        .filter(|piece| batched.push(&placed_background(**piece, fill), **piece, PPP, clip))
        .count();
    assert_eq!(
        hoisted,
        pieces.len(),
        "every piece is hoisted whether or not its quad survives the cull"
    );
    let batched = vec![egui::epaint::ClippedShape {
        clip_rect: clip,
        shape: batched.finish().expect("four pieces are on the pane"),
    }];

    let before = primitive_digest(&frame_tessellator(PPP).tessellate_shapes(per_tile));
    let after = primitive_digest(&frame_tessellator(PPP).tessellate_shapes(batched));
    assert!(
        !before.is_empty() && before.iter().any(|p| p.contains("vertices=[(")),
        "fixture: the per-tile run must actually emit vertices, else this compares nothing"
    );
    assert_eq!(
        before, after,
        "batching the hoisted backgrounds moved the stream the renderer sees"
    );
}

/// A slippy grid of raster cells as `draw_tile_layer`'s second walk meets
/// them: the rect the cell goes at, the atlas page it is a slot of, and the
/// window of that page its pixels occupy.
///
/// **Two pages, interleaved, and two cells wholly off the pane — one of them
/// in the MIDDLE of a run.** A viewport whose cells do not all fit one page of
/// `crate::raster_atlas` spills into a second, and nothing sorts the walk by
/// page; an off-pane cell is what `tiles::tile_span` produces at the edges of
/// the grid and what epaint drops today off the shape's own bounds.
///
/// The mid-run one is the whole of the cull case and the reason it is placed
/// there. An off-pane cell that *ends* a run gets a mesh of its own, whose
/// bounds miss the clip, so epaint drops it and the stream is unchanged
/// whether or not [`RasterQuads`] culls — a fixture with only that cell
/// cannot see the defect at all. Inside a run of on-pane cells the union
/// intersects the clip, epaint keeps the whole mesh, and the batch puts
/// vertices in the stream that were never there.
fn raster_grid() -> Vec<(egui::TextureId, egui::Rect, egui::Rect)> {
    let page_a = egui::TextureId::Managed(7);
    let page_b = egui::TextureId::Managed(9);
    // Windows of a 1806x1806 page, as `RasterTile::window_of` hands them
    // over: not the unit rect, so a batch that dropped the uv would be
    // visible here.
    let window = |col: f32, row: f32| {
        egui::Rect::from_min_size(
            egui::pos2(col * 258.0 / 1806.0, row * 258.0 / 1806.0),
            egui::vec2(256.0 / 1806.0, 256.0 / 1806.0),
        )
    };
    let at = |x: f32, y: f32| egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(256.0, 256.0));
    vec![
        // A run of two on one page.
        (page_a, at(0.0, 0.0), window(0.0, 0.0)),
        (page_a, at(256.0, 0.0), window(1.0, 0.0)),
        // Wholly off the pane, on that same page, BETWEEN two cells that are
        // on it: the cull case. It ends no run.
        (page_a, at(-1024.0, -1024.0), window(4.0, 0.0)),
        (page_a, at(256.0, 256.0), window(2.0, 0.0)),
        // One page apart in the middle of the walk, which ends that run.
        (page_b, at(0.0, 256.0), window(0.0, 0.0)),
        // Back to the first page: a third run, not a re-opening of the first.
        // Straddling the pane's right edge.
        (page_a, at(384.0, 0.0), window(3.0, 0.0)),
        // Wholly off the pane at the end of the walk, where a batch that
        // never culled would still agree with the per-cell spelling.
        (page_b, at(-512.0, -512.0), window(1.0, 1.0)),
    ]
}

/// The pane the grid above is drawn under.
const RASTER_CLIP: egui::Rect =
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(512.0, 384.0));

/// Batch [`raster_grid`] exactly as the tile walk does, handing back the
/// shapes in the order the painter was given them.
///
/// One stretch: `raster_grid` is what one uninterrupted run of raster cells
/// looks like, which is what a shipped terrain pass is.
fn batched_raster_shapes() -> Vec<egui::Shape> {
    let mut quads = RasterQuads::default();
    for (texture, rect, uv) in raster_grid() {
        quads.push(texture, rect, uv, egui::Color32::WHITE, RASTER_CLIP);
    }
    quads.finish()
}

/// The one-`Painter::image`-per-cell spelling of [`raster_grid`] -- what the
/// shipped call produced before any of this -- in the walk's own order.
fn per_cell_raster_shapes() -> Vec<egui::epaint::ClippedShape> {
    raster_grid()
        .into_iter()
        .map(|(texture, rect, uv)| egui::epaint::ClippedShape {
            clip_rect: RASTER_CLIP,
            shape: egui::Shape::image(texture, rect, uv, egui::Color32::WHITE),
        })
        .collect()
}

/// [`per_cell_raster_shapes`] **stably sorted by page**, first-seen page
/// first: the per-cell spelling of exactly the order the page grouping draws
/// its quads in.
fn per_cell_raster_shapes_in_page_order() -> Vec<egui::epaint::ClippedShape> {
    let mut pages: Vec<egui::TextureId> = Vec::new();
    for (texture, rect, _) in raster_grid() {
        // The cull is the batch's, so the reference order has to make it too:
        // a culled cell puts no page on the glass and cannot be what opens
        // one.
        if RASTER_CLIP.intersects(rect) && !pages.contains(&texture) {
            pages.push(texture);
        }
    }
    let mut out: Vec<egui::epaint::ClippedShape> = Vec::new();
    for page in pages {
        for clipped in per_cell_raster_shapes() {
            let egui::Shape::Mesh(ref mesh) = clipped.shape else {
                panic!("Shape::image is a mesh");
            };
            if mesh.texture_id == page {
                out.push(clipped.clone());
            }
        }
    }
    out
}

/// **The batch draws the cells the one-`Painter::image`-per-cell spelling
/// drew, byte for byte — texture ids included — once that spelling is put in
/// the order the grouping draws them in.**
///
/// This is the equivalence the cut asserts at the stream, and it is
/// deliberately **weaker than the byte identity the consecutive-run batch
/// held**: grouping by page reorders the primitive stream, so full-stream
/// identity is false by construction and would be the wrong thing to demand.
/// What is true, and what this holds, is that the reorder is a **permutation
/// by page that keeps walk order inside each page** -- every quad the walk
/// drew, with the vertices, uvs, colours and texture id it drew them with,
/// and no others.
///
/// **What it does not cover: the glass.** A permutation of the stream is only
/// invisible because the cells are a disjoint cover, and that argument is
/// about fragments rather than about vertices; nothing here can see it. It is
/// held by a readback through a real adapter --
/// `squallar-gpu/tests/tile_mesh_gpu.rs`,
/// `page_grouped_raster_cells_put_the_same_bytes_on_screen_as_one_image_per_cell`
/// -- which is the tool `013d2f480` established for exactly this case.
#[test]
fn a_page_grouped_raster_pass_is_the_per_cell_stream_permuted_by_page() {
    const PPP: f32 = 1.0;
    let batched: Vec<egui::epaint::ClippedShape> = batched_raster_shapes()
        .into_iter()
        .map(|shape| egui::epaint::ClippedShape {
            clip_rect: RASTER_CLIP,
            shape,
        })
        .collect();

    let walk_order =
        primitive_digest(&frame_tessellator(PPP).tessellate_shapes(per_cell_raster_shapes()));
    let page_order = primitive_digest(
        &frame_tessellator(PPP).tessellate_shapes(per_cell_raster_shapes_in_page_order()),
    );
    let after = primitive_digest(&frame_tessellator(PPP).tessellate_shapes(batched));

    assert!(
        walk_order.len() > 1 && walk_order.iter().any(|p| p.contains("vertices=[(")),
        "fixture: the per-cell run must emit more than one primitive with vertices \
         in it, else this compares nothing"
    );
    assert!(
        walk_order.iter().any(|p| p.contains("tex=Managed(7)"))
            && walk_order.iter().any(|p| p.contains("tex=Managed(9)")),
        "fixture: both pages must reach the stream, else the texture-id claim is vacuous"
    );
    // **The fixture must actually interleave**, or the reorder is the
    // identity and every assertion below is answered by a walk that never
    // needed grouping.
    assert_ne!(
        walk_order, page_order,
        "fixture: the walk order already is page order, so this test cannot \
         tell a page-grouped stream from a consecutive-run one"
    );
    assert_eq!(
        page_order, after,
        "the page grouping is not the per-cell stream permuted by page: it \
         moved a vertex, a uv, a colour or a texture id, or it reordered two \
         cells of ONE page"
    );
}

/// **One shape per page, and a culled cell puts no page on the glass.**
///
/// The permutation gate above holds the stream; this holds the quantity the
/// cut is for. [`raster_grid`] draws five of its seven cells — page A four
/// times and page B once, interleaved — which the consecutive-run batch drew
/// as three shapes and this draws as **two**: seven `Painter::add` calls
/// become two, and two is the floor, because a textured quad can only join a
/// mesh of its own texture.
#[test]
fn a_raster_pass_is_one_shape_per_page() {
    let shapes = batched_raster_shapes();
    let textures: Vec<egui::TextureId> = shapes
        .iter()
        .map(|shape| match shape {
            egui::Shape::Mesh(mesh) => mesh.texture_id,
            other => panic!("the raster arm handed the painter a {other:?}"),
        })
        .collect();
    assert_eq!(
        textures,
        vec![egui::TextureId::Managed(7), egui::TextureId::Managed(9)],
        "the pass did not hand over one shape per page, first-seen page first"
    );
    let quads: Vec<usize> = shapes
        .iter()
        .map(|shape| match shape {
            egui::Shape::Mesh(mesh) => mesh.indices.len() / 6,
            other => panic!("the raster arm handed the painter a {other:?}"),
        })
        .collect();
    assert_eq!(
        quads,
        vec![4, 1],
        "an off-pane cell was kept, or a drawn one was dropped"
    );
    assert_eq!(
        quads.iter().sum::<usize>(),
        raster_grid().len() - 2,
        "every cell but the two off-pane ones draws"
    );
}

/// **The pass counts the pages it drew on, and a culled cell is not one of
/// them.**
///
/// `raster_pages` is the floor the shape count is claimed to have fallen to,
/// so it has to be a reading rather than a restatement: [`raster_grid`] names
/// two pages and its two off-pane cells sit on both of them, so a counter
/// that counted asks rather than draws would still say two here -- which is
/// why the second half of this drives the count to ONE by culling every cell
/// of page B.
#[test]
fn the_pass_counts_the_pages_its_drawn_cells_sat_on() {
    let mut quads = RasterQuads::default();
    assert_eq!(quads.pages(), 0, "a pass that drew nothing sat on no page");
    for (texture, rect, uv) in raster_grid() {
        quads.push(texture, rect, uv, egui::Color32::WHITE, RASTER_CLIP);
    }
    assert_eq!(
        quads.pages(),
        2,
        "the pass did not count the two pages its cells sat on"
    );
    assert_eq!(
        quads.pages() as usize,
        quads.finish().len(),
        "the shapes handed over and the pages counted disagree"
    );

    // Page B's only drawn cell, culled: the page is asked for and never
    // drawn.
    let mut one_page = RasterQuads::default();
    for (texture, rect, uv) in raster_grid() {
        let rect = if texture == egui::TextureId::Managed(9) {
            egui::Rect::from_min_size(egui::pos2(-4096.0, -4096.0), rect.size())
        } else {
            rect
        };
        one_page.push(texture, rect, uv, egui::Color32::WHITE, RASTER_CLIP);
    }
    assert_eq!(
        one_page.pages(),
        1,
        "a page every cell of which was culled was counted as drawn on"
    );
}

/// **A stretch is handed over whole, and what interrupts it is not reordered
/// across.**
///
/// The reorder is only sound inside one stretch: a vector tile drawn between
/// two raster cells used to end a run for a reason, and it still ends the
/// stretch. Two stretches of the same page are two shapes, not one.
#[test]
fn a_page_drawn_in_two_stretches_is_two_shapes() {
    let page = egui::TextureId::Managed(7);
    let uv = egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV);
    let at = |x: f32| egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(64.0, 64.0));

    let mut quads = RasterQuads::default();
    quads.push(page, at(0.0), uv, egui::Color32::WHITE, RASTER_CLIP);
    let first: Vec<egui::Shape> = quads.take().collect();
    quads.push(page, at(64.0), uv, egui::Color32::WHITE, RASTER_CLIP);
    let second = quads.finish();

    assert_eq!(first.len(), 1, "the first stretch did not go over whole");
    assert_eq!(second.len(), 1, "the second stretch did not go over whole");
}
