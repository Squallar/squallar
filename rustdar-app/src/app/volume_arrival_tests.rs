//! **The arrival path: a 3D pane's volume is built on the frame it lands**
//! (WO-M14c), not on the frame after the draw loop notices.
//!
//! The subject is a **moment**, not a mechanism. Everything dispatched here is
//! work the draw-time level-trigger would have dispatched a frame or more
//! later, through the identical call; the level-trigger stays exactly where it
//! is as the fallback, and `Building` dedupe makes the two converge. So most
//! of these tests are about what gets **nothing**, because the boundary — the
//! same work moved earlier, never speculation — is the whole order.

use super::tests::headless;
use super::*;
use crate::platform_double::TestBridge;
use rustdar_egui::CurrentVolumeStamp;
use rustdar_egui::pane::{VolumeStamp, VolumeTarget};
use rustdar_egui::radar_layer;
use rustdar_source::id::known;

const SITE: &str = "KTLX";
const OTHER: &str = "KOUN";

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
        .expect("a real date")
        .and_hms_opt(18, minute, 0)
        .expect("a real time")
}

fn arrival(site: &str, minute: u32) -> HashMap<String, CurrentVolumeStamp> {
    HashMap::from([(
        site.to_owned(),
        CurrentVolumeStamp {
            newest: at(minute),
            base_started: Some(at(0)),
        },
    )])
}

/// A painter, so the arrival path is not refused for having nothing that
/// could draw the grid. Built exactly as `install_volume_painter` builds it,
/// against a `VolumeSupport` this headless build states rather than probes.
fn give_it_a_painter(app: &mut App) {
    app.volume_painter = Some(Arc::new(
        rustdar_volumetric::bridge::BridgeVolumePainter::new(
            Arc::clone(&app.volume_store),
            rustdar_device_profile::quality::VolumeQuality::BEST,
            app.budgets.offscreen_bytes,
            rustdar_volumetric::VolumeSupport::Supported,
        ),
    ));
}

/// A vertical field that is **not** the one a freshly built app's radar slot
/// config already names.
///
/// It matters: `set_selected_product` is a plain assignment, and the slot the
/// handler answers `current_field` out of is only made current by the
/// hydrate. On a fresh build that slot already says `Reflectivity` — so a
/// fixture that selected `Reflectivity` would agree with the stale slot and
/// could not tell "read the pane's current selection" from "read whatever the
/// file said", which is precisely the mechanism WO-M14b-1 needed three
/// fixtures to expose.
fn field() -> rustdar_source::product::FieldId {
    let field = rustdar_radar::fields::known::VELOCITY;
    assert!(
        rustdar_radar::fields::spec_for(&field)
            .expect("a registered field")
            .vertical,
        "fixture precondition: the field must be registered as vertical, or \
         the walk refuses it for the wrong reason",
    );
    field
}

/// Make `pane_idx` a live 3D pane on `site`, with its radar layer on and
/// showing `field` — the state the level-trigger would dispatch from.
fn volume_pane_on(
    app: &mut App,
    pane_idx: usize,
    site: &str,
    field: &rustdar_source::product::FieldId,
) {
    let pane = app.gui.pane_mut(pane_idx).expect("the pane exists");
    pane.set_site(site.to_owned());
    pane.set_view(rustdar_radar::types::RenderView::Volume);
    pane.set_selected_product(field.clone());
    pane.set_overlay_enabled(known::RADAR, true);
    assert!(
        pane.volume().is_some_and(|v| v.rendered_for.is_none()),
        "fixture precondition: a 3D pane with no build behind it",
    );
}

/// What the pane's radar slot **currently publishes** as its field — stale
/// until a hydrate runs, which is the thing several assertions below turn on.
fn published_field(app: &App, pane_idx: usize) -> Option<rustdar_source::product::FieldId> {
    let pane = app.gui.pane(pane_idx).expect("the pane exists");
    let view = pane.view(pane_idx);
    app.gui
        .overlays
        .handler_by_id(&known::RADAR)
        .expect("this build serves the radar layer")
        .current_field(&view.layer(&known::RADAR))
}

