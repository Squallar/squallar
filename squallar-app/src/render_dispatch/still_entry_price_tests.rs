//! **What one still plan-view entry costs the render cache, on a sweep the
//! shape a WSR-88D actually delivers.**
//!
//! The `render cache` census family read 416.0 MiB on the six-pane arm, to
//! within 0.1 MiB, on every leg of two different trees. That figure is not a
//! measurement of anything: it is
//! `floor(byte_capacity / raster_entry) x resident_entry` — two entries at the
//! 7362 px side, `2 x (7362² x 4 + 1,320,064) = 436,232,480 B`, and the byte
//! capacity is `8 x converted_raster_bytes(4096)`, three compile-time
//! constants. Nothing about a pane, a gesture or a loop reaches it.
//!
//! What DOES reach it is which surface the render came back as, and that
//! moved on 2026-09-08: a still Level II plan view whose sweep a plane can
//! carry now caches a `FanSweep` and a byte-a-gate field instead of a
//! `side²` `Color32` raster. This module is what stops it moving back
//! silently, and it is a PRICE gate rather than a plumbing one — the arm
//! choice is already pinned in `squallar_radar::jobs`
//! (`a_fan_request_that_wants_the_numbers_reads_back_the_rasters_own`) and in
//! `app_render::still_fan_tests`, both on hand-built payloads. Neither can say
//! what an entry costs.
//!
//! **The property the input must have.** Every plane fixture in this
//! workspace is a 9x40 or 36x120 sweep, and a fan entry built from one is
//! 4,320 B — three orders below a real one. A price gate on such a fixture
//! would be green, non-vacuous and tampered while structurally unable to say
//! anything about the quantity in the census, because the raster arm's side
//! is floored at `IMAGE_SIZE` whatever the sweep's shape is: BOTH arms would
//! be dominated by constants and the ratio would hold for a sweep of four
//! gates. So the fixture here is a surveillance cut's own shape — 720 radials
//! of 1832 gates at 250 m, reaching 460 km — which is the shape the 416.0
//! figure was read at, and the shape whose admission by
//! `plane::sweep_code_plane` is the thing that can regress.
//!
//! That admission is asserted FIRST and on its own. Without it a refusal
//! would fall through to the raster and every assertion below would pass by
//! pinning the defect as the specification.

use std::sync::Arc;

use squallar_radar::types::RadarProduct;
use squallar_source::job::{JobGeometry, JobSpec};

use super::{CachedRenderOutput, RenderCache, rendered_image_from};

/// A surveillance cut's own shape: 0.5° radials all the way round, 1832 gates
/// of 250 m starting at 2.125 km — the cut the 7362 px raster side is derived
/// from (`2 x 460.11 / 0.25 x TEXELS_PER_SAMPLE`, ceiled).
const RADIALS: usize = 720;
const GATES: usize = 1832;
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

/// The two side ceilings the raster control is rendered at, both well below
/// the 7362 px a desktop plan view reaches.
///
/// Held down on purpose: `render_with_projection` keeps `side²` atomic cells
/// (8 B) and the RGBA buffer (4 B) live at once, so the shipped side is
/// 650,388,528 B inside one test process. The control is only ever the
/// *cheaper* half of every comparison below, so holding it down can only make
/// an assertion harder to pass, never easier. The shipped figure it stands in
/// for is pinned elsewhere as arithmetic: `7362² x 4 = 216,796,176 B`
/// (`squallar_gpu::egui_renderer::texture_upload::resident::tests`).
///
/// **The smaller one is a floor, not a choice.** `raster_side_from_rgba_len`
/// refuses to convert any raster under `LOOP_IMAGE_SIZE`, which is 2048 on
/// this target, so a smaller ceiling does not render a smaller picture — it
/// renders one the frame thread declines.
const SMALL_SIDE_PX: u32 = 2048;
const LARGE_SIDE_PX: u32 = 4096;

