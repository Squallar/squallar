//! The committed tile under the committed style, styled in slices, is the
//! tile styled at once — byte for byte.
//!
//! `walkers::mvt::styled` **is** a `StyledCursor` advanced to the end, so
//! parity holds by construction; this is the check that the construction is
//! what shipped, on the fixture the browser's tile pump actually styles rather
//! than on walkers' hand-encoded one. The allowances are the ones the pump can
//! reach: one feature, a handful, the pump's own slice, and no cut at all.

use std::sync::Arc;

use walkers::Style;
use walkers::mvt::{Step, StyledCursor, parse, styled};

/// z14/8529/5974 of `squallar-egui/testdata/monaco.pmtiles`, the same tile
/// `squallar-buildings` extrudes and `tests/mvt_tessellation_scratch.rs`
/// counts allocations over.
const TILE: &[u8] =
    include_bytes!("../../squallar-buildings/testdata/monaco-building-z14-8529-5974.mvt");

/// The committed dark style, verbatim.
const DARK: &str = include_str!("../../www/styles/dark.json");

/// Debug text is the comparison because `ShapeOrText` is not `PartialEq`
/// (`Text` does not derive it) and `f32`'s `Debug` is the shortest string that
/// round-trips, so every vertex, colour, width and label is in it exactly.
fn drawn(shapes: &[walkers::ShapeOrText]) -> String {
    format!("{shapes:?}")
}

#[test]
fn the_committed_tile_styled_in_slices_is_the_tile_styled_at_once() {
    let tile = Arc::new(parse(TILE).expect("the committed fixture decodes"));
    let style = Arc::new(Style::from_json(DARK).expect("the committed dark style parses"));
    let zoom = 14;

    let whole = styled(&tile, &style, zoom);
    // Vertices, not shapes: `styled` folds each run of adjacent meshes into
    // one, so a tile of thousands of fills is a handful of shapes.
    let vertices: usize = whole
        .iter()
        .map(|shape| match shape {
            walkers::ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => mesh.vertices.len(),
            _ => 0,
        })
        .sum();
    assert!(
        vertices >= 200,
        "the fixture tessellated only {vertices} vertices; a parity over a tile that \
         barely reaches the tessellator proves nothing about one the pump would cut"
    );
    let whole = drawn(&whole);

    let mut units = 0;
    for allowance in [1usize, 7, 64, usize::MAX] {
        let mut cursor = StyledCursor::new(Arc::clone(&tile), Arc::clone(&style), zoom);
        let mut calls = 0usize;
        let shapes = loop {
            calls += 1;
            if let Step::Done(shapes) = cursor.advance(allowance) {
                break shapes;
            }
        };
        assert_eq!(
            drawn(&shapes),
            whole,
            "styled in slices of {allowance} the committed tile came out different"
        );
        assert_eq!(
            calls,
            cursor.visited() / allowance + 1,
            "slices of {allowance} took the wrong number of calls for {} units",
            cursor.visited()
        );
        units = cursor.visited();
    }

    // Non-vacuity: every finite allowance above must have cut the walk, or
    // the loop is `styled` four times over.
    assert!(
        units > 64,
        "the committed tile spans only {units} units, so a slice of 64 never stopped"
    );
}
