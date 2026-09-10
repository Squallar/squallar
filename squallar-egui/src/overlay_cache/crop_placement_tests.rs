//! **A window is a fraction of its picture's own screen rect.**
//!
//! [`super::draw_overlay_texture`] draws a picture at the rect its
//! [`PlacedRaster`](squallar_geo::PlacedRaster) projects to, and a picture cut
//! down to a bounding box of its content
//! (`squallar_overlays::render::rasterize::PictureCrop`) is drawn at the
//! corresponding sub-rect of it. The arithmetic is three lines and every one of
//! them is an off-by-one away from an overlay in the wrong place, which is a
//! defect no counter in the tree reports: the picture is the right size, the
//! byte figures are right, and it is simply somewhere else.
//!
//! The rect is read out of the parent's rather than projected from the window's
//! own ground on purpose. Both are computable — a window's texel edges name two
//! longitudes and two Mercator Ys exactly — but [`super::geo_corner_rect`]
//! folds a rect towards the pane's turn **by its own middle**, and a window's
//! middle is not its picture's. Near the antimeridian the two can fold to
//! different turns, which puts the window a whole world away. A fraction of the
//! parent cannot fold anywhere the parent did not, and
//! [`a_window_cannot_fold_away_from_its_picture`] is that claim as a test.

use super::*;

fn tex(crop: Option<squallar_overlays::render::rasterize::PictureCrop>) -> OverlayTextureData {
    let ctx = egui::Context::default();
    OverlayTextureData {
        texture: ctx.load_texture(
            "placement",
            egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]),
            egui::TextureOptions::LINEAR,
        ),
        placed: squallar_geo::PlacedRaster::of(GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -100.0,
            max_lon: -95.0,
        }),
        data_generation: 0,
        render_zoom: 0,
        width: 200,
        height: 100,
        crop,
        radar_meta: None,
        hit_map: None,
    }
}

/// The picture's screen rect. Deliberately not square and not at the origin: a
/// placement that confused the axes or dropped the offset agrees with both.
const RECT: egui::Rect = egui::Rect {
    min: egui::pos2(40.0, 300.0),
    max: egui::pos2(440.0, 500.0),
};

#[test]
fn a_picture_with_no_window_is_drawn_where_it_always_was() {
    assert_eq!(
        super::crop_rect(RECT, &tex(None)),
        RECT,
        "a picture that cut no window moved on the glass, so every unchanged \
         overlay row has been displaced by a feature they do not use",
    );
}

#[test]
fn a_window_is_the_matching_fraction_of_its_pictures_rect() {
    // Half the width and a quarter of the height, a quarter in from the left
    // and a fifth down: 200x100 texels of picture, so the window is
    // 100x25 at (50, 20).
    let placed = super::crop_rect(
        RECT,
        &tex(Some(squallar_overlays::render::rasterize::PictureCrop {
            x: 50,
            y: 20,
            width: 100,
            height: 25,
            of_width: 200,
            of_height: 100,
        })),
    );
    // The picture is 400x200 points, so a texel is 2 points on each axis.
    assert_eq!(placed.min.x, RECT.min.x + 100.0);
    assert_eq!(placed.min.y, RECT.min.y + 40.0);
    assert_eq!(placed.width(), 200.0);
    assert_eq!(placed.height(), 50.0);
}

#[test]
fn a_window_that_is_the_whole_picture_is_the_whole_rect() {
    assert_eq!(
        super::crop_rect(
            RECT,
            &tex(Some(
                squallar_overlays::render::rasterize::PictureCrop::whole(200, 100)
            ))
        ),
        RECT,
        "a window spanning its picture — the case a rasterizer answers when its \
         content covers the viewport — must be exactly the picture's own rect, \
         or the degenerate case is a displacement rather than a no-op",
    );
}

/// **A window never leaves its picture.**
///
/// The dateline claim, stated as the property that matters rather than as a
/// projection: whatever rect the picture is placed at, every window of it is
/// inside that rect. A window projected on its own ground could fold a turn
/// away and satisfy nothing here.
#[test]
fn a_window_cannot_fold_away_from_its_picture() {
    for (x, y, w, h) in [
        (0u32, 0u32, 1u32, 1u32),
        (199, 99, 1, 1),
        (0, 0, 200, 100),
        (73, 11, 61, 47),
    ] {
        let placed = super::crop_rect(
            RECT,
            &tex(Some(squallar_overlays::render::rasterize::PictureCrop {
                x,
                y,
                width: w,
                height: h,
                of_width: 200,
                of_height: 100,
            })),
        );
        assert!(
            RECT.contains_rect(placed),
            "a {w}x{h} window at ({x}, {y}) was placed at {placed:?}, outside \
             the {RECT:?} its own picture occupies",
        );
    }
}
