//! The window's own arithmetic, on inputs a wire can carry.
//!
//! [`PictureCrop`] arrives in an offloaded rasterizer's reply, so its six
//! `u32`s are decoded bytes and not values this build produced.
//! [`PictureCrop::fits`] is the **only** guard on them — its own doc says why
//! nothing downstream can be: a window reaching past the grid it names places
//! a picture off the ground it was rendered for, which is a visibly misplaced
//! overlay and not a failed render, and no counter in the tree reports it.
//!
//! **These tests are about the binary that ships.** `[profile.release]` in the
//! workspace manifest sets no `overflow-checks`, so integer overflow wraps
//! there; a debug run would have panicked on the same input, which is a
//! different outcome and not the one users get. Every far edge below is
//! computed through [`std::hint::black_box`] so the check is made at run time
//! on values the optimiser cannot fold, and so is made under both profiles.

use crate::render::jobs::{decode_overlay_out, encode_overlay_out};
use crate::render::rasterize::PictureCrop;
use std::hint::black_box;

/// A window whose right edge does not exist in a `u32`.
///
/// `x + width` is `0` when the addition wraps, and `0 <= of_width` holds for
/// every grid — so an unchecked guard answers *yes* on the single input most
/// obviously outside every grid there is.
#[test]
fn a_window_whose_right_edge_wraps_does_not_fit() {
    let crop = black_box(PictureCrop {
        x: u32::MAX,
        y: 0,
        width: 1,
        height: 1,
        of_width: 256,
        of_height: 256,
    });
    assert!(
        !crop.fits(),
        "a window starting at u32::MAX and one texel wide is outside every \
         grid a u32 can name, but fits() accepted it",
    );
}

/// The same shape one field over: `y + height`.
#[test]
fn a_window_whose_bottom_edge_wraps_does_not_fit() {
    let crop = black_box(PictureCrop {
        x: 0,
        y: u32::MAX - 3,
        width: 1,
        height: 8,
        of_width: 256,
        of_height: 256,
    });
    assert!(
        !crop.fits(),
        "a window whose bottom edge is past u32::MAX is outside every grid a \
         u32 can name, but fits() accepted it",
    );
}

/// A window that wraps *and* names a grid as large as the type allows, so no
/// comparison against `of_width`/`of_height` can save the guard.
#[test]
fn a_wrapping_window_over_the_widest_grid_does_not_fit() {
    let crop = black_box(PictureCrop {
        x: u32::MAX,
        y: u32::MAX,
        width: u32::MAX,
        height: u32::MAX,
        of_width: u32::MAX,
        of_height: u32::MAX,
    });
    assert!(!crop.fits());
}

/// The control the three above are worth nothing without: an ordinary window
/// inside its grid, and the two edges flush against it, still fit.
#[test]
fn ordinary_windows_still_fit() {
    assert!(black_box(PictureCrop::whole(200, 100)).fits());
    assert!(
        black_box(PictureCrop {
            x: 190,
            y: 90,
            width: 10,
            height: 10,
            of_width: 200,
            of_height: 100,
        })
        .fits(),
        "a window flush against the far edge of its grid is inside it",
    );
    assert!(
        black_box(PictureCrop {
            x: 4,
            y: 4,
            width: 8,
            height: 8,
            of_width: 200,
            of_height: 100,
        })
        .fits(),
    );
}

/// **The wire, end to end.** The guard is asked twice — once in `decode_crop`
/// and once at the arrival — and this is the first of the two, on a reply
/// encoded exactly as a rasterizer encodes one. A decoder that accepts this
/// hands a misplaced window to a pane.
#[test]
fn the_reply_decoder_refuses_a_window_whose_edge_wraps() {
    let rgba = [0u8; 4];
    let mut bytes = Vec::new();
    encode_overlay_out(
        &rgba,
        None,
        None,
        None,
        Some(black_box(PictureCrop {
            x: u32::MAX,
            y: 0,
            width: 1,
            height: 1,
            of_width: 1,
            of_height: 1,
        })),
        &mut bytes,
    );
    assert!(
        decode_overlay_out(&bytes).is_none(),
        "a reply carrying a window whose right edge wraps was decoded rather \
         than refused",
    );
}

/// And the control for that one: the same reply with a window that does fit
/// decodes, and decodes to the window it was given.
#[test]
fn the_reply_decoder_keeps_a_window_that_fits() {
    let crop = PictureCrop {
        x: 1,
        y: 1,
        width: 1,
        height: 1,
        of_width: 4,
        of_height: 4,
    };
    let mut bytes = Vec::new();
    encode_overlay_out(&[0u8; 4], None, None, None, Some(crop), &mut bytes);
    let (_, _, _, _, decoded) = decode_overlay_out(&bytes).expect("a window inside its grid");
    assert_eq!(decoded, Some(crop));
}

/// [`PictureCrop::bytes`] prices the window a cache holds. On the pair above
/// the product `4 * width * height` does not exist in a `u64`, and a wrapped
/// product is a *small* figure — an impossible picture priced as a cheap one.
#[test]
fn the_byte_price_of_an_impossible_window_saturates() {
    let crop = black_box(PictureCrop {
        x: 0,
        y: 0,
        width: u32::MAX,
        height: u32::MAX,
        of_width: u32::MAX,
        of_height: u32::MAX,
    });
    assert_eq!(crop.bytes(), u64::MAX);
    // Every window a dispatch can really answer is exact, which is the whole
    // point of saturating rather than clamping at some budget.
    assert_eq!(black_box(PictureCrop::whole(1920, 1080)).bytes(), 8_294_400);
}

/// [`ContentExtent::crop`] takes its far edge outward and then adds one. The
/// cast saturates a huge-but-finite extent at `u32::MAX` — `add_rect` refuses
/// only the non-finite ones — and the `+ 1` after it wrapped to `0`, which
/// `.min(width)` reads as a far edge of zero and the window collapses to a
/// single texel. That is a **clipped** overlay, which is the defect the
/// outward rounding exists to avoid, reached by wrapping.
#[test]
fn an_extent_past_the_far_end_of_u32_still_asks_for_the_whole_picture() {
    let mut extent = crate::render::rasterize::ContentExtent::new();
    extent.add_rect(
        black_box(0.0),
        black_box(0.0),
        black_box(1e30),
        black_box(1e30),
    );
    let crop = extent
        .crop(256, 128)
        .expect("a bounded extent asks for a window");
    assert_eq!(
        (crop.width, crop.height),
        (256, 128),
        "an extent covering the whole picture and more must ask for the whole \
         picture, not collapse to one texel",
    );
    assert!(crop.fits());
    assert!(crop.is_whole());
}
