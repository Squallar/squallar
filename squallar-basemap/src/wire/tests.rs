//! Round-trip identity for the styled-tile wire.
//!
//! **The assertion is `Debug` equality of the whole shape list**, not a
//! field-by-field walk and not a count. `ShapeOrText` derives no `PartialEq`,
//! and the vendored `mvt.rs` already pins its own emitter output as a `GOLDEN`
//! debug string for the same reason: a debug rendering names every field of
//! every variant, so a term this codec silently drops shows up as a diff rather
//! than as a test that still passes over the fields it happened to compare.

use super::*;

/// The committed z14/8529/5974 Monaco tile's `building` source-layer,
/// decompressed — the same fixture `squallar-buildings` extrudes and
/// `squallar-egui`'s allocation scratch styles. Real geometry through the real
/// tessellator, so the mesh arm is exercised by lyon's output rather than by
/// vertices this file made up.
const TILE: &[u8] =
    include_bytes!("../../../squallar-buildings/testdata/monaco-building-z14-8529-5974.mvt");

/// Encode `shapes` and read them back through a fresh cursor set.
fn round_trip(shapes: &[ShapeOrText]) -> Vec<ShapeOrText> {
    let mut head = Vec::new();
    let mut tails = Tails::default();
    encode_shapes(shapes, &mut head, &mut tails);

    let tails = tails.into_vec();
    let mut cursors = TailCursors::new(&tails).expect("four tails were written");
    let mut reader = Reader::new(&head);
    let back = decode_shapes(shapes.len(), &mut reader, &mut cursors)
        .expect("what this module encoded, it decodes");
    assert!(
        reader.at_end(),
        "the head had bytes left over after {} shapes",
        shapes.len(),
    );
    assert!(
        cursors.all_consumed(),
        "a tail had bytes the head did not describe",
    );
    back
}

/// Every shape the wire has a tag for, with the edges a fixture will not
/// reach: an empty mesh, a closed and filled path, both `Option` arms of the
/// two wrapping terms, a non-ASCII label, and a fully transparent colour.
fn synthetic_shapes() -> Vec<ShapeOrText> {
    let mesh = Mesh {
        indices: vec![0, 1, 2, 2, 1, 3],
        vertices: vec![
            Vertex {
                pos: Pos2::new(0.0, 0.0),
                uv: egui::epaint::WHITE_UV,
                color: Color32::from_rgba_premultiplied(1, 2, 3, 4),
            },
            Vertex {
                pos: Pos2::new(4096.0, -1.5),
                uv: egui::epaint::WHITE_UV,
                color: Color32::TRANSPARENT,
            },
            Vertex {
                pos: Pos2::new(-0.25, 4096.0),
                uv: egui::epaint::WHITE_UV,
                color: Color32::WHITE,
            },
            Vertex {
                pos: Pos2::new(1.0, 1.0),
                uv: egui::epaint::WHITE_UV,
                color: Color32::from_rgba_premultiplied(255, 0, 128, 255),
            },
        ],
        texture_id: TextureId::default(),
    };
    let empty = Mesh {
        indices: Vec::new(),
        vertices: Vec::new(),
        texture_id: TextureId::default(),
    };
    vec![
        ShapeOrText::Shape(Shape::rect_filled(
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(4096.0, 4096.0)),
            0.0,
            Color32::from_rgba_premultiplied(0x10, 0x20, 0x30, 0xFF),
        )),
        ShapeOrText::Shape(Shape::Mesh(mesh.into())),
        ShapeOrText::Shape(Shape::Mesh(empty.into())),
        ShapeOrText::Shape(Shape::Path(PathShape {
            points: vec![Pos2::new(10.0, 20.0), Pos2::new(100.0, 20.0)],
            closed: false,
            fill: Color32::TRANSPARENT,
            stroke: PathStroke::new(4.0, Color32::from_rgba_premultiplied(0xCC, 0, 0, 0xCC)),
        })),
        ShapeOrText::Shape(Shape::Path(PathShape {
            points: vec![
                Pos2::new(0.5, 0.5),
                Pos2::new(9.5, 0.5),
                Pos2::new(9.5, 9.5),
            ],
            closed: true,
            fill: Color32::from_rgba_premultiplied(9, 8, 7, 6),
            stroke: PathStroke::new(0.0, Color32::TRANSPARENT),
        })),
        ShapeOrText::Shape(Shape::Path(PathShape {
            points: Vec::new(),
            closed: false,
            fill: Color32::TRANSPARENT,
            stroke: PathStroke::new(1.0, Color32::WHITE),
        })),
        ShapeOrText::Text(
            Text::new(
                Pos2::new(500.0, 600.0),
                "Warsaw".to_owned(),
                12.0,
                Color32::WHITE,
                0.0,
            )
            .with_wrapping(Some(10.0), None),
        ),
        ShapeOrText::Text(
            Text::new(
                Pos2::new(-3.5, 0.0),
                "Sainte-Devote - Monako - Tokyo".to_owned(),
                14.5,
                Color32::from_rgba_premultiplied(0xCC, 0xCC, 0xCC, 0xFF),
                1.25,
            )
            .with_wrapping(None, Some(1.2)),
        ),
        ShapeOrText::Text(Text::new(
            Pos2::new(0.0, 0.0),
            String::new(),
            10.0,
            Color32::BLACK,
            0.0,
        )),
    ]
}