/// The target the pane above is about when `site`'s merge is newest at
/// `minute` — built through the pane's own function, so a fixture cannot
/// disagree with production about which grid this is.
fn expected_target(
    app: &App,
    pane_idx: usize,
    minute: u32,
    field: &rustdar_source::product::FieldId,
) -> VolumeTarget {
    let pane = app.gui.pane(pane_idx).expect("the pane exists");
    let (stamp, _) = pane
        .volume_stamp(Some(CurrentVolumeStamp {
            newest: at(minute),
            base_started: Some(at(0)),
        }))
        .expect("the fixture arrival gives a stamp");
    pane.volume_target_for(field, stamp)
}

/// **The ask a just-arrived volume produces** — a live 3D pane on the site is
/// dispatched, naming the layer its own walk landed on and the volume that
/// just landed.
#[test]
fn a_volume_arriving_produces_the_ask_the_draw_time_trigger_would_have_made() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(&mut app, 0, SITE, &field());
    let region = rustdar_egui::pane::VolumeRegion::new(
        rustdar_geo::GeoPoint {
            lat: 35.33,
            lon: -97.28,
        },
        rustdar_radar::voxel::HalfExtentKm {
            east_km: 30.0,
            north_km: 20.0,
        },
    )
    .expect("a fixture region must be on Earth with a finite extent");
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .volume_mut()
        .expect("a 3D pane")
        .region = Some(region);
    assert_ne!(
        published_field(&app, 0),
        Some(field()),
        "fixture precondition: the radar slot must still publish the field \
         the app was BUILT with, not the one this pane has selected. Without \
         this the walk's hydrate is invisible — it is what makes the slot \
         current — and every assertion below would pass on a stale slot that \
         happened to agree",
    );
    let want = expected_target(&app, 0, 6, &field());
    assert_eq!(
        want.region,
        Some(region),
        "fixture: the aimed box is carried"
    );

    let asks = app.arrived_volume_asks(&arrival(SITE, 6));

    assert_eq!(
        asks,
        vec![(0, known::RADAR, want)],
        "the arriving volume produced no ask, named a layer the pane's own \
         walk did not land on, asked for the field the slot was stale at, or \
         dropped the box the pane is aimed at",
    );
}

/// **A pane not in Volume mode gets nothing.** Anti-findings AF2 and C5 are
/// rejected by name: no likely-next-product build, no guess at a mode this
/// pane might switch to. It is on the same site, at the same moment, with the
/// same field selected — the only difference is the mode.
#[test]
fn a_pane_that_is_not_in_volume_mode_gets_nothing() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    assert_eq!(
        app.arrived_volume_asks(&arrival(SITE, 6)).len(),
        1,
        "precondition: in Volume mode the same pane is asked for",
    );

    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .set_view(rustdar_radar::types::RenderView::PlanView);

    assert!(
        app.arrived_volume_asks(&arrival(SITE, 6)).is_empty(),
        "a plan-view pane was given a 3D build it would never draw — that is \
         speculation, and this order builds only what the draw loop was \
         already going to build",
    );
}

/// **A 3D pane on another site gets nothing**: the arrival is not its volume,
/// and building for it would be a grid of a radar nobody is looking at.
#[test]
fn a_3d_pane_on_another_site_gets_nothing() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        OTHER,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    assert!(app.arrived_volume_asks(&arrival(SITE, 6)).is_empty());
    assert_eq!(
        app.arrived_volume_asks(&arrival(OTHER, 6)).len(),
        1,
        "control: the same pane IS asked for when its own site's volume \
         arrives, so the assertion above is about the site and not about the \
         fixture being inert",
    );
}

