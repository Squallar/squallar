//! A slot holds one buffer, hands it to the next render, and declines one with
//! nothing to lend.
//!
//! `POOLED_IMAGE` and `POOLED_VALUES` are one `Option` each, not a free list.
//! Three claims follow from that and none of them is about a render's output:
//!
//! 1. a buffer offered to an empty slot is the buffer the next render draws
//!    into — otherwise `recycle_image` is a `drop` with extra steps and the
//!    whole change is inert;
//! 2. a buffer offered to a *full* slot is dropped where it stands rather than
//!    displacing the one already there — otherwise the slot is a one-deep queue
//!    whose depth depends on how many renders happened to finish first;
//! 3. a buffer with no capacity is declined — otherwise a `Vec::new()` parks in
//!    the slot, reads as full to every offer after it, and lends the next render
//!    nothing.
//!
//! # Why this is an integration test, and why it is not in the other one
//!
//! The claims are about **process-wide** slots, and the library's own test
//! binary has twenty-odd rendering tests taking and filling both of them on
//! other threads. A test that asserts a slot's exact contents from in there
//! reads the wrong answer about twice in every two thousand runs — measured, not
//! feared — which is a flake that hides for a year and then cannot be
//! reproduced. An integration test file is its own process; this is the only
//! test in it, so the only offers and the only checkouts are the ones below.
//!
//! `tests/render_output_pool.rs` is its own process for the same reason and is
//! deliberately a *different* one: it is about what a render **shows**, and its
//! module doc says why a second `#[test]` beside it would silently undo its
//! isolation. This file makes the complementary claim — about what a slot
//! **is** — and needs the same solitude, so it is a second binary rather than a
//! second test.
//!
//! # How a slot is observed from outside the crate
//!
//! `POOLED_IMAGE`, `image_pool` and `checkout_image` are all private, and making
//! them `pub` to be testable would put a process-wide mutable into the public
//! API for the sake of a test. What is public is [`recycle_image`],
//! [`recycle_values`] and a render — so the slot is read through the render, by
//! **capacity**. Contents cannot do it (a checkout zeroes the texture and
//! empties the grid, which is the whole point of the sibling file) and neither
//! can length (a checkout fits both to the raster asking). Capacity is what
//! survives a checkout: `clear`/`resize` and `extend` reserve and never shrink,
//! so a buffer offered with `RASTER + MARK` worth of room comes back out of the
//! render still holding at least that much.
//!
//! The one thing that rests on the allocator rather than on the standard
//! library's guarantees is the *negative* direction: that a render which
//! allocates for itself does **not** come back with a quarter more room than it
//! asked for. `Vec::with_capacity(n)` is only documented to give *at least* `n`,
//! and `vec![0u8; n]` is `alloc_zeroed(n)`; [`MARK`] is a megabyte against a
//! four-megabyte raster so that the gap is far outside anything a real allocator
//! rounds by.

use nexrad_level3::model::{RadialPacket, RadialRun};
use rustdar_radar::render::{recycle_image, recycle_values, render_level3_radial_to_image};
use rustdar_radar::types::RadarProduct;

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const RADIALS: usize = 360;

/// Quarter-kilometre gates, so `BINS` is four bins to the kilometre.
const SCALE_FACTOR: f32 = 4.0;

/// 920 quarter-kilometre gates is 230 km, the base extent — so the raster's side
/// is the ceiling handed in and nothing else.
const BINS: usize = 920;

/// The side every render below is taken at. 1024 is what a browser renders its
/// loop frames at, so it is a production size, and it is the smallest one — four
/// buffers of a 2048² raster to prove an `Option` would be sixteen times the
/// memory for no more certainty.
const SIDE: usize = 1024;

/// Elements in the value grid, and pixels in the texture.
const PIXELS: usize = SIDE * SIDE;

/// Bytes in the texture: RGBA.
const TEXTURE_LEN: usize = PIXELS * 4;

/// The slack that makes an offered buffer recognisable when it comes back.
///
/// A megabyte of it, in both units — 1 MiB of texture bytes and 256 Ki of `f32`,
/// which is the same megabyte. Large enough that no allocator's rounding could
/// hand a self-allocated raster this much spare room, small enough that offering
/// it four times costs nothing.
const MARK: usize = 1 << 20;

/// The same in `f32` elements.
const MARK_VALUES: usize = MARK / 4;

