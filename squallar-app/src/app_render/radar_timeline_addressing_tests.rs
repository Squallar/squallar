//! **Which timeline the radar render funnel addresses** (WO-T3.8).
//!
//! `app_render.rs` carries one note for the whole funnel saying why every loop
//! read in it is `time_state(&known::RADAR)`, spelled out: the payloads are
//! radar's — a `RenderTarget` of site/product/elevation, a `LoopFrameImage`
//! whose `view()` answers `None` for the overlay arm, keys cut out of a
//! decoded NEXRAD volume, and a geometry anchor only a radar timeline carries.
//!
//! WO-T3.7 retargeted every one of those reads at `transport_state()` and
//! found that all of them but `loop_demand` and `retarget_renders` passed the
//! whole tree. The prose was right and nothing enforced it. These are the
//! gates.
//!
//! **The reachable state they all fork on** is a pane whose transport sits on
//! a **satellite** loop while radar animates underneath:
//! `PaneState::refresh_transport` returns early while the transport's own loop
//! is active, so arming a GMGSI loop and *then* enabling radar leaves the
//! controls on the satellite. Every pin below reaches it the way the WO-T3.7
//! eviction pin does, through `set_transport_layer` — the config loader's own
//! door — and every one of them asserts first that the two reads really are
//! two different objects.
//!
//! **The floor of every pin is the common configuration, radar driving**,
//! where the two reads are one object and the answer must be identical. It is
//! asserted against a literal, so "both arms did nothing" fails the floor
//! rather than passing the comparison.

use super::*;
use crate::app::tests::{empty_scan, headless, n_pane_app, two_pane_app};
use crate::platform_double::TestBridge;
use squallar_egui::pane::{LayerTimeState, LoopFrame, LoopPhase};
use squallar_radar::sites::RadarSite;
use squallar_radar::types::{RadarProduct, RenderView};
use squallar_source::id::known;

const SITE: &str = "KTLX";
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const PRODUCT_ID: squallar_source::product::FieldId = squallar_radar::fields::known::REFLECTIVITY;
const TILT: f32 = 0.5;

fn at(minute: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
        .unwrap()
        .and_hms_opt(18, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minute)
}

/// Hours away from anything radar carries, so a keep-set, a frame lookup or a
/// broadcast built from the satellite's timeline cannot land on radar's answer
/// by accident.
fn satellite_time() -> chrono::NaiveDateTime {
    at(-600)
}

fn site() -> RadarSite {
    crate::test_sites::install();
    squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone()
}

/// **Arm a running satellite loop on `pane_idx` and hand it the transport**,
/// leaving radar's own timeline exactly where it was.
///
/// The state is reachable, not contrived: `PaneState::refresh_transport`
/// returns early while the transport's own loop is active, so a pane that
/// armed a GMGSI loop and then enabled radar keeps the controls on the
/// satellite while radar animates underneath.
fn a_satellite_loop_takes_the_transport(app: &mut crate::app::App, pane_idx: usize) {
    let pane = app
        .gui
        .pane_mut(pane_idx)
        .expect("the fixture built this pane");
    let mut sat = LayerTimeState::new();
    sat.phase = LoopPhase::Playing;
    sat.span_secs = 43_200;
    sat.frames = vec![LoopFrame {
        timestamp: satellite_time(),
        image: None,
        render_in_flight: false,
        render_failed: false,
    }];
    *pane.time_state_mut(&known::GMGSI) = sat;
    pane.set_transport_layer(known::GMGSI);

    assert!(
        !std::ptr::eq(pane.transport_state(), pane.time_state(&known::RADAR)),
        "precondition: the transport really addresses another timeline, or the \
         two reads are one object and the case is vacuous",
    );
    assert!(
        pane.transport_state().is_active(),
        "precondition: the satellite loop is genuinely running",
    );
    assert!(
        pane.transport_state().rendered_for.is_none(),
        "precondition: the satellite timeline carries no render target — it is \
         not a radar timeline and cannot have one",
    );
}

