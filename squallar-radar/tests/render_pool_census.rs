//! What the plan-view render pools **report holding** is what they hold: the
//! figure the heap census's `render pools` family is published from rises by
//! exactly a buffer's bytes when that buffer is parked through the pool's own
//! door, and falls by exactly them when a render takes it back.
//!
//! **The defect this pins is invisibility, not retention.** On the measured
//! scene the parked cell buffer was 413.5 MiB and the parked texture 206.8 MiB
//! — 620 MiB of a 1,805 MiB heap — and neither was in any census family, while
//! the staging pools a tenth their size were. `pooled_bytes` existed and
//! nothing shipped read it; it also walked three locks, which the telemetry
//! tick and the allocation-error hook may not do (`render::tests` pins that
//! half). This file pins the other half: that the lock-free figure is the
//! slots' truth at every step of a render's life.
//!
//! **Relations, not pinned byte counts.** Every expectation below is computed
//! from the buffer that moved — its `capacity`, or the pixel count of the
//! render that made it — never from a constant. The side is a test constant
//! only so the renders are cheap; nothing here would change at 7362.
//!
//! **Why one `#[test]`.** The pools and the demand behind them are
//! process-wide. A second test in this file would let the harness interleave
//! them on two threads and every reading below would be some other arm's.

use nexrad_level3::model::{RadialPacket, RadialRun};
use squallar_radar::frame::{RasterImage, RenderedFrame};
use squallar_radar::render::{
    parked_bytes, pooled_bytes, recycle_image, render_level3_radial_to_image, trim_pools,
};
use squallar_radar::types::RadarProduct;

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;

/// Quarter-kilometre gates, so `BINS` is four bins to the kilometre.
const SCALE_FACTOR: f32 = 4.0;

/// A 30 km disc: enough gates that every render paints something, few enough
/// that the raster's side is the ceiling and nothing else.
const BINS: usize = 120;
const RADIALS: usize = 60;

/// The side every render here asks for — the side a browser draws its loop
/// frames at, so a production size, and the cheapest one.
const SIDE: usize = 1024;

/// Bytes one cell costs. The one figure here that is a constant of the
/// representation rather than of the scene: a cell is a `u64`.
const CELL_BYTES: usize = 8;

fn packet() -> RadialPacket {
    let radials = (0..RADIALS)
        .map(|i| RadialRun {
            start_angle: i as f32 * (360.0 / RADIALS as f32),
            angle_delta: 360.0 / RADIALS as f32,
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

/// One render at [`SIDE`], its buffers still in hand — nothing recycled yet.
fn render(p: &RadialPacket) -> squallar_radar::render::SweepRender {
    render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, SIDE)
        .expect("the packet renders")
}

#[test]
fn the_pools_report_exactly_what_they_park_and_nothing_they_have_lent_out() {
    let sweep = packet();

    // Premise: nothing parked. `trim_pools` is the one door that empties
    // every slot, and a census that started from an unknown level could not
    // show a rise of exactly anything.
    trim_pools();
    assert_eq!(pooled_bytes(), 0, "premise: the slots are not empty");

    // (1) **A first render parks nothing and the level says so.** Its cell
    // buffer is declined by the retention rule (a size seen once earns no
    // carry), and its texture and grid are in the output, not in a slot.
    let first = render(&sweep);
    assert_eq!(
        pooled_bytes(),
        0,
        "the first render of a size left {} B on the level with no buffer parked",
        pooled_bytes()
    );
    // Dropped through no door: an allocator free, and the level must not move.
    drop(first);
    assert_eq!(
        pooled_bytes(),
        0,
        "a buffer freed without a door moved the level"
    );

    // (2) **The cell buffer's park, through its only door.** `into_output`
    // recycles the drained cells before it hands the texture back, and a size
    // seen twice is carried — so the level is now exactly one cell per pixel
    // of this render, eight bytes each, and nothing else.
    let second = render(&sweep);
    let pixels = second.values.len();
    let cells = pixels * CELL_BYTES;
    assert_eq!(
        pooled_bytes(),
        cells,
        "after a repeated render the level is {} B where the parked cell buffer is {cells} B \
         ({pixels} px at {CELL_BYTES} B)",
        pooled_bytes()
    );

    // (3) **The value grid's park, through its production door.** The grid
    // dies in `From<SweepRender> for RenderedFrame`; the level must rise by
    // exactly the grid's capacity in bytes — capacity, because that is what
    // the allocator holds whatever the grid's length.
    let grid_bytes = second.values.capacity() * std::mem::size_of::<f32>();
    let image_bytes = second.image.capacity();
    let frame = RenderedFrame::from(second);
    assert_eq!(
        pooled_bytes(),
        cells + grid_bytes,
        "parking the value grid moved the level by {} B, not by its {grid_bytes} B",
        pooled_bytes() as isize - cells as isize
    );

    // (4) **The texture's park, through its production door.** A renderer's
    // own output is always `Bytes`, and the app recycles it after copying.
    let RasterImage::Bytes(texture) = frame.image else {
        panic!("a renderer's own output is `Bytes`; `Pixels` exists only past a wire decode")
    };
    recycle_image(texture);
    assert_eq!(
        pooled_bytes(),
        cells + grid_bytes + image_bytes,
        "parking the texture moved the level by {} B, not by its {image_bytes} B",
        pooled_bytes() as isize - (cells + grid_bytes) as isize
    );

    // (5) **A declined offer leaves the level where it was.** The texture
    // slot is full; a second offer of the same shape passes the retention
    // rule and is refused by the slot, and a refused buffer is nobody's to
    // price.
    let before_decline = pooled_bytes();
    recycle_image(vec![0u8; image_bytes]);
    assert_eq!(
        pooled_bytes(),
        before_decline,
        "an offer the full slot declined moved the level"
    );

    // (6) **A take lowers the level by exactly what left.** The third render
    // takes all three buffers; while it runs they are its, and when it
    // returns the cells are back in their slot (recycled inside
    // `into_output`) while the texture and grid are in its output. So the
    // level fell by exactly the texture and the grid.
    let third = render(&sweep);
    assert_eq!(
        pooled_bytes(),
        cells,
        "with the texture and grid out with a render the level is {} B, not the {cells} B of \
         the one buffer (cells) that is parked",
        pooled_bytes()
    );

    // (7) **Every render buffer this crate parks, in one figure.** No section
    // has been cut in this process, so the section slot contributes nothing
    // and the crate-wide figure is the plan-view one.
    assert_eq!(
        parked_bytes(),
        pooled_bytes(),
        "the crate-wide parked figure disagrees with the plan-view slots in a process that \
         cut no section"
    );

    // (8) **An explicit release empties the level with the slots.**
    drop(RenderedFrame::from(third));
    assert!(pooled_bytes() > 0, "nothing parked going into the trim arm");
    trim_pools();
    assert_eq!(
        pooled_bytes(),
        0,
        "an explicit trim left {} B on the level with every slot empty",
        pooled_bytes()
    );
}
