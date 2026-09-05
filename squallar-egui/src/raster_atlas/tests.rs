use super::*;
use egui::Color32;

/// A tile of `side` texels, every pixel a different value along the top row so
/// the gutter's duplication is checkable.
fn tile(side: usize) -> ColorImage {
    let pixels = (0..side * side)
        .map(|i| Color32::from_gray((i % 251) as u8))
        .collect();
    ColorImage::new([side, side], pixels)
}

/// The size the shipped hillshade serves, which is what the page geometry is
/// chosen against.
const HILLSHADE: usize = 256;

#[test]
fn two_tiles_of_one_size_share_one_texture() {
    let ctx = Context::default();
    let a = place(&ctx, "a", tile(HILLSHADE));
    let b = place(&ctx, "b", tile(HILLSHADE));

    assert!(a.is_shared() && b.is_shared(), "neither tile was atlased");
    assert_eq!(
        a.id(),
        b.id(),
        "two tiles the same size took two textures, which is the whole defect"
    );
    assert_ne!(a.window_of(FULL), b.window_of(FULL), "both took one slot");
}

/// The whole of a tile, as `draw_tile_layer` asks for a tile answered by
/// itself.
const FULL: Rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

#[test]
fn a_page_fits_inside_what_webgl2_guarantees() {
    let cols = slots_along(HILLSHADE).expect("a 256 tile shares");
    assert!(cols >= 2, "a page must hold at least four tiles");
    assert!(
        cols * (HILLSHADE + 2 * GUTTER) <= MAX_PAGE_SIDE,
        "the page is wider than the 2048 WebGL2 is required to support, so it \
         allocates on native and fails on the browser arm"
    );
}

#[test]
fn a_tile_too_big_to_share_keeps_a_texture_of_its_own() {
    let ctx = Context::default();
    // One texel over half the page: two of them cannot fit side by side.
    let big = place(&ctx, "big", tile(MAX_PAGE_SIDE / 2));

    assert!(
        !big.is_shared(),
        "a tile that cannot share was atlased anyway"
    );
    assert_eq!(
        big.window_of(FULL),
        FULL,
        "a tile with a texture of its own must window onto the whole of it"
    );
}

#[test]
fn the_window_is_the_tile_and_the_gutter_is_outside_it() {
    let ctx = Context::default();
    let a = place(&ctx, "a", tile(HILLSHADE));
    let cols = slots_along(HILLSHADE).expect("a 256 tile shares");
    let page = (cols * (HILLSHADE + 2 * GUTTER)) as f32;
    let window = a.window_of(FULL);

    assert!(
        (window.min.x - GUTTER as f32 / page).abs() < 1e-6
            && (window.min.y - GUTTER as f32 / page).abs() < 1e-6,
        "the first slot's window did not start one gutter in: {window:?}"
    );
    assert!(
        (window.width() - HILLSHADE as f32 / page).abs() < 1e-6,
        "the window is not the tile's own width: {window:?}"
    );
}

#[test]
fn an_ancestor_window_composes_into_the_slot() {
    let ctx = Context::default();
    let a = place(&ctx, "a", tile(HILLSHADE));
    let whole = a.window_of(FULL);
    // What `interpolate_from_lower_zoom` hands back for the south-east
    // quarter of an ancestor.
    let quarter = a.window_of(Rect::from_min_max(
        egui::pos2(0.5, 0.5),
        egui::pos2(1.0, 1.0),
    ));

    assert!(
        (quarter.min.x - whole.center().x).abs() < 1e-6
            && (quarter.min.y - whole.center().y).abs() < 1e-6,
        "a quarter window did not start at the slot's centre: {quarter:?} of {whole:?}"
    );
    assert!(
        (quarter.max.x - whole.max.x).abs() < 1e-6 && (quarter.max.y - whole.max.y).abs() < 1e-6,
        "a quarter window did not end at the slot's corner: {quarter:?} of {whole:?}"
    );
}

#[test]
fn a_dropped_tile_hands_its_slot_to_the_next_one() {
    let ctx = Context::default();
    let keep = place(&ctx, "keep", tile(HILLSHADE));
    let first = place(&ctx, "first", tile(HILLSHADE));
    let vacated = first.window_of(FULL);
    drop(first);

    let next = place(&ctx, "next", tile(HILLSHADE));
    assert_eq!(
        next.id(),
        keep.id(),
        "the page was rebuilt while still live"
    );
    assert_eq!(
        next.window_of(FULL),
        vacated,
        "the freed slot was not reused, so a page leaks a slot per evicted tile"
    );
}

#[test]
fn an_emptied_page_is_released() {
    let ctx = Context::default();
    let first = place(&ctx, "first", tile(HILLSHADE));
    let was = first.id();
    drop(first);

    let next = place(&ctx, "next", tile(HILLSHADE));
    assert_ne!(
        next.id(),
        was,
        "a page with nothing in it stayed allocated, holding its whole area \
         for a layer that has been switched off"
    );
}

