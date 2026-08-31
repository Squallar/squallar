//! When a re-render may be dispatched: the settle is the gesture's own state
//! counted in frames, the mid-gesture band is a platform policy, and the
//! picture judged is the newest one the cache has — held or shown.

use super::*;

/// The content token every question below is asked with, and the one every
/// fixture texture carries — so a `true` answer is never a token mismatch.
const TOKEN: u64 = 4242;

/// The zoom the fixture textures are rasterised at.
const ZOOM: f64 = 7.0;

/// Fixture texture dimensions — only their *consistency* with [`plan`] matters.
const W: u32 = 8;
const H: u32 = 5;

/// Run the settle countdown to zero from a standing start: the frames a pane
/// spends at rest before the settle fires, when nothing re-arms it.
fn rest_until_settled(cache: &mut OverlayTextureCache, zoom: f64) {
    for _ in 0..SETTLE_QUIET_FRAMES {
        cache.needs_rerender(TOKEN, zoom, ZoomDrive::AT_REST, &viewport(), &plan());
    }
}

/// The viewport every question is asked for.
fn viewport() -> GeoBounds {
    GeoBounds {
        min_lat: 34.5,
        max_lat: 35.5,
        min_lon: -97.5,
        max_lon: -96.5,
    }
}

/// Ground the fixture textures cover — far wider than [`viewport`], so
/// `pan_exceeds_coverage` cannot be what makes a re-render look necessary.
fn covered() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -102.0,
        max_lon: -92.0,
    }
}

/// The plan a frame would produce for a texture of exactly the fixture's size,
/// so the size test cannot be the reason an answer comes back `true`.
fn plan() -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: W,
        height: H,
        overdraw: 0.25,
        pixels_per_point: 1.0,
    }
}

