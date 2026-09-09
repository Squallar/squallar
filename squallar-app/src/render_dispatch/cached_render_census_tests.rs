//! The `cached renders` census family, and the `rasters shared` term beside
//! it.
//!
//! `cached renders` was added on 2026-09-07 to name bytes no family had ever
//! named: on the empty steady scene one pane held 216,796,176 B of `Color32`
//! and 5,281,920 B of hover for restore -- 211.8 MiB, 87 % of the whole
//! unaccounted heap -- and `publish_heap_census` did not mention a byte of it.
//! The holder was removed the same day, so the family reads zero, and these
//! tests say so **and say what it would take for that to be a lie**: the panes
//! are shown rasters, through the same door the app shows them, and the figure
//! still reads zero.
//!
//! `rasters shared` keeps its own job. The cache prices its entries one at a
//! time while several keys can hold one `Arc` -- which is what
//! `PlanViewUploads::handle` exists to arrange -- so that term is what turns
//! the family's sum into a range, and it is measured rather than bounded.

use super::*;

/// Small on purpose. The family prices `pixels * 4`, so the side is free to be
/// whatever makes the arithmetic unmissable; the shipped 7362 would cost the
/// test 206 MiB an image to say the same thing.
const SIDE: usize = 64;

/// What one of these rasters costs as `Color32`.
const IMAGE_BYTES: u64 = (SIDE * SIDE * std::mem::size_of::<egui::Color32>()) as u64;

fn raster() -> Arc<egui::ColorImage> {
    Arc::new(egui::ColorImage::filled(
        [SIDE, SIDE],
        egui::Color32::TRANSPARENT,
    ))
}

fn hover() -> Arc<squallar_radar::hover::HoverSource> {
    Arc::new(squallar_radar::hover::HoverSource::empty())
}

/// A dispatcher whose panes have each been shown the raster given, through the
/// door `apply_render_to_pane` shows one through.
fn showing(panes: Vec<Option<Arc<egui::ColorImage>>>) -> RenderDispatcher {
    let mut dispatcher = RenderDispatcher::new();
    dispatcher.ensure_pane_count(panes.len());
    for (idx, image) in panes.into_iter().enumerate() {
        if let Some(image) = image {
            dispatcher.pane_render[idx].note_uploaded(&image);
        }
    }
    dispatcher
}

/// A dispatcher whose panes have been shown nothing prices nothing -- the
/// state the family reads in before the first render lands.
#[test]
fn panes_shown_nothing_price_nothing() {
    assert_eq!(showing(vec![None, None]).cached_render_bytes(), 0);
}

/// **A pane that has been shown a raster still prices nothing**, which is the
/// whole of what this family now reports and the whole of what the removal
/// bought.
///
/// The pane keeps the buffer's identity and not the buffer, so there is
/// nothing here to price. `a_pane_does_not_keep_the_pixels_it_was_shown` is
/// the half that can fail -- it drops every other holder and requires the
/// allocation to be gone. This one records what the census line then says.
#[test]
fn a_pane_shown_a_raster_prices_nothing() {
    let image = raster();
    let dispatcher = showing(vec![Some(Arc::clone(&image))]);
    assert!(
        dispatcher.pane_render[0].shows_buffer(&image),
        "control: the pane must actually have been shown this raster, or the \
         zero below is a zero about nothing",
    );
    assert_eq!(
        dispatcher.cached_render_bytes(),
        0,
        "a pane is pricing raster bytes again -- the CPU twin is back",
    );
}

/// And two panes on two distinct rasters price nothing either. Without this,
/// the zero above is satisfied by a walk that happens to collapse everything
/// it is shown onto one entry.
#[test]
fn two_panes_shown_distinct_rasters_price_nothing() {
    let first = raster();
    let second = raster();
    let dispatcher = showing(vec![Some(Arc::clone(&first)), Some(Arc::clone(&second))]);
    assert!(
        dispatcher.pane_render[0].shows_buffer(&first)
            && dispatcher.pane_render[1].shows_buffer(&second),
        "control: each pane must be showing its own raster",
    );
    assert_eq!(dispatcher.cached_render_bytes(), 0);
}