/// A plan-view loop over `stamps`, keyed to this suite's target.
fn active_loop(stamps: &[chrono::NaiveDateTime]) -> LayerTimeState {
    let mut ls = squallar_egui::radar_layer::begin_loop(3600, &site(), RenderView::PlanView);
    ls.phase = LoopPhase::Rendering;
    ls.frames = stamps
        .iter()
        .map(|&timestamp| LoopFrame {
            timestamp,
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    ls.retarget_renders(&PRODUCT_ID, TILT);
    assert!(
        ls.rendered_for.is_some(),
        "precondition: a fresh loop must take its first target",
    );
    ls
}

fn point_at_site(app: &mut crate::app::App, pane_idx: usize) {
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(PRODUCT, vec![TILT]);
    let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
    pane.set_site(SITE.to_string());
    pane.set_selected_product(PRODUCT_ID);
    pane.set_selected_elevation(TILT);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx,
            info: squallar_radar::types::ScanInfo {
                site: site(),
                site_source: squallar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: at(0),
                vcp_number: 212,
                available_products: vec![PRODUCT],
                product_elevations,
                status: String::new(),
            },
        });
}

/// A loop frame texture, so a "did this frame get a picture" assertion is
/// about a picture rather than about a flag.
fn textured(ctx: &egui::Context) -> squallar_egui::pane::LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    squallar_egui::pane::LoopFrameImage::PlanView(squallar_egui::pane::RadarImageData {
        texture: ctx.load_texture("donor", image, egui::TextureOptions::NEAREST),
        lat: 35.33,
        lon: -97.27,
        max_range_km: 230.0,
        placed: squallar_radar::types::ImageBounds::from_radar_site(35.33, -97.27, 230.0).into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
    })
}

// ---------------------------------------------------------------------------
// 1. The arrival path: `accept_scan_listing`.

/// Run a radar scan listing all the way through the production arrival path,
/// with the transport optionally parked on a satellite loop first, and answer
/// the stamps radar's **own** frame list ended up holding.
fn radars_frames_after_a_listing(park_on_the_satellite: bool) -> Vec<chrono::NaiveDateTime> {
    use squallar_overlays::render::overlay_state::SourceEvent;
    use squallar_source::time::{FrameListing, FrameStamp};

    let stamps = [at(-8), at(-4), at(0)];
    let range = (at(-10), at(0));

    let mut app = n_pane_app(1, SITE);
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        *pane.time_state_mut(&known::RADAR) =
            squallar_egui::radar_layer::begin_loop(600, &site(), RenderView::PlanView);
        pane.time_state_mut(&known::RADAR).asked_range = Some(range);
        assert_eq!(
            pane.time_state(&known::RADAR).phase,
            LoopPhase::FetchingScanList,
            "precondition: radar must be waiting on a listing, or the arrival \
             has nothing to answer",
        );
        assert!(
            pane.time_state(&known::RADAR).frames.is_empty(),
            "precondition: radar's loop has no frames yet",
        );
    }
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut app, 0);
    }

    let scans: Vec<(chrono::NaiveDateTime, squallar_radar::archive::Identifier)> = stamps
        .iter()
        .map(|&valid| {
            (
                valid,
                squallar_radar::archive::Identifier::new(format!("{SITE}{valid}")),
            )
        })
        .collect();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: known::RADAR,
            listing: FrameListing {
                range,
                frames: stamps
                    .iter()
                    .map(|&valid| FrameStamp { valid, run: None })
                    .collect(),
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: SITE.to_string(),
                range,
                scans: scans.clone(),
            }),
        })
        .expect("the receiver lives on the App");

    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();

    app.gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR)
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect()
}

