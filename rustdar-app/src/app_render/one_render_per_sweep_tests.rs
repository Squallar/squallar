//! One sweep is one *render*, however many panes are looking at it.

use rustdar_radar::types::RadarProduct;
use rustdar_source::id::known;
use rustdar_worker::offload::{JobRequest, JobSink};
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";
const OTHER_SITE: &str = "KMPX";
const TILT: f32 = 0.5;
const OTHER_TILT: f32 = 1.5;

/// A worker port that keeps what it was handed instead of running it.
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

/// Install a recorder, and the guard that retires it however the test ends.
fn recorder() -> (
    Arc<Mutex<Vec<Vec<u8>>>>,
    rustdar_worker::offload::InstalledTestWorker,
) {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let installed =
        rustdar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));
    (posted, installed)
}

/// Every rasterizing job posted so far, decoded.
fn posted(recorded: &Mutex<Vec<Vec<u8>>>) -> Vec<JobRequest> {
    recorded
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| JobRequest::from_bytes(bytes).expect("a job this build posted decodes"))
        .collect()
}

/// The `(product, elevation)` of each posted plan-view job, in post order.
fn asked_for(recorded: &Mutex<Vec<Vec<u8>>>) -> Vec<(RadarProduct, f32)> {
    posted(recorded)
        .iter()
        .map(|job| {
            let plan = job
                .job
                .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
                .unwrap_or_else(|| panic!("a plan-view dispatch posted {job:?}"));
            (plan.input.product(), plan.input.elevation())
        })
        .collect()
}

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 12)
        .unwrap()
        .and_hms_opt(15, 5, 0)
        .unwrap()
}

/// A two-cut volume carrying reflectivity and velocity, so an extraction reaches a sweep at
/// either tilt and a real `JobRequest::Radar` is built.
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
            vec![cut(TILT as f64), cut(OTHER_TILT as f64)],
        ),
        vec![sweep(1, TILT), sweep(2, OTHER_TILT)],
    ))
}

/// Aim pane `idx` at `site` showing `product` at `elevation`, far enough along that the
/// dispatcher will act on it, and put the volume where it will look.
fn point_at(
    app: &mut crate::app::App,
    idx: usize,
    site: &str,
    product: RadarProduct,
    elevation: f32,
) {
    let radar = rustdar_radar::sites::get_radar_site(site)
        .unwrap_or_else(|| panic!("{site} is in the test site table"))
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, vec![TILT, OTHER_TILT]);
    {
        let pane = app.gui.pane_mut(idx).expect("pane exists");
        pane.site = site.to_string();
        pane.selected_product = product;
        pane.selected_elevation = elevation;
    }
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: idx,
            info: rustdar_radar::types::ScanInfo {
                site: radar,
                site_source: rustdar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![product],
                product_elevations,
                status: String::new(),
            },
        });
    app.scan_data.insert(
        site.to_string(),
        (
            sample_scan(),
            Arc::new(rustdar_radar::nyquist::DeclaredNyquist::empty()),
        ),
    );
}

/// `n` map panes, each aimed by `aim`.
fn app_with(n: usize, aim: impl Fn(&mut crate::app::App, usize)) -> crate::app::App {
    let mut app = crate::app::tests::n_pane_app(n, SITE);
    for idx in 0..n {
        aim(&mut app, idx);
    }
    app
}

/// `n` panes all showing one site, product and tilt.
fn app_on_one_sweep(n: usize, product: RadarProduct) -> crate::app::App {
    app_with(n, |app, idx| point_at(app, idx, SITE, product, TILT))
}

