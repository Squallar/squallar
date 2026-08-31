//! **A pane clock that sweeps, and what the dispatch policy spends following
//! it.**
//!
//! The rig here is [`coverage_dispatch_tests::PanRig`]'s twin with the moving
//! term swapped: there the viewport moves and the token stands still, here the
//! token moves and the viewport stands still. It is the same asynchronous
//! pipeline — a raster takes frames to come back, an upload takes frames to
//! land — because a *synchronous* fixture cannot reproduce a discarded upload
//! at all (see [`ledger::Totals::superseded`]'s note, which records that
//! mistake being made once already).
//!
//! Everything asserted is a count. Nothing here reads a clock.

use super::*;

/// The viewport every question is asked at. It never moves, so the coverage
/// arm can never be what answers and the content arm is alone under test.
fn viewport() -> GeoBounds {
    GeoBounds {
        min_lat: 34.5,
        max_lat: 35.5,
        min_lon: -97.5,
        max_lon: -96.5,
    }
}

const ZOOM: f64 = 7.0;
const W: u32 = 8;
const H: u32 = 5;

fn plan() -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: W,
        height: H,
        overdraw: OVERDRAW_FRACTION,
        pixels_per_point: 1.0,
    }
}

fn data_at(texture: &egui::TextureHandle, token: u64) -> OverlayTextureData {
    OverlayTextureData {
        texture: texture.clone(),
        placed: PlacedRaster::of(plan().coverage(&viewport())),
        data_generation: token,
        render_zoom: current_quantized_zoom(ZOOM),
        width: W,
        height: H,
        radar_meta: None,
        hit_map: None,
    }
}

fn a_texture(ctx: &egui::Context) -> egui::TextureHandle {
    ctx.load_texture(
        "clock-sweep",
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::NEAREST,
    )
}

/// **The pane's frame, in the order its host runs it**, with the pane clock
/// sweeping under it: promote a delivered hold, file the rasters that came
/// back, ask the gate, dispatch what it asks for, then move this frame's
/// upload bands.
///
/// The clock is the frame index and the token is a function of it, exactly as
/// `as_of_term` makes it a function of the depicted instant: `ticks_per_token`
/// frames share one as-of bucket, and the token moves when the bucket does.
pub(super) struct ClockRig {
    cache: OverlayTextureCache,
    texture: egui::TextureHandle,
    /// Frames one as-of bucket lasts. `1` is a clock sweeping a bucket per
    /// frame — the playing loop, where the pane's tick is worth minutes and
    /// the quantum is a minute.
    frames_per_token: u32,
    /// Frames between a dispatch and the raster coming back.
    raster_frames: u32,
    /// Frames a banded upload takes: 2 with a staging ring, 3 without.
    upload_frames: u32,
    /// Rasters on their way back: the frame they land on, and the token they
    /// were asked at. A list, not a slot — see [`super::coverage_dispatch_tests`].
    in_flight: Vec<(u32, u64)>,
    bands_left: u32,
    pub dispatches: u32,
    /// Pictures that reached the screen after their last band landed.
    pub promotions: u32,
    /// Uploads thrown away by a newer picture for the same destination — the
    /// ledger's `superseded`, reproduced.
    pub superseded: u32,
    /// Rasters that came back for a dispatch the cache had already moved past.
    pub discarded: u32,
    /// Frames on which the layer had nothing at all on the glass.
    pub dry: u32,
    /// How far the token on screen lagged the pane clock, summed over counted
    /// frames, in as-of buckets. The freshness figure: a policy that stops
    /// rasterizing altogether shows a big number here.
    pub lag_bucket_frames: u64,
    /// The furthest the token on screen ever fell behind the pane clock, in
    /// as-of buckets. The freshness ceiling, as one exact number.
    pub max_lag: u64,
    pub counted: u32,
}

impl ClockRig {
    pub(super) fn new(ctx: &egui::Context, frames_per_token: u32, upload_frames: u32) -> Self {
        let texture = a_texture(ctx);
        let mut cache = OverlayTextureCache::new();
        cache.show(data_at(&texture, 0));
        Self {
            cache,
            texture,
            frames_per_token,
            raster_frames: 1,
            upload_frames,
            in_flight: Vec::new(),
            bands_left: 0,
            dispatches: 0,
            promotions: 0,
            superseded: 0,
            discarded: 0,
            dry: 0,
            lag_bucket_frames: 0,
            max_lag: 0,
            counted: 0,
        }
    }

