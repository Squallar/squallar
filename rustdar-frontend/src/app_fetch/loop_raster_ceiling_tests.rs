//! What size a loop frame is dispatched at.
//!
//! The observable is the job that goes on the wire, read back through the same
//! `JobRequest::from_bytes` a worker uses — because "the loop renders leaner"
//! is a property of what is *asked for*, and by the time a frame comes back
//! there is nothing left to distinguish a policy from a coincidence.

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

/// The smallest real volume a `RenderInput` extracts from, copied from
/// `offload::tests` because it is the same requirement: a VCP that declares
/// its cuts, so the extraction reaches a tilt rather than the refusal path.
fn sample_scan() -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, PulseWidth, Radial, RadialStatus, Scan, Sweep,
        VolumeCoveragePattern, WaveformType,
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
                    Some(nexrad_model::data::MomentData::from_fixed_point(
                        120,
                        0,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![200; 120],
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

/// A loop frame is dispatched at the loop ceiling, and a still frame on the
/// same app at the one the device reported.
///
/// The pairing is the test. Asserting the loop's ceiling alone would pass
/// against a dispatcher that never reads the device at all, which is a display
/// that quietly lost the long-range raster; asserting both from one app says
/// the ceiling is a decision the two paths make differently rather than a
/// constant.
#[test]
fn a_loop_frame_is_dispatched_leaner_than_the_still_frame_beside_it() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    // Retired by the guard rather than by the call that used to sit below the
    // decode: an `assert!`, two `expect`s and four `unwrap`s stand between the
    // install and that call, and any of them unwinding past it would hand this
    // port to the next test on this harness thread. See
    // `offload::InstalledTestWorker`.
    let _worker = crate::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().site = SITE.to_string();
    app.render.ensure_pane_count(1);
    // What the device said it can hold, which is what makes the still frame's
    // ceiling meaningful — see `RenderDispatcher::set_raster_side_ceiling_px`.
    // Deliberately not the long-range constant: a still frame that came back at
    // 4096 here would be reading a literal rather than this number.
    const DEVICE_CEILING: usize = 8192;
    app.render.set_raster_side_ceiling_px(DEVICE_CEILING);

    let params = || crate::render_dispatch::RenderParams {
        product: RadarProduct::Reflectivity,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };

    // The loop path.
    assert!(
        app.spawn_loop_frame_render(
            0,
            chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            crate::loop_downloads::LoopFrameData::Volume(
                std::sync::Arc::new(sample_scan()),
                Default::default(),
            ),
            params(),
            rustdar_egui::pane::RenderTarget {
                site: SITE.to_string(),
                product: RadarProduct::Reflectivity,
                elevation: 0.5,
            },
        ),
        "the fixture must actually reach the dispatch",
    );

    // The still path, through the site's own render params.
    let (sender, _rx) = std::sync::mpsc::channel();
    app.render.spawn_level2_render(
        0,
        &params(),
        SITE,
        std::sync::Arc::new(sample_scan()),
        &rustdar_radar::nyquist::DeclaredNyquist::empty(),
        chrono::NaiveDateTime::default(),
        sender,
        None,
    );

    let jobs: Vec<JobRequest> = posted
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| JobRequest::from_bytes(bytes).expect("a job this build posted decodes"))
        .collect();

    assert_eq!(jobs.len(), 2, "one loop frame and one still frame");
    let ceiling = |job: &JobRequest| match job {
        JobRequest::Radar {
            side_ceiling_px, ..
        } => *side_ceiling_px as usize,
        other => panic!("expected a Level II render job, got {other:?}"),
    };
    assert_eq!(
        ceiling(&jobs[0]),
        crate::constants::LOOP_IMAGE_SIZE,
        "a loop frame took more than the loop ceiling: thirty textured 4096 \
         frames is 1.9 GiB a pane against a 512 MiB loop budget",
    );
    assert_eq!(
        ceiling(&jobs[1]),
        DEVICE_CEILING,
        "a still frame was dispatched at something other than what the device \
         said it can hold",
    );
}