#[test]
fn every_tag_round_trips_to_an_identical_shape_list() {
    let shapes = synthetic_shapes();
    let back = round_trip(&shapes);
    assert_eq!(
        format!("{back:?}"),
        format!("{shapes:?}"),
        "the wire did not reproduce the shape list it was given",
    );
}

/// Non-vacuity for the test above: a list that reached no tag would round-trip
/// perfectly and prove nothing. All four must actually be present.
#[test]
fn the_synthetic_list_reaches_all_four_tags() {
    let shapes = synthetic_shapes();
    let (mut meshes, mut paths, mut rects, mut texts) = (0, 0, 0, 0);
    for shape in &shapes {
        match shape {
            ShapeOrText::Shape(Shape::Mesh(_)) => meshes += 1,
            ShapeOrText::Shape(Shape::Path(_)) => paths += 1,
            ShapeOrText::Shape(Shape::Rect(_)) => rects += 1,
            ShapeOrText::Text(_) => texts += 1,
            other => panic!("the fixture grew a shape the wire has no tag for: {other:?}"),
        }
    }
    assert!(
        meshes >= 2 && paths >= 3 && rects >= 1 && texts >= 3,
        "tag coverage: {meshes} meshes, {paths} paths, {rects} rects, {texts} texts",
    );
}

/// The real thing: a committed MVT body, through the real style and the real
/// tessellator, round-tripped.
#[test]
fn a_real_styled_tile_round_trips_to_an_identical_shape_list() {
    let parsed = walkers::mvt::parse(TILE).expect("the committed fixture parses");
    let style = crate::style::committed(true);
    let shapes = walkers::mvt::styled(&parsed, &style, 14);

    let vertices: usize = shapes
        .iter()
        .map(|s| match s {
            ShapeOrText::Shape(Shape::Mesh(m)) => m.vertices.len(),
            _ => 0,
        })
        .sum();
    assert!(
        shapes.len() >= 2 && vertices >= 200,
        "the fixture must actually reach the tessellator: {} shapes, {vertices} vertices",
        shapes.len(),
    );

    let back = round_trip(&shapes);
    assert_eq!(
        format!("{back:?}"),
        format!("{shapes:?}"),
        "the wire did not reproduce the committed tile's styling",
    );
}

/// A tail count this module did not write is refused rather than indexed.
#[test]
fn a_foreign_tail_count_is_refused() {
    for count in [0usize, 1, 3, 5] {
        let tails: Vec<Vec<u8>> = vec![Vec::new(); count];
        assert!(
            TailCursors::new(&tails).is_none(),
            "{count} tails was accepted; this wire writes exactly four",
        );
    }
    let four: Vec<Vec<u8>> = vec![Vec::new(); 4];
    assert!(TailCursors::new(&four).is_some(), "four tails is the wire");
}

/// A tag this build does not allocate is refused, not guessed at.
#[test]
fn an_unknown_tag_is_refused() {
    let head = vec![9u8];
    let tails: Vec<Vec<u8>> = vec![Vec::new(); 4];
    let mut cursors = TailCursors::new(&tails).expect("four tails");
    assert!(decode_shapes(1, &mut Reader::new(&head), &mut cursors).is_none());
}

