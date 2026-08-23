//! **What the satellite layer puts on the glass while a loop plays** — the
//! user's second report of the same defect, at the layer the first fix did not
//! reach.
//!
//! *"satellite view still doesn't loop either, I did a 600 min loop and the
//! GMGSI never changes during it"*
//!
//! # The mechanism, and why the transport-only suites are all green over it
//!
//! `gmgsi_loop_tests` arms GMGSI as the pane's **transport** by hand and
//! proves the whole chain works there: thirteen frames, thirteen granules,
//! thirteen pictures. It does. The user's pane is not that pane. GMGSI's
//! `draw_order_weight` is **5**, the lowest any layer claims, so any other
//! frame-series layer the pane draws — radar at 30, the national mosaic at 15,
//! the model at 10 — is the topmost and takes the transport. And
//! `handle_enable_loop` armed the transport **and nothing else**: every other
//! frame-series layer stayed `Inactive`, held no frames, and therefore fell
//! through [`PaneState::overlay_texture_on_screen`]'s not-animating arm to its
//! own **live** raster. One instant, painted unlabelled under a loop that was
//! otherwise running perfectly, for the whole playback. Not blank, not wrong,
//! never changing.
//!
//! Measured on this fixture before the fix: **1 of 11 distinct pictures** —
//! and the one was `None`, the satellite layer holding no loop frames at all
//! while the mosaic transport swept all eleven of its own. The denominator is
//! the transport's own frame count and moves by one with the minute the run
//! starts on (the last half-hour stamp falls past `now` for part of every
//! hour), which is why every message below prints it rather than naming it.
//!
//! # What is driven and what is arranged
//!
//! The transport is the **real** `MrmsHandler` and the satellite layer is the
//! **real** `GmgsiHandler`; the loop is armed through the real
//! `GuiAction::EnableLoop`, listed by the real `accept_loop_scan_listings`,
//! dispatched by the real `dispatch_overlay_loop_renders`, **rasterized by the
//! real renderer** (no job sink is installed, so `offload_job` runs each
//! described job through the production `execute`), filed by the real
//! `poll_overlay_render_results` and read back through the real
//! `PaneState::overlay_texture_on_screen` — the draw fork itself. Playback is
//! the real `advance_loop_playback`.
//!
//! **Two things are modelled, both of them the network.** The frame listings
//! and the granules are answered on the wire, because the real ones are S3
//! LISTs and 7.5 MB GETs and no test in this tree reaches a network. The
//! *asks* are the app's own — they are read off the production wire, not
//! invented — and the answers are handed back on the production arrival paths.
//!
//! # How a picture is identified, and why not by its handle
//!
//! Counting distinct `TextureId`s proves nothing here: `file_overlay_loop_frame`
//! uploads once per response, so N responses are N handles **whatever they
//! depict**. What this suite reads instead is the **pixels**: every finished
//! raster is intercepted on the render channel on its way to the app, hashed,
//! and filed under the stamp it is a picture of. The picture on the glass is
//! then resolved handle → frame → stamp → hash, so "twelve distinct pictures"
//! means twelve distinct rasters, byte for byte, out of the production
//! renderer.
//!
//! [`PaneState::overlay_texture_on_screen`]: rustdar_egui::pane::PaneState::overlay_texture_on_screen

use chrono::Timelike;
use rustdar_geo::GeoBounds;
use rustdar_source::handler::SourceEvent;
use rustdar_source::id::known;
use rustdar_source::time::{FrameListing, FrameStamp};
use std::collections::HashMap;
use std::sync::Arc;

use rustdar_overlays::gmgsi::decode::GmgsiGrid;
use rustdar_overlays::gmgsi::{GmgsiChannel, GmgsiFrameFetch, GmgsiListing};
use rustdar_overlays::hrrr::GridCoords;
use rustdar_overlays::mrms::{MrmsFrameFetch, MrmsGrid, MrmsListing, MrmsProduct};
use rustdar_overlays::render::gridded::ResidentGrid;

/// The channel the satellite layer opens on.
const CHANNEL: GmgsiChannel = GmgsiChannel::LongwaveIr;

