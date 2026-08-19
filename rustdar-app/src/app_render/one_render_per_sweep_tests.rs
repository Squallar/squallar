//! One sweep is one *render*, however many panes are looking at it.
//!
//! The sibling of `radar_texture_sharing_tests`, one step earlier in the same
//! path. That module removed the duplicate **upload**: several panes holding one
//! `Arc<ColorImage>` used to turn it into one `TextureId` each. This one removes
//! the duplicate **render** that produced the buffer in the first place —
//! `dispatch_pane_renders` walks the panes in a single pass, and the render
//! cache it consults is only written when a result comes *back*, so on the frame
//! a volume lands it missed for every pane at once and each pane started its own
//! job.
//!
//! The observable is the **job posted on the wire**, counted through the same
//! `JobSink` a browser has and read back with the same `from_bytes` a worker
//! uses. A timing would not do: on a 32-core desktop the duplicates run in
//! parallel and the wall-clock latency barely moves, while the CPU and the
//! resident memory they cost move a great deal — and on wasm, where
//! `MAX_CONCURRENT_RENDERS` is 1, they do not run in parallel at all. The count
//! is the property; the cost of one render is not this module's business.
//!
//! **Two regression tests and two invariant guards, and it is worth knowing
//! which is which.** Only `one_sweep_on_several_panes_is_one_render` and
//! `a_render_that_answers_with_nothing_releases_its_siblings` fail if the
//! suppression is taken back out — the first counting two jobs where it wants
//! one, the second at its own precondition. The other two hold in both arms:
//! `the_panes_that_asked_for_nothing_are_served_anyway` pins the broadcast this
//! change leans on rather than the change, and
//! `panes_wanting_different_pictures_each_get_one` pins the key against being
//! coarsened later. Both are guards worth keeping and neither is evidence that
//! the suppression works.

use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::types::RadarProduct;
use rustdar_worker::offload::{JobRequest, JobSink};
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";
const OTHER_SITE: &str = "KMPX";
const TILT: f32 = 0.5;
const OTHER_TILT: f32 = 1.5;

/// A worker port that keeps what it was handed instead of running it.
///
/// Installing one is also what makes these tests deterministic: with a port in
/// place `offload_job` posts and returns, so no render thread is spawned, no
/// rasterizer runs, and the count of jobs is taken at the moment of dispatch
/// rather than raced against workers finishing.
struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);

impl JobSink for Recorder {
    /// Serialises here, as the browser's own sink does, so what these tests
    /// read back has been through `to_bytes`/`from_bytes` exactly as a job
    /// crossing a real worker boundary has. The funnel stopped doing it on
    /// every sink's behalf; a recorder standing in for the browser still must.
    fn send(&self, _id: u64, request: JobRequest) -> Result<(), JobRequest> {
        self.0.lock().unwrap().push(request.to_bytes());
        Ok(())
    }
}

/// Install a recorder, and the guard that retires it however the test ends.
///
/// The guard is not decoration. `WORKER` is a thread-local the harness's
/// threads are reused across, so a failed assertion that unwound past a plain
/// retirement call would leave this port installed for the *next* test on that
/// thread — which would then post its renders into a recorder nobody reads and
/// fail for reasons of its own. See `offload::InstalledTestWorker`.
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

/// A two-cut volume carrying reflectivity and velocity, so an extraction
/// reaches a sweep at either tilt and a real `JobRequest::Radar` is built.
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

/// Aim pane `idx` at `site` showing `product` at `elevation`, far enough along
/// that the dispatcher will act on it, and put the volume where it will look.
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

/// **The finding.** A volume landing on a split is one render, not one per pane.
///
/// Run at every pane count a desktop split reaches, and over both a per-tilt
/// product and one whose payload is the whole velocity ladder — the second
/// because the extraction this suppresses is far larger there, and because a
/// dedupe that keyed on the tilt rather than on the render cache's own key
/// would behave differently for the two.
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
///
/// **An invariant guard, not a regression test.** It holds with the
/// suppression and without it — with it because the broadcast serves the panes
/// that skipped, without it because each pane's own render serves it — so it
/// pins no behaviour this change introduced. It is kept because it pins the
/// half the change *leans* on: the broadcast in `poll_render_results` is the
/// entire reason a suppressed pane is not simply a blank one, and if that half
/// ever narrowed, the saving would silently become three blank panes of a
/// four-pane split until something unrelated re-dispatched them.
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
                .overlay_cache_mut(OverlayKind::Radar)
                .current()
                .is_some(),
            "pane {idx} has no radar texture",
        );
    }
}

/// The suppression is per picture, not per site: two panes wanting two
/// different pictures of one volume still get two renders.
///
/// Both axes the key discriminates on, because a dedupe that collapsed either
/// would leave a pane showing another product's or another tilt's field with
/// nothing saying so.
///
/// **An invariant guard, not a regression test.** Two panes on different
/// pictures got two renders before this change as well — the old path gave
/// every pane its own — so it passes in both arms. What it is here for is the
/// direction this mechanism could still be got wrong in: a key coarsened even
/// slightly, on the raw elevation rather than the cache's slot, say, would
/// suppress a render nothing was going to serve, and the symptom is a pane
/// quietly showing another product's or another tilt's field.
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

/// A render that answers releases the panes it was holding back — including
/// when it answers with nothing.
///
/// This is the pairing the private field exists for. `render_finished` clears
/// the flag and the key together; a path that cleared only the flag would leave
/// every sibling deferring for ever to a render that had already answered, and
/// the symptom would be panes that stay blank until the next volume.
///
/// A render that drew nothing is the case to test it on, because it is the one
/// that leaves no cache entry behind: the siblings cannot be rescued by the
/// cache hit at the top of the dispatch, so if the key were not cleared there
/// would be nothing left to save them.
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
    // And the pane list agrees: with the key gone, the volume is asked for
    // again rather than waited on.
    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        posted(&recorded).len(),
        2,
        "nothing was re-dispatched after the render that drew nothing, so \
         these panes are wedged until the next volume",
    );
}