/// **A radar scan listing becomes radar's own frame list, never the
/// transport's.**
///
/// `accept_loop_scan_listings` hands `accept_scan_listing` the timeline the
/// frames are for. A transport-addressed read there offers a NEXRAD listing to
/// a satellite timeline, whose `radar_layer::site` answers `""`, so the
/// listing is refused outright and **radar's loop never leaves
/// `FetchingScanList`** — the ∞ button is on, the frames never arrive, and
/// nothing ever says why. (The listing is not merely misfiled: the site check
/// is what stops it landing in the satellite's frames, and it is the only
/// thing that does.)
#[test]
fn a_radar_listing_becomes_radars_own_frames_not_the_transports() {
    let radar_driving = radars_frames_after_a_listing(false);
    assert_eq!(
        radar_driving,
        vec![at(-8), at(-4), at(0)],
        "floor: with radar driving the transport, the listing becomes radar's \
         three frames — if this arm built none the comparison below would be \
         satisfied by two empty lists",
    );

    let satellite_driving = radars_frames_after_a_listing(true);
    assert_eq!(
        satellite_driving, radar_driving,
        "a pane whose transport sits on a satellite loop never got its radar \
         frames: the listing was offered to the satellite's timeline, whose \
         `radar_layer::site` answers \"\", so it was refused and radar's loop \
         is stuck in FetchingScanList for ever with the ∞ button lit",
    );
}

// ---------------------------------------------------------------------------
// 2. The arrival path: a finished render, and its broadcast to siblings.

/// Deliver one finished plan-view loop render for pane 0 into two layer-linked
/// panes, with both transports optionally parked on satellite loops first, and
/// answer whether each pane's radar frame ended up with a picture.
fn frames_holding_a_picture_after_a_render(park_on_the_satellite: bool) -> (bool, bool) {
    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    assert!(
        app.gui.pane_layer_linked(0) && app.gui.pane_layer_linked(1),
        "precondition: both panes are layer-linked, or the broadcast half of \
         this pin never runs",
    );
    app.loop_mgr = squallar_radar::loop_downloads::LoopDownloadManager::new();
    app.loop_mgr.cache_scan(
        SITE,
        at(0),
        super::loop_dispatch_tests::volume_with_sweeps(&[TILT]),
    );
    for idx in 0..2 {
        let ls = app
            .gui
            .pane_mut(idx)
            .expect("the fixture built two panes")
            .time_state_mut(&known::RADAR);
        *ls = active_loop(&[at(0)]);
        ls.frames[0].render_in_flight = true;
    }
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut app, 0);
        a_satellite_loop_takes_the_transport(&mut app, 1);
    }

    let target = app
        .gui
        .pane(0)
        .expect("the fixture built two panes")
        .time_state(&known::RADAR)
        .rendered_for
        .clone()
        .expect("the fixture loop is keyed");
    let side = squallar_device_profile::constants::LOOP_IMAGE_SIZE;
    app.channels
        .loop_render_sender
        .send(crate::channels::LoopRenderResponse {
            pane_idx: 0,
            timestamp: at(0),
            target,
            snapped: TILT,
            site_lat: 35.33,
            site_lon: -97.27,
            image: Some(egui::ColorImage::from_rgba_unmultiplied(
                [side, side],
                &vec![0u8; side * side * 4],
            )),
            max_range_km: 230.0,
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
            polar: Default::default(),
        })
        .expect("the receiver lives on the App");
    app.poll_loop_render_results(&egui::Context::default());

    let holds = |app: &crate::app::App, idx: usize| {
        app.gui
            .pane(idx)
            .expect("the fixture built two panes")
            .time_state(&known::RADAR)
            .frames[0]
            .image
            .is_some()
    };
    (holds(&app, 0), holds(&app, 1))
}