/// The product the mosaic layer opens on.
const PRODUCT: MrmsProduct = MrmsProduct::ReflectivityComposite;

/// The window the ∞ button arms for this pane: twelve hours, which is what
/// `Gmgsi::min_loop_frames` buys and what the user's 600-minute slider is
/// raised to. Passed to `EnableLoop` directly, exactly as
/// `Gui::loop_span_secs_for` would have raised it.
const WINDOW_SECS: u64 = 43_200;

/// The area every picture in this suite covers.
fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -105.0,
        max_lon: -95.0,
    }
}

/// A satellite granule for `valid`, whose values are a function of the hour —
/// so two hours' granules rasterize to two different pictures, which is the
/// property every assertion below rests on.
///
/// Small: the real mosaic is 3000x5000 and what is under test is which grid
/// reaches which frame, not how big it is.
fn satellite_granule(valid: chrono::NaiveDateTime, hour: i64) -> GmgsiGrid {
    let spec = rustdar_overlays::gmgsi::fields::spec(CHANNEL);
    GmgsiGrid {
        channel: CHANNEL,
        grid: ResidentGrid {
            field: spec.id.clone(),
            ni: 8,
            nj: 8,
            coords: GridCoords::Separable {
                lat_axis: (0..8).map(|j| 30.0 + f64::from(j) * 1.4).collect(),
                lon_axis: (0..8).map(|i| -105.0 + f64::from(i) * 1.4).collect(),
            },
            // Brightness temperatures a whole colour step apart, **inside the
            // band the IR ramp actually discriminates over**. The ramp is
            // flat at its warm end — cloud-top scales spend their resolution
            // on cold tops — so a ladder that walked out of it would hand
            // several hours the same picture and read as the defect. The
            // sixteen-step wrap keeps thirteen consecutive hours distinct.
            values: (0..64)
                .map(|_| 190.0 + hour.rem_euclid(16) as f32 * 3.0)
                .collect(),
        },
        bounds: bounds(),
        valid_time: valid,
    }
}

/// A mosaic granule for `valid`. The transport's own picture, so the sweep can
/// be told apart from a pane where nothing at all is moving.
fn mosaic_granule(valid: chrono::NaiveDateTime, step: i64) -> MrmsGrid {
    let spec = rustdar_overlays::mrms::fields::spec(PRODUCT);
    let dbz = 10.0 + step as f32 * 3.0;
    MrmsGrid {
        product: PRODUCT,
        grid: Arc::new(ResidentGrid {
            field: spec.id.clone(),
            ni: 4,
            nj: 1,
            coords: GridCoords::Regular {
                lat0: bounds().max_lat,
                lon0: bounds().min_lon,
                dlat: -0.01,
                dlon: (bounds().max_lon - bounds().min_lon) / 3.0,
                ni: 4,
                nj: 1,
                scan_mode: 0,
            },
            values: vec![dbz; 4],
        }),
        bounds: bounds(),
        valid,
        visible_points: 4,
        value_range: Some((dbz, dbz)),
    }
}

/// Draw `id` on pane 0, or stop drawing it — the same four calls
/// `Gui::write_pane_overlay` makes, in the same order, so the pane below is in
/// a state the real door can produce.
fn draw(app: &mut crate::app::App, id: &rustdar_source::id::LayerId, on: bool) {
    let (panes, overlays) = app.gui.panes_and_overlays_mut();
    panes[0].hydrate_layer_states(overlays, 0);
    panes[0].set_layer_enabled(overlays, 0, id, on);
    panes[0].adopt_handler_state(overlays);
    panes[0].refresh_transport(overlays);
}

fn a_render_request() -> crate::app::fetch::OverlayRenderRequest {
    crate::app::fetch::OverlayRenderRequest {
        geo_bounds: bounds(),
        texture: rustdar_egui::overlay_cache::OverlayTexturePlan {
            width: 32,
            height: 32,
            overdraw: 0.0,
            pixels_per_point: 1.0,
        },
        data_generation: 0,
        zoom: 32,
    }
}

