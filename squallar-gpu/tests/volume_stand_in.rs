//! What a 3D pane **paints** while the grid for its box is still building.

use std::sync::Arc;

use squallar_device_profile::quality::VolumeQuality;
use squallar_egui::pane::{OrbitCamera, VolumeRegion, VolumeStamp, VolumeTarget, VolumeViewMode};
use squallar_egui::volume_view::{VolumeFrameState, VolumePaint, VolumePainter};
use squallar_geo::GeoPoint;
use squallar_radar::types::RadarProduct;
use squallar_volumetric::VolumeSupport;
use squallar_volumetric::bridge::{BridgeVolumePainter, VolumeEntry, VolumeStore};

/// The fixture's radar, and the box the grid below is resampled over.
const SITE: (f64, f64) = (35.33, -97.27);
const HALF_KM: f64 = 40.0;

/// A resolved grid over `HALF_KM` about `SITE`.
fn ready_grid() -> VolumeEntry {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
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
        ElevationCut::new(
            angle,
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
    let request = squallar_radar::voxel::VoxelRequest {
        centre: SITE,
        // A picked width, not the volume's own reach: this fixture's scan is
        // synthetic and what its gates happen to reach is not the subject.
        half_extent_km: Some(squallar_radar::voxel::HalfExtentKm::square(HALF_KM)),
        base_km_msl: 0.0,
        top_km_msl: 10.0,
        product: RadarProduct::Reflectivity,
        shape: squallar_radar::voxel::WASM_SHAPE,
        values_wanted: false,
    };
    VolumeEntry::Ready(Arc::new(
        squallar_radar::voxel::build_voxels(&scan, &request, SITE.0, SITE.1)
            .expect("the fixture volume resamples"),
    ))
}

fn target(half_width_km: f64) -> VolumeTarget {
    VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2024, 5, 6)
                .unwrap()
                .and_hms_opt(22, 0, 0)
                .unwrap(),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: Some(
            VolumeRegion::new(
                GeoPoint {
                    lat: SITE.0,
                    lon: SITE.1,
                },
                squallar_radar::voxel::HalfExtentKm::square(half_width_km),
            )
            .expect("a finite in-range half-width on a real centre is a region"),
        ),
    }
}

fn frame(target: &VolumeTarget) -> VolumeFrameState {
    VolumeFrameState {
        pane_idx: 0,
        target: target.clone(),
        camera: OrbitCamera::default(),
        size_px: [800, 600],
        pixels_per_point: 1.0,
        // No floor: the mirror is a frame-path render target and this test has
        // no frame. Nothing the file asserts is about the floor, and raising
        // the flag without a source is the one combination `paint` refuses.
        floor: false,
        source: None,
        mirror_size_points: [800.0, 600.0],
        alpha: None,
        view_mode: VolumeViewMode::LitVolume,
        iso_threshold: 18.0,
        light: squallar_egui::volume_view::VolumeLight::Headlight,
        heights: None,
    }
}

fn painter(store: Arc<VolumeStore>) -> BridgeVolumePainter {
    // The offscreen budget this build resolves, so a stand-in fits its panes
    // against the same figure the application would.
    let budgets = squallar_device_profile::budget::resolve(
        &squallar_device_profile::budget::DeviceProfile::for_target(),
    );
    BridgeVolumePainter::new(
        store,
        VolumeQuality::BEST,
        budgets.offscreen_bytes,
        VolumeSupport::Supported,
    )
}

/// **The defect.** A zoom must leave a picture on screen, and the caption must
/// describe the picture rather than the box that is still building.
#[test]
fn a_zoom_keeps_a_picture_on_screen_and_the_caption_says_what_it_is() {
    let store = Arc::new(VolumeStore::new());
    let painter = painter(store.clone());
    let wide = target(HALF_KM);
    let tight = target(HALF_KM / 2.0);

    let VolumePaint::Empty(why) = painter.paint(&frame(&wide)) else {
        panic!("with nothing in the store there is no picture to paint");
    };
    assert!(
        why.contains("Building"),
        "the first build's own message is the one thing that should still \
         blank a pane; got {why:?}",
    );

    store.begin_build(0, &wide);
    assert!(store.complete(&wide, ready_grid()));

    let VolumePaint::Callback { showing, .. } = painter.paint(&frame(&wide)) else {
        panic!("a resolved grid for the pane's own box must paint");
    };
    assert_eq!(
        (showing.stale, showing.partial),
        (false, false),
        "the settled pane is showing exactly what it asked for, and a caption \
         that said otherwise would put a permanent 'sharpening' on a picture \
         that is already sharp",
    );

    // The zoom. A new box, a build opened for it, nothing resolved.
    assert!(!store.share(0, &tight), "a new box needs a new build");
    store.begin_build(0, &tight);
    assert!(
        matches!(
            store.lookup(&tight).map(|l| l.entry),
            Some(VolumeEntry::Building)
        ),
        "precondition: the zoomed box is genuinely still building",
    );

    let VolumePaint::Callback { showing, .. } = painter.paint(&frame(&tight)) else {
        panic!(
            "the pane must go on painting the grid it has while the zoomed \
             box builds — blanking here is the whole defect"
        );
    };
    assert!(
        showing.stale,
        "the caption must be told the picture is the older, coarser grid; \
         reporting the requested box's cell size would claim a sharpness that \
         is not on screen",
    );
    assert!(
        !showing.partial,
        "zooming IN, the held grid covers the whole new box, so nothing is \
         missing and the caption must not say anything is",
    );
    let held_cell = (2.0 * HALF_KM / f64::from(squallar_radar::voxel::WASM_SHAPE.nx as u32)) as f32;
    assert_eq!(
        showing.cell_km,
        Some((held_cell, held_cell)),
        "the cell size reported is the held grid's own, on both axes",
    );
}

/// Zooming **out**, the held grid answers the middle of the new box and
/// nothing else — which the caption has to say, because a volume that simply
/// stops is read as weather that stops.
#[test]
fn zooming_out_paints_the_middle_and_says_the_rest_is_coming() {
    let store = Arc::new(VolumeStore::new());
    let painter = painter(store.clone());
    let held = target(HALF_KM);
    let wider = target(HALF_KM * 2.0);

    store.begin_build(0, &held);
    assert!(store.complete(&held, ready_grid()));
    assert!(!store.share(0, &wider));
    store.begin_build(0, &wider);

    let VolumePaint::Callback { showing, .. } = painter.paint(&frame(&wider)) else {
        panic!("the held grid must still paint over the middle of a wider box");
    };
    assert_eq!(
        (showing.stale, showing.partial),
        (true, true),
        "the picture is real data in the middle and nothing outside it, and \
         both facts belong in the caption",
    );
}

/// A retarget the crop cannot redeem still blanks — and must, because there is
/// no transform that makes one radar's grid an answer about another's.
#[test]
fn another_radars_grid_is_still_no_picture_at_all() {
    let store = Arc::new(VolumeStore::new());
    let painter = painter(store.clone());
    let here = target(HALF_KM);
    let elsewhere = VolumeTarget {
        volume: VolumeStamp {
            site: "KFWS".to_owned(),
            ..here.volume.clone()
        },
        ..here.clone()
    };

    store.begin_build(0, &here);
    assert!(store.complete(&here, ready_grid()));
    assert!(!store.share(0, &elsewhere));
    store.begin_build(0, &elsewhere);

    assert!(
        matches!(painter.paint(&frame(&elsewhere)), VolumePaint::Empty(_)),
        "a KTLX grid under a KFWS caption is the one lie the stand-in must \
         never tell, however much the user would rather see something",
    );
}