#[test]
fn one_sweep_on_several_panes_is_one_render() {
    for product in [RadarProduct::Reflectivity, RadarProduct::Velocity] {
        for panes in [1, 2, 4] {
            let (recorded, _worker) = recorder();
            let ctx = egui::Context::default();
            let mut app = app_on_one_sweep(panes, product);

            app.dispatch_pane_renders(&ctx);

            assert_eq!(
                asked_for(&recorded),
                vec![(product, TILT)],
                "{panes} panes on one {product:?} sweep posted {} jobs; every \
                 one past the first rasterizes a buffer the broadcast in \
                 `poll_render_results` is about to hand them anyway",
                asked_for(&recorded).len(),
            );
            assert!(
                app.render.pane_render[0].render_in_flight(),
                "the pane that asked is not marked as having asked",
            );
            for idx in 1..panes {
                assert!(
                    !app.render.pane_render[idx].render_in_flight(),
                    "pane {idx} started a render of its own",
                );
            }
        }
    }
}

/// And the panes that asked for nothing still get a picture.
#[test]
fn the_panes_that_asked_for_nothing_are_served_anyway() {
    const PANES: usize = 4;
    let (_recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_one_sweep(PANES, RadarProduct::Reflectivity);
    app.dispatch_pane_renders(&ctx);

    // The one render answering, on the channel the worker's reply arrives on.
    let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [2, 2],
        &[9, 8, 7, 180].repeat(4),
    ));
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                image: Arc::clone(&image),
                max_range_km: 230.0,
                hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
            }),
            product: RadarProduct::Reflectivity,
            elevation: TILT,
            generation: app.render.render_generation,
            pane_idx: 0,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
    app.poll_render_results(&ctx);

    for idx in 0..PANES {
        assert_eq!(
            app.render.pane_render[idx].last_rendered,
            Some((RadarProduct::Reflectivity, TILT)),
            "pane {idx} was suppressed and then never served",
        );
        assert!(
            app.gui
                .pane_mut(idx)
                .expect("pane exists")
                .overlay_cache_mut(&known::RADAR)
                .current()
                .is_some(),
            "pane {idx} has no radar texture",
        );
    }
}

/// The suppression is per picture, not per site: two panes wanting two different pictures
/// of one volume still get two renders.
#[test]
fn panes_wanting_different_pictures_each_get_one() {
    for (label, aim) in [
        (
            "products",
            &(|app: &mut crate::app::App, idx: usize| {
                let product = if idx == 0 {
                    RadarProduct::Reflectivity
                } else {
                    RadarProduct::Velocity
                };
                point_at(app, idx, SITE, product, TILT);
            }) as &dyn Fn(&mut crate::app::App, usize),
        ),
        ("tilts", &|app: &mut crate::app::App, idx: usize| {
            let tilt = if idx == 0 { TILT } else { OTHER_TILT };
            point_at(app, idx, SITE, RadarProduct::Reflectivity, tilt);
        }),
        ("sites", &|app: &mut crate::app::App, idx: usize| {
            let site = if idx == 0 { SITE } else { OTHER_SITE };
            point_at(app, idx, site, RadarProduct::Reflectivity, TILT);
        }),
    ] {
        let (recorded, _worker) = recorder();
        let ctx = egui::Context::default();
        let mut app = app_with(2, aim);

        app.dispatch_pane_renders(&ctx);

        assert_eq!(
            posted(&recorded).len(),
            2,
            "two panes on different {label} were served one render, so one of \
             them is showing the other's picture",
        );
    }
}

/// A render that answers releases the panes it was holding back — including when it answers
/// with nothing.
#[test]
fn a_render_that_answers_with_nothing_releases_its_siblings() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_one_sweep(2, RadarProduct::Reflectivity);

    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        posted(&recorded).len(),
        1,
        "precondition: one render started"
    );

    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: None,
            product: RadarProduct::Reflectivity,
            elevation: TILT,
            generation: app.render.render_generation,
            pane_idx: 0,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
    app.poll_render_results(&ctx);

    assert!(
        !app.render
            .plan_view_in_flight(SITE, RadarProduct::Reflectivity, TILT),
        "the render has answered and the key it was dispatched under is still \
         set, so every pane wanting this picture will defer for ever to a \
         render that finished",
    );
    // And the pane list agrees: with the key gone, the volume is asked for again rather
    // than waited on.
    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        posted(&recorded).len(),
        2,
        "nothing was re-dispatched after the render that drew nothing, so \
         these panes are wedged until the next volume",
    );
}
