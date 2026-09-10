//! **What a cache may decline to re-ask for: the empty answers it has already
//! been given, and nothing else.**
//!
//! A playing loop walks a fixed, small set of depicted instants over and over,
//! and every one of them mints its own cache token. Before the memo, a layer
//! whose answer at most of those stops was *empty* re-rasterized the same
//! whole-viewport blank once per stop per pass, for ever — the single-slot
//! [`OverlayTextureCache::blank`] can only recognise the one it answered last.
//!
//! Every fixture below is written to fail against **both** tampers of
//! [`OverlayTextureCache::content_answer_is_known_blank`] — `true` (the memo
//! made unconditional) and `false` (the memo deleted) — because a fixture that
//! only catches one of them is satisfied by a constant and is measuring
//! nothing. The split is: one case that must be suppressed, and seven that
//! must not, one per term of the claim the memo makes.

use super::*;

fn ground() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -100.0,
        max_lon: -90.0,
    }
}

/// Well inside [`ground`], so the coverage arm has nothing to say about a
/// picture rendered for it.
fn viewport() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -97.0,
        max_lon: -95.0,
    }
}

fn plan() -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: 1,
        height: 1,
        overdraw: 0.0,
        pixels_per_point: 1.0,
        pane_px: [0, 0],
    }
}

/// The shape a blank for `token` was rendered at, sized and zoomed to match
/// [`plan`] so no arm below the content one can fire and answer for it.
fn blank_shape(token: u64) -> PictureShape {
    PictureShape {
        placed: PlacedRaster::of(ground()),
        data_generation: token,
        render_zoom: 0,
        width: 1,
        height: 1,
    }
}

/// The pane's whole question at a still zoom the answers were rendered at,
/// with no gesture driving it.
fn asks(cache: &mut OverlayTextureCache, token: u64) -> bool {
    cache.needs_rerender(token, 0.0, ZoomDrive::AT_REST, &viewport(), &plan())
}

/// A pane that has been answered "nothing here" at two stops of its loop and
/// is drawing the second of them.
fn walked_two_stops() -> OverlayTextureCache {
    let mut cache = OverlayTextureCache::new();
    cache.show_blank(blank_shape(10));
    cache.show_blank(blank_shape(20));
    cache
}

/// **The cut.** The clock comes back round to a stop this cache has already
/// been told is empty, and the raster is not spent again.
#[test]
fn a_stop_already_answered_empty_is_not_rasterized_again() {
    let mut cache = walked_two_stops();
    assert!(
        !asks(&mut cache, 10),
        "a stop this cache was handed a blank for, at the same size, zoom and \
         ground, must not mint a second whole-viewport raster of the same \
         absence"
    );
}

/// The other half of the same pass: a stop the cache has never been answered
/// for is asked about, whatever it has filed for its neighbours.
#[test]
fn a_stop_never_answered_is_still_asked_for() {
    let mut cache = walked_two_stops();
    assert!(
        asks(&mut cache, 30),
        "a token no filed answer carries is not an answer, and the memo must \
         not stand in for one"
    );
}

/// **A pane drawing ink is never held on the memo.** Suppressing there would
/// leave the ink of one stop on the glass at a stop that draws nothing, which
/// is the one thing a blank exists to prevent.
#[test]
fn a_pane_drawing_ink_is_never_held_on_a_filed_blank() {
    let ctx = egui::Context::default();
    let mut cache = walked_two_stops();
    cache.show(OverlayTextureData {
        crop: None,
        texture: ctx.load_texture(
            "ink",
            egui::ColorImage::filled([1, 1], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        placed: PlacedRaster::of(ground()),
        data_generation: 40,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map: None,
    });
    assert!(
        asks(&mut cache, 10),
        "the pane is drawing lightning for stop 40; stop 10 draws none, and \
         the raster that clears it must go out"
    );
}

/// The memo's ground term. A blank says nothing painted over *its own*
/// ground; a viewport that has left it is a different question.
#[test]
fn a_blank_filed_over_ground_the_viewport_has_left_is_not_reused() {
    let mut cache = OverlayTextureCache::new();
    let mut narrow = blank_shape(10);
    narrow.placed = PlacedRaster::of(GeoBounds {
        min_lat: 30.0,
        max_lat: 31.0,
        min_lon: -100.0,
        max_lon: -99.0,
    });
    cache.show_blank(narrow);
    cache.show_blank(blank_shape(20));
    assert!(
        asks(&mut cache, 10),
        "nothing painted over a patch of Texas is no statement at all about \
         the viewport now on screen"
    );
}

/// The plan-size term: the size reaches the rasterizer and is not in the
/// token.
#[test]
fn a_blank_filed_at_another_plan_size_is_not_reused() {
    let mut cache = OverlayTextureCache::new();
    let mut resized = blank_shape(10);
    resized.width = 2;
    cache.show_blank(resized);
    cache.show_blank(blank_shape(20));
    assert!(
        asks(&mut cache, 10),
        "a blank rendered at another texel count is an answer about another \
         picture"
    );
}

/// The zoom term: item sizes scale with zoom, so a blank at one zoom is no
/// statement about another.
#[test]
fn a_blank_filed_at_another_zoom_is_not_reused() {
    let mut cache = OverlayTextureCache::new();
    let mut zoomed = blank_shape(10);
    zoomed.render_zoom = quantize_zoom(6.0);
    cache.show_blank(zoomed);
    cache.show_blank(blank_shape(20));
    assert!(
        asks(&mut cache, 10),
        "a blank rendered at zoom 6 is no statement about the picture at zoom 0"
    );
}

/// **The latency claim, as a fixture.** A layer's token carries its
/// `data_generation`, so an arrival moves every stop's token at once and no
/// filed answer can be reached again — the pass after new data dispatches
/// exactly as it did before the memo existed.
#[test]
fn an_arrival_moves_every_token_so_no_filed_answer_can_delay_it() {
    let mut cache = OverlayTextureCache::new();
    // A whole cycle of stops, all empty, at one generation.
    for stop in 0..12u64 {
        cache.show_blank(blank_shape(1000 + stop));
    }
    // Data lands. `overlay_cache_token` folds `data_generation` into every
    // one of this layer's tokens, so the same twelve stops are twelve new
    // tokens.
    for stop in 0..12u64 {
        assert!(
            asks(&mut cache, 2000 + stop),
            "stop {stop} after an arrival must be rasterized on the frame it \
             is next asked about"
        );
    }
}

/// The ring's size is the loop's cycle, and the boundary is stated rather
/// than implied: a walk that fits is recognised whole, and the stop past the
/// end is the one that goes.
#[test]
fn the_ring_holds_a_whole_cycle_and_the_oldest_stop_is_what_goes() {
    let mut cache = OverlayTextureCache::new();
    for stop in 0..BLANK_MEMO_SLOTS as u64 {
        cache.show_blank(blank_shape(stop));
    }
    assert!(
        !asks(&mut cache, 0),
        "a cycle of exactly {BLANK_MEMO_SLOTS} stops must be recognised whole"
    );
    // One stop past the ring. The oldest is overwritten and nothing else is.
    cache.show_blank(blank_shape(BLANK_MEMO_SLOTS as u64));
    assert!(
        asks(&mut cache, 0),
        "the oldest stop is what a full ring gives up"
    );
    assert!(
        !asks(&mut cache, 1),
        "and it gives up nothing else: the stop after the oldest is still filed"
    );
}
