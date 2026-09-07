//! **A session that stops rendering gives its render buffers back.**
//!
//! `squallar_radar::render` keeps three plan-view buffers between renders and
//! decides whether to keep each on a checkout — so on a scene that has finished
//! drawing, the decision never runs again. Measured on the campaign's floor
//! legs: four renders at side 7362 off one volume arrival, in two pairs 155–167 s
//! apart, then ~250 s of quiet with the heap census reading `render pools` at
//! 867,184,704 B on 99 of its 100 ticks.
//!
//! What closes it is `squallar_app::render_pool_trim`, folded once a telemetry
//! tick. This drives the same policy against a real render and pins both
//! directions: the buffers go back when the readings are quiet, and they are
//! **kept** when they are not.
//!
//! **Why an integration test.** The slots, the render count and the policy's own
//! two readings are process-global, and `squallar-app`'s lib test binary
//! rasterizes on other threads. This is the only test in this process, so every
//! figure below is one this file put there.

use squallar_app::render_pool_trim::{QUIET_READINGS, Reading, observe_reading};
use squallar_radar::frame::{RasterImage, RenderedFrame};
use squallar_radar::render::{pooled_bytes, recycle_image, render_level3_radial_to_image};
use squallar_radar::types::RadarProduct;

/// The side a browser draws its loop frames at — a production size, and the
/// cheapest one this build accepts. The defect is about *how much* is held and
/// not about the side, so the cheapest production side proves it at the least
/// cost; the figure the campaign quotes comes from side 7362.
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

/// One render taken to both of its buffers' production death sites — the value
/// grid in `From<SweepRender> for RenderedFrame`, the texture in
/// `recycle_image`, which is what `render_dispatch::rendered_image_from` calls
/// — answering the RGBA bytes it painted.
fn render_and_recycle(p: &nexrad_level3::model::RadialPacket) -> Vec<u8> {
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
        RasterImage::Bytes(bytes) => {
            let painted = bytes.clone();
            recycle_image(bytes);
            painted
        }
        RasterImage::Pixels(_) => {
            panic!("a renderer's own output is `Bytes`; `Pixels` exists only past a wire decode")
        }
    }
}

/// Fold quiet readings in until the policy gives the buffers up, and answer how
/// many it took. Bounded, so a policy that never trims fails here rather than
/// hanging.
fn read_until_trim() -> u32 {
    for n in 1..=QUIET_READINGS * 4 {
        match observe_reading(true) {
            Reading::Trim => return n,
            Reading::Settled => panic!("reading {n} settled without ever trimming"),
            Reading::Busy | Reading::Quiet(_) => {}
        }
    }
    panic!(
        "{} quiet readings in a row and the pools were never given up; \
         `render pools` still holds {} B",
        QUIET_READINGS * 4,
        pooled_bytes()
    );
}

/// Wait, bounded, for the free lane to finish with whatever it was handed.
/// Natively `discard` rides `rd-free`, a real thread, so this is a handful of
/// milliseconds; the bound is what turns a lane that never runs into a failure
/// rather than a hang.
fn wait_for_the_free_lane() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while squallar_device_profile::discard_ledger::in_flight_bytes() > 0
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(
        squallar_device_profile::discard_ledger::in_flight_bytes(),
        0,
        "the free lane still holds the trimmed buffers ten seconds on"
    );
}

/// **One test, because everything it touches is process-global**: the three
/// slots, the render count and the policy's own two readings. Two `#[test]`s
/// in this binary would run on two threads against one set of statics, and the
/// answer would depend on which got there first. The phases are ordered so each
/// one's premise is the previous one's result.
#[test]
fn a_quiet_session_gives_its_render_pools_back_and_a_busy_one_keeps_them() {
    // Premise: nothing parked, so every figure below is this file's.
    assert_eq!(
        pooled_bytes(),
        0,
        "premise: this process parked something before its first render"
    );

    // ---- Two renders at one size, then quiet. The floor leg's shape. ----
    //
    // The first render of a size earns no carry; the second is what fills the
    // slots.
    let sweep = packet();
    let painted = render_and_recycle(&sweep);
    render_and_recycle(&sweep);
    let parked = pooled_bytes();
    assert!(
        parked > 0,
        "two renders at one size parked nothing, so this test cannot show a fall"
    );

    // The reading that adopts the new render count is busy by definition — it
    // is the one that saw the renders happen.
    assert_eq!(
        observe_reading(true),
        Reading::Busy,
        "the reading a render landed in was not busy"
    );
    let readings = read_until_trim();
    assert_eq!(
        readings, QUIET_READINGS,
        "the policy gave the buffers up on quiet reading {readings} where it pins {QUIET_READINGS}"
    );

    // `pooled_bytes` is the sum of the three levels the census reads — the
    // cells', the texture's and the value grid's — and each is a count of
    // bytes, so a sum of zero is all three at zero.
    assert_eq!(
        pooled_bytes(),
        0,
        "a quiet session still holds {} B of the {parked} B it parked",
        pooled_bytes()
    );
    wait_for_the_free_lane();

    // A further quiet reading must settle rather than trim again: there is
    // nothing left to give up and the free lane is not to be handed an empty
    // payload every tick for the rest of the session.
    assert_eq!(
        observe_reading(true),
        Reading::Settled,
        "a trimmed session kept trimming"
    );

    // ---- The trim cost a cache miss and nothing else. ----
    let after = render_and_recycle(&sweep);
    assert_eq!(
        after.len(),
        painted.len(),
        "the raster after a trim is a different size"
    );
    assert!(
        after == painted,
        "the raster after a trim differs from the one before it, byte for byte"
    );

    // The pool is a cache and not a switch a trim throws: a second render at
    // the same size fills the slots again, exactly as a cold start does.
    render_and_recycle(&sweep);
    assert_eq!(
        pooled_bytes(),
        parked,
        "after a trim the slots refilled to {} B where the same two renders had parked {parked} B",
        pooled_bytes()
    );

    // ---- The inverse: a session rendering steadily keeps its buffers. ----
    for tick in 0..QUIET_READINGS * 3 {
        render_and_recycle(&sweep);
        let reading = observe_reading(true);
        assert_eq!(
            reading,
            Reading::Busy,
            "a steadily rendering session reached {reading:?} on tick {tick}"
        );
        assert_eq!(
            pooled_bytes(),
            parked,
            "a steadily rendering session lost its buffers on tick {tick}"
        );
    }

    // ---- And a reply still in flight is not quiet, however still the count. ----
    for tick in 0..QUIET_READINGS * 3 {
        let reading = observe_reading(false);
        assert_eq!(
            reading,
            Reading::Busy,
            "an outstanding reply read {reading:?} on tick {tick}"
        );
    }
    assert_eq!(
        pooled_bytes(),
        parked,
        "the buffers were given up with a reply still in flight"
    );
}
