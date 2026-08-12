//! What every dispatch path says about where the sweep it asked for folds.
//!
//! The observable is the **job that goes on the wire**, read back through the
//! same `JobRequest::from_bytes` a browser worker uses. That is the only place
//! the question has one answer: the fold limit is applied on whatever thread
//! ends up rasterizing, so by the time a picture comes back there is nothing
//! left to distinguish a payload that carried the RDA's declaration from one
//! that let the dealiaser estimate — the two differ in a band of borderline
//! gates and in nothing else.
//!
//! The band matters wherever the estimate can be wrong, which is any cut whose
//! calmest sector reports well under its real limit. It is the **WSR-88D** that
//! puts a number on the wire for this to carry: its Doppler cuts declare
//! 23.84–62.94 m/s across the ten volumes `rustdar_radar::nyquist` measured.
//!
//! Not the TDWR, which is worth stating because the case for declared-over-
//! estimated was originally argued on it. Its short PRT does make folding
//! routine, but it never says where: across 22 volumes from 10 TDWR sites,
//! every cut declares `nyquist_velocity = 0`, which
//! `DeclaredNyquist::declare` refuses as the absence it is. A TDWR therefore
//! posts jobs carrying `None` here and is dealiased against the estimate, the
//! same as it was before any of this existed.

use crate::offload::{JobRequest, WorkerPort};
use crate::platform_double::TestBridge;
use rustdar_radar::types::RadarProduct;
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";
/// The 0.5° cut's own statement — KTLX's real declaration on 2026-08-11 at
/// 10:09, and a value no default anywhere produces, so a payload that reached
/// the assertions carrying it can only have been stamped.
///
/// It reads `KTLX` above and it is a WSR-88D number, which it was not before:
/// this constant was 22.14 and called itself "a real TDWR Doppler figure",
/// and no TDWR declares anything at all.
const DECLARED_MS: f64 = 23.84;
/// A second cut, declaring something else, so a stamp that copied one cut's
/// number onto every sweep would be visible.
const OTHER_CUT_MS: f64 = 31.35;

/// A worker port that keeps what it was handed instead of posting it.
struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);

impl WorkerPort for Recorder {
    fn post(&self, _id: u64, request: Vec<u8>) -> bool {
        self.0.lock().unwrap().push(request);
        true
    }
}

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(18, 30, 0)
        .unwrap()
}

/// What the two cuts below declared.
fn declared() -> Arc<rustdar_radar::nyquist::DeclaredNyquist> {
    Arc::new(
        [(1, DECLARED_MS), (2, OTHER_CUT_MS)]
            .into_iter()
            .collect::<rustdar_radar::nyquist::DeclaredNyquist>(),
    )
}

