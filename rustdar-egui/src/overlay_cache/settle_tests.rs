//! When a re-render may be dispatched: the settle is a duration, the
//! mid-gesture band is a platform policy, and the picture judged is the newest
//! one the cache has — held or shown.

use super::*;

/// The content token every question below is asked with, and the one every
/// fixture texture carries — so a `true` answer is never a token mismatch.
const TOKEN: u64 = 4242;

/// The zoom the fixture textures are rasterised at.
const ZOOM: f64 = 7.0;

/// A wall-clock origin far enough from zero that no elapsed-time arithmetic
/// below accidentally compares against the field's initial value.
const T0: f64 = 100.0;

/// Fixture texture dimensions — only their *consistency* with [`plan`] matters.
const W: u32 = 8;
const H: u32 = 5;

/// The delay the settle is defined by, in the unit the clock parameter uses.
fn settle_delay() -> f64 {
    SETTLE_REPAINT_DELAY.as_secs_f64()
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
        placed: rustdar_geo::PlacedRaster::of(covered()),
        data_generation: TOKEN,
        render_zoom: current_quantized_zoom(render_zoom),
        width: W,
        height: H,
        radar_meta: None,
        hit_map: None,
    }
}

/// A cache showing a texture rasterised at [`ZOOM`], asked once at [`T0`] so
/// its zoom clock is running.
fn satisfied_cache(ctx: &egui::Context) -> OverlayTextureCache {
    let mut cache = OverlayTextureCache::new();
    cache.show(data_at(ctx, "satisfied", ZOOM));
    assert!(
        !cache.needs_rerender(TOKEN, ZOOM, T0, &viewport(), &plan()),
        "fixture: the parked texture must satisfy the gate at its own zoom, or \
         every `true` below is the fixture's and not the arm under test's",
    );
    cache
}

/// **The misfire the phone reproduced.** Bit-identical zoom on two frames
/// running, inside the settle window, is a coalesced input stream — not
/// fingers at rest — and dispatches nothing. The same stillness sustained for
/// [`SETTLE_REPAINT_DELAY`] is fingers at rest, and dispatches once.
#[test]
fn bit_identical_zoom_across_nearby_frames_is_not_a_settle() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);

    // The gesture moves: 0.2 zoom units, inside the band, quantised well away
    // from the texture's own — so the settle arm is the only arm that could
    // ever answer `true` at this zoom.
    let paused_at = ZOOM + 0.2;
    assert!(
        !cache.needs_rerender(TOKEN, paused_at, T0 + 0.008, &viewport(), &plan()),
        "a zoom inside the band dispatched while moving",
    );

    // The digitizer under-samples: three more frames, 8 ms apart, all reading
    // exactly the same zoom. Under the two-frame equality every one of these
    // after the first was a settle misfire.
    for frame in 1..=3 {
        let now = T0 + 0.008 + 0.008 * frame as f64;
        assert!(
            !cache.needs_rerender(TOKEN, paused_at, now, &viewport(), &plan()),
            "frame {frame} of a coalesced gesture read bit-identical zoom and \
             was called a settle: a full-size raster dispatched mid-gesture",
        );
    }

    // The fingers actually stop. One settle delay later, the exact render is
    // asked for — which is what proves the stanza above was withholding the
    // dispatch on time and not on some other arm.
    assert!(
        cache.needs_rerender(
            TOKEN,
            paused_at,
            T0 + 0.032 + settle_delay(),
            &viewport(),
            &plan(),
        ),
        "the zoom has been still for the whole settle delay and nothing asked \
         for the exact texture: the overlay stays soft forever",
    );
}

/// The settle threshold is the constant, not a number near it: still for less
/// than [`SETTLE_REPAINT_DELAY`] is not settled, still for exactly it is.
#[test]
fn the_settle_threshold_is_settle_repaint_delay_itself() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);

    let paused_at = ZOOM + 0.2;
    let paused_since = T0 + 1.0;
    assert!(!cache.needs_rerender(TOKEN, paused_at, paused_since, &viewport(), &plan()));
    assert!(
        !cache.needs_rerender(
            TOKEN,
            paused_at,
            paused_since + settle_delay() * 0.9,
            &viewport(),
            &plan(),
        ),
        "nine tenths of the settle delay counted as settled",
    );
    assert!(
        cache.needs_rerender(
            TOKEN,
            paused_at,
            paused_since + settle_delay(),
            &viewport(),
            &plan(),
        ),
        "the whole settle delay did not count as settled",
    );
}