/// A texture rasterised at `render_zoom`, satisfying every non-zoom arm of the
/// gate for [`viewport`] and [`plan`].
fn data_at(ctx: &egui::Context, name: &str, render_zoom: f64) -> OverlayTextureData {
    OverlayTextureData {
        texture: ctx.load_texture(
            name,
            egui::ColorImage::filled([1, 1], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        placed: squallar_geo::PlacedRaster::of(covered()),
        data_generation: TOKEN,
        render_zoom: current_quantized_zoom(render_zoom),
        width: W,
        height: H,
        radar_meta: None,
        hit_map: None,
    }
}

/// A cache showing a texture rasterised at [`ZOOM`], already run down to a
/// settled rest at that zoom — so every `true` below is the arm under test's.
fn satisfied_cache(ctx: &egui::Context) -> OverlayTextureCache {
    let mut cache = OverlayTextureCache::new();
    cache.show(data_at(ctx, "satisfied", ZOOM));
    rest_until_settled(&mut cache, ZOOM);
    assert!(
        !cache.needs_rerender(TOKEN, ZOOM, ZoomDrive::AT_REST, &viewport(), &plan()),
        "fixture: the parked texture must satisfy the gate at its own zoom, or \
         every `true` below is the fixture's and not the arm under test's",
    );
    assert!(
        !cache.settle_is_counting_down(),
        "fixture: a parked pane must not still be asking for frames",
    );
    cache
}

/// **The settle is counted in frames, and the count is the same at every
/// refresh rate.** This is the whole point of the mechanism: the wall clock it
/// replaced was 30 frames of soft overlay at 60 Hz and 125 at 250 Hz — the
/// same rule charging a person on a faster display four times more frames for
/// the same stop.
///
/// Nothing here reads a clock, which is why the assertion can be an equality
/// between two refresh rates rather than a ratio against one.
#[test]
fn the_settle_is_the_same_frame_count_at_every_refresh_rate() {
    let ctx = egui::Context::default();

    // The frames a settle takes, at a rate that is only a label: the cache is
    // never told what it is. Two rates that differ by more than 4x.
    let frames_to_settle = |_label_hz: u32| {
        let mut cache = satisfied_cache(&ctx);
        // One frame of gesture at a zoom inside the band, so the settle arm is
        // the only arm that can ever answer `true` here.
        let paused_at = ZOOM + 0.2;
        assert!(
            !cache.needs_rerender(TOKEN, paused_at, ZoomDrive::LIVE, &viewport(), &plan()),
            "a live gesture dispatched a full-size raster mid-zoom",
        );
        for frame in 1..=64u32 {
            if cache.needs_rerender(TOKEN, paused_at, ZoomDrive::AT_REST, &viewport(), &plan()) {
                return frame;
            }
        }
        panic!("the gesture ended and 64 frames of rest never settled: the overlay stays soft");
    };

    let at_60 = frames_to_settle(60);
    let at_250 = frames_to_settle(250);
    assert_eq!(
        at_60, at_250,
        "the same stop cost {at_60} frames at 60 Hz and {at_250} at 250 Hz. A \
         settle that is a duration charges the faster display more frames of \
         soft overlay for the same gesture; this one must not",
    );
    assert_eq!(
        at_60,
        u32::from(SETTLE_QUIET_FRAMES),
        "the settle no longer fires on the frame SETTLE_QUIET_FRAMES names",
    );
}

/// **A gesture that holds still is still a gesture.** Fingers resting on the
/// glass mid-pinch, a trackpad between two moves, a wheel action egui has not
/// yet called finished: the zoom is bit-identical on every one of those frames
/// and the person has not stopped. The drive says so; no amount of elapsed
/// time may overrule it.
///
/// This is the misfire the phone reproduced, and under the duration it was
/// only *hidden*: a hold longer than the delay dispatched a full-size raster
/// into the middle of the gesture.
#[test]
fn a_gesture_that_holds_still_never_settles_however_long_it_holds() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);

    let paused_at = ZOOM + 0.2;
    // Far more frames than any duration-based settle would have tolerated:
    // 600 frames is ten seconds at 60 Hz and twenty times the delay this
    // replaced, at a rate the cache is never told.
    for frame in 1..=600u32 {
        assert!(
            !cache.needs_rerender(TOKEN, paused_at, ZoomDrive::LIVE, &viewport(), &plan()),
            "frame {frame} of a gesture that is still live was called a settle: \
             a full-size raster asked for while the person is still zooming",
        );
    }

    // The gesture actually ends, and the settle is owed at once — which is
    // what proves the stanza above was withholding on the drive and not on
    // some other arm.
    rest_until_settled(&mut cache, paused_at);
    assert!(
        cache.needs_rerender(TOKEN, paused_at, ZoomDrive::AT_REST, &viewport(), &plan()),
        "the gesture ended and nothing asked for the exact texture: the \
         overlay stays soft forever",
    );
}

/// A zoom that moves without any gesture behind it — a keyboard step, a
/// restored viewport, a pane following another — re-arms the countdown too.
/// Without this arm the settle would fire on the same frame the zoom moved,
/// and a zoom that moves over several frames would spend a raster on each.
#[test]
fn a_zoom_that_moves_re_arms_the_countdown_even_with_no_gesture() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);

    let mut zoom = ZOOM;
    for step in 1..=8u32 {
        zoom += 0.05;
        assert!(
            !cache.needs_rerender(TOKEN, zoom, ZoomDrive::AT_REST, &viewport(), &plan()),
            "step {step} of a moving zoom dispatched on the frame it moved",
        );
    }
    rest_until_settled(&mut cache, zoom);
    assert!(
        cache.needs_rerender(TOKEN, zoom, ZoomDrive::AT_REST, &viewport(), &plan()),
        "the zoom stopped moving and nothing asked for the exact texture",
    );
}

/// The countdown is what the pane asks for frames on, and it says so only
/// while it is really counting: a pane whose picture is right must not be
/// holding the frame loop open.
#[test]
fn the_countdown_asks_for_frames_only_while_it_is_counting() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);

    let paused_at = ZOOM + 0.2;
    cache.needs_rerender(TOKEN, paused_at, ZoomDrive::LIVE, &viewport(), &plan());
    assert!(
        cache.settle_is_counting_down(),
        "a live gesture left nothing owed, so nothing will ask for the frame \
         the countdown needs and the picture stays soft",
    );
    for _ in 1..SETTLE_QUIET_FRAMES {
        cache.needs_rerender(TOKEN, paused_at, ZoomDrive::AT_REST, &viewport(), &plan());
        assert!(
            cache.settle_is_counting_down(),
            "the countdown stopped asking for frames before it had run out",
        );
    }
    assert!(
        cache.needs_rerender(TOKEN, paused_at, ZoomDrive::AT_REST, &viewport(), &plan()),
        "fixture: the last frame of the countdown must be the settle",
    );
    assert!(
        !cache.settle_is_counting_down(),
        "the countdown reached zero and the pane is still asking for a frame \
         every frame: a spin the settle can never end",
    );
}