/// One radial of ascending codes, so no two gates of a radial carry the same
/// one and the plane cannot pass by holding a single value.
fn radial(index: usize) -> nexrad_model::data::Radial {
    use nexrad_model::data::{MomentData, Radial, RadialStatus};
    let spacing = 360.0 / RADIALS as f32;
    let bytes: Vec<u8> = (0..GATES).map(|g| ((index + g) % 254 + 2) as u8).collect();
    Radial::new(
        0,
        index as u16,
        index as f32 * spacing,
        spacing,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(MomentData::from_fixed_point(
            GATES as u16,
            FIRST_GATE_M,
            GATE_M,
            8,
            2.0,
            66.0,
            bytes,
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// One volume holding one surveillance-shaped reflectivity cut.
fn surveillance_scan() -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, PulseWidth, Scan, Sweep, VolumeCoveragePattern,
        WaveformType,
    };
    let cut = ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        20,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    );
    let radials = (0..RADIALS).map(radial).collect();
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![cut],
        ),
        vec![Sweep::new(1, radials)],
    )
}

/// The still dispatch's own request, on `surface`, run through the row the
/// worker runs — `values_wanted: true`, because a still pane reads a hover
/// and that flag is what tied this path to the raster until 2026-09-08.
fn still_frame(
    surface: squallar_radar::jobs::PlanSurface,
) -> Option<squallar_radar::frame::RenderedFrame> {
    still_frame_at(surface, SMALL_SIDE_PX)
}

/// [`still_frame`] at a stated side ceiling — the one input the raster arm
/// reads and the fan arm does not.
fn still_frame_at(
    surface: squallar_radar::jobs::PlanSurface,
    side_ceiling_px: u32,
) -> Option<squallar_radar::frame::RenderedFrame> {
    let scan = surveillance_scan();
    let input = squallar_radar::render_input::RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::Reflectivity,
        35.33,
        -97.27,
        None,
        None,
    )
    .expect("a surveillance-shaped reflectivity cut extracts");
    let job = squallar_radar::jobs::RadarPlanJob {
        input: Box::new(input),
        values_wanted: true,
        surface,
    };
    let geometry = JobGeometry {
        width: 64,
        height: 32,
        bounds: squallar_geo::GeoBounds {
            min_lat: 30.0,
            max_lat: 40.0,
            min_lon: -100.0,
            max_lon: -90.0,
        },
        side_ceiling_px,
    };
    squallar_radar::jobs::RadarPlanJob::run(&job, &geometry)
}

/// The frame as the render cache would hold it, priced by the cache's own
/// `entry_bytes` — the `render cache` census family's denominator, pixels or
/// payload plus the resident hover field.
fn entry_bytes(frame: squallar_radar::frame::RenderedFrame) -> usize {
    let rendered = rendered_image_from(frame, (35.33, -97.27))
        .expect("the reply converts into the shape the frame thread applies");
    RenderCache::entry_bytes(&CachedRenderOutput {
        surface: rendered.surface,
        max_range_km: rendered.max_range_km,
        hover: Arc::clone(&rendered.hover),
        nyquist_ms: rendered.nyquist_ms,
        melting_layer_source: rendered.melting_layer_source,
        storm_motion: rendered.storm_motion,
    })
}

/// **The fixture reaches the defect class**, asserted before anything is
/// priced: a surveillance-shaped sweep is one an eight-bit plane carries.
///
/// Every reason `render_sweep_plane` answers `None` is a fidelity reason and
/// the answer to all of them is the raster — so a refusal here is not a
/// failure of this module's subject, it is the *disappearance* of it, and
/// every price assertion below would then be pinning the raster as the
/// specification.
///
/// TAMPER: ask for `PlanSurface::Raster` and this goes red.
#[test]
fn a_surveillance_shaped_sweep_is_carried_by_a_plane() {
    let frame = still_frame(squallar_radar::jobs::PlanSurface::Fan)
        .expect("the fixture renders on the fan arm");
    assert!(
        frame.codes.is_some(),
        "a 720x1832 reflectivity cut fell back to the raster; every price below \
         would then be pinning the fallback as the specification",
    );
    assert!(
        frame.image.is_empty(),
        "the frame carried a plane and a raster at once",
    );
    assert!(
        frame.polar.has_values(),
        "a still pane reads a hover, and the field came back empty",
    );
}

