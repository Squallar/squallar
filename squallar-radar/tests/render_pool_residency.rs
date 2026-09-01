//! What the plan-view render pools keep reserved, as a function of the sizes
//! the session actually asked for.
//!
//! **The defect this pins is retention, not sizing.** A render takes the side
//! `types::raster_side_px` gives it, and below `BASE_EXTENT_KM` that is
//! `IMAGE_SIZE` whatever the adapter offers — no render is oversized for its
//! sweep. What was wrong is what the pools *kept* afterwards. All three slots
//! are `side²`: eight bytes a pixel of cells, four of texture, four of value
//! grid. A browser on a real driver resolves the ceiling to 4096 px, so one
//! long-range sweep reserved 4096² × 16 B = 256 MiB across the three — and kept
//! it. The cell guard that was supposed to release it compared `len` against
//! four times the request, and `4096² = 4 · 2048²` exactly, so on the one
//! transition it existed for it sat on its own boundary and read "reuse";
//! `resize_with` never returns capacity either way. The other two slots had no
//! rule at all. Measured on a 34-minute web leg: the rasterization worker's
//! linear memory rose to 385.2 MiB between 11.5 s and 14.7 s and did not move
//! again for 2028 s.
//!
//! **Why 2048 → 1024 and not 4096 → 2048.** The same 4× step of the same
//! ladder — 1024 is the side a browser draws its loop frames at and 2048 is
//! `types::IMAGE_SIZE`, so both are production sides — at a quarter of the
//! bytes and a quarter of the render time. The rule under test is a function of
//! the *ratio* and of `capacity`, not of any absolute figure.
//!
//! **Why one `#[test]`.** The pools and the demand behind them are
//! process-wide. Splitting these arms would let the harness interleave them on
//! two threads and every reading below would be some other arm's.

use nexrad_level3::model::{RadialPacket, RadialRun};
use squallar_radar::frame::{RasterImage, RenderedFrame};
use squallar_radar::render::{
    pooled_bytes, recycle_image, render_level3_radial_to_image, trim_pools,
};
use squallar_radar::types::{IMAGE_SIZE, RadarProduct};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;

/// Quarter-kilometre gates, so `BINS` is four bins to the kilometre.
const SCALE_FACTOR: f32 = 4.0;

/// A 30 km disc: enough gates that every render below paints something, few
/// enough that the raster's side is the ceiling and nothing else — 30 km is
/// well under `types::BASE_EXTENT_KM`, where `raster_side_px` answers
/// `IMAGE_SIZE.min(ceiling)`.
const BINS: usize = 120;
const RADIALS: usize = 60;

/// The larger of the two production sides this file steps between.
const LARGE: usize = IMAGE_SIZE;

/// The smaller. What a browser renders its loop frames at.
const SMALL: usize = 1024;

/// Bytes one pixel of a finished render costs across all three slots: eight for
/// the cell it was claimed in, four for its RGBA texel, four for its `f32`
/// value. Every slot has a production death site, and [`render_at`] walks all
/// of them.
const SLOT_BYTES_PER_PX: usize = 8 + 4 + 4;

/// How much larger than the demand behind it a carried buffer may be. Stated
/// here rather than imported because the claim is about bytes reserved, not
/// about a constant's spelling.
const SLACK: usize = 2;

/// Renders at [`SMALL`] needed to retire every generation that saw [`LARGE`].
/// The pool's window is eight renders and two generations are live, so
/// sixteen is the bound and twenty leaves margin without leaving the claim
/// vague — the assertion after them is on bytes, not on the count.
const RENDERS_TO_RETIRE_LARGE: usize = 20;

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