/// **The identity the panes DO keep is exact.** The family reads zero because
/// a pane holds a `Weak`, and this is what that `Weak` is for: a pane must
/// answer for the buffer it was shown and for no other, or
/// `apply_render_to_pane` would skip an upload the GPU needs.
#[test]
fn a_pane_answers_only_for_the_buffer_it_was_shown() {
    let shown = raster();
    let other = raster();
    let dispatcher = showing(vec![Some(Arc::clone(&shown))]);
    assert!(dispatcher.pane_render[0].shows_buffer(&shown));
    assert!(
        !dispatcher.pane_render[0].shows_buffer(&other),
        "a pane claimed a raster it was never shown; an upload would be \
         skipped and the pane would draw the wrong sweep",
    );
}

/// A cache entry built on `image`/`hover`, so a test can put the SAME buffer
/// in the cache and on a pane.
fn cache_entry(
    image: Arc<egui::ColorImage>,
    hover: Arc<squallar_radar::hover::HoverSource>,
) -> CachedRenderOutput {
    CachedRenderOutput {
        surface: crate::channels::StillSurface::Raster(image),
        max_range_km: 230.0,
        hover,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

fn plan_key(elevation: f32) -> RenderKey {
    render_cache_key(
        "KTLX",
        &squallar_radar::fields::known::REFLECTIVITY,
        RenderView::PlanView,
        elevation,
    )
}

/// **The raster a pane is showing is priced once, by the cache**, and the two
/// families do not name it twice.
///
/// This was the ordinary double-count, and the reason `raster shared` was
/// written: a pane held an `Arc` clone of the entry the cache had also filed.
/// The pane holds an identity now, the cache is the only holder, and the
/// correction across the two families is nothing -- so `raster total` and
/// `raster floor` agree.
#[test]
fn a_raster_a_pane_shows_is_priced_once_by_the_cache() {
    let image = raster();
    let hover = hover();
    let one = IMAGE_BYTES + hover.resident_bytes() as u64;

    let mut dispatcher = showing(vec![Some(Arc::clone(&image))]);
    dispatcher
        .render_cache
        .insert(plan_key(0.5), cache_entry(image, hover));

    assert_eq!(
        dispatcher.render_cache.resident_bytes() as u64 + dispatcher.cached_render_bytes(),
        one,
        "the buffer is priced twice across the two families again",
    );
    assert_eq!(
        dispatcher.raster_shared_bytes(),
        0,
        "the two families share bytes again -- a second holder is back",
    );
}

/// **The case where the correction must vanish**, kept as the control it
/// always was: a cache entry no pane is showing shares nothing, and
/// `raster shared` must read zero. Without it, a term that always answered
/// "one raster" would pass the test above and quietly subtract a real buffer
/// out of the floor.
#[test]
fn a_cache_entry_no_pane_is_showing_shares_nothing() {
    let mut dispatcher = showing(vec![Some(raster())]);
    dispatcher
        .render_cache
        .insert(plan_key(0.5), cache_entry(raster(), hover()));
    assert_eq!(
        dispatcher.raster_shared_bytes(),
        0,
        "unshared buffers were reported as shared; the floor would eat them",
    );
}

/// Two cache keys naming ONE `Arc` — what `PlanViewUploads::handle` arranges —
/// is double-counted by `render cache` alone, and `raster shared` catches it
/// with no pane involved at all. This is the 211.8 MiB the empty scene held.
#[test]
fn two_cache_keys_on_one_raster_are_shared_within_the_cache() {
    let image = raster();
    let hover = hover();
    let one = IMAGE_BYTES + hover.resident_bytes() as u64;

    let mut dispatcher = showing(vec![None]);
    dispatcher.render_cache.insert(
        plan_key(0.5),
        cache_entry(Arc::clone(&image), Arc::clone(&hover)),
    );
    dispatcher
        .render_cache
        .insert(plan_key(0.9), cache_entry(image, hover));

    assert_eq!(dispatcher.render_cache.resident_bytes() as u64, 2 * one);
    assert_eq!(
        dispatcher.raster_shared_bytes(),
        one,
        "the cache's own double-count is not in the shared term",
    );
}

// ── The polar arm: a picture the pane really is holding ─────────────────────

/// One fan payload, small enough that its price is unmissable and well formed
/// enough that a pane would actually draw it.
fn fan() -> Arc<squallar_egui::radar_fan::FanSweep> {
    let sweep = Arc::new(squallar_egui::radar_fan::FanSweep {
        field: squallar_radar::fields::known::REFLECTIVITY,
        radials: 4,
        gates: 8,
        codes: vec![0; 4 * 8],
        level_offsets: vec![0],
        lut_rgba: vec![0; squallar_egui::radar_fan::LUT_BYTES],
        edges: vec![[0.0, 90.0], [90.0, 180.0], [180.0, 270.0], [270.0, 360.0]],
        geometry: squallar_egui::radar_fan::FanGeometry {
            site_lat: 35.33,
            site_lon: -97.27,
            first_gate_slant_km: 2.125,
            gate_interval_slant_km: 0.25,
            elevation_deg: Some(0.5),
            reach_gates: 8,
            reach_km: 2.5,
            first_gate_km: 2.0,
            earth_radius_km: squallar_geo::EARTH_RADIUS_KM,
            effective_radius_km: squallar_radar::beam::RE_EFF_KM,
        },
    });
    assert!(sweep.is_well_formed(), "the fixture describes itself");
    sweep
}

/// A cache entry over a fan payload, so a test can put the SAME payload in the
/// cache and on a pane.
fn fan_entry(
    sweep: Arc<squallar_egui::radar_fan::FanSweep>,
    hover: Arc<squallar_radar::hover::HoverSource>,
) -> CachedRenderOutput {
    CachedRenderOutput {
        surface: crate::channels::StillSurface::Fan(sweep),
        max_range_km: 230.0,
        hover,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

/// **A pane showing a fan prices its payload, and the figure is the payload's
/// own.**
///
/// The raster arm reads zero because a texture's pixels are the GPU's; a fan's
/// are not. The pane holds the buffer for as long as the picture is on the
/// glass, and the render cache's copy of the same `Arc` is evicted on its own
/// schedule — so a family that could only ever read zero would lose these
/// bytes the moment the cache did. That is the exact shape of the defect this
/// family was created by.
///
/// **Measured off the payload, never off a constant**: a plane is sized by the
/// radials and gates the radar chose.
///
/// TAMPER: drop the `note_fan` call in `apply_render_to_pane`'s polar arm, or
/// make `showing_bytes` answer zero, and this goes red.
#[test]
fn a_pane_showing_a_fan_prices_its_payload() {
    let sweep = fan();
    let mut dispatcher = RenderDispatcher::new();
    dispatcher.ensure_pane_count(1);
    dispatcher.pane_render[0].note_fan(&sweep);
    assert!(
        sweep.resident_bytes() > 0,
        "control: the fixture holds something, or the equality below is two zeros",
    );
    assert_eq!(
        dispatcher.cached_render_bytes(),
        sweep.resident_bytes() as u64,
    );
}

/// **A pane holds one picture, and the two claims are exclusive.**
///
/// A pane shown a raster after a fan must stop pricing the fan, and one shown
/// a fan after a raster must stop answering for the buffer. Left set, either
/// claim prices or admits a picture this pane let go of — the second would
/// skip an upload the GPU needs.
#[test]
fn a_pane_answers_for_one_surface_at_a_time() {
    let sweep = fan();
    let image = raster();
    let mut dispatcher = RenderDispatcher::new();
    dispatcher.ensure_pane_count(1);

    dispatcher.pane_render[0].note_fan(&sweep);
    assert!(!dispatcher.pane_render[0].shows_buffer(&image));

    dispatcher.pane_render[0].note_uploaded(&image);
    assert_eq!(
        dispatcher.cached_render_bytes(),
        0,
        "a pane showing a raster still prices the fan it replaced",
    );
    assert!(dispatcher.pane_render[0].shows_buffer(&image));

    dispatcher.pane_render[0].note_fan(&sweep);
    assert!(
        !dispatcher.pane_render[0].shows_buffer(&image),
        "a pane drawing a fan claimed a texture it no longer shows",
    );
}

/// **A payload the cache and a pane both hold is priced twice across the two
/// families, and `rasters shared` names the whole of the difference.**
///
/// This is the correction the raster arm no longer needs and the polar arm
/// does: the pane really is a second holder here, so the term that measures
/// the overlap has to see it.
///
/// TAMPER: drop the fan term from `cached_render_bytes` and both assertions
/// go red — the family stops naming the payload, so there is no overlap left
/// to correct. Dropping the pane walk from `raster_union_bytes` does **not**
/// redden this one, and that is why the control below exists: with the cache
/// holding the payload the union finds it either way, and only a pane holding
/// it alone separates the two.
#[test]
fn a_fan_the_cache_and_a_pane_both_hold_is_shared_between_them() {
    let sweep = fan();
    let hover = hover();
    let one = sweep.resident_bytes() as u64 + hover.resident_bytes() as u64;

    let mut dispatcher = RenderDispatcher::new();
    dispatcher.ensure_pane_count(1);
    dispatcher.pane_render[0].note_fan(&sweep);
    dispatcher
        .render_cache
        .insert(plan_key(0.5), fan_entry(Arc::clone(&sweep), hover));

    assert_eq!(
        dispatcher.render_cache.resident_bytes() as u64 + dispatcher.cached_render_bytes(),
        one + sweep.resident_bytes() as u64,
        "the two families must both name the payload, or there is no overlap to correct",
    );
    assert_eq!(
        dispatcher.raster_shared_bytes(),
        sweep.resident_bytes() as u64,
        "the payload both holders name is not reported as shared",
    );
}

/// And the control the test above needs: a fan **only** a pane holds is not
/// shared with anything, so the correction is zero and the floor keeps it.
///
/// **This is the arm the pane leg of `raster_union_bytes` exists for.** With
/// the cache also holding the payload the union finds it through the cache; a
/// pane holding it alone is the only state in which the walk is the sole
/// witness.
///
/// TAMPER: drop the pane walk at the end of `raster_union_bytes` and the
/// second assertion goes red — a payload nobody else holds is reported as
/// shared and subtracted straight out of the floor.
#[test]
fn a_fan_only_a_pane_holds_shares_nothing() {
    let sweep = fan();
    let mut dispatcher = RenderDispatcher::new();
    dispatcher.ensure_pane_count(1);
    dispatcher.pane_render[0].note_fan(&sweep);
    assert_eq!(
        dispatcher.cached_render_bytes(),
        sweep.resident_bytes() as u64,
        "control: the pane is the sole holder and prices the payload",
    );
    assert_eq!(
        dispatcher.raster_shared_bytes(),
        0,
        "a payload nobody else holds was subtracted out of the floor",
    );
}

/// **A pane that let its payload go prices nothing**, because the claim is a
/// `Weak` and never keeps one alive.
#[test]
fn a_dropped_payload_leaves_nothing_priced() {
    let mut dispatcher = RenderDispatcher::new();
    dispatcher.ensure_pane_count(1);
    {
        let sweep = fan();
        dispatcher.pane_render[0].note_fan(&sweep);
        assert!(
            dispatcher.cached_render_bytes() > 0,
            "control: it was priced"
        );
    }
    assert_eq!(
        dispatcher.cached_render_bytes(),
        0,
        "the pane is holding a payload every other owner has dropped",
    );
}
