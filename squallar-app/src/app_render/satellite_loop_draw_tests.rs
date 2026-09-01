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
//! Measured on this fixture: **0 of 13 distinct pictures** — the satellite
//! layer holding no loop frames at all while the mosaic transport swept all
//! thirteen of its own. The denominator is the transport's own frame count,
//! which is why every message below prints it rather than naming it.
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
//! [`PaneState::overlay_texture_on_screen`]: squallar_egui::pane::PaneState::overlay_texture_on_screen

use chrono::Timelike;
use squallar_geo::GeoBounds;
use squallar_source::handler::SourceEvent;
use squallar_source::id::known;
use squallar_source::time::{FrameListing, FrameStamp};
use std::collections::HashMap;
use std::sync::Arc;

use squallar_overlays::gmgsi::decode::GmgsiGrid;
use squallar_overlays::gmgsi::{GmgsiChannel, GmgsiFrameFetch, GmgsiListing};
use squallar_overlays::hrrr::GridCoords;
use squallar_overlays::mrms::{MrmsFrameFetch, MrmsGrid, MrmsListing, MrmsProduct};
use squallar_overlays::render::gridded::ResidentGrid;

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
    let spec = squallar_overlays::gmgsi::fields::spec(CHANNEL);
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
    let spec = squallar_overlays::mrms::fields::spec(PRODUCT);
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
fn draw(app: &mut crate::app::App, id: &squallar_source::id::LayerId, on: bool) {
    let (panes, overlays) = app.gui.panes_and_overlays_mut();
    panes[0].hydrate_layer_states(overlays, 0);
    panes[0].set_layer_enabled(overlays, 0, id, on);
    panes[0].adopt_handler_state(overlays);
    panes[0].refresh_transport(overlays);
}

fn a_render_request() -> crate::app::fetch::OverlayRenderRequest {
    crate::app::fetch::OverlayRenderRequest {
        geo_bounds: bounds(),
        texture: squallar_egui::overlay_cache::OverlayTexturePlan {
            width: 32,
            height: 32,
            overdraw: 0.0,
            pixels_per_point: 1.0,
        },
        data_generation: 0,
        zoom: 32,
    }
}

/// The whole hour `at` sits in.
fn top_of_hour(at: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
    at.with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .expect("a whole hour exists")
}

/// The hours a real GMGSI listing of `range` would have found, one blended
/// mosaic per hour — **`hours_in_range`'s own shape, both edges rounding
/// down.**
///
/// The hour a window *starts inside* is in the list: its granule depicts that
/// hour and is the only picture the window's first minutes can be drawn from.
/// Copying the leading round-up this fixture used to carry is what made it
/// blind to the blank leading frame — the satellite's oldest granule landed
/// after the transport's oldest stamp and nothing painted there.
fn hours_inside(
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
) -> Vec<chrono::NaiveDateTime> {
    let (lo, hi) = range;
    let mut at = top_of_hour(lo);
    let mut hours = Vec::new();
    while at <= hi {
        hours.push(at);
        at += chrono::Duration::hours(1);
    }
    hours
}

