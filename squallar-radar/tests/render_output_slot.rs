//! A slot holds one buffer, hands it to the next render, and declines one with
//! nothing to lend.

use nexrad_level3::model::{RadialPacket, RadialRun};
use squallar_radar::render::{recycle_image, recycle_values, render_level3_radial_to_image};
use squallar_radar::types::RadarProduct;

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

    // The process's first render.
    let (fresh_image, fresh_values) = render_capacities(&sweep);

    // **A second render before the first offer, and it is load-bearing.** The
    // slots weigh an offer against the demand the session showed *before* the
    // render handing it back — so that a warm-up render, the only one of its
    // size the session will ever do, cannot leave its buffers reserved. One
    // render into a process that figure is still zero and every offer is
    // declined, which is the rule working and not the slot failing. What this
    // file is about is what a slot does with an offer it may accept, so it
    // takes the process to the point where it may.
    render_capacities(&sweep);
    assert!(
        fresh_image < TEXTURE_LEN + MARK && fresh_values < PIXELS + MARK_VALUES,
        "a render that allocated for itself came back with {fresh_image} bytes and \
         {fresh_values} values of room against a raster of {TEXTURE_LEN} and {PIXELS} — which \
         is more slack than this file's MARK, so nothing below can tell a pooled buffer from a \
         fresh one"
    );

    // (1) An offer to an empty slot is what the next render draws into.
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

    // (2) A second offer is dropped rather than displacing the first.
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

    // (3) A buffer with no capacity is declined.
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
