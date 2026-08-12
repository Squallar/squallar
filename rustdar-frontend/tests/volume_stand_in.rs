//! What a 3D pane **paints** while the grid for its box is still building.
//!
//! # Why this is an integration test and not a `#[cfg(test)]` module
//!
//! `BridgeVolumePainter::paint` consults `volume::support`, which folds in
//! `volume::degrade`'s process-global surface-loss counter. That counter is
//! deliberately never reset, and one lib test
//! (`the_global_loss_counter_survives_and_retires_the_view`) drives it past the
//! retirement threshold on purpose — after which every `paint` in that binary
//! answers `Empty`, whatever the store holds. `cargo test` runs a binary's
//! tests in parallel threads of one process, so a lib test that called `paint`
//! would pass or fail on scheduling order. An integration test is its own
//! process and sees a clean counter.
//!
//! # What it pins
//!
//! Scroll on a 3D pane zooms the *geography*, and the box is the pane's own
//! viewport, so a zoom names a new `VolumeTarget` on the frame the wheel turns.
//! The grid for that box is ~89 ms of resampling plus ~51 ms of upload away.
//! The pane must keep drawing through that window — the held grid, put into the
//! box the user just asked for — and its caption must say what is really on
//! screen. Both halves are asserted here, and the file's shape is chosen so
//! that neither can pass vacuously: every assertion that something *is* painted
//! sits beside one showing the same painter answering `Empty` when it should.

use std::sync::Arc;

use rustdar_egui::pane::{
    GeoPoint, OrbitCamera, VolumeRegion, VolumeStamp, VolumeTarget, VolumeViewMode,
};
use rustdar_egui::volume_view::{VolumeFrameState, VolumePaint, VolumePainter};
use rustdar_frontend::volume::VolumeSupport;
use rustdar_frontend::volume::bridge::{BridgeVolumePainter, VolumeEntry, VolumeStore};
use rustdar_frontend::volume::quality::VolumeQuality;
use rustdar_radar::types::RadarProduct;

/// The fixture's radar, and the box the grid below is resampled over.
const SITE: (f64, f64) = (35.33, -97.27);
const HALF_KM: f64 = 40.0;

/// A resolved grid over `HALF_KM` about `SITE`.
///
/// Its own copy of the scan rather than a share with `volume_bridge`'s
/// `ready_grid`: that one is `#[cfg(test)]` inside the library and an
/// integration binary links the library's *public* surface only. `build_voxels`
/// is the sole constructor a `VoxelGrid` has, so a fixture here has to go
/// through a scan whichever crate it lives in.
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
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: SITE,
        // A picked width, not the volume's own reach: this fixture's scan is
        // synthetic and what its gates happen to reach is not the subject.
        half_width_km: Some(HALF_KM),
        base_km_msl: 0.0,
        top_km_msl: 10.0,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::WASM_SHAPE,
        values_wanted: false,
    };
    VolumeEntry::Ready(Arc::new(
        rustdar_radar::voxel::build_voxels(&scan, &request, SITE.0, SITE.1)
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
        product: RadarProduct::Reflectivity,
        region: Some(
            VolumeRegion::new(
                GeoPoint {
                    lat: SITE.0,
                    lon: SITE.1,
                },
                half_width_km,
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
    }
}

fn painter(store: Arc<VolumeStore>) -> BridgeVolumePainter {
    BridgeVolumePainter::new(store, VolumeQuality::BEST, VolumeSupport::Supported)
}

/// **The defect.** A zoom must leave a picture on screen, and the caption must
/// describe the picture rather than the box that is still building.
///
/// The `Empty` assertion at the top is not decoration: it is what makes the
/// rest of the file impossible to pass vacuously. The same painter, over the
/// same target, answers `Empty` with an empty store and `Callback` once a grid
/// is in hand — so a `Callback` further down cannot be an artefact of a painter
/// that answers `Callback` to everything.
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
        matches!(store.lookup(&tight).map(|l| l.entry), Some(VolumeEntry::Building)),
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
    assert_eq!(
        showing.cell_km,
        Some((2.0 * HALF_KM / f64::from(rustdar_radar::voxel::WASM_SHAPE.nx as u32)) as f32),
        "the cell size reported is the held grid's own",
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
///
/// The counterweight to every `Callback` above.
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