/// A two-cut volume carrying velocity, so the extraction reaches a sweep and
/// the elevation numbers the table is keyed by are real.
fn sample_scan() -> Arc<nexrad_model::data::Scan> {
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
    let sweep = |elevation_number: u8, elevation: f32| {
        let radials = (0..36)
            .map(|i| {
                let moment =
                    MomentData::from_fixed_point(120, 0, 250, 8, 2.0, 66.0, vec![200; 120]);
                Radial::new(
                    0,
                    i,
                    f32::from(i) * 10.0,
                    10.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation,
                    Some(moment.clone()),
                    Some(moment),
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
    Arc::new(Scan::new(
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
    ))
}

/// An app with one map pane on [`SITE`], its volume in the plan view's own
/// holder and in the loop cache, and the declarations with it in both.
fn app_showing_site() -> crate::app::App {
    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(RadarProduct::NormalizedRotation, vec![0.5]);
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.site = SITE.to_string();
        pane.selected_product = RadarProduct::NormalizedRotation;
        pane.selected_elevation = 0.5;
    }
    app.gui.set_scan_info_for_pane(
        0,
        rustdar_radar::types::ScanInfo {
            site,
            site_source: rustdar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            timestamp: volume_time(),
            vcp_number: 212,
            available_products: vec![RadarProduct::NormalizedRotation],
            product_elevations,
            status: String::new(),
        },
    );
    app.render.ensure_pane_count(1);
    app.scan_data
        .insert(SITE.to_string(), (sample_scan(), declared()));
    app.base_scans
        .insert(SITE.to_string(), (sample_scan(), declared(), volume_time()));
    app.loop_mgr
        .cache_scan(SITE, volume_time(), (sample_scan(), declared()));
    app
}

/// The declared table a posted job carries, whatever kind of job it is.
///
/// Both kinds carry more than one entry here, and for the same reason: NROT and
/// SRV seed their dealiasers from a wind profile fitted over the *volume's*
/// velocity tilts, so even a single-tilt payload for one of them reaches the
/// worker with every velocity sweep in it.
fn declaration_on_the_wire(job: &JobRequest) -> Vec<(u8, f64)> {
    let input = match job {
        JobRequest::Radar { input, .. } | JobRequest::Section { input, .. } => input,
        other => panic!("expected a payload-carrying job, got {other:?}"),
    };
    input.declared_nyquist().iter().collect()
}

/// What a payload says about the cut this fixture's panes are drawing, out of
/// whatever else it carries.
fn the_drawn_cut(declared: &[(u8, f64)]) -> Vec<(u8, f64)> {
    declared
        .iter()
        .copied()
        .filter(|(elevation_number, _)| *elevation_number == 1)
        .collect()
}

fn posted_jobs(recorded: &Mutex<Vec<Vec<u8>>>) -> Vec<JobRequest> {
    recorded
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| JobRequest::from_bytes(bytes).expect("a job this build posted decodes"))
        .collect()
}

/// **The pin this task exists for.** One sweep, two views, one fold limit.
///
/// The plan view rasterizes NROT on the worker and the section derives NROT on
/// the worker, and each unfolds velocity before it computes shear. Between them
/// they used to disagree: the section payload was stamped with the volume's
/// declarations and the plan view's was not, so a section of a Doppler cut
/// unfolded around the RDA's own number while the map under it unfolded around
/// whatever its calmest sector happened to observe. Nothing errored, nothing
/// warned, and the two pictures of one cut simply differed.
#[test]
fn a_plan_view_and_a_section_of_one_sweep_fold_at_the_same_speed() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    crate::offload::set_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = app_showing_site();
    let ctx = egui::Context::default();
    // The map pane's own dispatch, through the pass that reads `scan_data` —
    // not `spawn_level2_render` called by hand, because the store carrying the
    // table and the dispatcher reading it are two separate things to get wrong.
    app.dispatch_pane_renders(&ctx);

    // And a section of the same volume, aimed and cut. The in-flight mark goes
    // first: `dispatch_section_renders` refuses a pane that already has a
    // render out, and the mark is cleared on receipt of a `RenderResponse` —
    // which this fixture, holding the job rather than answering it, never
    // sends.
    app.render.pane_render[0].render_in_flight = false;
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.set_kind(rustdar_egui::pane::PaneKind::CrossSection);
        pane.selected_product = RadarProduct::NormalizedRotation;
        pane.cross_section_mut().unwrap().line = Some(
            rustdar_egui::pane::SectionLine::new(
                rustdar_egui::pane::GeoPoint {
                    lat: 35.0,
                    lon: -98.0,
                },
                rustdar_egui::pane::GeoPoint {
                    lat: 36.0,
                    lon: -97.0,
                },
            )
            .expect("two distinct points on Earth"),
        );
    }
    app.dispatch_section_renders();

    let jobs = posted_jobs(&posted);
    crate::offload::abandon_worker("test teardown");

    let plan = jobs
        .iter()
        .find(|job| matches!(job, JobRequest::Radar { .. }))
        .expect("the map pane must have dispatched a plan-view render");
    let section = jobs
        .iter()
        .find(|job| matches!(job, JobRequest::Section { .. }))
        .expect("the section pane must have dispatched a cut");

    let plan_declared = declaration_on_the_wire(plan);
    assert_eq!(
        the_drawn_cut(&plan_declared),
        vec![(1, DECLARED_MS)],
        "the plan view's payload does not name the 0.5° cut's declaration, so \
         its NROT unfolds around an estimate; it carries {plan_declared:?}",
    );
    let section_declared = declaration_on_the_wire(section);
    assert_eq!(
        the_drawn_cut(&section_declared),
        the_drawn_cut(&plan_declared),
        "the map and the section fold the same sweep at different speeds; \
         section says {section_declared:?}, plan view says {plan_declared:?}",
    );
}

/// The same statement, one datasource further out: a loop frame of a sweep and
/// the still frame of that sweep fold at the same speed.
///
/// The loop download path used to drop the table outright, on the reasoning
/// that a loop frame is a plan-view raster and nothing on that path
/// interpolates across a fold seam. That was true of the *sampler's* guard and
/// never of the dealiaser: NROT and SRV unfold before they compute anything,
/// on every path that draws them.
#[test]
fn a_loop_frame_and_the_still_frame_beside_it_fold_at_the_same_speed() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    crate::offload::set_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = app_showing_site();
    let ctx = egui::Context::default();
    app.dispatch_pane_renders(&ctx);

    let site = rustdar_radar::sites::get_radar_site(SITE).expect("KTLX is a real radar");
    let params = crate::render_dispatch::RenderParams {
        product: RadarProduct::NormalizedRotation,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };
    let frame = app
        .loop_mgr
        .frame_data(SITE, RadarProduct::NormalizedRotation, &volume_time())
        .expect("the fixture cached this frame's volume");
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume_time(),
            frame,
            params,
            rustdar_egui::pane::RenderTarget {
                site: SITE.to_string(),
                product: RadarProduct::NormalizedRotation,
                elevation: 0.5,
            },
        ),
        "the fixture must actually reach the dispatch",
    );

    let jobs = posted_jobs(&posted);
    crate::offload::abandon_worker("test teardown");

    let radar: Vec<Vec<(u8, f64)>> = jobs
        .iter()
        .filter(|job| matches!(job, JobRequest::Radar { .. }))
        .map(declaration_on_the_wire)
        .collect();
    assert_eq!(radar.len(), 2, "one still frame and one loop frame");
    assert_eq!(
        radar[0], radar[1],
        "a loop frame and the still frame beside it declare different fold \
         limits for one sweep",
    );
    assert_eq!(
        the_drawn_cut(&radar[1]),
        vec![(1, DECLARED_MS)],
        "and the number they agree on has to be the one the cut stated, not \
         two payloads agreeing that nothing was declared",
    );
}
