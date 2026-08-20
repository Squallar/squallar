//! Which **per-volume render input** each dispatch path actually puts on the wire: the
//! RPG's melting layer for the hybrid classification, and the RPG's storm motion vector for
//! storm-relative velocity.

use super::*;
use crate::platform_double::TestBridge;
use rustdar_worker::offload::{JobRequest, JobSink};
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";

/// A worker port that keeps what it was handed instead of posting it.
struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);

impl JobSink for Recorder {
    /// Serialises here, as the browser's own sink does, so what these tests read back has
    /// been through `to_bytes`/`from_bytes` exactly as a job crossing a real worker
    /// boundary has.
    fn send(&self, _id: u64, request: JobRequest) -> Result<(), JobRequest> {
        self.0.lock().unwrap().push(request.to_bytes());
        Ok(())
    }
}

fn volume(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
        .unwrap()
        .and_hms_opt(12, minute, 0)
        .unwrap()
}

/// The `N0M` bytes the cache is primed with.
const OBJECT: &[u8] = &[0xAB; 16];

/// A dual-pol volume small enough to build here and complete enough for the hybrid
/// classification to extract from: every moment the classifier reads, on every radial.
fn dual_pol_scan() -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
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
    let moment = |scale: f32, offset: f32| {
        Some(MomentData::from_fixed_point(
            120,
            0,
            250,
            8,
            scale,
            offset,
            vec![200; 120],
        ))
    };
    let sweep = |elevation_number: u8, elevation: f32| {
        let radials = (0..36)
            .map(|i| {
                Radial::new(
                    0,
                    i,
                    f32::from(i) * 10.0,
                    10.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation,
                    moment(2.0, 66.0),
                    moment(2.0, 129.0),
                    moment(2.0, 129.0),
                    moment(16.0, 128.0),
                    moment(2.8361, 2.0),
                    moment(300.0, -60.5),
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    };
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
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5), sweep(2, 1.5)],
    )
}

/// The melting-layer blob each posted job carries, in dispatch order.
fn dispatched_objects(posted: &Arc<Mutex<Vec<Vec<u8>>>>) -> Vec<Option<Vec<u8>>> {
    posted
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| {
            let job = JobRequest::from_bytes(bytes).expect("a job this build posted decodes");
            let plan = job
                .job
                .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
                .unwrap_or_else(|| panic!("expected a Level II render job, got {job:?}"));
            plan.input
                .melting_layer_product()
                .map(|o| o.as_ref().clone())
        })
        .collect()
}

#[test]
fn both_dispatch_paths_classify_against_this_volumes_melting_layer_and_no_other() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker =
        rustdar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().site = SITE.to_string();
    app.render.ensure_pane_count(1);
    app.render.set_melting_layer(
        SITE,
        crate::render_dispatch::MeltingLayerObject {
            volume_start: volume(0),
            bytes: std::sync::Arc::new(OBJECT.to_vec()),
        },
        &app.gui,
    );

    let params = || crate::render_dispatch::RenderParams {
        product: RadarProduct::HydrometeorClassification,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };
    let target = || rustdar_egui::pane::RenderTarget {
        site: SITE.to_string(),
        product: RadarProduct::HydrometeorClassification,
        elevation: 0.5,
    };

    // 1.
    let (sender, _rx) = std::sync::mpsc::channel();
    app.render.spawn_level2_render(
        0,
        &params(),
        SITE,
        std::sync::Arc::new(dual_pol_scan()),
        &rustdar_radar::nyquist::DeclaredNyquist::empty(),
        volume(0),
        sender,
        None,
    );

    // 2.
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume(0),
            rustdar_radar::loop_downloads::LoopFrameData::Volume(
                std::sync::Arc::new(dual_pol_scan()),
                Default::default(),
            ),
            params(),
            target(),
        ),
        "the fixture must actually reach the loop dispatch",
    );

    // 3.
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume(6),
            rustdar_radar::loop_downloads::LoopFrameData::Volume(
                std::sync::Arc::new(dual_pol_scan()),
                Default::default(),
            ),
            params(),
            target(),
        ),
        "the fixture must actually reach the loop dispatch",
    );

    let objects = dispatched_objects(&posted);
    assert_eq!(objects.len(), 3, "one still frame and two loop frames");
    assert_eq!(
        objects[0].as_deref(),
        Some(OBJECT),
        "the still frame classified without the melting layer the app holds \
         for its own volume",
    );
    assert_eq!(
        objects[1].as_deref(),
        Some(OBJECT),
        "a loop frame of the same volume classified against a different \
         melting layer than the still frame beside it",
    );
    assert_eq!(
        objects[2], None,
        "a loop frame took another volume's melting layer, which reports a \
         guess as a measurement",
    );
}

