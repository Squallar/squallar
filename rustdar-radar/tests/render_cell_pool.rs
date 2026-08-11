//! A render never inherits a pixel from the render before it.
//!
//! `RenderBuffers` carries its cell buffer from one render to the next — see
//! `POOLED_CELLS` for why — and the whole safety of that rests on two
//! invariants: what comes back out of the pool is `EMPTY` everywhere, and it is
//! as long as the raster asking for it, so a render handed a used buffer cannot
//! tell it from a fresh allocation.
//!
//! Break the content half and the failure is not a crash, it is a *smaller*
//! render showing the larger one's echoes in the ring it does not itself
//! paint — which is exactly the shape of bug that survives a test asserting
//! only that the output has the right size.
//!
//! The length half became a live question when a raster's side stopped being
//! one constant: `raster_side_px` answers 2048 at the floor, a device's
//! ceiling past it, and 1024 for a browser loop frame, so the buffer in the
//! slot is routinely the wrong length for the render that takes it. `checkout`
//! resizes it to fit, which makes both directions failable — a buffer
//! truncated to a smaller raster, and one grown back for a larger one, where
//! everything past the old length is a cell the pool has just put there.
//!
//! # Why this is an integration test
//!
//! The claim is about a **process-wide** value. `POOLED_CELLS` holds one
//! buffer, and which render receives it depends on which renders are running:
//! inside the library's own test binary, other tests rasterize on other threads
//! and any of them can take the slot in between, so a wide render's buffer is
//! not reliably the one the narrow render that follows it receives — the test
//! would pass without ever exercising the case it is named for. An integration
//! test file is its own process, and this is the only test in it, so the
//! renders below are the only renders there are and the buffer handed to each
//! one is the buffer the previous one gave back.
//!
//! Adding a second `#[test]` to this file would silently undo that: libtest
//! would run the two in parallel and put the interleaving back.
//!
//! # What this file does *not* check
//!
//! That the buffer is reused at all. Every assertion below passes just as well
//! against a renderer that allocates afresh every time — correctly, because the
//! property being pinned is "a render's output does not depend on what ran
//! before it", and a fresh allocation satisfies it trivially. So this file
//! cannot fail if the pool is ever removed or quietly bypassed; it only fails
//! if the pool is kept and the reset is not.
//!
//! Reuse itself is a *performance* claim and is measured out of tree — minor
//! faults per call and `mmap`/`munmap` counts under `strace`, both quoted in
//! `POOLED_CELLS`'s documentation — because an instrument that could assert it
//! from in here is a harness, and harnesses do not ship on main.

use nexrad_level3::model::{RadialPacket, RadialRun};
use rustdar_radar::render::render_level3_radial_to_image;
use rustdar_radar::types::{IMAGE_SIZE, RadarProduct};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const RADIALS: usize = 360;

/// Quarter-kilometre gates, so `bins` is four bins to the kilometre and the
/// packet's radius in km is `bins / 4`.
const SCALE_FACTOR: f32 = 4.0;

/// A full sweep of `bins` gates per radial.
///
/// `paint` false leaves every gate at 0, which the rasterizer skips (`<= 1`),
/// so the packet is well-formed and renders successfully while claiming no
/// pixel at all — the sharpest probe there is of what the buffer arrived
/// holding.
fn packet(bins: usize, paint: bool) -> RadialPacket {
    let radials = (0..RADIALS)
        .map(|i| RadialRun {
            start_angle: i as f32,
            angle_delta: 1.0,
            gate_values: (0..bins)
                .map(|j| {
                    if !paint {
                        return 0;
                    }
                    // A field that varies with both range and azimuth, so the
                    // wide packet's outer ring cannot happen to agree with the
                    // narrow one's.
                    let dbz =
                        20.0 + (j as f64 / 30.0).sin() * 25.0 + (i as f64 / 45.0).cos() * 15.0;
                    ((dbz * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 250) as u16
                })
                .collect(),
        })
        .collect();
    RadialPacket {
        first_range_bin: 0,
        num_range_bins: bins as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: SCALE_FACTOR,
        is_legacy: false,
        xdr_data_scale: None,
        xdr_data_offset: None,
        radials,
    }
}

/// A side the pool has to serve that is not the base one. 1024 is what a
/// browser renders its loop frames at, so this is a production size rather than
/// an invented one, and being *under* [`IMAGE_SIZE`] it is what `raster_side_px`
/// answers whatever ground the sweep covers.
const SMALL_SIDE: usize = 1024;

/// The RGBA texture and the value grid as raw bits, which is what
/// "byte-identical" has to mean here: the grid is `NaN` wherever nothing was
/// painted, and `NaN != NaN` would make a plain comparison of two correct
/// results fail.
fn render_at(p: &RadialPacket, side_ceiling_px: usize) -> (Vec<u8>, Vec<u32>) {
    let out =
        render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, side_ceiling_px)
            .expect("the packet renders");
    (out.image, out.values.iter().map(|v| v.to_bits()).collect())
}

