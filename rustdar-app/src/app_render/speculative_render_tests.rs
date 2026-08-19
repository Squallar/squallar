//! The adjacent-tilt pre-render (WO-E4.10): after a static plan view
//! DELIVERS, one speculative render of the nearest tilt above (else below)
//! goes into the existing RenderCache — never marking a pane, never taking a
//! pane render slot, never more than one at a time, and never on wasm (AF8)
//! or under a small budget.

use rustdar_radar::types::RadarProduct;
use rustdar_worker::offload::{JobRequest, JobSink};
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;
const MID_TILT: f32 = 1.5;
const TOP_TILT: f32 = 2.4;

/// The recorder `one_render_per_sweep_tests` installs, for the same reason:
/// jobs are counted at the moment of dispatch, never raced against workers.
struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);

impl JobSink for Recorder {
    fn send(&self, _id: u64, request: JobRequest) -> Result<(), JobRequest> {
        self.0.lock().unwrap().push(request.to_bytes());
        Ok(())
    }
}

fn recorder() -> (
    Arc<Mutex<Vec<Vec<u8>>>>,
    rustdar_worker::offload::InstalledTestWorker,
) {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let installed =
        rustdar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));
    (posted, installed)
}

/// The `(product, elevation, values_wanted)` of each posted plan job.
fn asked_for(recorded: &Mutex<Vec<Vec<u8>>>) -> Vec<(RadarProduct, f32, bool)> {
    recorded
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| {
            let job = JobRequest::from_bytes(bytes).expect("a job this build posted decodes");
            let plan = job
                .job
                .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
                .unwrap_or_else(|| panic!("a speculative dispatch posted {job:?}"));
            (
                plan.input.product(),
                plan.input.elevation(),
                plan.values_wanted,
            )
        })
        .collect()
}

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 19)
        .unwrap()
        .and_hms_opt(16, 10, 0)
        .unwrap()
}

/// A three-cut reflectivity volume, so "nearest above, else below" has real
/// choices at the bottom, middle and top of the ladder.
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
            vec![cut(TILT as f64), cut(MID_TILT as f64), cut(TOP_TILT as f64)],
        ),
        vec![sweep(1, TILT), sweep(2, MID_TILT), sweep(3, TOP_TILT)],
    ))
}

/// Aim pane 0 at the site with the three-tilt ladder and put the volume
/// where the speculative spawn will look.
fn aim(app: &mut crate::app::App, product: RadarProduct, elevation: f32) {
    let radar = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is in the test site table")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, vec![TILT, MID_TILT, TOP_TILT]);
    {
        let pane = app.gui.pane_mut(0).expect("pane exists");
        pane.site = SITE.to_string();
        pane.selected_product = product;
        pane.selected_elevation = elevation;
    }
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
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
        SITE.to_string(),
        (
            sample_scan(),
            Arc::new(rustdar_radar::nyquist::DeclaredNyquist::empty()),
        ),
    );
}

/// A finished interactive render for pane 0, put on the channel.
fn post_interactive(app: &mut crate::app::App, product: RadarProduct, elevation: f32) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                image: Arc::new(egui::ColorImage::filled([4, 4], egui::Color32::TRANSPARENT)),
                max_range_km: 230.0,
                hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
            }),
            product,
            elevation,
            generation: app.render.render_generation,
            pane_idx: 0,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
}

/// A finished SPECULATIVE render for the site, put on the channel — what
/// `spawn_speculative_render`'s deliver sends when the raster is done.
fn post_speculative(app: &mut crate::app::App, product: RadarProduct, elevation: f32) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                image: Arc::new(egui::ColorImage::filled([4, 4], egui::Color32::TRANSPARENT)),
                max_range_km: 230.0,
                hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
            }),
            product,
            elevation,
            generation: app.render.render_generation,
            pane_idx: usize::MAX,
            speculative_for: Some(SITE.to_string()),
        })
        .expect("the receiver lives on the App");
}

/// **Ordered test (a).** A speculative dispatch never sets `render_in_flight`
/// on any pane — and it really did dispatch: one job, for the nearest tilt
/// above the delivered one, with `values_wanted: true` (it becomes the
/// pane's static render on tilt-step; values-stripped would kill hover).
#[test]
fn a_speculative_dispatch_never_marks_a_pane_in_flight() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    aim(&mut app, RadarProduct::Reflectivity, TILT);

    post_interactive(&mut app, RadarProduct::Reflectivity, TILT);
    app.poll_render_results(&ctx);

    assert_eq!(
        asked_for(&recorded),
        vec![(RadarProduct::Reflectivity, MID_TILT, true)],
        "the delivered 0.5° plan view must speculate exactly the nearest \
         tilt above, values kept",
    );
    assert!(
        app.render.speculative_in_flight(),
        "the one-at-a-time bool must be set while the speculative is out",
    );
    for (idx, prs) in app.render.pane_render.iter().enumerate() {
        assert!(
            !prs.render_in_flight(),
            "pane {idx} was marked in flight by a speculative dispatch — \
             it would refuse its own interactive renders until reset",
        );
    }
    assert!(
        !app.render
            .plan_view_in_flight(SITE, RadarProduct::Reflectivity, MID_TILT),
        "a speculative render must not claim the plan-view dedupe key — a \
         sibling pane wanting that picture interactively must not defer to it",
    );
}