/// No other product carries the object, however it is dispatched.
#[test]
fn a_product_that_classifies_nothing_carries_no_melting_layer() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker =
        rustdar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().site = SITE.to_string();
    app.render.ensure_pane_count(1);
    app.render.set_melting_layer(
        SITE,
        crate::render_dispatch::MeltingLayerObject {
            volume_start: volume(0),
            bytes: std::sync::Arc::new(OBJECT.to_vec()),
        },
        &app.gui,
    );

    let (sender, _rx) = std::sync::mpsc::channel();
    app.render.spawn_level2_render(
        0,
        &crate::render_dispatch::RenderParams {
            product: RadarProduct::Reflectivity,
            elevation: 0.5,
            lat: site.lat,
            lon: site.lon,
        },
        SITE,
        std::sync::Arc::new(dual_pol_scan()),
        &rustdar_radar::nyquist::DeclaredNyquist::empty(),
        volume(0),
        sender,
        None,
    );

    assert_eq!(dispatched_objects(&posted), vec![None]);
}

/// The RPG storm motion vector each posted job carries, in dispatch order.
fn dispatched_motions(posted: &Arc<Mutex<Vec<Vec<u8>>>>) -> Vec<Option<(f32, f32)>> {
    posted
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| {
            let job = JobRequest::from_bytes(bytes).expect("a job this build posted decodes");
            let plan = job
                .job
                .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
                .unwrap_or_else(|| panic!("expected a Level II render job, got {job:?}"));
            plan.input.rpg_storm_motion()
        })
        .collect()
}

/// The vector the cache is primed with.
const MOTION: (f32, f32) = (37.5, 218.5);

#[test]
fn both_dispatch_paths_shift_by_this_volumes_storm_motion_and_no_other() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker =
        rustdar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().site = SITE.to_string();
    app.render.ensure_pane_count(1);
    app.render.set_storm_motion(
        SITE,
        crate::render_dispatch::StormMotionObject {
            volume_start: volume(0),
            motion: MOTION,
        },
        &app.gui,
    );

    let params = || crate::render_dispatch::RenderParams {
        product: RadarProduct::StormRelativeVelocity,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };
    let target = || rustdar_egui::pane::RenderTarget {
        site: SITE.to_string(),
        product: RadarProduct::StormRelativeVelocity,
        elevation: 0.5,
    };

    // 1.
    let (sender, _rx) = std::sync::mpsc::channel();
    app.render.spawn_level2_render(
        0,
        &params(),
        SITE,
        std::sync::Arc::new(dual_pol_scan()),
        &rustdar_radar::nyquist::DeclaredNyquist::empty(),
        volume(0),
        sender,
        None,
    );

    // 2.
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume(0),
            rustdar_radar::loop_downloads::LoopFrameData::Volume(
                std::sync::Arc::new(dual_pol_scan()),
                Default::default(),
            ),
            params(),
            target(),
        ),
        "the fixture must actually reach the loop dispatch",
    );

    // 3.
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume(6),
            rustdar_radar::loop_downloads::LoopFrameData::Volume(
                std::sync::Arc::new(dual_pol_scan()),
                Default::default(),
            ),
            params(),
            target(),
        ),
        "the fixture must actually reach the loop dispatch",
    );

    let motions = dispatched_motions(&posted);
    assert_eq!(motions.len(), 3, "one still frame and two loop frames");
    assert_eq!(
        motions[0],
        Some(MOTION),
        "the still frame shifted without the storm motion the app holds for \
         its own volume",
    );
    assert_eq!(
        motions[1],
        Some(MOTION),
        "a loop frame of the same volume shifted by a different vector than \
         the still frame beside it",
    );
    assert_eq!(
        motions[2], None,
        "a loop frame took another volume's storm motion, which reports a \
         prediction as the vector the RPG applied",
    );
}

/// No other product carries the vector, however it is dispatched.
#[test]
fn a_product_that_applies_no_storm_motion_carries_none() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker =
        rustdar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().site = SITE.to_string();
    app.render.ensure_pane_count(1);
    app.render.set_storm_motion(
        SITE,
        crate::render_dispatch::StormMotionObject {
            volume_start: volume(0),
            motion: MOTION,
        },
        &app.gui,
    );

    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::HydrometeorClassification,
    ] {
        posted.lock().unwrap().clear();
        let (sender, _rx) = std::sync::mpsc::channel();
        app.render.spawn_level2_render(
            0,
            &crate::render_dispatch::RenderParams {
                product,
                elevation: 0.5,
                lat: site.lat,
                lon: site.lon,
            },
            SITE,
            std::sync::Arc::new(dual_pol_scan()),
            &rustdar_radar::nyquist::DeclaredNyquist::empty(),
            volume(0),
            sender,
            None,
        );
        assert_eq!(
            dispatched_motions(&posted),
            vec![None],
            "{product:?} was handed a storm motion vector it does not shift by",
        );
    }
}