/// **The instants the transport's own listing names inside `range`**: an
/// hourly rail anchored on the **window's own start**, and never on a whole
/// hour.
///
/// Two properties, and the fixture is blind to a different defect without
/// each.
///
/// **It starts where the window starts.** Real MRMS lists 2-minute objects
/// from `range.0` (`squallar_overlays::render::handlers::mrms`), so the pane's
/// clock stops on instants inside the window's first partial hour — instants
/// *earlier* than any whole hour in it. Those stops are where the satellite
/// has to carry an earlier granule forward, and they are the ones the user saw
/// blank. A rail that began at the first whole hour instead could never reach
/// them, which is what the "half past each of the satellite's hours" rail did.
/// The two-minute nudge only matters in the one minute of every hour where the
/// wall clock would put `range.0` exactly on the hour; every other start is
/// already inside one.
///
/// **It is off the satellite's grid.** Each mosaic stamp therefore settles the
/// satellite playhead onto exactly one hour — `qualifying_frame_at` takes the
/// newest frame at or before the clock — and a full sweep visits a different
/// satellite hour every tick. Equal cadences *on the same grid* would make the
/// two timelines one list and would prove nothing about the settling.
///
/// **Derived from the window it was asked for, like the satellite's.** A
/// bucket answers the range it was given; answering a *later* ask with an
/// earlier ask's stamps lists nothing inside it, and `accept_loop_scan_listings`
/// retires a loop that lists nothing — which is a fixture that takes its own
/// subject away, not a defect.
fn mosaic_stamps(
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
) -> Vec<chrono::NaiveDateTime> {
    let mut at = range
        .0
        .max(top_of_hour(range.0) + chrono::Duration::minutes(2));
    let mut stamps = Vec::new();
    while at <= range.1 {
        stamps.push(at);
        at += chrono::Duration::hours(1);
    }
    stamps
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
    id: squallar_source::id::LayerId,
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
                squallar_source::origins::DataSources::mrms_key(PRODUCT.prefix_name(), &at),
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

/// **Every picture the app really handed to egui, by the texture it became** —
/// taken off the texture manager's own delta queue, downstream of the poller.
///
/// **This replaced a sniff of the render channel, and the reason is a measured
/// flake.** The old helper drained `overlay_render_receiver`, hashed each
/// GMGSI loop frame's response and re-sent it; `pump` then ran
/// `poll_overlay_render_results`, which drains the same receiver. Two consumers,
/// one channel — and the window between them is not a few instructions but the
/// whole `Ingest` phase plus four `Apply` rows of `FRAME_PUMP`. A raster
/// finishing on a pool thread inside that window was taken by the *production*
/// poller, uploaded correctly, and never seen here. The frame then drew
/// perfectly and its pixels were unknown, so its rail stop filed `None` — and
/// because the byte budget holds all thirteen 32x32 textures, nothing was ever
/// evicted and nothing re-rendered, so the miss was permanent. That is exactly
/// `left: 12, right: 13`, one loop step short of a full sweep: **1 failure in
/// 30 runs on a branch, 1 in 20 on unmodified main.**
///
/// Reading the texture manager instead removes the second consumer entirely.
/// The delta is created by `Context::load_texture` *inside* the poller, and is
/// read here after `pump` has returned, so there is no interleaving left to
/// lose: it is ordered by the frame, not by a thread. It is also the stronger
/// reading — these are the pixels egui was given for that exact texture id,
/// rather than a response believed to have become it.
///
/// Whole-image deltas only (`pos.is_none()`), which is every overlay upload;
/// a banded partial would carry a fragment and hashing it as a picture would
/// be a different claim.
fn capture_uploads(ctx: &egui::Context, seen: &mut HashMap<egui::TextureId, u64>) {
    for (id, delta) in ctx.tex_manager().write().take_delta().set {
        if delta.pos.is_some() {
            continue;
        }
        let egui::epaint::image::ImageData::Color(image) = delta.image;
        seen.insert(id, hash_of(&image));
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
fn live_raster(app: &mut crate::app::App, ctx: &egui::Context, id: squallar_source::id::LayerId) {
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            ink: true,
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
    app.http_client = crate::app::tests::unreachable_http_client();

    draw(&mut app, &known::GMGSI, true);
    draw(&mut app, &known::MRMS, true);
    draw(&mut app, &known::RADAR, false);
    app
}

/// **Everything one full sweep of that pane put on the glass**, tick by tick.
///
/// Both acceptances below are questions about the same run. Driving it twice
/// would pay for a second six-hundred-pump fill through the real renderer to
/// ask a second question about a picture already taken.
struct Sweep {
    /// The window the pane's clock was armed over.
    transport_range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    /// The window the satellite layer was listed over — `None` if it was never
    /// armed at all, which is the shape of the defect `5ef52be5` fixed.
    satellite_range: Option<(chrono::NaiveDateTime, chrono::NaiveDateTime)>,
    /// **The transport's own frame list**: the instants the pane's clock stops
    /// on, oldest first. Its length is the sweep.
    rail: Vec<chrono::NaiveDateTime>,
    /// How many frames the satellite layer's own timeline holds.
    satellite_frames: usize,
    /// How many distinct satellite instants the production renderer produced a
    /// raster for.
    rasters: usize,
    /// The satellite picture on the glass at each stop of the rail, in rail
    /// order, hashed — `None` where nothing painted at all. Keyed by the stop
    /// the pane's clock named when it was read, never by which iteration of
    /// the sweep loop read it — see the read loop for why the difference is a
    /// flake.
    drawn: Vec<Option<u64>>,
    /// The instant the satellite playhead claimed at each stop, in rail order.
    instants: Vec<Option<chrono::NaiveDateTime>>,
    /// The transport's own handle at each stop, which is the control.
    mosaic_handles: Vec<Option<egui::TextureId>>,
    /// How many satellite granules were asked for on the wire across the run.
    satellite_gets: usize,
}

/// Enable a twelve-hour loop on a pane drawing the satellite layer under
/// another layer's transport, fill it through the real supply, then play it
/// and read — every tick — what `overlay_texture_on_screen` hands the painter
/// for the satellite layer and which raster that handle is.
///
/// Everything asserted here is a **premise**: a fixture that fails one of them
/// has no sweep to read, so a red below would be about the fixture rather than
/// about the defect. The claims themselves are in the two tests.
fn sweep_a_satellite_loop() -> Sweep {
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
        squallar_egui::actions::GuiAction::EnableLoop {
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

    // Floor B's reading, taken at the moment both timelines were armed.
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
    // Keyed by the texture the picture became, and filled after every pump —
    // see `capture_uploads` for why this is not read off the render channel.
    let mut uploads: HashMap<egui::TextureId, u64> = HashMap::new();
    capture_uploads(&ctx, &mut uploads);

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
        pump(&mut app, &ctx);
        capture_uploads(&ctx, &mut uploads);
        let pane = app.gui.pane(0).expect("pane 0");
        let satellite = pane.time_state(&known::GMGSI);
        let mosaic = pane.time_state(&known::MRMS);
        let full = |ls: &squallar_egui::pane::LayerTimeState| {
            !ls.frames.is_empty() && ls.frames.iter().all(|f| f.image.is_some())
        };
        transport_filled = full(mosaic);
        if full(satellite) && transport_filled {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    // **Non-triviality of the fixture itself, and the completeness of the
    // capture, in one block.** Every hour's granule must have rasterized to
    // its own picture; if two did not, the count below would read as the
    // defect when what really happened is that two hours landed on one colour.
    //
    // The first assertion is what the channel sniff this replaced could never
    // make: every filed frame's texture is one `capture_uploads` saw. A miss
    // there was the flake — a picture correctly drawn whose pixels were
    // unknown here, filing `None` at its rail stop for ever.
    let rasters = {
        let pane = app.gui.pane(0).expect("pane 0");
        let ls = pane.time_state(&known::GMGSI);
        let filed: Vec<egui::TextureId> = ls
            .frames
            .iter()
            .filter_map(|f| {
                f.image
                    .as_ref()
                    .and_then(squallar_egui::pane::LoopFrameImage::overlay)
            })
            .map(|o| o.texture.id())
            .collect();
        let mut shades: Vec<u64> = filed
            .iter()
            .filter_map(|id| uploads.get(id).copied())
            .collect();
        assert_eq!(
            shades.len(),
            filed.len(),
            "fixture: {} of the satellite layer's {} filed frames carry a \
             texture this run never saw uploaded. Those steps can only ever \
             read as blank, whatever the app draws — which is the shape of \
             the flake this capture was rewritten to remove, not a defect in \
             the loop.",
            filed.len() - shades.len(),
            filed.len(),
        );
        let described = shades.len();
        assert!(
            described > 1,
            "fixture: the satellite layer filed {described} pictures, so the \
             distinctness asserted next is over nothing",
        );
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
        described
    };

    let (rail, satellite_frames) = {
        let pane = app.gui.pane(0).expect("pane 0");
        (
            pane.time_state(&known::MRMS)
                .frames
                .iter()
                .map(|f| f.timestamp)
                .collect::<Vec<_>>(),
            pane.time_state(&known::GMGSI).frames.len(),
        )
    };
    let sweep = rail.len();
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
        pane.transport_state_mut().phase = squallar_egui::pane::LoopPhase::Playing;
    }
    // **Read by stop, never by iteration count.** The pump above is the
    // production frame, and its Advance phase runs `advance_loop_playback`
    // behind a wall-clock throttle (`loop_interval`: 100 ms at the default
    // 10 fps); the forced advance at the bottom of this loop is the fixture's
    // own driver. Two drivers, one playhead: any iteration that stalls past
    // the interval — one did, on a machine running the whole workspace suite —
    // lets the pump fire a second advance between the forced one and the next
    // read, and a schedule of exactly `sweep` positional reads then skips one
    // stop and wraps onto a duplicate of its first. Reproduced on demand with
    // a doctored 150 ms stall after one forced advance: 12 of 13 distinct
    // pictures, the first picture read at both ends, one hour never read.
    // So each read files under the stop the pane's clock actually names —
    // first observation wins, which is what keeps a blank step a blank step —
    // and the sweep is read when every stop of the rail has been, however the
    // two drivers interleaved.
    let mut drawn_at: HashMap<chrono::NaiveDateTime, Option<u64>> = HashMap::new();
    let mut instants_at: HashMap<chrono::NaiveDateTime, Option<chrono::NaiveDateTime>> =
        HashMap::new();
    let mut mosaic_at: HashMap<chrono::NaiveDateTime, Option<egui::TextureId>> = HashMap::new();
    for _ in 0..sweep * 8 {
        wire.answer(&mut app);
        pump(&mut app, &ctx);
        capture_uploads(&ctx, &mut uploads);

        let pane = app.gui.pane(0).expect("pane 0");
        if let Some(stop) = pane.time.mode.as_of() {
            // **Handle -> pixels, and nothing in between.** The handle alone
            // is upload identity, which every frame has by construction; what
            // is counted is the raster it carries, looked up by the very id
            // egui was handed those pixels under. The old spelling went
            // handle -> frame -> stamp -> pixels through a `find` over the
            // frame list, which needed a second map that a channel race could
            // leave a hole in.
            let ls = pane.time_state(&known::GMGSI);
            let depicts = pane
                .overlay_texture_on_screen(&known::GMGSI)
                .and_then(|tex| uploads.get(&tex.texture.id()).copied());
            drawn_at.entry(stop).or_insert(depicts);
            instants_at
                .entry(stop)
                .or_insert_with(|| ls.playhead_stamp());
            mosaic_at.entry(stop).or_insert_with(|| {
                pane.overlay_texture_on_screen(&known::MRMS)
                    .map(|tex| tex.texture.id())
            });
        }
        if rail.iter().all(|stop| drawn_at.contains_key(stop)) {
            break;
        }

        app.gui
            .pane_mut(0)
            .expect("pane 0")
            .transport_state_mut()
            .last_advance = None;
        app.advance_loop_playback();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let unvisited: Vec<&chrono::NaiveDateTime> = rail
        .iter()
        .filter(|stop| !drawn_at.contains_key(stop))
        .collect();
    assert!(
        unvisited.is_empty(),
        "premise: playback has to stop on every instant of the rail before the \
         sweep can be read at all; after {} ticks it never reached {unvisited:?}. \
         A red here is about this fixture's scheduling, not about the defect.",
        sweep * 8,
    );
    // In rail order, so every claim below is about the sweep the user watches.
    let drawn: Vec<Option<u64>> = rail.iter().map(|stop| drawn_at[stop]).collect();
    let instants: Vec<Option<chrono::NaiveDateTime>> =
        rail.iter().map(|stop| instants_at[stop]).collect();
    let mosaic_handles: Vec<Option<egui::TextureId>> =
        rail.iter().map(|stop| mosaic_at[stop]).collect();

    Sweep {
        transport_range,
        satellite_range,
        rail,
        satellite_frames,
        rasters,
        drawn,
        instants,
        mosaic_handles,
        satellite_gets: wire.satellite_gets.len(),
    }
}

/// **The acceptance for `5ef52be5`.** Every step of the sweep draws a
/// different satellite picture.
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
/// **Counted over the steps that painted, and only those.** A blank step is
/// not a picture. Deduping `Option<u64>` counted `None` as one more distinct
/// entry beside the real rasters, which is how this suite read green across
/// the whole of the blank leading frame — see
/// `a_satellite_loop_paints_from_the_first_step_of_the_sweep`, which is the
/// test for that defect and the reason the two are separate.
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
/// armed) — **0 of 13 distinct pictures**: the satellite layer holds no
/// frames, its playhead names no instant, and nothing paints for the whole
/// sweep.
///
/// **Observed red under a partial under-reach** (`half_a_window`: hand every
/// layer after the transport `window.map(|(s, e)| (s + (e - s) / 2, e))`, so
/// the satellite is armed but over the newer half of the pane's window) —
/// **7 of 13 distinct pictures**: seven real rasters over the half it holds,
/// nothing over the half it does not.
#[test]
fn every_step_of_a_loop_draws_its_own_satellite_picture() {
    let run = sweep_a_satellite_loop();
    let Sweep {
        transport_range,
        satellite_range,
        satellite_frames,
        rasters,
        ref drawn,
        ref instants,
        ref mosaic_handles,
        satellite_gets,
        ..
    } = run;
    let sweep = run.rail.len();

    // **Distinct over the steps that painted.** `None` is the absence of a
    // picture, not one more of them.
    let mut distinct: Vec<u64> = drawn.iter().flatten().copied().collect();
    distinct.sort_unstable();
    distinct.dedup();

    // 1. The user's sentence.
    assert_eq!(
        distinct.len(),
        sweep,
        "satellite view still doesn't loop either, I did a 600 min loop and \
         the GMGSI never changes during it — {} of {sweep} distinct pictures \
         reached the glass across a full sweep. The satellite layer holds \
         {satellite_frames} frames and {rasters} of them carry a raster; what \
         each step of the sweep drew was {drawn:?}, and the instant it claimed \
         to depict was {instants:?}.",
        distinct.len(),
    );
    assert!(
        drawn.iter().all(Option::is_some),
        "a step of the sweep painted nothing at all: {drawn:?}",
    );

    // 2. The depicted instant really moves, across the whole list — and, as
    //    above, an unnamed instant is not one of them.
    let mut visited: Vec<chrono::NaiveDateTime> = instants.iter().flatten().copied().collect();
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
        satellite_gets <= ceiling,
        "the satellite layer was asked for {satellite_gets} granules over a \
         run holding {satellite_frames} frames (ceiling {ceiling}). Arming a \
         second layer must not put a 7.5 MB GET per frame on the wire per \
         frame of the pump; the in-flight mark in `refetch_owed_loop_frames` \
         is what holds that, and this is the count that says so.",
    );
}

/// **The acceptance for the blank leading frame** — the user's third report of
/// the same area.
///
/// *"it does update, but some of the frames have no data at all" / "er the
/// first frame doesn't have data it seems like"*
///
/// A loop enabled at `HH:MM` arms every layer over `HH:MM - 12h ..= HH:MM`.
/// The transport lists objects from the window's start, so the pane's clock
/// stops inside the window's first partial hour — before any whole hour in it.
/// The satellite's oldest granule is the hour that window *starts inside*, and
/// every one of those early stops is drawn by carrying it forward. Listing
/// from `ceil(range.0)` instead left them with nothing at all: `60 - MM`
/// minutes of blank rail at the head of every loop, escaped only by enabling
/// one exactly on the hour.
///
/// **Floors.**
///
/// (a) **The first step's satellite stamp is strictly earlier than the rail's
/// own start.** Carrying forward is what has to have happened; a fixture whose
/// rail happened to begin on a whole hour would paint from the first step
/// without ever exercising it. The rail is nudged off the hour for exactly
/// this reason (`mosaic_stamps`), and the premise below says so rather than
/// trusting it.
///
/// (b) The two assertions in
/// `every_step_of_a_loop_draws_its_own_satellite_picture` that a blank step
/// used to slip past — the all-`Some` one and the visited-instants one — are
/// kept there and now reachable.
///
/// (c) **The transport's own rail is untouched, stamp for stamp.** Making the
/// satellite paint everywhere by trimming the pane's clock down to the
/// satellite's coarser grid would satisfy every other assertion here and lose
/// the user the loop they asked for.
#[test]
fn a_satellite_loop_paints_from_the_first_step_of_the_sweep() {
    let run = sweep_a_satellite_loop();

    let first_stop = *run.rail.first().expect("the sweep has a first step");
    assert!(
        top_of_hour(first_stop) < first_stop,
        "premise: the rail's first stop must sit strictly inside an hour, or \
         nothing below is about carrying a granule forward. It is {first_stop}.",
    );

    // Floor (c): the transport's own rail, stamp for stamp.
    assert_eq!(
        run.rail,
        mosaic_stamps(run.transport_range),
        "the transport's own frame list must be exactly what its listing \
         named. Trimming the pane's clock down to the satellite's hourly grid \
         would paint every step and take the loop away.",
    );

    // Floor (a) and the claim itself, in one reading: the first step painted,
    // and what it painted is the granule of the hour the window opened inside.
    assert_eq!(
        run.instants.first().copied().flatten(),
        Some(top_of_hour(first_stop)),
        "the first step of the sweep stops at {first_stop}, inside hour {}. \
         The satellite granule for that hour is the only picture it can be \
         drawn from, and the playhead named {:?} instead. The satellite layer \
         holds {} frames over {:?}; what each step drew was {:?}.",
        top_of_hour(first_stop),
        run.instants.first().copied().flatten(),
        run.satellite_frames,
        run.satellite_range,
        run.drawn,
    );
    assert!(
        run.drawn.first().copied().flatten().is_some(),
        "er the first frame doesn't have data it seems like — the first step \
         of the sweep painted no satellite picture at all. Across the whole \
         sweep the steps drew {:?} at {:?}.",
        run.drawn,
        run.instants,
    );
}
