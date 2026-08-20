//! The arrival-time extraction cache (WO-E4.9): `RenderInput::extract` moves to volume
//! arrival, and the dispatch serves the walk from the cache instead of paying it on the
//! frame thread.

use rustdar_radar::types::RadarProduct;
use rustdar_worker::offload::{JobRequest, JobSink};
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";
const OTHER_SITE: &str = "KMPX";
const TILT: f32 = 0.5;
const OTHER_TILT: f32 = 1.5;

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

/// The `RenderInput` bytes of each posted plan-view job, in post order.
fn posted_inputs(recorded: &Mutex<Vec<Vec<u8>>>) -> Vec<Vec<u8>> {
    recorded
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| {
            let job = JobRequest::from_bytes(bytes).expect("a job this build posted decodes");
            job.job
                .downcast_ref::<rustdar_radar::jobs::RadarPlanJob>()
                .unwrap_or_else(|| panic!("a plan-view dispatch posted {job:?}"))
                .input
                .to_bytes()
        })
        .collect()
}

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 19)
        .unwrap()
        .and_hms_opt(15, 5, 0)
        .unwrap()
}

/// A two-cut volume carrying reflectivity and velocity at `fill` everywhere, so two fills
/// are two byte-distinct volumes of one shape.
fn sample_scan(fill: u8) -> Arc<nexrad_model::data::Scan> {
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
                    MomentData::from_fixed_point(120, 0, 250, 8, 2.0, 66.0, vec![fill; 120]);
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

/// A declared-Nyquist table with real rows, so the dispatch-time `with_declared_nyquist`
/// stamp is a stamp of substance in the byte pin.
fn declared() -> Arc<rustdar_radar::nyquist::DeclaredNyquist> {
    let mut table = rustdar_radar::nyquist::DeclaredNyquist::empty();
    table.declare(1, 26.0);
    table.declare(2, 28.5);
    Arc::new(table)
}

/// Aim pane `idx` at `site` showing `product` at `elevation`, far enough along that the
/// dispatcher and the arrival hook both act on it.
fn point_at(
    app: &mut crate::app::App,
    idx: usize,
    site: &str,
    product: RadarProduct,
    elevation: f32,
    fill: u8,
) {
    let radar = rustdar_radar::sites::get_radar_site(site)
        .unwrap_or_else(|| panic!("{site} is in the test site table"))
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, vec![TILT, OTHER_TILT]);
    {
        let pane = app.gui.pane_mut(idx).expect("pane exists");
        pane.set_site(site.to_string());
        pane.set_selected_product(product);
        pane.set_selected_elevation(elevation);
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
    app.scan_data
        .insert(site.to_string(), (sample_scan(fill), declared()));
}

/// Drain the (native, asynchronous) arrival-time extraction until `want` payloads are
/// resident — the pump row's job, done by hand because a test has no frame loop.
fn wait_for_extracts(app: &mut crate::app::App, want: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.render.poll_extract_results();
        if app.render.extract_cache_len() >= want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the arrival-time extraction never homed over the dispatcher's channel",
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// one pane showing the site, the following dispatch performs ZERO frame-thread extractions
/// — and still posts the render job.
#[test]
fn an_arrival_populated_cache_makes_the_dispatch_extraction_free() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity, TILT, 200);

    app.refresh_extract_cache_for_site(SITE);
    wait_for_extracts(&mut app, 1);

    app.render.plan_view_extractions.set(0);
    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        app.render.plan_view_extractions.get(),
        0,
        "the dispatch paid a frame-thread extraction the arrival had \
         already performed off-thread",
    );
    assert_eq!(
        posted_inputs(&recorded).len(),
        1,
        "a cache hit must still dispatch the render itself",
    );
}

/// frame-thread extraction — today's path, pinned as the fallback.
#[test]
fn a_cold_cache_pays_exactly_one_dispatch_extraction() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity, TILT, 200);

    app.render.plan_view_extractions.set(0);
    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        app.render.plan_view_extractions.get(),
        1,
        "the entry-less dispatch is today's inline extraction, exactly once",
    );
    assert_eq!(posted_inputs(&recorded).len(), 1);
}

