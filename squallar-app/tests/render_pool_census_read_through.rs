//! The heap census's `render pools` family is wired to radar's own slots.
//!
//! The family is **read through**, not published: `heap_census::census()`
//! calls `squallar_radar::render::parked_bytes()` at the instant of the read,
//! because a buffer is parked and taken between renders and a 2 s telemetry
//! tick would catch it almost never — and because the allocation-error hook
//! reads `census()` after the allocator has already refused, when no
//! publisher is going to run at all. On the measured scene the slots held
//! 827 MiB and no family named them.
//!
//! **Why an integration test.** The slots and the census are process-global,
//! and inside `squallar-app`'s lib test binary other tests rasterize on other
//! threads and park buffers of their own — the first version of this check
//! lived there and read another test's 18,874,368 B where it required an
//! empty pool. An integration test is its own process, and this is the only
//! test in it, so every figure below is one this file put there. The reply
//! half of the same commit stays in the lib tests, where the family it
//! touches has no other writer.

use squallar_egui::heap_census::census;
use squallar_radar::frame::{RasterImage, RenderedFrame};
use squallar_radar::render::{parked_bytes, recycle_image, render_level3_radial_to_image};
use squallar_radar::types::RadarProduct;

/// The side a browser draws its loop frames at — a production size, and the
/// cheapest one this build accepts.
const SIDE: usize = squallar_device_profile::constants::LOOP_IMAGE_SIZE;

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;

/// A 30 km disc of quarter-kilometre gates — enough that a render paints
/// something, few enough that [`SIDE`] is the raster's side and nothing else.
fn packet() -> nexrad_level3::model::RadialPacket {
    const BINS: usize = 120;
    const RADIALS: usize = 60;
    let radials = (0..RADIALS)
        .map(|i| nexrad_level3::model::RadialRun {
            start_angle: i as f32 * (360.0 / RADIALS as f32),
            angle_delta: 360.0 / RADIALS as f32,
            gate_values: (0..BINS)
                .map(|j| {
                    let dbz = 20.0 + (j as f64 / 30.0).sin() * 25.0;
                    ((dbz * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 250) as u16
                })
                .collect(),
        })
        .collect();
    nexrad_level3::model::RadialPacket {
        first_range_bin: 0,
        num_range_bins: BINS as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: 4.0,
        is_legacy: false,
        xdr_data_scale: None,
        xdr_data_offset: None,
        radials,
    }
}

/// One render at [`SIDE`] taken to both of its buffers' production death
/// sites — the value grid in `From<SweepRender> for RenderedFrame`, the
/// texture in `recycle_image`, which is what `render_dispatch`'s
/// `rendered_image_from` calls — answering what radar's slots hold after.
fn render_and_recycle(p: &nexrad_level3::model::RadialPacket) -> u64 {
    let out = render_level3_radial_to_image(
        p,
        RadarProduct::Reflectivity,
        LAT,
        LON,
        SCALE,
        OFFSET,
        None,
        SIDE,
    )
    .expect("the packet renders");
    let frame = RenderedFrame::from(out);
    match frame.image {
        RasterImage::Bytes(bytes) => recycle_image(bytes),
        RasterImage::Pixels(_) => {
            panic!("a renderer's own output is `Bytes`; `Pixels` exists only past a wire decode")
        }
    }
    parked_bytes() as u64
}

#[test]
fn the_census_reads_the_render_pools_radar_is_holding() {
    // Premise: nothing parked, so every figure below is this file's.
    assert_eq!(
        census().render_pool_bytes,
        0,
        "premise: this process has parked something before its first render"
    );

    // The first render of a size parks nothing — a size seen once earns no
    // carry — so the census must still read zero with a render behind it.
    let sweep = packet();
    render_and_recycle(&sweep);
    assert_eq!(
        census().render_pool_bytes,
        0,
        "the census reads {} B where radar parked nothing",
        census().render_pool_bytes
    );

    // The second parks, and the census must show it with nothing publishing
    // in between: no tick runs in this process, and there is no setter.
    let parked = render_and_recycle(&sweep);
    assert!(
        parked > 0,
        "a repeated render parked nothing, so this test cannot show a level"
    );
    assert_eq!(
        census().render_pool_bytes,
        parked,
        "the census reads {} B where radar's slots hold {parked} B — the family is not \
         wired to the pools",
        census().render_pool_bytes
    );

    // And it follows the slots back down.
    squallar_radar::render::trim_pools();
    assert_eq!(
        census().render_pool_bytes,
        0,
        "the census still reads {} B after an explicit trim emptied every slot",
        census().render_pool_bytes
    );
}