/// **A navigated pane gets nothing from a live arrival.** It is looking at an
/// older scan; the volume that just landed is not the one on its screen, and
/// dragging it forward would move the picture under the reader.
#[test]
fn a_navigated_3d_pane_gets_nothing_from_a_live_arrival() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    assert_eq!(
        app.arrived_volume_asks(&arrival(SITE, 6)).len(),
        1,
        "precondition: live, the same pane is asked for",
    );

    let pane = app.gui.pane_mut(0).expect("pane 0");
    pane.viewing_live = false;
    pane.scan_info = Some(rustdar_radar::types::ScanInfo {
        site: rustdar_radar::sites::get_radar_site(SITE)
            .expect("a known fixture site")
            .clone(),
        site_source: rustdar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp: at(3),
        vcp_number: 212,
        available_products: Vec::new(),
        product_elevations: HashMap::new(),
        status: String::new(),
    });

    assert!(
        app.arrived_volume_asks(&arrival(SITE, 6)).is_empty(),
        "a pane that had stepped back to 18:03 was dispatched for the 18:06 \
         volume it is not showing",
    );
}

/// **A pane the layout is not showing gets nothing.** It is the same
/// denominator `release_hidden_pane_volumes` uses: a hidden pane's grid is
/// being given back on this very frame, so building one for it would be work
/// undone a few microseconds later.
#[test]
fn a_pane_the_layout_hides_gets_nothing() {
    let mut app = super::tests::two_pane_app(SITE, SITE);
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        1,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    assert_eq!(
        app.arrived_volume_asks(&arrival(SITE, 6))
            .iter()
            .map(|(idx, _, _)| *idx)
            .collect::<Vec<_>>(),
        vec![1],
        "precondition: while it is visible, pane 1 is the one asked for",
    );

    let store = rustdar_kv::MemoryKvStore::default();
    rustdar_kv::KvStore::store(
        &store,
        rustdar_egui::UI_CONFIG_KEY,
        &format!(r#"{{"pane_count":1,"site":"{SITE}","panes":[{{"site":"{SITE}"}}]}}"#),
    )
    .expect("the memory store always accepts a write");
    assert!(app.gui.load_ui_config(&store), "the one-pane config parsed");
    assert_eq!(
        app.gui.panes().len(),
        1,
        "precondition: the layout now shows one pane",
    );
    assert_eq!(
        app.gui.remembered_pane_count(),
        2,
        "precondition: the hidden 3D pane is still in the vector — if it were \
         dropped this test would pass for the wrong reason",
    );

    assert!(
        app.arrived_volume_asks(&arrival(SITE, 6)).is_empty(),
        "a hidden pane was dispatched a build the very frame its grid is \
         released",
    );
}

/// **No painter, no build.** Every headless machine, every suspended app and
/// every lost surface is in this state, and the 3D arm returns its empty
/// state before ever reaching the level-trigger there — so dispatching would
/// make the eager set larger than the draw-time set it is supposed to be a
/// re-timing of.
#[test]
fn a_build_nothing_could_draw_is_not_started() {
    let mut app = headless(TestBridge::desktop());
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    assert!(
        app.volume_painter.is_none(),
        "precondition: a headless app has no painter",
    );
    assert!(app.arrived_volume_asks(&arrival(SITE, 6)).is_empty());

    give_it_a_painter(&mut app);
    assert_eq!(
        app.arrived_volume_asks(&arrival(SITE, 6)).len(),
        1,
        "control: the painter is the only thing that changed, so the refusal \
         above is about the painter",
    );
}

/// **The bookkeeping is never bypassed.** The eager dispatch goes through the
/// identical `handle_prepare_volume` the action path uses, so `rendered_for`
/// is written and the level-trigger quiesces — and a second arrival of the
/// *same* volume produces nothing at all.
#[test]
fn an_eager_build_marks_the_pane_and_the_trigger_quiesces() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    app.volumes.install_base(
        SITE.to_owned(),
        (crate::volume_fixture::ready_scan(), Arc::default(), at(6)),
    );
    let stamp = app
        .current_volume_stamp(SITE)
        .expect("the fixture volume gives the site a current stamp");
    let want = app.gui.pane(0).expect("pane 0").volume_target_for(
        &rustdar_radar::fields::known::REFLECTIVITY,
        VolumeStamp {
            site: SITE.to_owned(),
            collected: stamp.newest,
        },
    );

    let arrived = app.publish_base_volumes();
    assert_eq!(
        arrived.get(SITE).map(|s| s.newest),
        Some(stamp.newest),
        "precondition: the site's stamp moved on this frame, which is the \
         arrival this order fires on",
    );
    app.dispatch_arrived_volumes(&arrived);

    assert_eq!(
        app.gui
            .pane(0)
            .and_then(|p| p.volume())
            .and_then(|v| v.rendered_for.clone()),
        Some(want.clone()),
        "the eager dispatch did not mark the pane, so the draw loop would ask \
         for the same volume again on the very next frame",
    );
    assert!(
        !app.gui.pane(0).expect("pane 0").volume_build_due(&want),
        "the level-trigger is still armed for a volume that is already being \
         built",
    );

    let extractions = app.volume_extractions.get();
    assert!(
        extractions > 0,
        "precondition: the eager path really paid for an extraction, or the \
         no-second-extraction claim below is vacuous",
    );
    let again = app.publish_base_volumes();
    assert!(
        again.is_empty(),
        "nothing moved, so nothing arrived — a second publish of an unchanged \
         stamp must not look like an arrival",
    );
    app.dispatch_arrived_volumes(&again);
    assert_eq!(
        app.volume_extractions.get(),
        extractions,
        "the same volume was extracted twice",
    );
}

