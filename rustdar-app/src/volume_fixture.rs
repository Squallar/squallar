//! The `Ready` volume fixture for the app-side tests that measure what a
//! resident grid costs and releases.

use std::sync::Arc;

use rustdar_radar::types::RadarProduct;
use rustdar_volumetric::bridge::VolumeEntry;

/// A real, tiny grid, for the tests whose subject is what may *stand in* on screen —
/// only a `Ready` entry ever does.
pub(crate) fn ready_grid() -> VolumeEntry {
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };
    let sweep = |number: u8, elevation: f32| {
        let radials = (0..8u16)
            .map(|i| {
                Radial::new(
                    1_760_000_000_000 + i64::from(i),
                    i + 1,
                    f32::from(i) * 45.0,
                    45.0,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(MomentData::from_fixed_point(
                        4,
                        2125,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![120, 140, 160, 180],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    };
    let cut = |angle: f64| {
        nexrad_model::data::ElevationCut::new(
            angle,
            nexrad_model::data::ChannelConfiguration::ConstantPhase,
            nexrad_model::data::WaveformType::CS,
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
        )
    };
    let scan = Scan::new(
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
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5), sweep(2, 1.5)],
    );
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (35.33, -97.27),
        half_extent_km: Some(rustdar_radar::voxel::HalfExtentKm::square(40.0)),
        base_km_msl: 0.0,
        top_km_msl: 10.0,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::WASM_SHAPE,
        values_wanted: false,
    };
    let grid = rustdar_radar::voxel::build_voxels(&scan, &request, 35.33, -97.27)
        .expect("the fixture volume resamples");
    VolumeEntry::Ready(Arc::new(grid))
}