/// The settle is level-triggered on the clock: a `true` answer nothing acted
/// on is still `true` on the next frame, for as long as the map is still.
#[test]
fn an_unanswered_settle_stays_owed() {
    let ctx = egui::Context::default();
    let mut cache = satisfied_cache(&ctx);

    let paused_at = ZOOM + 0.2;
    let paused_since = T0 + 1.0;
    assert!(!cache.needs_rerender(TOKEN, paused_at, paused_since, &viewport(), &plan()));
    for frame in 1..=3 {
        let now = paused_since + settle_delay() + 0.016 * frame as f64;
        assert!(
            cache.needs_rerender(TOKEN, paused_at, now, &viewport(), &plan()),
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
        native
            .needs_rerender_with_policy(TOKEN, past_band, T0 + 0.008, &viewport(), &plan(), true,),
        "with the mid-gesture band allowed, a crossing past ZOOM_REBUILD_BAND \
         did not dispatch",
    );

    // wasm arm: the same crossing, mid-gesture, dispatches nothing — the
    // policy hold on gesture-time raster work (see
    // `mid_gesture_rerender_allowed`).
    let mut wasm = satisfied_cache(&ctx);
    assert!(
        !wasm
            .needs_rerender_with_policy(TOKEN, past_band, T0 + 0.008, &viewport(), &plan(), false,),
        "with the mid-gesture band disallowed, a band crossing still \
         dispatched mid-gesture: a full-size raster asked for in the middle \
         of the user's zoom, on the arm that holds that work back",
    );

    // ...and the settle recovers it: the same cache, still past the band,
    // fingers at rest. This is what bounds the wasm arm's softness in time.
    assert!(
        wasm.needs_rerender_with_policy(
            TOKEN,
            past_band,
            T0 + 0.008 + settle_delay(),
            &viewport(),
            &plan(),
            false,
        ),
        "with the mid-gesture band disallowed the settle no longer fires \
         either, so a wasm gesture past the band would stay soft forever",
    );
}

/// The host build allows the mid-gesture band. The wasm side of this `cfg!` is
/// deliberately not claimed by any test: it is covered by the
/// `wasm32-unknown-unknown` type-check gate and by review of the one-line body.
/// What that arm *is* — a policy hold rather than a physical necessity — is on
/// `mid_gesture_rerender_allowed` itself.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_host_arm_of_the_band_policy_allows_mid_gesture_rerenders() {
    assert!(mid_gesture_rerender_allowed());
}

/// The wasm arm of the policy cannot *execute* on the host — on this target
/// `!cfg!(target_arch = "wasm32")` and a bare `true` are the same value — so
/// its text is pinned instead, the way this workspace pins other facts no type
/// can carry. This is a source assertion, stated as one: it proves the body
/// still keys on the wasm target, and nothing about what a wasm build does
/// with it. That behavior's coverage is the `wasm32-unknown-unknown`
/// type-check gate plus review of the one-line body.
#[test]
fn the_band_policy_keys_on_the_wasm_target() {
    let source = include_str!("../overlay_cache.rs");
    let body = source
        .split_once("fn mid_gesture_rerender_allowed() -> bool {")
        .expect("the band policy function is gone from overlay_cache.rs")
        .1;
    let body = &body[..body.find('}').expect("the function body ends")];
    assert!(
        body.contains(r#"!cfg!(target_arch = "wasm32")"#),
        "the mid-gesture band policy no longer keys on the wasm target. That \
         arm is a deliberate hold on gesture-time raster work, not a \
         consequence of where the raster runs, and the host tests cannot \
         catch its loss",
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
    let after_settle = T0 + 10.0 + settle_delay();

    // Control: with only the on-screen picture, the settled zoom re-dispatches.
    let mut without_hold = satisfied_cache(&ctx);
    assert!(
        !without_hold.needs_rerender(TOKEN, settled_at, T0 + 10.0, &viewport(), &plan()),
        "fixture: the first frame at the new zoom must not dispatch on some \
         other arm, or the control below is not about the settle",
    );
    assert!(
        without_hold.needs_rerender(TOKEN, settled_at, after_settle, &viewport(), &plan()),
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
    assert!(!with_hold.needs_rerender(TOKEN, settled_at, T0 + 10.0, &viewport(), &plan()));
    with_hold.hold(data_at(&ctx, "arriving", settled_at), None);

    assert!(
        !with_hold.needs_rerender(TOKEN, settled_at, after_settle, &viewport(), &plan()),
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
    assert!(!cache.needs_rerender(TOKEN, moved_on, T0 + 20.0, &viewport(), &plan()));
    assert!(
        cache.needs_rerender(
            TOKEN,
            moved_on,
            T0 + 20.0 + settle_delay(),
            &viewport(),
            &plan(),
        ),
        "a stale hold suppressed the dispatch that would supersede it, so the \
         pane settles onto a picture rasterised for a zoom the map has left",
    );
}