/// The settle is level-triggered: a `true` answer nothing acted on is still
/// `true` on the next frame, for as long as the map is still.
#[test]
fn an_unanswered_settle_stays_owed() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);

    let paused_at = ZOOM + 0.2;
    cache.needs_rerender(TOKEN, paused_at, ZoomDrive::LIVE, &viewport(), &plan());
    rest_until_settled(&mut cache, paused_at);
    for frame in 1..=3 {
        assert!(
            cache.needs_rerender(TOKEN, paused_at, ZoomDrive::AT_REST, &viewport(), &plan()),
            "the settle was consumed by a frame that could not dispatch it \
             (frame {frame}), and the overlay is now permanently soft",
        );
    }
}

/// The mid-gesture band arm obeys the platform policy; the settle does not.
#[test]
fn the_mid_gesture_band_is_platform_policy_and_the_settle_is_not() {
    let ctx = egui::Context::default();
    let past_band = ZOOM + ZOOM_REBUILD_BAND + 0.5;

    // Native arm: a band crossing mid-gesture dispatches.
    let mut native = satisfied_cache(&ctx);
    assert!(
        native.needs_rerender_with_policy(
            TOKEN,
            past_band,
            ZoomDrive::LIVE,
            &viewport(),
            &plan(),
            true,
        ),
        "with the mid-gesture band allowed, a crossing past ZOOM_REBUILD_BAND \
         did not dispatch",
    );

    // wasm arm: the same crossing, mid-gesture, dispatches nothing — the
    // policy hold on gesture-time raster work (see [`MID_GESTURE_REBUILDS`]).
    let mut wasm = satisfied_cache(&ctx);
    assert!(
        !wasm.needs_rerender_with_policy(
            TOKEN,
            past_band,
            ZoomDrive::LIVE,
            &viewport(),
            &plan(),
            false,
        ),
        "with the mid-gesture band disallowed, a band crossing still \
         dispatched mid-gesture: a full-size raster asked for in the middle \
         of the user's zoom, on the arm that holds that work back",
    );

    // ...and the settle recovers it: the same cache, still past the band,
    // fingers at rest. This is what bounds the wasm arm's softness.
    for _ in 0..SETTLE_QUIET_FRAMES {
        wasm.needs_rerender_with_policy(
            TOKEN,
            past_band,
            ZoomDrive::AT_REST,
            &viewport(),
            &plan(),
            false,
        );
    }
    assert!(
        wasm.needs_rerender_with_policy(
            TOKEN,
            past_band,
            ZoomDrive::AT_REST,
            &viewport(),
            &plan(),
            false,
        ),
        "with the mid-gesture band disallowed the settle no longer fires \
         either, so a wasm gesture past the band would stay soft forever",
    );
}

/// The band policy is one value on every target, and that value is quiet:
/// no build re-rasterizes mid-gesture on the band arm. This replaced the
/// per-target `!cfg!(target_arch = "wasm32")` at WO-8, when the native arm
/// was measured as the re-raster storm; the settle arm — pinned above and
/// below — is what ends the resulting softness.
#[test]
// The lint is right that this is a constant; pinning it is the point.
#[allow(clippy::assertions_on_constants)]
fn the_band_policy_is_one_quiet_value_on_every_target() {
    assert!(
        !MID_GESTURE_REBUILDS,
        "the mid-gesture band re-raster came back on. That arm dispatches a \
         full-size raster per band crossing while the user is still zooming \
         — the measured storm WO-8 removed — and turning it back on is a \
         re-measurement's decision, not a refactor's",
    );
}