/// **Ordered test (b).** The platform/budget gate, host-tested on BOTH arms
/// of the WEB const: wasm never speculates whatever the budget claims, and
/// `concurrent_renders <= 2` never speculates on any platform — plus the
/// integration arm: a dispatcher rebuilt at budget 2 posts zero speculative
/// jobs on the same delivery that posted one at the desktop budget.
#[test]
fn a_small_budget_or_the_web_never_speculates() {
    use crate::render_dispatch::speculative_render_allowed;
    // The parameterized fn, both arms — wasm (true) refused at every budget,
    // native (false) admitted only past 2.
    for budget in [0, 1, 2, 3, 6, 64] {
        assert!(
            !speculative_render_allowed(true, budget),
            "wasm must never speculate (AF8) — budget {budget} was admitted",
        );
        assert_eq!(
            speculative_render_allowed(false, budget),
            budget > 2,
            "native speculation is exactly `concurrent_renders > 2` — \
             budget {budget} answered wrongly",
        );
    }

    // Integration: the same delivery that speculates at the desktop budget
    // dispatches NOTHING when the resolved budget is 2.
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    let mut budgets = rustdar_device_profile::budget::resolve(
        &rustdar_device_profile::budget::DeviceProfile::for_target(),
    );
    budgets.concurrent_renders = 2;
    app.render = crate::render_dispatch::RenderDispatcher::with_budgets(&budgets);
    aim(&mut app, RadarProduct::Reflectivity, TILT);

    post_interactive(&mut app, RadarProduct::Reflectivity, TILT);
    app.poll_render_results(&ctx);
    assert_eq!(
        asked_for(&recorded),
        vec![],
        "a budget of 2 must never speculate — both slots belong to \
         interactive work",
    );
    assert!(!app.render.speculative_in_flight());
}

/// **Ordered test (c).** At most one speculative in flight: a second
/// delivery while one is out dispatches nothing; the speculative result's
/// arrival lands in the RenderCache ONLY (no pane touched), clears the bool,
/// and re-arms speculation for the next delivery.
#[test]
fn at_most_one_speculative_is_ever_in_flight() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    aim(&mut app, RadarProduct::Reflectivity, TILT);

    // First delivery speculates 1.5°; the second finds one out and defers.
    post_interactive(&mut app, RadarProduct::Reflectivity, TILT);
    app.poll_render_results(&ctx);
    post_interactive(&mut app, RadarProduct::Reflectivity, TILT);
    app.poll_render_results(&ctx);
    assert_eq!(
        asked_for(&recorded).len(),
        1,
        "a second delivery while a speculative is out must dispatch nothing",
    );

    // The speculative answers: RenderCache gains the tilt, no pane moved,
    // the bool clears. The recorder holds jobs instead of running them, so
    // the deliver closure that would drop the RenderGuard never runs — the
    // store below stands in for that unwind, exactly as the budget suites'
    // own stores do.
    post_speculative(&mut app, RadarProduct::Reflectivity, MID_TILT);
    app.poll_render_results(&ctx);
    app.render
        .renders_in_flight
        .store(0, std::sync::atomic::Ordering::Relaxed);
    assert!(
        !app.render.speculative_in_flight(),
        "the speculative deliver must clear the one-at-a-time bool",
    );
    assert!(
        app.render
            .get_cached_render(
                SITE,
                RadarProduct::Reflectivity,
                rustdar_radar::types::RenderView::PlanView,
                MID_TILT,
            )
            .is_some(),
        "the speculative deliver must insert into the RenderCache — that \
         insert IS the feature",
    );
    for (idx, prs) in app.render.pane_render.iter().enumerate() {
        assert!(
            !prs.render_in_flight(),
            "pane {idx} moved on a speculative delivery",
        );
    }

    // Re-armed: a delivery at 1.5° speculates its neighbour above (2.4°) —
    // the 0.5° below is skipped because above wins, and 1.5° itself is now
    // cached.
    post_interactive(&mut app, RadarProduct::Reflectivity, MID_TILT);
    app.poll_render_results(&ctx);
    let asked = asked_for(&recorded);
    assert_eq!(
        asked.len(),
        2,
        "a cleared bool must re-arm speculation on the next delivery",
    );
    assert_eq!(
        (asked[1].0, asked[1].1),
        (RadarProduct::Reflectivity, TOP_TILT),
        "the 1.5° delivery must speculate the nearest tilt above it",
    );

    // And the top of the ladder speculates the nearest BELOW — but 1.5° is
    // already cached, so nothing dispatches: the goal state needs no work.
    // (The guard stand-in again, so the residency check is the ONLY gate
    // this arm can refuse on — a raised counter would refuse too and make
    // the assertion vacuous.)
    post_speculative(&mut app, RadarProduct::Reflectivity, TOP_TILT);
    app.poll_render_results(&ctx);
    app.render
        .renders_in_flight
        .store(0, std::sync::atomic::Ordering::Relaxed);
    post_interactive(&mut app, RadarProduct::Reflectivity, TOP_TILT);
    app.poll_render_results(&ctx);
    assert_eq!(
        asked_for(&recorded).len(),
        2,
        "a target already resident in the RenderCache must dispatch nothing",
    );
}
