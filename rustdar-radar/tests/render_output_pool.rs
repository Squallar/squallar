//! A render never inherits a texel from the render before it.
//!
//! `into_output` carries the RGBA texture and the value grid from one plan-view
//! render to the next — see `POOLED_IMAGE` for why — and what makes that safe is
//! that a buffer coming out of a slot is indistinguishable from a fresh
//! allocation: zeroed everywhere for the texture, empty for the grid, and as
//! long as the raster asking for it in both cases.
//!
//! # Why this one has teeth, and where they are
//!
//! The texture's colouring pass has **no `else` arm**. A pixel whose value is
//! `NaN` and whose bits are not the range-folded sentinel is left exactly as the
//! buffer delivered it, and every real sweep leaves most of the raster that way
//! — the corners outside the disc, the gaps between radials, the whole of a
//! sweep that paints nothing at all. `vec![0u8; n]` is what used to make those
//! pixels transparent. So a pooled texture handed out unreset does not merely
//! *risk* showing the previous render, it shows it, and the `blank` renders
//! below are what see it: they claim no gate, so every one of their 4 M texels
//! comes straight from whatever the slot held.
//!
//! That is the opposite of the position `tests/render_cell_pool.rs` and
//! `tests/section_plane_pool.rs` are in. In both of those the pass that fills
//! the buffer covers every element of it, so an end-to-end assertion about a
//! pooled buffer's *contents* cannot fail however the reset is broken — and
//! both files say so about themselves.
//! The **value grid** is a different shape of claim and not a weaker one. It is
//! filled by `extend`, which writes every element it produces, so nothing this
//! file renders will ever observe a *stale value* — but `extend` **appends**,
//! and that is what gives this file teeth on the grid too. A checkout that
//! skipped the `clear` would leave every grid after the first longer than the
//! raster it describes, which the length assertions below read directly and the
//! `painted` counts read as a blank render claiming pixels: the assertion at the
//! blank render is the first to fail, not the last. So the vacuity here is
//! narrow — it covers what a *value* is, not how many there are.
//!
//! What this file cannot reach is the arithmetic:
//! `render::tests::a_checked_out_value_grid_is_empty_at_every_length` poisons the
//! slot directly at five lengths, including ones no raster side takes, and that
//! is what fails a checkout which is right for the sides production happens to
//! use and wrong in general. The two are deliberately not the same test: this one
//! is about what a render *shows*, that one about what a checkout *is*.
//!
//! # Why this is an integration test
//!
//! The claim is about **process-wide** values. Two slots hold one buffer each,
//! and which render receives them depends on which renders are running: inside
//! the library's own test binary other tests rasterize on other threads and any
//! of them can take a slot in between, so the buffer a render here gives back is
//! not reliably the one the next render here receives, and the file would pass
//! without ever exercising the case it is named for. An integration test file is
//! its own process, and this is the only test in it, so the renders below are
//! the only renders there are.
//!
//! Adding a second `#[test]` to this file would silently undo that: libtest
//! would run the two in parallel and put the interleaving back.
//!
//! `tests/render_output_slot.rs` is the sibling that needed the same solitude
//! for the other half of the claim — what the slot *is*, rather than what a
//! render shows — and is a second binary rather than a second test here for
//! exactly that reason.

use nexrad_level3::model::{RadialPacket, RadialRun};
use rustdar_radar::render::{recycle_image, recycle_values, render_level3_radial_to_image};
use rustdar_radar::types::{IMAGE_SIZE, RadarProduct};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const RADIALS: usize = 360;

/// Quarter-kilometre gates, so `bins` is four bins to the kilometre.
const SCALE_FACTOR: f32 = 4.0;

/// A side the pool has to serve that is not the base one. 1024 is what a browser
/// renders its loop frames at, so this is a production size rather than an
/// invented one.
const SMALL_SIDE: usize = 1024;