/// A held picture is a dispatch already answered: while it satisfies the gate,
/// nothing dispatches again — and the predecessor stays on screen.
#[test]
fn a_fresh_hold_is_a_dispatch_already_answered() {
    let ctx = egui::Context::default();
    // Inside the band, so the settle arm is the only arm in play on any
    // platform — this test is about the hold, not the band policy.
    let settled_at = ZOOM + 0.2;

    // Control: with only the on-screen picture, the settled zoom re-dispatches.
    let mut without_hold = satisfied_cache(&ctx);
    assert!(
        !without_hold.needs_rerender(TOKEN, settled_at, ZoomDrive::LIVE, &viewport(), &plan()),
        "fixture: the frame the zoom arrives on must not dispatch on some \
         other arm, or the control below is not about the settle",
    );
    rest_until_settled(&mut without_hold, settled_at);
    assert!(
        without_hold.needs_rerender(TOKEN, settled_at, ZoomDrive::AT_REST, &viewport(), &plan()),
        "control: without a hold this cache must want a render, or the false \
         below is vacuous",
    );

    // The same state, with the render's result already held.
    let mut with_hold = satisfied_cache(&ctx);
    let shown = with_hold
        .current()
        .expect("fixture cache shows a texture")
        .texture
        .id();
    assert!(!with_hold.needs_rerender(TOKEN, settled_at, ZoomDrive::LIVE, &viewport(), &plan()));
    with_hold.hold(data_at(&ctx, "arriving", settled_at), None);
    rest_until_settled(&mut with_hold, settled_at);

    assert!(
        !with_hold.needs_rerender(TOKEN, settled_at, ZoomDrive::AT_REST, &viewport(), &plan()),
        "a held result that answers the pane's own question was dispatched \
         again: every re-dispatch supersedes the hold and restarts its bands, \
         so the upload never completes and the dispatch never stops",
    );
    assert_eq!(
        with_hold
            .current()
            .expect("the predecessor is still on screen")
            .texture
            .id(),
        shown,
        "judging the hold replaced the on-screen picture: the swap belongs to \
         the promotion, not to the staleness question",
    );
    assert!(with_hold.is_holding());
}