/// The whole hours inside `range` — the instants a real GMGSI listing of that
/// window would have found, one blended mosaic per hour.
fn hours_inside(
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
) -> Vec<chrono::NaiveDateTime> {
    let (lo, hi) = range;
    let mut at = lo
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .expect("a whole hour exists");
    if at < lo {
        at += chrono::Duration::hours(1);
    }
    let mut hours = Vec::new();
    while at <= hi {
        hours.push(at);
        at += chrono::Duration::hours(1);
    }
    hours
}

/// **The instants the transport's own listing names inside `range`**: half
/// past each of the satellite's hours, and never on one.
///
/// Off the hour on purpose. Each mosaic frame therefore settles the satellite
/// playhead onto exactly one hour — `qualifying_frame_at` takes the newest
/// frame at or before the clock — and a full sweep of the transport visits a
/// different satellite hour every tick. Equal cadences would make the two
/// timelines the same list and would prove nothing about the settling.
///
/// **Derived from the window it was asked for, like the satellite's.** A
/// bucket answers the range it was given; answering a *later* ask with an
/// earlier ask's stamps lists nothing inside it, and `accept_loop_scan_listings`
/// retires a loop that lists nothing — which is a fixture that takes its own
/// subject away, not a defect.
fn mosaic_stamps(
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
) -> Vec<chrono::NaiveDateTime> {
    hours_inside(range)
        .into_iter()
        .map(|h| h + chrono::Duration::minutes(30))
        .filter(|t| *t <= range.1)
        .collect()
}

/// Which step of the fixture's own timeline `at` is, counted in whole hours
/// from the epoch — a stable index whatever window it was asked for in, so a
/// re-listed window hands back the same picture it did the first time.
fn step_of(at: chrono::NaiveDateTime) -> i64 {
    at.and_utc().timestamp().div_euclid(3600)
}

/// **The listing the bucket would answer `range` with**, for either layer.
///
/// Sent from two places on purpose. Intercepting the app's own listing event
/// and replacing it is the general case, but it is a **race**: the real task
/// runs against a client that cannot connect, and its failure — an empty
/// listing over the same window — can reach `poll_overlay_fetch_results`
/// between the interception and the pump. `accept_loop_scan_listings` retires
/// a loop that lists nothing, so the transport would vanish before the sweep
/// ever ran (measured: 1 run in 7 of the full suite). Answering *first*,
/// straight after the ∞ button, closes it: the full listing lands while the
/// pane is still `FetchingScanList`, and the empty one that follows finds a
/// pane no longer waiting and is ignored — which is exactly what a real
/// out-of-order listing gets.
fn listing_event(
    id: rustdar_source::id::LayerId,
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
) -> SourceEvent {
    if id == known::GMGSI {
        let keys: Vec<(chrono::NaiveDateTime, String)> = hours_inside(range)
            .into_iter()
            .map(|at| {
                (
                    at,
                    format!("GMGSI_LW/{}/blend.nc", at.format("%Y/%m/%d/%H")),
                )
            })
            .collect();
        return SourceEvent::Frames {
            id: known::GMGSI,
            listing: FrameListing {
                range,
                frames: keys.iter().map(|(v, _)| stamp(*v)).collect(),
                complete: true,
            },
            scope: Box::new(GmgsiListing {
                channel: CHANNEL,
                range,
                keys,
                complete: true,
            }),
        };
    }
    let keys: Vec<(chrono::NaiveDateTime, String)> = mosaic_stamps(range)
        .into_iter()
        .map(|at| {
            (
                at,
                rustdar_source::origins::DataSources::mrms_key(PRODUCT.prefix_name(), &at),
            )
        })
        .collect();
    SourceEvent::Frames {
        id: known::MRMS,
        listing: FrameListing {
            range,
            frames: keys.iter().map(|(v, _)| stamp(*v)).collect(),
            complete: true,
        },
        scope: Box::new(MrmsListing {
            product: PRODUCT,
            range,
            keys,
            complete: true,
        }),
    }
}

/// **What the S3 side answers, and what it was asked for.**
///
/// Every listing and every granule the app puts on the wire is taken off it
/// here and answered on the production arrival path. Nothing else is touched:
/// events for other layers go back unread.
#[derive(Default)]
struct Wire {
    /// Every satellite granule ask that has been answered, by instant. The
    /// over-correction floor counts these.
    satellite_gets: Vec<chrono::NaiveDateTime>,
}