/// **The draw-time ask attaches to the eager build rather than starting a
/// second one.** The two paths converge on the store's entry, which is what
/// makes keeping the level-trigger as a fallback free.
#[test]
fn the_draw_time_ask_attaches_to_the_build_the_arrival_opened() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    app.volumes.install_base(
        SITE.to_owned(),
        (crate::volume_fixture::ready_scan(), Arc::default(), at(6)),
    );
    let arrived = app.publish_base_volumes();
    app.dispatch_arrived_volumes(&arrived);
    let target = app
        .gui
        .pane(0)
        .and_then(|p| p.volume())
        .and_then(|v| v.rendered_for.clone())
        .expect("the arrival dispatched a build");
    let extractions = app.volume_extractions.get();
    assert!(
        extractions > 0,
        "precondition: the arrival paid for one extraction",
    );

    // The level-trigger firing anyway — a pane re-stacked, a frame that ran
    // before the mark, or simply the fallback doing its job.
    app.handle_prepare_volume(0, &known::RADAR, target);

    assert_eq!(
        app.volume_extractions.get(),
        extractions,
        "the draw-time ask re-extracted a volume the arrival path was already \
         building; the second asker must attach to the first's entry, never \
         open a second",
    );
}

/// **The eager ask passes the same budget gate, and losing it is correct.**
/// On wasm the render budget is 1, so a busy slot means the eager ask is
/// turned away — and because nothing is marked, the draw-time trigger is
/// still armed to pick it up. That is the fallback working.
#[test]
fn a_busy_render_slot_leaves_the_ask_to_the_draw_time_trigger() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    app.volumes.install_base(
        SITE.to_owned(),
        (crate::volume_fixture::ready_scan(), Arc::default(), at(6)),
    );
    while app.render.render_slot_free() {
        app.render
            .renders_in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    assert!(
        !app.render.render_slot_free(),
        "precondition: every render slot is spent",
    );

    let arrived = app.publish_base_volumes();
    assert!(
        !arrived.is_empty(),
        "precondition: the volume did arrive, so the refusal below is the \
         budget gate and not an absent arrival",
    );
    app.dispatch_arrived_volumes(&arrived);

    assert_eq!(
        app.volume_extractions.get(),
        0,
        "the budget gate sits ahead of extraction, and the eager path must \
         not step around it",
    );
    let pane = app.gui.pane(0).expect("pane 0");
    assert_eq!(
        pane.volume().and_then(|v| v.rendered_for.clone()),
        None,
        "a turned-away ask was marked as served, which would leave the pane \
         waiting for a build nobody ever started",
    );
    let stamp = radar_layer::current_volume_for(&app.liveness, SITE)
        .expect("the site's stamp was published");
    let (stamp, _) = pane.volume_stamp(Some(stamp)).expect("a stamp");
    assert!(
        pane.volume_build_due(
            &pane.volume_target_for(&rustdar_radar::fields::known::REFLECTIVITY, stamp)
        ),
        "the draw-time level-trigger must still be armed for the volume the \
         eager ask lost",
    );
}

