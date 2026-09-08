//! **The radar site label must ask egui for one text size, not a slope of
//! them.**
//!
//! Every distinct `f32` size is its own set of glyphs in epaint's font atlas
//! (`GlyphCacheKey` hashes `px_scale_factor`, which is `size × ppp` scaled),
//! epaint evicts none of them, and `TextureAtlas::allocate` doubles the
//! image's height to fit whatever arrives. Nothing recycles until
//! `Fonts::begin_pass` sees the atlas over 80 % full — and `fill_ratio`'s
//! denominator is `max(height, width)`, i.e. the *width*, so on a 16384-wide
//! atlas that valve does not open until 13,107 rows, which is 838 MiB. A text
//! size that moves with a gesture therefore grows a holder with no working
//! set and no effective ceiling.
//!
//! That defect has already shipped twice from this tree. `station_model`'s
//! size was a continuous function of zoom, and a pinch doubled the atlas twice
//! in five seconds until `quarter_points` quantised it; the same growth is
//! what drove the atlas past the renderer's band cap and drew Android's place
//! names as bars. `station_model`'s own sweep is the pin on that one.
//!
//! [`super::site_label_font_size`] is the last text size in this tree derived
//! from the raw map zoom, and it is safe only by *composition*: the sloped
//! stretch of its clamp exists, and the draw gate is what keeps the caller off
//! it. Composition across two functions is exactly what no compiler checks, so
//! it is checked here.
//!
//! **Denominator.** Zoom, swept over the range the label is actually drawn at
//! — `SITE_LABEL_MIN_ZOOM` up to the map's own ceiling — at a step far finer
//! than a gesture delivers. Sizes are compared by `to_bits`, which is stricter
//! than `==` (it separates `-0.0` and every `NaN`); stricter is the safe
//! direction, since a spurious distinct size fails this test while a spurious
//! collision would hide the defect.

use super::{SITE_LABEL_MIN_ZOOM, site_icon_size, site_label_font_size};
use std::collections::BTreeSet;

/// Above the map's own zoom ceiling, so the sweep cannot stop short of a
/// stretch the user can reach.
const SWEEP_TOP: f64 = 24.0;

/// Steps across the swept range. A pinch delivers a fresh float per frame;
/// this is finer than any of them.
const STEPS: u32 = 40_000;

fn swept_zooms() -> impl Iterator<Item = f64> {
    (0..=STEPS).map(|i| {
        SITE_LABEL_MIN_ZOOM + (SWEEP_TOP - SITE_LABEL_MIN_ZOOM) * f64::from(i) / f64::from(STEPS)
    })
}

/// The sweep is over a range where something actually moves, so a pass here
/// cannot come from a sweep that never varied its input.
///
/// Without this, a `site_icon_size` that had been flattened to a constant —
/// or a `SWEEP_TOP` that had collapsed onto `SITE_LABEL_MIN_ZOOM` — would make
/// the size test below pass by saying nothing at all.
#[test]
fn the_sweep_covers_a_range_where_the_marker_geometry_really_moves() {
    let icons: BTreeSet<u32> = swept_zooms().map(|z| site_icon_size(z).to_bits()).collect();
    assert!(
        icons.len() > 1,
        "the swept zoom range met only {} distinct icon sizes, so the sweep \
         is not exercising a range and the size assertion below is vacuous",
        icons.len(),
    );
}

/// **One size, at every zoom that draws.**
///
/// The label is drawn only at or above [`SITE_LABEL_MIN_ZOOM`], and at that
/// zoom `site_icon_size` is already 20.0 — whose 0.6 is over the clamp's
/// 12.0 ceiling. So the clamp pins every drawn zoom to the same size and the
/// atlas sees one glyph set however hard the map is pinched.
///
/// A failure here does not mean a wrong label. It means the draw gate, the
/// icon ramp or the clamp moved so that the sloped stretch is now reachable,
/// and that every frame of a zoom gesture will ask epaint for a size nobody
/// has seen — the atlas growth this module's header describes.
#[test]
fn the_site_label_asks_for_one_size_at_every_zoom_it_draws() {
    let sizes: BTreeSet<u32> = swept_zooms()
        .map(|z| site_label_font_size(z).to_bits())
        .collect();
    // The first few only: a broken clamp meets thousands of sizes, and a
    // panic that dumps every one of them buries the sentence that says what
    // went wrong. Measured on the tamper for this test: 4,212 of them.
    let sample: Vec<f32> = sizes.iter().take(6).map(|&b| f32::from_bits(b)).collect();
    assert_eq!(
        sizes.len(),
        1,
        "a zoom sweep from {SITE_LABEL_MIN_ZOOM} to {SWEEP_TOP} met {} \
         distinct site-label text sizes (first few: {sample:?}); each is its \
         own glyph set in a font atlas that never evicts and doubles to fit, \
         so a pinch now grows it every frame",
        sizes.len(),
    );
}

/// The size the one glyph set is at, pinned as a literal.
///
/// Separate from the count above because the two fail for different reasons:
/// the count says the size stopped being constant, this says the constant
/// moved. A moved constant is a legible change to every site name on the
/// glass, and it costs one extra glyph set for the life of the process on any
/// build that draws both.
#[test]
fn the_one_site_label_size_is_twelve_points() {
    assert_eq!(site_label_font_size(SITE_LABEL_MIN_ZOOM), 12.0);
    assert_eq!(site_label_font_size(SWEEP_TOP), 12.0);
}