#[test]
fn an_arrival_populates_only_the_sites_shown_panes() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(2, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity, TILT, 200);
    point_at(&mut app, 1, OTHER_SITE, RadarProduct::Velocity, TILT, 180);

    app.refresh_extract_cache_for_site(SITE);
    wait_for_extracts(&mut app, 1);
    // Settle: nothing else may trickle in for the other pane.
    std::thread::sleep(std::time::Duration::from_millis(50));
    app.render.poll_extract_results();
    assert_eq!(
        app.render.extract_cache_len(),
        1,
        "an arrival for one site populated another pane's tuple — the hook \
         is iterating something other than the site's shown panes",
    );
    assert_eq!(
        app.render.extract_cache_sites(),
        vec![SITE.to_string()],
        "an arrival for one site populated another site's tuple — the hook \
         is keying something other than the arrival site",
    );

    // And the payload under the site's key is the SITE's extraction: the pane 0 dispatch
    // must post KTLX's gates (fill 200), byte for byte — never the other pane's volume
    // filed under this key.
    app.render.plan_view_extractions.set(0);
    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        app.render.plan_view_extractions.get(),
        1,
        "precondition: exactly the other-site pane misses (its site was \
         never populated); the site's own pane must hit",
    );
    let site_radar = rustdar_radar::sites::get_radar_site(SITE).unwrap();
    let fresh = rustdar_radar::render_input::RenderInput::extract(
        &sample_scan(200),
        TILT,
        RadarProduct::Reflectivity,
        site_radar.lat,
        site_radar.lon,
        None,
        None,
    )
    .expect("the fixture extracts")
    .with_declared_nyquist(&declared())
    .with_srv_fallback(rustdar_radar::srv::SrvFallback::default())
    .with_melting_layer_product(None)
    .with_rpg_storm_motion(None)
    .to_bytes();
    let posted = posted_inputs(&recorded);
    assert!(
        posted.contains(&fresh),
        "the payload cached under the site's key is not the site's own \
         extraction — another pane's volume was filed under it",
    );
}

/// a fresh dispatch-time extraction posts — cached-then-stamped == freshly-extracted-then-
/// stamped, `RenderInput::to_bytes` compared whole.
#[test]
fn a_cached_then_stamped_payload_is_the_fresh_then_stamped_bytes() {
    // Velocity, so the declared-Nyquist stamp is substance, not a no-op.
    let hit_bytes = {
        let (recorded, _worker) = recorder();
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::n_pane_app(1, SITE);
        point_at(&mut app, 0, SITE, RadarProduct::Velocity, TILT, 200);
        app.refresh_extract_cache_for_site(SITE);
        wait_for_extracts(&mut app, 1);
        app.render.plan_view_extractions.set(0);
        app.dispatch_pane_renders(&ctx);
        assert_eq!(
            app.render.plan_view_extractions.get(),
            0,
            "precondition: this arm must be served from the cache",
        );
        posted_inputs(&recorded).remove(0)
    };
    let miss_bytes = {
        let (recorded, _worker) = recorder();
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::n_pane_app(1, SITE);
        point_at(&mut app, 0, SITE, RadarProduct::Velocity, TILT, 200);
        app.dispatch_pane_renders(&ctx);
        posted_inputs(&recorded).remove(0)
    };
    assert_eq!(
        hit_bytes, miss_bytes,
        "a cached-then-stamped payload posted different bytes than a \
         freshly-extracted-then-stamped one",
    );
}

#[test]
fn a_second_arrival_never_serves_the_previous_snapshots_payload() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity, TILT, 200);
    app.refresh_extract_cache_for_site(SITE);
    wait_for_extracts(&mut app, 1);

    // The next round of the live feed: same volume identity, more data.
    app.scan_data
        .insert(SITE.to_string(), (sample_scan(90), declared()));
    app.refresh_extract_cache_for_site(SITE);
    assert_eq!(
        app.render.extract_cache_len(),
        0,
        "the stale payload must be dropped BEFORE the rebuild homes — the \
         gap belongs to the miss fallback, never to a stale hit",
    );
    wait_for_extracts(&mut app, 1);

    app.dispatch_pane_renders(&ctx);
    let posted = posted_inputs(&recorded).remove(0);
    let fresh = rustdar_radar::render_input::RenderInput::extract(
        &sample_scan(90),
        TILT,
        RadarProduct::Reflectivity,
        rustdar_radar::sites::get_radar_site(SITE).unwrap().lat,
        rustdar_radar::sites::get_radar_site(SITE).unwrap().lon,
        None,
        None,
    )
    .expect("the fixture extracts")
    .with_declared_nyquist(&declared())
    .with_srv_fallback(rustdar_radar::srv::SrvFallback::default())
    .with_melting_layer_product(None)
    .with_rpg_storm_motion(None)
    .to_bytes();
    assert_eq!(
        posted, fresh,
        "the dispatch served the previous snapshot's bytes after a new \
         arrival for the site",
    );
}