/// **A finished radar render is filed in radar's own frame list — on the
/// originating pane and on every sibling it is broadcast to — never in the
/// transport's.**
///
/// Three reads carry this: the acceptance on the origin pane, the sibling's
/// `is_rendered_for` test, and the sibling's own frame lookup. A
/// transport-addressed read at any of them offers a NEXRAD plan-view texture
/// keyed by `RenderTarget` to a satellite timeline, which has no such key and
/// no frame at that stamp, so the result is **dropped on the floor**: the
/// frame stays blank and `render_in_flight` is never cleared, so the loop
/// never settles and the pane waits for a picture that already arrived.
#[test]
fn a_finished_radar_render_lands_in_radars_own_frames_not_the_transports() {
    let radar_driving = frames_holding_a_picture_after_a_render(false);
    assert_eq!(
        radar_driving,
        (true, true),
        "floor: with radar driving, the render lands on its own pane and is \
         broadcast to the linked sibling — if neither took it the comparison \
         below would be satisfied by two blank panes",
    );

    let satellite_driving = frames_holding_a_picture_after_a_render(true);
    assert_eq!(
        satellite_driving, radar_driving,
        "a finished radar render was thrown away because the panes' transports \
         sat on satellite loops. The texture is keyed by RenderTarget \
         (site+product+elevation), which no satellite timeline carries, so it \
         matched no frame: the loop's frames stay blank, `render_in_flight` is \
         never cleared, and the loop never settles into playback",
    );
}

// ---------------------------------------------------------------------------
// 3. The dispatch pass.

/// An app with one pane running a plan-view radar loop over `at(0)`, no
/// volumes cached, and the transport optionally parked on a satellite loop.
fn app_with_a_plan_view_loop(park_on_the_satellite: bool) -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    app.render.ensure_pane_count(1);
    app.loop_mgr = squallar_radar::loop_downloads::LoopDownloadManager::new();
    point_at_site(&mut app, 0);
    *app.gui
        .pane_mut(0)
        .expect("a fresh Gui has one pane")
        .time_state_mut(&known::RADAR) = active_loop(&[at(0)]);
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut app, 0);
    }
    app
}

/// Run one dispatch pass over a loop whose only frame's volume carries **no
/// sweep** for the selected product, and answer whether radar's own frame was
/// retired.
fn radars_frame_is_retired(park_on_the_satellite: bool) -> bool {
    let mut app = app_with_a_plan_view_loop(park_on_the_satellite);
    // A volume with no sweeps at all: `frame_sweep` answers `Unrenderable`,
    // which is the verdict this pass has to record ON RADAR'S FRAME.
    app.loop_mgr
        .cache_scan(SITE, at(0), (Arc::new(empty_scan()), Default::default()));

    app.dispatch_loop_renders();

    app.gui
        .pane(0)
        .expect("a fresh Gui has one pane")
        .time_state(&known::RADAR)
        .frames[0]
        .render_failed
}

/// **The dispatch pass plans off radar's own frame list, and records its
/// verdicts there.**
///
/// Two reads: the planning walk's `ls`, and the retirement that writes
/// `render_failed` back. A transport-addressed planning walk finds no
/// `rendered_for` on a satellite timeline and skips the pane entirely, so
/// **nothing is ever dispatched for the radar loop** — every frame stays blank
/// and the loop never settles. A transport-addressed retirement records the
/// verdict on a timeline that has no such frame, so an unrenderable frame is
/// re-judged on every single pass, for ever.
#[test]
fn the_dispatch_pass_judges_radars_own_frames_not_the_transports() {
    assert!(
        radars_frame_is_retired(false),
        "floor: with radar driving, a frame whose volume carries no sweep for \
         the selection is retired on the first pass — if this arm retired \
         nothing the assertion below would be satisfied by two un-retired \
         frames",
    );
    assert!(
        radars_frame_is_retired(true),
        "a pane whose transport sits on a satellite loop stopped dispatching \
         for its radar loop altogether: the planning walk asked a satellite \
         timeline for a RenderTarget, got none, and skipped the pane — so no \
         frame is rendered, none is retired, and the loop hangs in Rendering \
         with a blank glass for as long as the satellite plays",
    );
}