/// **The storm gate.** Over the scripted pan-zoom loop, the zoom dispatch
/// count is exactly one settle per scripted wheel notch — the script's own
/// published [`gesture_player::pan_zoom_2d::NOTCHES_PER_LEG`], over both legs
/// and every loop — and nothing at all inside a notch's own motion.
///
/// **This replaced WO-8's quiet-phase equality, which could not survive the
/// mechanism it was written for.** That gate counted settles per scripted
/// *quiet phase* because a settle could only fire in one: the 500 ms duration
/// was wider than the script's 0.35 s notch gap by construction, so the ten
/// notches of a leg collapsed into the one dispatch that followed them. The
/// drive-based settle fires when the gesture is over rather than when a
/// stopwatch says so, and a deliberate notch *is* a gesture that is over —
/// so the honest count is per notch, and a gate still reading per quiet phase
/// would now be asserting that nine of every ten stops go unanswered.
///
/// An equality, not a ceiling, on both sides. **Below it** a notch's settle
/// never fired and the overlay is soft at a zoom the map has been resting at.
/// **Above it** something dispatched inside a notch's own motion, which is
/// the shape of the re-raster storm: a dispatch rate set by the pipeline's
/// round trip rather than by the person's hand.
///
/// The expected count comes from the script's constants and nothing else. The
/// drive is modelled the way egui reports one — live while a notch is being
/// applied and for the end-of-scroll window after it — and the fixture asserts
/// that window is comfortably inside the script's own notch gap, or the count
/// would be measuring the model instead of the rule.
#[test]
fn a_scripted_zoom_dispatches_exactly_one_raster_per_scripted_notch() {
    use crate::gesture_player::{self, GesturePlayer};

    const LOOPS: u32 = 2;
    /// The two zoom legs each leg-scripted loop has (in, then out).
    const ZOOM_LEGS_PER_LOOP: u32 = 2;
    let k = gesture_player::pan_zoom_2d::NOTCHES_PER_LEG as u32 * ZOOM_LEGS_PER_LOOP * LOOPS;

    let hz: f64 = 175.0;
    // egui holds a scroll action open until the platform's end phase or 150 ms
    // past the last wheel event, whichever comes first; a discrete wheel has no
    // phases, so this is the window a notch's drive really lasts.
    let end_of_scroll_frames = (0.150 * hz).ceil() as u32;
    // The script's own notch gap, in frames. The whole gate depends on a notch
    // being over before the next one starts.
    let notch_gap_frames = ((gesture_player::pan_zoom_2d::ZOOM_IN_END
        - gesture_player::pan_zoom_2d::QUIET_1_END)
        / gesture_player::pan_zoom_2d::NOTCHES_PER_LEG as f64
        * hz) as u32;
    assert!(
        end_of_scroll_frames + u32::from(SETTLE_QUIET_FRAMES) < notch_gap_frames,
        "the modelled drive ({end_of_scroll_frames} frames) plus the countdown \
         does not fit inside the script's notch gap ({notch_gap_frames} \
         frames), so the count below measures the model rather than the rule",
    );

    let ctx = egui::Context::default();
    let mut player = GesturePlayer::from_name("pan-zoom-2d").expect("the script name is known");
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));

    let mut zoom = ZOOM;
    let mut cache = satisfied_cache(&ctx);

    // One raster in flight at most, arriving one frame after its dispatch —
    // the tightest pipeline, which maximises what a storm could dispatch.
    let mut in_flight: Option<(RenderTicket, f64, u64)> = None;
    let mut dispatches: u32 = 0;
    let mut frames_since_event = u32::MAX;

    let frames = (gesture_player::LOOP_SECONDS * f64::from(LOOPS) * hz) as u64;
    for frame in 0..=frames {
        let now = frame as f64 / hz;
        let events = player.events_for_frame(now, screen);
        if events.is_empty() {
            frames_since_event = frames_since_event.saturating_add(1);
        } else {
            frames_since_event = 0;
        }
        for event in &events {
            if let egui::Event::MouseWheel { delta, .. } = event {
                zoom += f64::from(delta.y);
            }
        }
        let drive = if frames_since_event <= end_of_scroll_frames {
            ZoomDrive::LIVE
        } else {
            ZoomDrive::AT_REST
        };
        if let Some((ticket, asked_at, due)) = in_flight
            && frame >= due
        {
            assert!(
                cache.renders.retire(&ticket),
                "the fixture retires only what it recorded"
            );
            cache.show(data_at(&ctx, "arrived", asked_at));
            in_flight = None;
        }
        if cache.needs_rerender(TOKEN, zoom, drive, &viewport(), &plan())
            && cache.renders.admits(RenderSlot::WHOLE, 1)
        {
            dispatches += 1;
            let ticket = RenderTicket::whole(TOKEN, covered());
            cache.renders.record(ticket);
            in_flight = Some((ticket, zoom, frame + 1));
        }
    }

    assert_eq!(
        dispatches, k,
        "the scripted zoom asked for {dispatches} rasters where the script's \
         own notch count says exactly {k}: below it a notch's settle never \
         fired and the overlay is soft at a zoom the map rested at; above it \
         something dispatched inside a notch's own motion, which is the \
         re-raster storm's shape",
    );
}

/// A hold that no longer describes what the pane wants does not block the
/// dispatch that will supersede it: nothing waits behind a stale hold.
#[test]
fn a_stale_hold_does_not_block_the_dispatch_that_supersedes_it() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);
    cache.hold(data_at(&ctx, "arriving", ZOOM + 0.2), None);

    // The zoom has moved on — still inside the band, so the settle arm alone
    // decides on every platform — and settled somewhere the held picture was
    // not rasterised for.
    let moved_on = ZOOM + 0.4;
    assert!(!cache.needs_rerender(TOKEN, moved_on, ZoomDrive::LIVE, &viewport(), &plan()));
    rest_until_settled(&mut cache, moved_on);
    assert!(
        cache.needs_rerender(TOKEN, moved_on, ZoomDrive::AT_REST, &viewport(), &plan()),
        "a stale hold suppressed the dispatch that would supersede it, so the \
         pane settles onto a picture rasterised for a zoom the map has left",
    );
}