/// **A still entry is priced by the sweep, not by the side.**
///
/// This is the whole of what `render cache` measures, and the assertion is
/// against the shape the entry describes rather than a recorded number: a
/// plane is the sweep's own bytes with a mip chain over them, which sums
/// under `2 x radials x gates`, and the field beside it is one byte a gate
/// with a 256-entry table. So an entry is bounded by four times the sweep,
/// and that bound holds for every cut rather than for the one that was
/// measured on the day.
///
/// The figure it replaces is quadratic in a side no sweep chooses:
/// `side² x 4`, 216,796,176 B at the 7362 px a desktop plan view reaches,
/// which two of is the 416.0 MiB the census read.
///
/// TAMPER: return `PlanSurface::Raster` from `App::plan_surface`, or drop the
/// fan arm from `rendered_image_from`, and this goes red by three orders.
#[test]
fn a_still_fan_entry_is_bounded_by_the_sweep_it_holds() {
    let frame = still_frame(squallar_radar::jobs::PlanSurface::Fan)
        .expect("the fixture renders on the fan arm");
    let sweep_bytes = RADIALS * GATES;
    let entry = entry_bytes(frame);
    println!("still fan entry: {entry} B over a {RADIALS}x{GATES} sweep ({sweep_bytes} B)");
    assert!(
        entry < 4 * sweep_bytes,
        "a still fan entry is {entry} B against a {sweep_bytes} B sweep; a plane \
         is the sweep's bytes and a chain over them, and a field beside it is \
         one byte a gate",
    );
}

/// **The raster arm prices the side; the fan arm prices the sweep.** The
/// claim stated as a derivative rather than as a ratio, so nothing here is a
/// magic number.
///
/// The same sweep is rendered on the raster arm at two side ceilings. Its
/// entry rises by **exactly** the pixels the larger side added — the hover
/// field beside it is the sweep's own radials and gates and cannot move with
/// a ceiling — so the entry is a function of a side the data did not choose.
/// The fan arm's entry over the same two ceilings is byte-identical: the
/// ceiling never reaches it.
///
/// That is what makes the 416.0 MiB census reading arithmetic over a
/// constant. At the 7362 px a desktop plan view reaches, the raster term is
/// `7362² x 4 = 216,796,176 B`, and `floor(536,870,912 / 216,796,176) = 2`
/// entries is the whole of the figure.
///
/// TAMPER: price a `StillSurface::Raster` at `sweep.resident_bytes()` in
/// `RenderCache::entry_budget_bytes`, or let the fan arm read
/// `side_ceiling_px`, and this goes red.
#[test]
fn the_raster_entry_tracks_the_side_and_the_fan_entry_does_not() {
    let priced = |surface, ceiling| {
        let frame = still_frame_at(surface, ceiling).expect("the fixture renders");
        entry_bytes(frame)
    };
    use squallar_radar::jobs::PlanSurface::{Fan, Raster};

    let small = priced(Raster, SMALL_SIDE_PX);
    let large = priced(Raster, LARGE_SIDE_PX);
    let added = (LARGE_SIDE_PX as usize * LARGE_SIDE_PX as usize
        - SMALL_SIDE_PX as usize * SMALL_SIDE_PX as usize)
        * squallar_device_profile::constants::PLAN_VIEW_TEXEL_BYTES;
    println!("raster entry: {small} B at {SMALL_SIDE_PX} px, {large} B at {LARGE_SIDE_PX} px");
    assert_eq!(
        large - small,
        added,
        "a raster entry rose by {} B when the side ceiling added {added} B of \
         pixels; the entry is not the picture's own area",
        large - small,
    );

    let fan_small = priced(Fan, SMALL_SIDE_PX);
    let fan_large = priced(Fan, LARGE_SIDE_PX);
    println!("fan entry: {fan_small} B at {SMALL_SIDE_PX} px, {fan_large} B at {LARGE_SIDE_PX} px");
    assert_eq!(
        fan_small, fan_large,
        "a fan entry moved with a side ceiling no plane is drawn at",
    );
    assert!(
        fan_large < small,
        "the fan entry is {fan_large} B against a raster entry of {small} B at the \
         SMALLER of the two ceilings, a thirteenth of the shipped 7362 px area",
    );
}