    pub(super) fn with_raster_frames(mut self, frames: u32) -> Self {
        self.raster_frames = frames;
        self
    }

    /// The bucket the pane clock is in on frame `f` — `0` while the clock is
    /// parked (`frames_per_token == 0`).
    fn token_at(&self, f: u32) -> u64 {
        if self.frames_per_token == 0 {
            return 0;
        }
        (f / self.frames_per_token) as u64
    }

    /// Rasters this rig is still carrying when a run ends: on the wire, or
    /// held with their upload unfinished. The conservation term — every
    /// dispatch is promoted, superseded, discarded, or still one of these.
    pub(super) fn outstanding(&self) -> u32 {
        self.in_flight.len() as u32 + u32::from(self.cache.is_holding())
    }

    pub(super) fn run(&mut self, frames: u32) {
        for f in 0..frames {
            let token = self.token_at(f);

            if self.bands_left == 0
                && let Some(held) = self.cache.take_held_if_delivered(|_| true)
            {
                self.cache.show(held.data);
                self.promotions += 1;
            }

            let mut landed: Vec<u64> = Vec::new();
            self.in_flight.retain(|(arrives, asked_at)| {
                if f >= *arrives {
                    landed.push(*asked_at);
                    false
                } else {
                    true
                }
            });
            for asked_at in landed {
                if !self
                    .cache
                    .renders
                    .retire(&RenderTicket::whole(asked_at, plan().coverage(&viewport())))
                {
                    self.discarded += 1;
                    continue;
                }
                let data = data_at(&self.texture, asked_at);
                if self.cache.current().is_none() {
                    self.cache.show(data);
                } else {
                    if self.cache.is_holding() {
                        self.superseded += 1;
                    }
                    self.cache.hold(data, None);
                    self.bands_left = self.upload_frames;
                }
            }

            let needs =
                self.cache
                    .needs_rerender(token, ZOOM, ZoomDrive::AT_REST, &viewport(), &plan());
            if needs && self.cache.renders.admits(RenderSlot::WHOLE, 1) {
                self.cache
                    .renders
                    .record(RenderTicket::whole(token, plan().coverage(&viewport())));
                self.in_flight.push((f + self.raster_frames, token));
                self.dispatches += 1;
            }

            self.counted += 1;
            match self.cache.current() {
                None => self.dry += 1,
                Some(tex) => {
                    let lag = token.saturating_sub(tex.data_generation);
                    self.lag_bucket_frames += lag;
                    self.max_lag = self.max_lag.max(lag);
                }
            }

            if self.cache.is_holding() {
                self.bands_left = self.bands_left.saturating_sub(1);
            }
        }
    }
}

/// The pipeline the gates below are run at unless they say otherwise: a raster
/// back in one frame, a banded upload three frames long — the shape a device
/// with no staging ring has, and the one the coverage sweep calls the deep end
/// of what the tree models. At one as-of bucket per frame it cannot keep up,
/// which is the whole point: a pipeline that keeps up has nothing to discard.
const RASTER_FRAMES: u32 = 1;
const UPLOAD_FRAMES: u32 = 3;

/// Frames every run below is scripted for. Long enough that the steady state,
/// not the first cycle, is what the counts are made of.
const FRAMES: u32 = 620;

