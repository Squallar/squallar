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
        image,
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
