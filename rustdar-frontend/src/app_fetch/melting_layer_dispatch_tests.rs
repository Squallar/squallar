//! Which melting layer each dispatch path actually puts on the wire.
//!
//! Read back through `JobRequest::from_bytes`, the same decode a worker runs,
//! for the reason `loop_raster_ceiling_tests` does it: the melting layer is a *render
//! input*, and by the time a raster comes back there is nothing left in it to
//! distinguish "classified against the RPG's own layer" from "classified
//! against a fleet constant" — that is the entire problem this workstream
//! exists to solve.
//!
//! Two paths dispatch a classification and they are written in two files.
//! Asserting both from one app is what makes this a property of the dispatch
//! rather than of whichever half was edited last: a loop frame and the still
//! frame beside it must classify the same volume against the same layer, and
//! *different* volumes against different ones.

use super::*;
use crate::offload::{JobRequest, WorkerPort};
use crate::platform_double::TestBridge;
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";

/// A worker port that keeps what it was handed instead of posting it.
struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);

impl WorkerPort for Recorder {
    fn post(&self, _id: u64, request: Vec<u8>) -> bool {
        self.0.lock().unwrap().push(request);
        true
    }
}

fn volume(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
        .unwrap()
        .and_hms_opt(12, minute, 0)
        .unwrap()
}

/// The `N0M` bytes the cache is primed with.
///
/// Never decoded on this path: `RenderInput` carries the object as a blob and
/// the worker is what decodes it, so a recognisable pattern is all this needs
/// to be — what is under test is *which volume's* blob travels, not what is in
/// it.
const OBJECT: &[u8] = &[0xAB; 16];

/// A dual-pol volume small enough to build here and complete enough for the
/// hybrid classification to extract from: every moment the classifier reads,
/// on every radial.
///
/// The single-moment scan `loop_raster_ceiling_tests` uses would answer `None` from
/// `RenderInput::extract` for this product and dispatch `Job::renders_nothing`,
/// which posts nothing at all — so the test would pass against a dispatcher
/// that had lost the melting layer entirely.
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
            match job {
                JobRequest::Radar { input, .. } => {
                    input.melting_layer_product().map(|o| o.as_ref().clone())
                }
                other => panic!("expected a Level II render job, got {other:?}"),
            }
        })
        .collect()
}

/// **Both classification dispatch paths carry the object, and only for the
/// volume it names.**
///
/// The still frame and the loop frame build their own `RenderInput`s in
/// different files, so "the melting layer reaches the renderer" is two claims,
/// not one — and the second is the one that decays quietly, because a loop
/// frame with no melting layer still draws a full, plausible classification.
///
/// The third dispatch here is the point of the whole design: a loop frame from
/// a *different* volume gets `None`. Handing it the cached object would put a
/// real, measured melting layer under a volume nobody measured it for, and the
/// render would then report itself as `Rpg` — measured, for this volume —
/// while being neither. That is strictly worse than the fleet default, which at
/// least says it is guessing.
#[test]
fn both_dispatch_paths_classify_against_this_volumes_melting_layer_and_no_other() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker = crate::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

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

    // 1. The still frame, on the volume the object names.
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

    // 2. A loop frame of that same volume.
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume(0),
            crate::loop_downloads::LoopFrameData::Volume(
                std::sync::Arc::new(dual_pol_scan()),
                Default::default(),
            ),
            params(),
            target(),
        ),
        "the fixture must actually reach the loop dispatch",
    );

    // 3. A loop frame of an *older* volume, which nothing fetched an object
    //    for. This is the frame the still frame's object must not reach.
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume(6),
            crate::loop_downloads::LoopFrameData::Volume(
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
///
/// A melting layer is a render input to the hybrid classification and to
/// nothing else. A reflectivity job carrying one would be harmless on the
/// wire and is refused anyway, at the accessor rather than at the far end,
/// because the far end refusing it is not something this side can see.
#[test]
fn a_product_that_classifies_nothing_carries_no_melting_layer() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker = crate::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

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