/// **The card, as an identity: every raster this pane spends reaches the
/// screen, bar the one that teaches it its own pipeline.**
///
/// A pane clock sweeping one as-of bucket per frame is a playing loop: the
/// tick is worth minutes, the quantum is a minute, and every
/// `TimeAxis::EventLifetime` layer re-tokenizes on every one of them. The
/// shipped policy answered each of those moves with a whole-picture raster,
/// and the pipeline landed one in two — so half of every picture rasterized
/// was uploaded and then thrown away before a band of it was drawn.
///
/// **What is asserted is an equality on every term, never a ratio and never a
/// ceiling.** A ceiling passes on "stop rasterizing", which is a worse bug
/// wearing this one's clothes; the promotion count below is pinned to the
/// script's own pipeline depth so that failure reddens this test rather than
/// satisfying it, and [`a_pipeline_that_keeps_up_keeps_every_dispatch_it_had`]
/// is the second control on the same direction.
///
/// **RED on the unmodified baseline**, measured at f3c254c7 by disarming this
/// module's refusal alone: 248 dispatched, 123 promoted, **124 superseded** —
/// one discarded upload per picture that reached the screen, for the whole 620
/// frames. The brake it shipped with is cleared by every delivery, so the pane
/// re-learned the same lesson once per promotion and paid a raster for it each
/// time. With the refusal: 156 dispatched, **154** promoted, **1** superseded —
/// fewer rasters spent and *more* pictures on the glass, which is the whole
/// claim.
#[test]
fn a_swept_clock_spends_no_raster_it_cannot_land() {
    let ctx = egui::Context::default();
    let mut rig = ClockRig::new(&ctx, 1, UPLOAD_FRAMES).with_raster_frames(RASTER_FRAMES);
    rig.run(FRAMES);

    // The non-vacuity floor, first: a run that dispatched nothing satisfies
    // every equality below by arithmetic and measures none of them.
    assert!(
        rig.dispatches > 0,
        "fixture: the sweep must ask for rasters at all",
    );
    // **The under-draw side, and the count is the SCRIPT's.** The pipeline
    // turns over every `raster + upload` frames, so 620 frames hold that many
    // turns, less the one still crossing when the run ends. No term of the
    // policy is in this number — it is what the rig's own timings oblige — so a
    // policy that bought its supersede count by rasterizing *less* fails here
    // instead of passing quietly. The baseline promotes 123 against this 154,
    // so this arm is RED there too, and RED in the opposite direction to the
    // one below.
    assert_eq!(
        rig.promotions,
        (FRAMES / (RASTER_FRAMES + UPLOAD_FRAMES)) - 1,
        "{} pictures reached the screen where the script's pipeline turns over \
         {} times. Fewer means the map is drawing less than it should — the \
         failure a supersede ceiling on its own would pass on",
        rig.promotions,
        (FRAMES / (RASTER_FRAMES + UPLOAD_FRAMES)) - 1,
    );
    assert_eq!(
        rig.superseded, 1,
        "a pane under a sweeping clock discarded {} uploads across {} frames. \
         One is the whole allowance: the discard that shows this pipeline \
         cannot overlap. Every one after it is a picture rasterized, handed to \
         the GPU, and thrown away before a band of it was drawn",
        rig.superseded, rig.counted,
    );
    // Conservation, which is the rig's own integrity rather than the policy's:
    // a raster that is neither on the screen, nor thrown away, nor still on
    // its way is a raster this test cannot see and must not be counting.
    assert_eq!(
        rig.dispatches,
        rig.promotions + rig.superseded + rig.outstanding(),
        "{} rasters were spent; {} reached the screen, {} were discarded and \
         {} are still in the pipeline. They must add up",
        rig.dispatches,
        rig.promotions,
        rig.superseded,
        rig.outstanding(),
    );
    // And the map is never left with nothing, which is the failure mode the
    // equalities above would otherwise be bought with.
    assert_eq!(
        rig.dry, 0,
        "the layer had no picture at all on {} of {} frames",
        rig.dry, rig.counted,
    );
}