/// Run one dispatch pass over two layer-linked panes where pane 0 already
/// holds a texture for the shared frame and pane 1 does not, and answer
/// whether pane 1's radar frame took the donor's picture.
fn a_sibling_texture_is_cloned(park_on_the_satellite: bool) -> bool {
    let ctx = egui::Context::default();
    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    assert!(
        app.gui.pane_layer_linked(0) && app.gui.pane_layer_linked(1),
        "precondition: both panes are layer-linked, or no donor is ever \
         looked for",
    );
    app.loop_mgr = squallar_radar::loop_downloads::LoopDownloadManager::new();
    for idx in 0..2 {
        *app.gui
            .pane_mut(idx)
            .expect("the fixture built two panes")
            .time_state_mut(&known::RADAR) = active_loop(&[at(0)]);
    }
    app.gui
        .pane_mut(0)
        .expect("the fixture built two panes")
        .time_state_mut(&known::RADAR)
        .frames[0]
        .image = Some(textured(&ctx));
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut app, 0);
        a_satellite_loop_takes_the_transport(&mut app, 1);
    }

    app.dispatch_loop_renders();

    app.gui
        .pane(1)
        .expect("the fixture built two panes")
        .time_state(&known::RADAR)
        .frames[0]
        .image
        .is_some()
}

/// **A donor texture is found in the sibling's radar frame list and applied to
/// the receiver's, never to either pane's transport.**
///
/// Three reads: the donor search's view of every sibling, the source frame
/// lookup, and the destination write. A satellite timeline holds no
/// `RenderTarget`, so a transport-addressed donor search finds nobody and the
/// receiving pane **re-renders a picture it could have had for free** — the
/// whole point of the linked-pane clone — while a transport-addressed source
/// or destination lookup drops the clone silently and leaves the frame blank.
#[test]
fn a_donor_texture_is_cloned_between_radars_own_frame_lists() {
    assert!(
        a_sibling_texture_is_cloned(false),
        "floor: with radar driving, a linked sibling's finished texture is \
         cloned into the pane that has none — if this arm cloned nothing the \
         assertion below would be satisfied by two blank frames",
    );
    assert!(
        a_sibling_texture_is_cloned(true),
        "the linked-pane clone stopped happening because the transports sat on \
         satellite loops: the donor search asked satellite timelines for a \
         matching RenderTarget and found none, so a second pane showing the \
         same site, product and tilt renders the identical picture again \
         instead of taking the one already on screen beside it",
    );
}

/// Run one dispatch pass over a loop whose frame's volume **does** carry the
/// selected sweep, and answer whether radar's own frame was marked in flight.
fn radars_frame_goes_in_flight(park_on_the_satellite: bool) -> bool {
    let mut app = app_with_a_plan_view_loop(park_on_the_satellite);
    app.loop_mgr.cache_scan(
        SITE,
        at(0),
        super::loop_dispatch_tests::volume_with_sweeps(&[TILT]),
    );

    app.dispatch_loop_renders();

    app.gui
        .pane(0)
        .expect("a fresh Gui has one pane")
        .time_state(&known::RADAR)
        .frames[0]
        .render_in_flight
}

/// **A spawned render marks the frame it is for in radar's own list.**
///
/// The mark is what stops the very next pass spawning the same render again.
/// A transport-addressed write puts it on a timeline that has no such frame,
/// so radar's frame reads "not in flight" for ever and **every dispatch pass
/// spawns the same render**, burning the concurrency budget on one frame while
/// the rest of the loop waits behind it.
#[test]
fn a_spawned_render_marks_radars_own_frame_in_flight_not_the_transports() {
    assert!(
        radars_frame_goes_in_flight(false),
        "floor: with radar driving, a frame whose data has arrived is spawned \
         and marked — if this arm marked nothing the assertion below would be \
         satisfied by two unmarked frames",
    );
    assert!(
        radars_frame_goes_in_flight(true),
        "a render was spawned for a frame that was never marked in flight, \
         because the pane's transport sat on a satellite loop: the next pass \
         sees an unmarked frame with no picture and spawns the same render \
         again, and the one after that does it too",
    );
}