#[test]
fn the_gutter_duplicates_the_edge_rather_than_clearing_it() {
    let source = ColorImage::new(
        [2, 2],
        vec![
            Color32::from_gray(1),
            Color32::from_gray(2),
            Color32::from_gray(3),
            Color32::from_gray(4),
        ],
    );
    let padded = gutter(&source);

    assert_eq!(padded.size, [4, 4]);
    let at = |x: usize, y: usize| padded.pixels[y * 4 + x];
    // Corners are the nearest real texel, not transparent: a sampler reaching
    // past the tile finds the tile's own edge, which is what ClampToEdge
    // gives a tile that owns its texture.
    assert_eq!(at(0, 0), Color32::from_gray(1), "top-left corner");
    assert_eq!(at(3, 0), Color32::from_gray(2), "top-right corner");
    assert_eq!(at(0, 3), Color32::from_gray(3), "bottom-left corner");
    assert_eq!(at(3, 3), Color32::from_gray(4), "bottom-right corner");
    // And the tile itself is unmoved, one gutter in.
    assert_eq!(at(1, 1), Color32::from_gray(1));
    assert_eq!(at(2, 2), Color32::from_gray(4));
}

/// The tile's texels land exactly on the slot, so a tile drawn at 1:1 samples
/// its own texels and nothing else.
#[test]
fn the_slot_holds_the_tile_and_the_gutter_holds_its_edge() {
    let ctx = Context::default();
    let source = tile(HILLSHADE);
    let placed = place(&ctx, "a", source.clone());
    let cols = slots_along(HILLSHADE).expect("a 256 tile shares");
    let page = cols * (HILLSHADE + 2 * GUTTER);
    let window = placed.window_of(FULL);

    // The window's corners in texels, which must be whole numbers: a window
    // landing between texels is what makes a 1:1 tile sample two of them.
    let left = window.min.x * page as f32;
    let right = window.max.x * page as f32;
    assert!(
        (left - left.round()).abs() < 1e-3 && (right - right.round()).abs() < 1e-3,
        "the slot's window is not on a texel boundary: {left} to {right}"
    );
    assert_eq!(
        (right - left).round() as usize,
        HILLSHADE,
        "the window does not cover exactly the tile"
    );
}

/// **The figure this module exists for, as a count.**
///
/// A row of raster tiles drawn the way `ui_map_overlays::draw_tile_layer`
/// draws one — one `Painter::image` per cell, under one clip rect, in
/// spatial order — is ONE `ClippedPrimitive` when the tiles share a texture
/// and one primitive EACH when they do not. The second half is not decoration:
/// it is what says this test would fail if `place` stopped sharing, and it is
/// the shape of the stream measured on the native rig before this landed (47
/// tiles, 47 primitives, 47 draws).
#[test]
fn a_row_of_raster_tiles_is_one_primitive_when_they_share_a_texture() {
    let ctx = Context::default();
    let tiles: Vec<_> = (0..TILES)
        .map(|_| place(&ctx, "t", tile(HILLSHADE)))
        .collect();
    let shared = primitives_of(&ctx, &tiles);

    let own: Vec<_> = (0..TILES)
        .map(|_| RasterTile::own(ctx.load_texture("t", tile(HILLSHADE), Default::default())))
        .collect();
    let separate = primitives_of(&ctx, &own);

    assert_eq!(
        separate, TILES,
        "a tile per texture must be a primitive per tile, or this test is \
         measuring something other than the texture split"
    );
    assert_eq!(
        shared, 1,
        "{TILES} tiles sharing one texture came out as {shared} primitives; \
         epaint merges a run only while the texture and the clip rect both \
         hold, so a page per tile buys nothing"
    );
}

/// Tiles in one row, enough that a run of them is unmistakable and few enough
/// that they all fit one page (a 256x256 page holds 49).
const TILES: usize = 12;

/// How many `ClippedPrimitive`s a row of `tiles` becomes, drawn one
/// `Painter::image` per tile under one clip rect.
fn primitives_of(ctx: &Context, tiles: &[RasterTile]) -> usize {
    let canvas = Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2((TILES * HILLSHADE) as f32, HILLSHADE as f32),
    );
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("raster_atlas_run"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );
    for (i, raster) in tiles.iter().enumerate() {
        let at = Rect::from_min_size(
            egui::pos2((i * HILLSHADE) as f32, 0.0),
            egui::vec2(HILLSHADE as f32, HILLSHADE as f32),
        );
        ui.painter()
            .image(raster.id(), at, raster.window_of(FULL), Color32::WHITE);
    }
    ctx.tessellate(ctx.end_pass().shapes, 1.0).len()
}