/// **The picture on the glass gets FRESHER, not staler.**
///
/// The refusal above is a refusal to spend a raster, so the reading it must
/// not license is "the map fell behind to pay for it". It does the opposite,
/// and the reason is arithmetic rather than luck: a discarded upload is a
/// picture that never reached the screen, so the pane went on drawing an
/// *older* one than the pipeline had already paid for.
///
/// The freshness figure is a count of as-of buckets, never a duration: the
/// furthest the token on screen ever fell behind the pane clock across the
/// whole run.
///
/// **RED on the unmodified baseline**, measured at f3c254c7: `max_lag` 8
/// buckets against the 7 asserted here, and 3697 bucket-frames of accumulated
/// lag against 3390.
#[test]
fn refusing_the_raster_leaves_the_glass_fresher_than_spending_it() {
    let ctx = egui::Context::default();
    let mut rig = ClockRig::new(&ctx, 1, UPLOAD_FRAMES).with_raster_frames(RASTER_FRAMES);
    rig.run(FRAMES);

    assert!(
        rig.promotions > 0,
        "fixture: pictures must reach the screen"
    );
    // **The ceiling is the script's, not the code's.** The pipeline turns over
    // every `raster + upload` frames and the clock moves a bucket per frame, so
    // a picture reaches the screen that many buckets behind and then stays on
    // it for one more whole turn: `2 x (raster + upload) - 1`, which is 7 here.
    // Anything past that is a turn whose raster was paid for and never drawn.
    assert_eq!(
        rig.max_lag,
        2 * (RASTER_FRAMES + UPLOAD_FRAMES) as u64 - 1,
        "the picture on screen fell {} as-of buckets behind the pane clock \
         where the pipeline's own depth allows {}. The excess is turns of the \
         pipeline that produced a picture and threw it away",
        rig.max_lag,
        2 * (RASTER_FRAMES + UPLOAD_FRAMES) - 1,
    );
}

/// **The control that stops the gates above from being satisfied by "never
/// re-rasterize".**
///
/// This one passes on the unmodified baseline too, and that is what it is
/// for: it pins the shape the refusal must NOT touch. A pipeline whose raster
/// lands exactly as its hold delivers discards nothing, so there is nothing
/// for the sweep to learn and nothing to refuse — and it must go on spending
/// one raster every two frames and putting every one of them on the screen.
///
/// The expected count is the SCRIPT's, not the code's: the pipeline is two
/// frames of raster into two frames of upload, so it turns over every two
/// frames, and 620 frames hold 310 turns.
#[test]
fn a_pipeline_that_keeps_up_keeps_every_dispatch_it_had() {
    let ctx = egui::Context::default();
    let mut rig = ClockRig::new(&ctx, 1, 2).with_raster_frames(2);
    rig.run(FRAMES);

    assert_eq!(
        rig.superseded, 0,
        "fixture: this pipeline must discard nothing, or it is not the shape \
         this control exists to protect",
    );
    assert_eq!(
        (rig.dispatches, rig.promotions + rig.outstanding()),
        (FRAMES / 2, FRAMES / 2),
        "a sweep whose pipeline keeps up was throttled: {} rasters spent and \
         {} shown where the script turns the pipeline over {} times. The \
         refusal is for panes that have PROVED they discard, and this one \
         never has",
        rig.dispatches,
        rig.promotions + rig.outstanding(),
        FRAMES / 2,
    );
    assert_eq!(rig.dry, 0, "the map went blank on a pipeline that keeps up");
}

/// **The refusal outlives the delivery that clears the old brake — and that
/// difference is the entire fix.**
///
/// The shipped brake is `hold_superseded && held.is_some()`, and
/// `take_held_if_delivered` clears the first term on every delivery. Under a
/// clock that goes on sweeping, that makes the brake a rediscovery rather than
/// a policy: dispatch, discard, brake, land, *unbrake*, dispatch, discard —
/// one thrown-away upload per picture promoted, for ever. `sweep_discarded`
/// is the term a delivery does not clear.
///
/// **RED on the unmodified baseline**, and this is the one unit gate that
/// separates the two policies rather than being satisfied by either: at the
/// final ask the baseline has `hold_superseded == false` (the delivery just
/// cleared it) with a picture in flight, so its content arm returns `true` and
/// spends the raster whose upload the next one throws away.
#[test]
fn the_refusal_survives_the_delivery_that_clears_the_old_brake() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let mut cache = OverlayTextureCache::new();
    let ask = |cache: &mut OverlayTextureCache, token: u64| {
        cache.needs_rerender(token, ZOOM, ZoomDrive::AT_REST, &viewport(), &plan())
    };

    // Sweep the token, and discard an upload while doing it.
    cache.show(data_at(&texture, 0));
    for token in 1..=4u64 {
        ask(&mut cache, token);
        cache.hold(data_at(&texture, token), None);
    }

    // The delivery that opens the shipped brake: `hold_superseded` is false
    // from here, and the clock has not stopped sweeping.
    let landed = cache
        .take_held_if_delivered(|_| true)
        .expect("the hold is delivered");
    cache.show(landed.data);
    cache.hold(data_at(&texture, 5), None);

    assert!(
        !ask(&mut cache, 6),
        "a pane that has already thrown an upload away during this sweep spent \
         another raster the moment one delivery cleared the old brake. Its \
         answer replaces the picture now crossing to the GPU, so neither \
         reaches the viewer — and the next frame owes another one",
    );
}