impl Wire {
    fn answer(&mut self, app: &mut crate::app::App) {
        let mut back: Vec<SourceEvent> = Vec::new();
        while let Ok(event) = app.channels.overlay_fetch_receiver.try_recv() {
            match event {
                SourceEvent::Frames { id, listing, .. }
                    if id == known::GMGSI || id == known::MRMS =>
                {
                    back.push(listing_event(id, listing.range));
                }
                SourceEvent::FrameReady { id, stamp, .. } if id == known::GMGSI => {
                    self.satellite_gets.push(stamp.valid);
                    let hour = step_of(stamp.valid);
                    back.push(SourceEvent::FrameReady {
                        id: known::GMGSI,
                        stamp,
                        data: Box::new(GmgsiFrameFetch {
                            channel: CHANNEL,
                            valid: stamp.valid,
                            grid: Some(satellite_granule(stamp.valid, hour)),
                        }),
                    });
                }
                SourceEvent::FrameReady { id, stamp, .. } if id == known::MRMS => {
                    let step = step_of(stamp.valid);
                    back.push(SourceEvent::FrameReady {
                        id: known::MRMS,
                        stamp,
                        data: Box::new(MrmsFrameFetch {
                            product: PRODUCT,
                            valid: stamp.valid,
                            grid: Some(mosaic_granule(stamp.valid, step)),
                        }),
                    });
                }
                other => back.push(other),
            }
        }
        for event in back {
            app.channels
                .overlay_fetch_sender
                .send(event)
                .expect("the receiver lives on the App");
        }
    }
}

fn stamp(valid: chrono::NaiveDateTime) -> FrameStamp {
    FrameStamp { valid, run: None }
}

/// **Every finished satellite raster, by the instant it depicts** — the pixels
/// the production renderer really produced, taken off the render channel on
/// their way to the app and put straight back.
///
/// This is what makes "distinct pictures" a claim about pictures. The app sees
/// exactly the responses it would have seen; nothing is substituted.
fn capture_rasters(app: &crate::app::App, seen: &mut HashMap<chrono::NaiveDateTime, u64>) {
    let mut back = Vec::new();
    while let Ok(resp) = app.channels.overlay_render_receiver.try_recv() {
        if resp.overlay_kind == known::GMGSI
            && let (Some(frame), Some(image)) = (resp.frame, resp.image.as_ref())
        {
            seen.insert(frame.valid, hash_of(image));
        }
        back.push(resp);
    }
    for resp in back {
        app.channels
            .overlay_render_sender
            .send(resp)
            .expect("the receiver lives on the App");
    }
}

/// A picture's identity: its pixels, and its size beside them so two rasters
/// of different shapes can never collide.
fn hash_of(image: &egui::ColorImage) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    image.size.hash(&mut hasher);
    for px in &image.pixels {
        px.to_array().hash(&mut hasher);
    }
    hasher.finish()
}

/// File a live (non-frame) raster for `id` on the production arrival path —
/// what the pane's own draw pass produces before anything loops.
fn live_raster(app: &mut crate::app::App, ctx: &egui::Context, id: rustdar_source::id::LayerId) {
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            image: Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                [32, 32],
                &[7u8; 32 * 32 * 4],
            ))),
            geo_bounds: bounds(),
            overlay_kind: id,
            generation: 0,
            pane_indices: vec![0],
            zoom: 32,
            hit_map: None,
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

/// One frame of the app, in `App::handle_redraw`'s own order. Anything
/// reordered here is a frame this app never runs.
fn pump(app: &mut crate::app::App, ctx: &egui::Context) {
    use crate::app::frame_pump::PumpPhase;
    app.run_frame_pump(PumpPhase::Ingest, None);
    app.run_frame_pump(PumpPhase::Apply, Some(ctx));
    app.run_frame_pump(PumpPhase::Advance, Some(ctx));
    app.run_frame_pump(PumpPhase::Dispatch, Some(ctx));
}