/// A full sweep of `bins` gates per radial, of which the first `painted_bins`
/// carry a value and the rest are 0.
///
/// **Every packet here declares the same `bins`, so every render is projected
/// at the same extent onto the same raster.** `plan_view_extent_km` frames a
/// raster at the range its data reaches, so packets declaring different bin
/// counts come back on different frames with each echo filling its own, and a
/// small render stops being a small *picture*. What has to differ is how much
/// of the shared frame is claimed, which is `painted_bins`. See
/// `rustdar-radar/tests/render_cell_pool.rs`, whose fixture this mirrors.
///
/// `painted_bins` of 0 leaves every gate at 0, which the rasterizer skips
/// (`<= 1`), so the packet is well-formed and renders successfully while
/// claiming no pixel at all — the sharpest probe there is of what the texture
/// arrived holding.
fn packet(bins: usize, painted_bins: usize) -> RadialPacket {
    assert!(painted_bins <= bins, "cannot paint more gates than there are");
    let radials = (0..RADIALS)
        .map(|i| RadialRun {
            start_angle: i as f32,
            angle_delta: 1.0,
            gate_values: (0..bins)
                .map(|j| {
                    if j >= painted_bins {
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

/// Render, then hand both buffers back exactly as the frontend does at their
/// death sites — which is the only reason there is anything in the slots for the
/// next call to inherit.
///
/// The value grid comes back as raw bits, which is what "byte-identical" has to
/// mean for it: the grid is `NaN` wherever nothing was painted, and `NaN != NaN`
/// would make a plain comparison of two correct results fail.
fn render_at(p: &RadialPacket, side_ceiling_px: usize) -> (Vec<u8>, Vec<u32>) {
    let out =
        render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, side_ceiling_px)
            .expect("the packet renders");
    let image = out.image;
    let bits: Vec<u32> = out.values.iter().map(|v| v.to_bits()).collect();
    recycle_image(image.clone());
    recycle_values(out.values);
    (image, bits)
}

/// [`render_at`] at the base raster side.
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
fn a_render_never_inherits_a_texel_from_the_one_before_it() {
    // Every packet declares 920 bins, so every render below is a 230 km frame
    // on the same raster; what differs is how much of it is claimed. 120
    // painted bins is a 30 km disc, and 920 is the whole paintable image — the
    // render maps that radius onto `IMAGE_SIZE / 2` pixels and no sweep can
    // claim a pixel outside it. Reaching the edge is load-bearing rather than
    // tidiness: a reset that covered only the middle of the buffer, or missed
    // the last few rows, has to fail here.
    //
    // The declared count is what holds the two on one frame. With 120 and 920
    // *declared*, each render is framed at its own reach and fills 78.5% of its
    // own raster — 3 236 167 against 3 293 133 pixels, 1.8% apart — and the
    // annulus a leak has to show up in stops existing.
    const BINS: usize = 920;
    let narrow = packet(BINS, 120);
    let wide = packet(BINS, BINS);
    let blank = packet(BINS, 0);

    // The reference: the narrow render on buffers nothing has used, because this
    // is the process's first render and both slots are empty.
    let (narrow_image, narrow_values) = render(&narrow);
    let narrow_pixels = painted(&narrow_values);

    let (wide_image, wide_values) = render(&wide);
    let wide_pixels = painted(&wide_values);

    // Neither claim below means anything unless both renders actually paint, and
    // unless the wide one really is the larger of the two.
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

    // The sharpest case, and the one an unreset texture cannot survive: a render
    // that claims no gate at all, on the buffer the *wide* render has just given
    // back. Every texel here is one the colouring pass does not write.
    let (blank_image, blank_values) = render(&blank);
    assert_eq!(
        painted(&blank_values),
        0,
        "a render that paints no gate must leave every value NaN"
    );
    assert!(
        blank_image.iter().all(|&b| b == 0),
        "a render that paints no gate must leave every texel zero; {} of {} were not, which is \
         the previous render showing through",
        blank_image.iter().filter(|&&b| b != 0).count(),
        blank_image.len()
    );

    // And the ordinary case: the same render, on the buffers a wider one has
    // just given back, is the render it was on fresh ones.
    let (again_image, again_values) = render(&narrow);
    // The count first: it is the coarsest way a leak shows up and the only one of
    // the three that prints something a reader can act on.
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

    // Now the shape half. This render needs a quarter of the bytes the last one
    // gave back, so a slot has to be fitted to the render taking it rather than
    // hand over the length it happens to be holding.
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
        "the smaller raster has to paint for the assertions below to say anything: {} pixels",
        painted(&small_values)
    );

    // Truncating a buffer keeps whatever the cut-off bytes held, so a render at
    // the smaller side has to come out as clean as one at the base side does.
    let (small_blank_image, small_blank_values) = render_at(&blank, SMALL_SIDE);
    assert_eq!(
        small_blank_values.len(),
        SMALL_SIDE * SMALL_SIDE,
        "a blank render at the smaller side got the pooled buffer's length, not its own"
    );
    assert_eq!(
        painted(&small_blank_values),
        0,
        "a render that paints no gate must leave every value NaN, at any raster side"
    );
    assert!(
        small_blank_image.iter().all(|&b| b == 0),
        "a render that paints no gate must leave every texel zero, at any raster side; {} of {} \
         were not",
        small_blank_image.iter().filter(|&&b| b != 0).count(),
        small_blank_image.len()
    );

    // And back up. The slots now hold SMALL_SIDE² pixels against the IMAGE_SIZE²
    // this asks for, so everything past the first quarter is memory the fit has
    // just grown into place — and a blank render over it is what says the grown
    // tail was zeroed rather than merely reserved.
    let (grown_blank_image, grown_blank_values) = render(&blank);
    assert_eq!(
        grown_blank_values.len(),
        IMAGE_SIZE * IMAGE_SIZE,
        "a render at the base side got the smaller pooled buffer's length, not its own"
    );
    assert!(
        grown_blank_image.iter().all(|&b| b == 0),
        "a blank render must leave every texel zero when the pooled texture had to grow for it; \
         {} of {} were not",
        grown_blank_image.iter().filter(|&&b| b != 0).count(),
        grown_blank_image.len()
    );

    // The same growth, painted, is byte-for-byte the render on fresh buffers.
    let (grown_image, grown_values) = render(&narrow);
    assert_eq!(
        painted(&grown_values),
        narrow_pixels,
        "the narrow render claimed a different number of pixels when it followed a smaller raster"
    );
    assert_eq!(
        grown_image, narrow_image,
        "the narrow render's texture changed when the pooled buffers had to grow for it"
    );
    assert_eq!(
        grown_values, narrow_values,
        "the narrow render's value grid changed when the pooled buffers had to grow for it"
    );
}