/// [`render_at`] at the base raster side, which is what every assertion of
/// byte-identity in this file is stated at.
fn render(p: &RadialPacket) -> (Vec<u8>, Vec<u32>) {
    render_at(p, IMAGE_SIZE)
}

/// How many pixels the render claimed.
fn painted(values: &[u32]) -> usize {
    values
        .iter()
        .filter(|&&bits| !f32::from_bits(bits).is_nan())
        .count()
}

#[test]
fn a_render_never_inherits_a_pixel_from_the_one_before_it() {
    // 120 bins is a 30 km disc; 920 is 230 km, which is `MAX_RANGE_KM` and so
    // the **whole** paintable image — the render maps that radius onto
    // `IMAGE_SIZE / 2` pixels, and no sweep can claim a pixel outside it.
    //
    // Reaching the edge is load-bearing, not tidiness. At 600 bins the wide
    // render stopped at 150 km — 668 px of a 1024 px radius — and the 157 to
    // 230 km annulus was never painted by anything here, so a reset that
    // covered only the middle of the buffer, or that missed the last few rows,
    // passed this test while leaking on any real full-range sweep. Both of
    // those were live mutants until this number changed.
    let narrow = packet(120, true);
    let wide = packet(920, true);
    let blank = packet(920, false);

    // The reference: the narrow render on a buffer nothing has used, because
    // this is the process's first render and the pool is empty.
    let (narrow_image, narrow_values) = render(&narrow);
    let narrow_pixels = painted(&narrow_values);

    let (wide_image, wide_values) = render(&wide);
    let wide_pixels = painted(&wide_values);

    // Neither claim below means anything unless both renders actually paint,
    // and unless the wide one really is the larger of the two.
    assert!(
        narrow_pixels > 10_000,
        "the narrow packet has to paint for this test to say anything: {narrow_pixels} pixels"
    );
    assert!(
        wide_pixels > narrow_pixels * 4,
        "the wide packet has to be much larger than the narrow one: {wide_pixels} against \
         {narrow_pixels} pixels"
    );
    assert_ne!(
        wide_image, narrow_image,
        "the two packets have to render differently, or nothing below can fail"
    );

    // The case this file exists for: the same render, on the buffer the wide
    // one has just given back.
    let (again_image, again_values) = render(&narrow);
    // The count first: it is the coarsest way the leak shows up and the only
    // one of the three that prints something a reader can act on.
    assert_eq!(
        painted(&again_values),
        narrow_pixels,
        "the narrow render claimed a different number of pixels when it followed a wider one"
    );
    assert_eq!(
        again_image, narrow_image,
        "the narrow render's texture changed when it followed a wider one"
    );
    assert_eq!(
        again_values, narrow_values,
        "the narrow render's value grid changed when it followed a wider one"
    );

    // Now the shape half, which only became failable when a raster's side
    // stopped being one constant. This render needs a quarter of the cells the
    // wide one just gave back, so the pool has to fit the buffer to the render
    // rather than hand over the length it happens to be holding.
    let (small_image, small_values) = render_at(&wide, SMALL_SIDE);
    assert_eq!(
        small_values.len(),
        SMALL_SIDE * SMALL_SIDE,
        "a render at a smaller side got a value grid of the pooled buffer's length, not its own"
    );
    assert_eq!(
        small_image.len(),
        small_values.len() * 4,
        "the texture and the value grid disagree about how many pixels were rendered"
    );
    assert!(
        painted(&small_values) > 10_000,
        "the smaller raster has to paint for the two assertions below to say anything: {} pixels",
        painted(&small_values)
    );

    // Truncating a buffer keeps whatever the cut-off cells held, so a render at
    // the smaller side has to be as clean as one at the base side is.
    let (small_blank_image, small_blank_values) = render_at(&blank, SMALL_SIDE);
    assert_eq!(
        painted(&small_blank_values),
        0,
        "a render that paints no gate must leave every value NaN, at any raster side"
    );
    assert!(
        small_blank_image.iter().all(|&b| b == 0),
        "a render that paints no gate must leave every texel zero, at any raster side"
    );

    // And back up. The pool now holds SMALL_SIDE² cells against the IMAGE_SIZE²
    // this asks for, so everything past the first quarter is a cell the pool
    // has just grown into place — and the narrow disc, centred in a raster four
    // times the area, lies entirely inside that grown tail.
    let (grown_image, grown_values) = render(&narrow);
    assert_eq!(
        painted(&grown_values),
        narrow_pixels,
        "the narrow render claimed a different number of pixels when it followed a smaller raster"
    );
    assert_eq!(
        grown_image, narrow_image,
        "the narrow render's texture changed when the pooled buffer had to grow for it"
    );
    assert_eq!(
        grown_values, narrow_values,
        "the narrow render's value grid changed when the pooled buffer had to grow for it"
    );

    // And the limit case: a render that claims nothing must produce nothing,
    // not the previous render's disc.
    let (blank_image, blank_values) = render(&blank);
    assert_eq!(
        painted(&blank_values),
        0,
        "a render that paints no gate must leave every value NaN"
    );
    assert!(
        blank_image.iter().all(|&b| b == 0),
        "a render that paints no gate must leave every texel zero"
    );
}
