use crate::platform_double::TestBridge;
use rustdar_radar::types::RadarProduct;
use rustdar_worker::offload::{JobRequest, JobSink};
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";
const DECLARED_MS: f64 = 23.84;
const OTHER_CUT_MS: f64 = 31.35;

struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);

impl JobSink for Recorder {
    fn send(&self, _id: u64, request: JobRequest) -> Result<(), JobRequest> {
        self.0.lock().unwrap().push(request.to_bytes());
        Ok(())
    }
}

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(18, 30, 0)
        .unwrap()
}

fn declared() -> Arc<rustdar_radar::nyquist::DeclaredNyquist> {
    Arc::new(
        [(1, DECLARED_MS), (2, OTHER_CUT_MS)]
            .into_iter()
            .collect::<rustdar_radar::nyquist::DeclaredNyquist>(),
    )
}

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

fn app_showing_site() -> crate::app::App {
    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(RadarProduct::NormalizedRotation, vec![0.5]);
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.set_site(SITE.to_string());
        pane.set_selected_product(rustdar_radar::fields::known::NORMALIZED_ROTATION);
        pane.set_selected_elevation(0.5);
    }
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: rustdar_radar::types::ScanInfo {
                site,
                site_source: rustdar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![RadarProduct::NormalizedRotation],
                product_elevations,
                status: String::new(),
            },
        });
    app.render.ensure_pane_count(1);
    app.scan_data
        .insert(SITE.to_string(), (sample_scan(), declared()));
    app.base_scans
        .insert(SITE.to_string(), (sample_scan(), declared(), volume_time()));
    app.loop_mgr
        .cache_scan(SITE, volume_time(), (sample_scan(), declared()));
    app
}

fn declaration_on_the_wire(job: &JobRequest) -> Vec<(u8, f64)> {
    let input = job
        .job
        .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
        .map(|plan| &plan.input)
        .or_else(|| {
            job.job
                .downcast_ref::<rustdar_radar::jobs::SectionJob>()
                .map(|section| &section.input)
        })
        .unwrap_or_else(|| panic!("expected a payload-carrying job, got {job:?}"));
    input.declared_nyquist().iter().collect()
}

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

#[test]
fn a_plan_view_and_a_section_of_one_sweep_fold_at_the_same_speed() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    rustdar_worker::offload::set_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = app_showing_site();
    let ctx = egui::Context::default();
    app.dispatch_pane_renders(&ctx);

    app.render.pane_render[0].render_finished();
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.set_kind(rustdar_egui::pane::PaneKind::CrossSection);
        pane.set_selected_product(rustdar_radar::fields::known::NORMALIZED_ROTATION);
        pane.cross_section_mut().unwrap().line = Some(
            rustdar_egui::pane::SectionLine::new(
                rustdar_geo::GeoPoint {
                    lat: 35.0,
                    lon: -98.0,
                },
                rustdar_geo::GeoPoint {
                    lat: 36.0,
                    lon: -97.0,
                },
            )
            .expect("two distinct points on Earth"),
        );
    }
    app.dispatch_section_renders();

    let jobs = posted_jobs(&posted);
    rustdar_worker::offload::abandon_worker("test teardown");

    let plan = jobs
        .iter()
        .find(|job| {
            job.job
                .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
                .is_some()
        })
        .expect("the map pane must have dispatched a plan-view render");
    let section = jobs
        .iter()
        .find(|job| {
            job.job
                .downcast_ref::<rustdar_radar::jobs::SectionJob>()
                .is_some()
        })
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

#[test]
fn a_loop_frame_and_the_still_frame_beside_it_fold_at_the_same_speed() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    rustdar_worker::offload::set_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = app_showing_site();
    let ctx = egui::Context::default();
    app.dispatch_pane_renders(&ctx);

    let site = rustdar_radar::sites::get_radar_site(SITE).expect("KTLX is a real radar");
    let params = crate::render_dispatch::RenderParams {
        product: rustdar_radar::types::RadarProduct::NormalizedRotation,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };
    assert!(
        app.loop_mgr
            .frame_data_arrived(SITE, RadarProduct::NormalizedRotation, &volume_time()),
        "the fixture cached this frame's volume",
    );
    assert!(
        app.spawn_loop_frame_render(
            0,
            volume_time(),
            params,
            rustdar_egui::pane::RenderTarget {
                site: SITE.to_string(),
                product: rustdar_radar::fields::known::NORMALIZED_ROTATION,
                elevation: 0.5,
            },
        ),
        "the fixture must actually reach the dispatch",
    );

    let jobs = posted_jobs(&posted);
    rustdar_worker::offload::abandon_worker("test teardown");

    let radar: Vec<Vec<(u8, f64)>> = jobs
        .iter()
        .filter(|job| {
            job.job
                .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
                .is_some()
        })
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