/// **The frame pump really runs it.** Every other test here calls the two
/// halves directly, which would stay green if the row that runs them were
/// deleted — the silent-partial-success shape. This one goes in through
/// `poll_data_channels`, the `Ingest` phase itself, and asserts the pane came
/// out marked.
///
/// It is also where the *timing claim* is checkable: `Ingest` runs at
/// `handle_redraw`'s early position, before `setup_egui_frame` builds the
/// paint list and before `present_frame`, whereas a draw-time
/// `GuiAction::PrepareVolume` is not processed until after the frame is on
/// screen. So the build starts a whole UI pass and present earlier.
#[test]
fn the_frame_pump_runs_the_arrival_dispatch() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    app.volumes.install_base(
        SITE.to_owned(),
        (crate::volume_fixture::ready_scan(), Arc::default(), at(6)),
    );
    assert!(
        app.gui
            .pane(0)
            .and_then(|p| p.volume())
            .is_some_and(|v| v.rendered_for.is_none()),
        "precondition: nothing has been built for this pane yet",
    );

    app.poll_data_channels();

    assert!(
        app.gui
            .pane(0)
            .and_then(|p| p.volume())
            .is_some_and(|v| v.rendered_for.is_some()),
        "the Ingest phase published the volume and did not dispatch it — the \
         pump row runs `publish_base_volumes` alone again, so the whole \
         arrival path is dead code that every other test in this file still \
         exercises directly",
    );
    assert!(
        app.volume_extractions.get() > 0,
        "the pane was marked without a build being paid for",
    );
}

/// **A pane playing a 3D loop gets nothing from a live arrival.** Its
/// playhead is on its own frame's resident grid; the live volume would be a
/// grid nothing puts on screen, and paying for it would interrupt the loop to
/// do it. The refusal is `volume_build_due`'s, the same one the draw arm
/// applies — but the arrival path is where a volume landing under a *playing*
/// loop actually happens, so it is pinned on this side too.
#[test]
fn an_arrival_is_refused_by_a_pane_playing_a_3d_loop() {
    let mut app = headless(TestBridge::desktop());
    give_it_a_painter(&mut app);
    volume_pane_on(
        &mut app,
        0,
        SITE,
        &rustdar_radar::fields::known::REFLECTIVITY,
    );
    let want = expected_target(&app, 0, 6, &rustdar_radar::fields::known::REFLECTIVITY);
    assert_eq!(
        app.arrived_volume_asks(&arrival(SITE, 6)).len(),
        1,
        "precondition: with no loop playing, this pane is asked for",
    );

    let pane = app.gui.pane_mut(0).expect("pane 0");
    let mut playing = radar_layer::begin_loop(
        3600,
        rustdar_radar::sites::get_radar_site(SITE).expect("a known fixture site"),
        rustdar_radar::types::RenderView::Volume,
    );
    playing.frames = vec![rustdar_egui::pane::LoopFrame {
        timestamp: at(2),
        image: Some(rustdar_egui::pane::LoopFrameImage::Volume(
            rustdar_egui::pane::VolumeFrameGrid {
                id: 7,
                target: want,
            },
        )),
        render_in_flight: false,
        render_failed: false,
    }];
    *pane.loop_state_mut() = playing;
    assert!(
        pane.active_volume_frame().is_some(),
        "fixture precondition: the playhead really is on a resident grid",
    );

    assert!(
        app.arrived_volume_asks(&arrival(SITE, 6)).is_empty(),
        "a playing 3D loop was interrupted to build the live volume, which \
         nothing would have drawn",
    );
}