/// **A clock that stops sweeping is a clock at rest, and the lesson dies with
/// the sweep.**
///
/// The refusal is armed by a discard *during a sweep*. If it outlived the
/// sweep it would be a latch, and a latch that never opens is exactly the
/// "never re-rasterize" failure the control above guards the other end of:
/// the pane would refuse the parked instant's own picture for ever.
///
/// **A control: this passes on the unmodified baseline too**, where the
/// delivery below is what opens the shipped brake. It is here to pin that the
/// new term is *also* released — by a different route, the quiet window — and
/// so cannot strand a parked pane.
///
/// Asserted at the unit, where the two states can be reached by hand.
#[test]
fn the_refusal_dies_with_the_sweep_that_armed_it() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let mut cache = OverlayTextureCache::new();
    let ask = |cache: &mut OverlayTextureCache, token: u64| {
        cache.needs_rerender(token, ZOOM, ZoomDrive::AT_REST, &viewport(), &plan())
    };

    // Sweep the token, and discard an upload while doing it.
    cache.show(data_at(&texture, 0));
    for token in 1..=4u64 {
        ask(&mut cache, token);
        cache.hold(data_at(&texture, token), None);
    }
    assert!(
        !ask(&mut cache, 5),
        "fixture: a sweep that has discarded uploads must refuse, or there is \
         no armed state for the rest of this test to disarm",
    );

    // The clock parks, and the pane drains: the last hold lands, which is what
    // clears the brake the shipped policy already had. What must ALSO have
    // been forgotten by now is that this sweep ever discarded anything.
    for _ in 0..SWEEP_QUIET_FRAMES {
        ask(&mut cache, 5);
    }
    let landed = cache
        .take_held_if_delivered(|_| true)
        .expect("the hold is delivered");
    cache.show(landed.data);

    // One picture in flight and none thrown away — the state the control
    // `a_single_token_move_still_pipelines_into_a_pending_hold` pins. An armed
    // sweep would refuse it; a disarmed one pipelines it.
    cache.hold(data_at(&texture, 5), None);
    assert!(
        ask(&mut cache, 6),
        "the refusal outlived the sweep that armed it. It would be a latch, \
         and a pane that had once fallen behind would never pipeline again",
    );
}

/// **A token that moves once is not a sweep**, so the one-shot stimuli — data
/// arriving, a theme flip, a filter change — keep the pipelining they had.
///
/// The distinction is the whole design: a sweep owes another raster on the
/// next frame, so a dispatch into a pending hold can only replace it; a single
/// move owes nothing more, so the same dispatch gets its picture on the glass
/// a cycle sooner. This is the second half of
/// [`a_pipeline_that_keeps_up_keeps_every_dispatch_it_had`], asserted where the
/// classification itself lives rather than through a rig.
#[test]
fn a_single_token_move_still_pipelines_into_a_pending_hold() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let mut cache = OverlayTextureCache::new();
    let ask = |cache: &mut OverlayTextureCache, token: u64| {
        cache.needs_rerender(token, ZOOM, ZoomDrive::AT_REST, &viewport(), &plan())
    };

    cache.show(data_at(&texture, 0));
    // A long quiet, then one move — a pane sitting still while its data
    // arrives.
    for _ in 0..(SWEEP_QUIET_FRAMES * 2) {
        ask(&mut cache, 0);
    }
    cache.hold(data_at(&texture, 1), None);
    assert!(
        ask(&mut cache, 2),
        "a pane whose token moved once, with one picture landing, refused to \
         pipeline the next. Nothing else is owed after a one-shot move, so \
         the raster asked for here reaches the screen a whole upload sooner",
    );
}