/// A declared length longer than the tail holding it answers `None` rather
/// than reserving against it. The wire arrives on a message port from a peer
/// that may be another build.
#[test]
fn a_length_past_its_tail_is_refused() {
    let mut head = vec![TAG_MESH];
    head.extend_from_slice(&4_000_000_000u32.to_le_bytes());
    head.extend_from_slice(&0u32.to_le_bytes());
    let tails: Vec<Vec<u8>> = vec![Vec::new(); 4];
    let mut cursors = TailCursors::new(&tails).expect("four tails");
    assert!(decode_shapes(1, &mut Reader::new(&head), &mut cursors).is_none());
}

/// A shape count past the ceiling is refused before anything is reserved.
#[test]
fn a_shape_count_past_the_ceiling_is_refused() {
    let tails: Vec<Vec<u8>> = vec![Vec::new(); 4];
    let mut cursors = TailCursors::new(&tails).expect("four tails");
    assert!(decode_shapes(MAX_SHAPES_PER_TILE + 1, &mut Reader::new(&[]), &mut cursors).is_none(),);
}

/// A presence byte that is not a bool is another build's layout.
#[test]
fn a_non_bool_presence_byte_is_refused() {
    let mut head = vec![TAG_TEXT];
    for _ in 0..2 {
        head.extend_from_slice(&0f32.to_le_bytes());
    }
    head.extend_from_slice(&12f32.to_le_bytes());
    head.extend_from_slice(&0u32.to_le_bytes());
    head.extend_from_slice(&0f32.to_le_bytes());
    head.push(7);
    head.extend_from_slice(&0f32.to_le_bytes());
    let tails: Vec<Vec<u8>> = vec![Vec::new(); 4];
    let mut cursors = TailCursors::new(&tails).expect("four tails");
    assert!(decode_shapes(1, &mut Reader::new(&head), &mut cursors).is_none());
}

/// The mesh arm drops `uv` and `texture_id` because they are invariants, and
/// the encoder CHECKS them. A mesh that breaks one must not encode silently.
#[test]
#[should_panic(expected = "uv")]
fn a_mesh_vertex_off_white_uv_is_refused() {
    let mesh = Mesh {
        indices: vec![0],
        vertices: vec![Vertex {
            pos: Pos2::ZERO,
            uv: Pos2::new(0.5, 0.5),
            color: Color32::WHITE,
        }],
        texture_id: TextureId::default(),
    };
    let mut head = Vec::new();
    let mut tails = Tails::default();
    encode_shapes(
        &[ShapeOrText::Shape(Shape::Mesh(mesh.into()))],
        &mut head,
        &mut tails,
    );
}

/// The same, for the texture id.
#[test]
#[should_panic(expected = "texture")]
fn a_mesh_on_another_texture_is_refused() {
    let mesh = Mesh {
        indices: Vec::new(),
        vertices: Vec::new(),
        texture_id: TextureId::User(7),
    };
    let mut head = Vec::new();
    let mut tails = Tails::default();
    encode_shapes(
        &[ShapeOrText::Shape(Shape::Mesh(mesh.into()))],
        &mut head,
        &mut tails,
    );
}

/// A shape the wire has no tag for is a panic, never a skip: a dropped shape
/// is a blank road, and a blank road cannot be told from a road that is not
/// there.
#[test]
#[should_panic(expected = "no tag")]
fn a_shape_with_no_tag_is_refused_rather_than_skipped() {
    let mut head = Vec::new();
    let mut tails = Tails::default();
    encode_shapes(
        &[ShapeOrText::Shape(Shape::circle_filled(
            Pos2::ZERO,
            1.0,
            Color32::WHITE,
        ))],
        &mut head,
        &mut tails,
    );
}

/// A rect that is not `rect_filled(rect, 0.0, fill)` carries fields this wire
/// does not send, so it is refused rather than silently flattened.
#[test]
#[should_panic(expected = "rect_filled")]
fn a_rect_the_wire_cannot_reconstruct_is_refused() {
    let mut rect = egui::epaint::RectShape::filled(
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        0.0,
        Color32::WHITE,
    );
    rect.blur_width = 2.0;
    let mut head = Vec::new();
    let mut tails = Tails::default();
    encode_shapes(
        &[ShapeOrText::Shape(Shape::Rect(rect))],
        &mut head,
        &mut tails,
    );
}