/// The pane the user described: a mosaic transport with the satellite layer
/// drawn under it, radar off, and an HTTP client that cannot reach a network.
fn a_pane_drawing_satellite_under_a_mosaic() -> crate::app::App {
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    app.http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1))
        .connect_timeout(std::time::Duration::from_millis(1))
        .build()
        .expect("a client with no connection to make");

    draw(&mut app, &known::GMGSI, true);
    draw(&mut app, &known::MRMS, true);
    draw(&mut app, &known::RADAR, false);
    app
}

/// **The acceptance.** Enable a twelve-hour loop on a pane drawing the
/// satellite layer under another layer's transport, fill it through the real
/// supply, then play it and read — every tick — what
/// `overlay_texture_on_screen` hands the painter for the satellite layer and
/// which raster that handle is.
///
/// Three things, and the first is the user's sentence:
///
/// 1. **every step of the sweep draws a different satellite picture**, counted
///    as distinct rasters out of the production renderer rather than as
///    distinct handles;
/// 2. the depicted instant really moves — the satellite playhead visits a
///    different stamp on every tick and covers the whole list;
/// 3. the transport is still doing its own job, so a green here is not a pane
///    where nothing is animating at all.
///
/// **Floor A — the ask must not become a storm.** Arming a second layer puts a
/// second set of 7.5 MB GETs on the wire. Every satellite granule may be asked
/// for a small number of times across the whole run, not once per frame of the
/// pump; the in-flight mark in `refetch_owed_loop_frames` is what holds it,
/// and this counts the asks off the wire to say so.
///
/// **Floor B — one window for the whole pane**, so a layer cannot hold frames
/// at instants the pane's clock never names. **What it can fail on here is
/// narrow, and saying so is the point**: both layers on this fixture reach
/// backward from the same wall clock, so a layer left to derive its own window
/// would derive the same one. It reds on the defect (an unarmed satellite
/// layer has no window at all) and it records the contract; the *contract*
/// itself is floored where it can actually be broken —
/// `a_layer_handed_a_window_is_listed_over_that_window` in `loop_pane_tests`,
/// against radar, whose own arm ends at the pane's scan rather than at now.
///
/// **Observed red at HEAD behaviour** (`no_second_layer`: drop the
/// `layers.extend(...)` in `begin_loop_for_pane` so only the transport is
/// armed) — **1 of 11 distinct pictures**, and the one is `None`: the
/// satellite layer holds no frames and its playhead names no instant for the
/// whole sweep.
///
/// **Observed red under a partial under-reach** (`half_a_window`: hand every
/// layer after the transport `window.map(|(s, e)| (s + (e - s) / 2, e))`, so
/// the satellite is armed but over the newer half of the pane's window) —
/// **6 of 11 distinct pictures**: five real rasters over the half it holds,
/// nothing over the half it does not.
#[test]
fn every_step_of_a_loop_draws_its_own_satellite_picture() {
    let ctx = egui::Context::default();
    let mut app = a_pane_drawing_satellite_under_a_mosaic();

    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::MRMS,
        "premise: the satellite layer's weight is the lowest any layer claims, \
         so a pane drawing anything else frame-shaped over it addresses that \
         layer as its transport. This is the user's pane; the one where GMGSI \
         *is* the transport is `gmgsi_loop_tests`, and it is green.",
    );

    // The ∞ button, with the window the UI floors for a satellite loop.
    app.handle_gui_action(
        rustdar_egui::actions::GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: WINDOW_SECS,
        },
        None,
    );

    let transport_range = app
        .gui
        .pane(0)
        .expect("pane 0")
        .time_state(&known::MRMS)
        .asked_range
        .expect("the transport was asked for a window");
    // Answer both listings before anything pumps — see `listing_event`.
    for id in [known::MRMS, known::GMGSI] {
        let range = app
            .gui
            .pane(0)
            .expect("pane 0")
            .time_state(&id)
            .asked_range
            .unwrap_or(transport_range);
        app.channels
            .overlay_fetch_sender
            .send(listing_event(id, range))
            .expect("the receiver lives on the App");
    }

    let hours = hours_inside(transport_range);
    assert!(
        hours.len() >= 12,
        "premise: a twelve-hour window holds at least twelve whole hours, and \
         this one holds {}",
        hours.len(),
    );

    // Floor B's reading, taken at the moment both timelines were armed. It is
    // asserted at the end so the headline count is what a red reports first.
    let satellite_range = app
        .gui
        .pane(0)
        .expect("pane 0")
        .time_state(&known::GMGSI)
        .asked_range;

    // The pane's own live rasters, which is what writes the geometry record a
    // loop frame's raster is sized by. In production the shipped `Gui::ui`
    // asks for these; here they are asked for directly, which is the one
    // arranged step on the render side.
    app.spawn_overlay_render(vec![0], known::MRMS, a_render_request(), None);
    app.spawn_overlay_render(vec![0], known::GMGSI, a_render_request(), None);
    // **And the live pictures themselves**, on the production arrival path.
    //
    // Not decoration: `overlay_frame_bytes` prices a loop frame off the
    // pane's own live raster and falls back to the **device class's nominal**
    // 18.66 MB overlay frame when there is none. A pane with no live picture
    // therefore prices two 32x32 test rasters at a third of the loop pool
    // each, `layer_share` divides that by the two animating layers, and the
    // dispatch evicts textures it could trivially afford — which is a fixture
    // measuring the byte budget rather than the defect. A real pane has drawn
    // both layers live before the user reaches for the ∞ button.
    for id in [known::MRMS, known::GMGSI] {
        live_raster(&mut app, &ctx, id);
    }

    let mut wire = Wire::default();
    let mut rasters: HashMap<chrono::NaiveDateTime, u64> = HashMap::new();

    // Fill both loops through the real supply.
    //
    // **Whether the TRANSPORT converged is recorded, not assumed** — and only
    // the transport. A fixture whose transport never filled has no sweep to
    // read, and a red below would be about the fixture; a fixture whose
    // *satellite* never filled is the defect itself, so it must fall through
    // to the count assertion rather than being reported as a load failure.
    let mut transport_filled = false;
    for _ in 0..600 {
        wire.answer(&mut app);
        capture_rasters(&app, &mut rasters);
        pump(&mut app, &ctx);
        let pane = app.gui.pane(0).expect("pane 0");
        let satellite = pane.time_state(&known::GMGSI);
        let mosaic = pane.time_state(&known::MRMS);
        let full = |ls: &rustdar_egui::pane::LayerTimeState| {
            !ls.frames.is_empty() && ls.frames.iter().all(|f| f.image.is_some())
        };
        transport_filled = full(mosaic);
        if full(satellite) && transport_filled {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    // **Non-triviality of the fixture itself.** Every hour's granule must
    // have rasterized to its own picture; if two did not, the count below
    // would read as the defect when what really happened is that two hours
    // landed on one colour.
    {
        let mut shades: Vec<u64> = rasters.values().copied().collect();
        let described = shades.len();
        shades.sort_unstable();
        shades.dedup();
        assert_eq!(
            shades.len(),
            described,
            "fixture: {described} satellite granules rasterized to \
             {} distinct pictures. The ladder of brightness temperatures has \
             walked out of the band the IR ramp discriminates over, so \
             nothing below can tell one hour from another.",
            shades.len(),
        );
    }

    let (sweep, satellite_frames) = {
        let pane = app.gui.pane(0).expect("pane 0");
        (
            pane.time_state(&known::MRMS).frames.len(),
            pane.time_state(&known::GMGSI).frames.len(),
        )
    };
    assert!(
        transport_filled,
        "the fixture's own transport never finished loading: it holds {sweep} \
         frames and the satellite {satellite_frames}. There is no sweep to \
         read, so nothing below would be about the defect.",
    );
    assert!(
        sweep > 1,
        "premise: the transport built a loop to sweep, and it holds {sweep} \
         frames",
    );

    // Play it, exactly as the pump does: results applied, then advance.
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        pane.transport_state_mut().phase = rustdar_egui::pane::LoopPhase::Playing;
    }
    let mut drawn: Vec<Option<u64>> = Vec::new();
    let mut instants: Vec<Option<chrono::NaiveDateTime>> = Vec::new();
    let mut mosaic_handles: Vec<Option<egui::TextureId>> = Vec::new();
    for _ in 0..sweep {
        wire.answer(&mut app);
        capture_rasters(&app, &mut rasters);
        pump(&mut app, &ctx);

        let pane = app.gui.pane(0).expect("pane 0");
        // **Handle -> frame -> stamp -> pixels.** The handle alone is upload
        // identity, which every frame has by construction; what is counted is
        // the raster it carries.
        let on_glass = pane
            .overlay_texture_on_screen(&known::GMGSI)
            .map(|tex| tex.texture.id());
        let ls = pane.time_state(&known::GMGSI);
        let depicts = on_glass.and_then(|id| {
            ls.frames
                .iter()
                .find(|f| {
                    f.image
                        .as_ref()
                        .and_then(rustdar_egui::pane::LoopFrameImage::overlay)
                        .is_some_and(|o| o.texture.id() == id)
                })
                .and_then(|f| rasters.get(&f.timestamp).copied())
        });
        drawn.push(depicts);
        instants.push(ls.playhead_stamp());
        mosaic_handles.push(
            pane.overlay_texture_on_screen(&known::MRMS)
                .map(|tex| tex.texture.id()),
        );

        app.gui
            .pane_mut(0)
            .expect("pane 0")
            .transport_state_mut()
            .last_advance = None;
        app.advance_loop_playback();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    let mut distinct: Vec<Option<u64>> = drawn.clone();
    distinct.sort_unstable();
    distinct.dedup();

    // 1. The user's sentence.
    assert_eq!(
        distinct.len(),
        sweep,
        "satellite view still doesn't loop either, I did a 600 min loop and \
         the GMGSI never changes during it — {} of {sweep} distinct pictures \
         reached the glass across a full sweep. The satellite layer holds \
         {satellite_frames} frames and {} of them carry a raster; what each \
         step of the sweep drew was {drawn:?}, and the instant it claimed to \
         depict was {instants:?}.",
        distinct.len(),
        rasters.len(),
    );
    assert!(
        drawn.iter().all(Option::is_some),
        "a step of the sweep painted nothing at all: {drawn:?}",
    );

    // 2. The depicted instant really moves, across the whole list.
    let mut visited: Vec<Option<chrono::NaiveDateTime>> = instants.clone();
    visited.sort_unstable();
    visited.dedup();
    assert_eq!(
        visited.len(),
        sweep,
        "the satellite playhead stopped on {} instants over {sweep} steps: \
         {instants:?}. A picture that changes while the caption does not is \
         the other half of the same defect.",
        visited.len(),
    );

    // 3. Non-triviality: the transport is animating too, so this is not a
    //    green earned by a pane where nothing moves.
    let mut mosaic_distinct = mosaic_handles.clone();
    mosaic_distinct.sort_by_key(|id| format!("{id:?}"));
    mosaic_distinct.dedup();
    assert_eq!(
        mosaic_distinct.len(),
        sweep,
        "control: the transport painted {} pictures over {sweep} steps, so \
         this fixture is not sweeping a loop at all and the satellite \
         assertion above is vacuous",
        mosaic_distinct.len(),
    );

    // Floor B: one window for the whole pane.
    assert_eq!(
        satellite_range,
        Some(transport_range),
        "the satellite layer must be listed over the window the pane's clock \
         actually sweeps. Listed over its own `min_loop_span_secs` floor \
         instead, its frames would sit outside the range the transport can \
         name and the playhead could never stop on them; listed over no window \
         at all, it is not armed and this is the defect itself.",
    );

    // Floor A: the ask is bounded, not one storm per pump frame.
    let ceiling = satellite_frames * 3;
    assert!(
        wire.satellite_gets.len() <= ceiling,
        "the satellite layer was asked for {} granules over a run holding \
         {satellite_frames} frames (ceiling {ceiling}). Arming a second layer \
         must not put a 7.5 MB GET per frame on the wire per frame of the \
         pump; the in-flight mark in `refetch_owed_loop_frames` is what holds \
         that, and this is the count that says so.",
        wire.satellite_gets.len(),
    );
}
