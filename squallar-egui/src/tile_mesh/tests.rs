use super::*;

const EXTENT: f32 = 4096.0;

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
    let flat = flatten(&shapes);

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
    let flat = flatten(&shapes);

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
    let flat = flatten(&[ShapeOrText::Shape(egui::Shape::Mesh(mesh.into()))]);

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
    let refused = flatten(&[ShapeOrText::Shape(egui::Shape::Mesh(textured.into()))]);
    assert!(
        refused.is_empty(),
        "a mesh carrying a user texture was flattened into a run the shader \
         would draw untextured"
    );

    // Non-triviality: the same geometry with egui's own atlas and WHITE_UV is
    // taken, so the refusal above is the texture and not the shape.
    assert_eq!(flatten(&[mesh(1)]).runs().len(), 1);
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
        flatten(&[ShapeOrText::Shape(egui::Shape::Mesh(sampled.into()))]).is_empty(),
        "a mesh reading real texels was flattened into a run drawn as flat colour"
    );
}

/// Nothing but a mesh is flattened: the background rect, the strokes and the
/// labels are the CPU path's, and a run claiming one of them would draw it
/// twice.
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
    let flat = flatten(&shapes);
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
        flatten(&[ShapeOrText::Shape(egui::Shape::Mesh(empty.into()))]).is_empty(),
        "an empty mesh became a run with zero-length buffers behind it"
    );
    assert_eq!(flatten(&[mesh(1)]).runs().len(), 1);
}

/// Identities are minted, never derived: the renderer keys residency on them,
/// and two tiles sharing a key would draw each other's geography.
#[test]
fn every_flatten_mints_its_own_identity() {
    let a = flatten(&[mesh(1)]);
    let b = flatten(&[mesh(1)]);
    assert_ne!(a.id(), b.id());
}

/// The bytes figure is the two buffers and nothing else — the number the
/// renderer's residency budget is read off.
#[test]
fn the_byte_figure_is_the_two_buffers() {
    let flat = flatten(&[mesh(3)]);
    assert_eq!(
        flat.bytes(),
        u64::from(flat.vertex_count()) * TILE_VERTEX_BYTES
            + u64::from(flat.index_count()) * TILE_INDEX_BYTES
    );
    assert_eq!(
        flat.vertex_bytes().len() as u64,
        u64::from(flat.vertex_count()) * TILE_VERTEX_BYTES
    );
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
    let flat = flatten(&shapes);
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