/// One render at `side`, taken all the way to both of its buffers' death sites,
/// and answering with how many pixels it claimed.
///
/// **The recycling here is the production shape, not a test fixture.** The value
/// grid dies in `squallar_radar::frame`'s `impl From<SweepRender> for
/// RenderedFrame`, which every rasterizing job goes through; the texture dies in
/// `squallar_app`'s `render_dispatch::rendered_image_from` and
/// `app_fetch::handle_jump_to_live`, both on the `Bytes` arm, which is the arm a
/// renderer's own output always takes — `Pixels` exists only past a wire decode.
/// A version of this file that skipped them would exercise one slot of three and
/// read green while two thirds of the reservation stayed resident.
fn render_at(p: &RadialPacket, side: usize) -> usize {
    let out = render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, side)
        .expect("the packet renders");
    assert_eq!(
        out.image.len(),
        side * side * 4,
        "a render asked for {side}² px came back with a texture of {} bytes",
        out.image.len()
    );
    assert_eq!(
        out.values.len(),
        side * side,
        "a render asked for {side}² px came back with {} values",
        out.values.len()
    );
    let painted = out.values.iter().filter(|v| !v.is_nan()).count();
    let frame = RenderedFrame::from(out);
    match frame.image {
        RasterImage::Bytes(bytes) => recycle_image(bytes),
        RasterImage::Pixels(_) => {
            panic!("a renderer's own output is `Bytes`; `Pixels` exists only past a wire decode")
        }
    }
    painted
}

#[test]
fn the_pools_reserve_for_the_demand_the_session_showed_and_not_for_the_ceiling() {
    let sweep = packet();

    // (1) **A size seen once is not reserved for.** The warm-up render is the
    // measured defect: on the web leg it was the only render at its size for
    // the following 2028 seconds, and its reservation outlived it by all of
    // them.
    let painted = render_at(&sweep, LARGE);
    assert!(
        painted > 0,
        "the first render claimed no pixel, so nothing below is measuring a render"
    );
    assert_eq!(
        pooled_bytes(),
        0,
        "the process's first render left {} bytes reserved across the three slots; a \
         size this session has seen exactly once has shown no demand to reserve against",
        pooled_bytes()
    );

    // (2) **A size that recurs is carried.** The floor under (1): a pool that
    // released everything always would be no pool.
    render_at(&sweep, LARGE);
    assert_eq!(
        pooled_bytes(),
        LARGE * LARGE * SLOT_BYTES_PER_PX,
        "a size rendered twice left {} bytes reserved rather than the {} its three slots \
         cost, so the pools are not carrying what the session keeps asking for",
        pooled_bytes(),
        LARGE * LARGE * SLOT_BYTES_PER_PX
    );

    // (3) **The reservation follows demand down.** This is the arm that is red
    // without the fix: every render here is a quarter of `LARGE`'s pixels, and
    // the old `len`-against-four rule reads `4 · SMALL² <= 4 · SMALL²` and
    // carries the `LARGE` allocation through all of them.
    for _ in 0..RENDERS_TO_RETIRE_LARGE {
        render_at(&sweep, SMALL);
    }
    let after_small = pooled_bytes();
    assert!(
        after_small <= SMALL * SMALL * SLOT_BYTES_PER_PX * SLACK,
        "after {RENDERS_TO_RETIRE_LARGE} renders at {SMALL}² the pools still hold \
         {after_small} bytes, which is more than the {} a {SMALL}² render's demand \
         accounts for — the reservation for {LARGE}² is still resident",
        SMALL * SMALL * SLOT_BYTES_PER_PX * SLACK
    );

    // (4) **The correctness floor.** A raster larger than the pool now carries
    // must still render, at its full size, painting what it painted before.
    let painted_large_again = render_at(&sweep, LARGE);
    assert_eq!(
        painted_large_again, painted,
        "a {LARGE}² render after the pools had shrunk to {SMALL}² claimed \
         {painted_large_again} pixels where the same sweep claimed {painted} before"
    );

    // (5) **An explicit release empties every slot.** The one path that reaches
    // the case no render can: a session whose last render was the large one and
    // that then goes quiet.
    render_at(&sweep, LARGE);
    assert!(
        pooled_bytes() > 0,
        "nothing was reserved going into the release arm, so it cannot show a release"
    );
    trim_pools();
    assert_eq!(
        pooled_bytes(),
        0,
        "an explicit trim left {} bytes reserved",
        pooled_bytes()
    );
}