/// A full sweep that paints, so every render below succeeds and produces a
/// raster of the full `SIDE`. What it paints does not matter here — this file
/// asks which buffer the render drew into, never what it drew.
fn packet() -> RadialPacket {
    let radials = (0..RADIALS)
        .map(|i| RadialRun {
            start_angle: i as f32,
            angle_delta: 1.0,
            gate_values: (0..BINS)
                .map(|j| {
                    let dbz =
                        20.0 + (j as f64 / 30.0).sin() * 25.0 + (i as f64 / 45.0).cos() * 15.0;
                    ((dbz * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 250) as u16
                })
                .collect(),
        })
        .collect();
    RadialPacket {
        first_range_bin: 0,
        num_range_bins: BINS as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: SCALE_FACTOR,
        is_legacy: false,
        xdr_data_scale: None,
        xdr_data_offset: None,
        radials,
    }
}

/// One render at [`SIDE`], answering with what its two buffers had room for.
///
/// Nothing is handed back here: every offer in this file is written out at the
/// site that means it, so the state each render starts from is on the page.
fn render_capacities(p: &RadialPacket) -> (usize, usize) {
    let out = render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, SIDE)
        .expect("the packet renders");
    assert_eq!(
        out.image.len(),
        TEXTURE_LEN,
        "the render did not take the side this file is written for"
    );
    assert_eq!(
        out.values.len(),
        PIXELS,
        "the render did not take the side this file is written for"
    );
    (out.image.capacity(), out.values.capacity())
}

#[test]
fn a_slot_holds_one_buffer_hands_it_over_and_declines_an_empty_one() {
    let sweep = packet();

    // The process's first render. Both slots are empty — nothing has been
    // offered yet — so this one allocates for itself, and it is here to say what
    // that looks like: a buffer fitted to the raster and no more.
    let (fresh_image, fresh_values) = render_capacities(&sweep);
    assert!(
        fresh_image < TEXTURE_LEN + MARK && fresh_values < PIXELS + MARK_VALUES,
        "a render that allocated for itself came back with {fresh_image} bytes and \
         {fresh_values} values of room against a raster of {TEXTURE_LEN} and {PIXELS} — which \
         is more slack than this file's MARK, so nothing below can tell a pooled buffer from a \
         fresh one"
    );

    // (1) An offer to an empty slot is what the next render draws into. This is
    // the claim that fails if `recycle_image` drops what it is given, or if
    // `into_output` stops asking the slot — the two mutations that would leave
    // every assertion about a render's *output* passing and the change inert.
    recycle_image(Vec::with_capacity(TEXTURE_LEN + MARK));
    recycle_values(Vec::with_capacity(PIXELS + MARK_VALUES));
    let (image, values) = render_capacities(&sweep);
    assert!(
        image >= TEXTURE_LEN + MARK,
        "the texture slot was offered a buffer with {} bytes of room and the next render came \
         back with {image}, so it did not draw into the offered buffer",
        TEXTURE_LEN + MARK
    );
    assert!(
        values >= PIXELS + MARK_VALUES,
        "the grid slot was offered a buffer with {} values of room and the next render came \
         back with {values}, so it did not fill the offered buffer",
        PIXELS + MARK_VALUES
    );

    // (2) A second offer is dropped rather than displacing the first. Both slots
    // are empty again — the render above took what was in them — so the ordinary
    // buffer goes in and the marked one must not.
    recycle_image(Vec::with_capacity(TEXTURE_LEN));
    recycle_image(Vec::with_capacity(TEXTURE_LEN + MARK));
    recycle_values(Vec::with_capacity(PIXELS));
    recycle_values(Vec::with_capacity(PIXELS + MARK_VALUES));
    let (image, values) = render_capacities(&sweep);
    assert!(
        image < TEXTURE_LEN + MARK,
        "the second texture offered displaced the first instead of being dropped: the render \
         came back with {image} bytes of room, which is the second buffer's"
    );
    assert!(
        values < PIXELS + MARK_VALUES,
        "the second grid offered displaced the first instead of being dropped: the render came \
         back with {values} values of room, which is the second buffer's"
    );

    // (3) A buffer with no capacity is declined. If it were kept, it would sit
    // in the slot lending nothing while the marked buffer behind it was dropped
    // for arriving second — so the marked one coming back out is exactly the
    // proof that the empty never took the slot.
    recycle_image(Vec::new());
    recycle_values(Vec::new());
    recycle_image(Vec::with_capacity(TEXTURE_LEN + MARK));
    recycle_values(Vec::with_capacity(PIXELS + MARK_VALUES));
    let (image, values) = render_capacities(&sweep);
    assert!(
        image >= TEXTURE_LEN + MARK,
        "a texture with no capacity took the slot: the render after it came back with {image} \
         bytes of room rather than the {} it was offered, so the empty was holding the slot \
         while the next render had to allocate anyway",
        TEXTURE_LEN + MARK
    );
    assert!(
        values >= PIXELS + MARK_VALUES,
        "a grid with no capacity took the slot: the render after it came back with {values} \
         values of room rather than the {} it was offered",
        PIXELS + MARK_VALUES
    );
}
