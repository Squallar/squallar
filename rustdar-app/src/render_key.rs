//! The identity of one render: *which picture*, as a value a cache can hash and
//! compare exactly.
//!
//! # This file is ONE of three concepts and the other two live elsewhere
//!
//! What looks like one idea at this seam is three, and collapsing them is a
//! behaviour change rather than a tidy-up. Two orders tried and stopped
//! correctly before the split was written down; it is written down here so a
//! third does not have to rediscover it.
//!
//! * **Identity — this file.** *Which picture is being made.* Quantized once,
//!   at construction, so two asks that mean one picture are literally one key
//!   and equality is derived `==`/`Hash`. That is the point of doing it here:
//!   a pairwise tolerance is not transitive, so `a≈b` and `b≈c` with `a≉c`
//!   gives one bucket three answers depending on what it is compared against.
//!   A key that is already collapsed cannot do that.
//! * **Selection — `rustdar_egui::pane::RenderTarget`, stored as
//!   `Pane::rendered_for`.** The pane's *live* `f32`, and a render **input**
//!   rather than a name: `frame_sweep` passes `target.elevation` into
//!   `find_closest_elevation`, which rounds candidate sweeps to tenths and
//!   takes the nearest, so a selection quantized *before* it arrives there
//!   changes which sweep is chosen at half-tenth selections (0.55 against
//!   sweeps 0.5 and 0.6). `broadcast_sweep` likewise re-snaps the **sender's
//!   selection** against the **receiver's** scan. **Never replace a selection
//!   with a key.**
//! * **Render inputs — [`RenderParams`].** Carries the **snapped** sweep the
//!   renderer is actually handed.
//!   `the_renderer_is_given_the_snapped_sweep_not_the_selection` pins by name
//!   that this differs from the selection, over a 0.5° selection that snapped
//!   to 1.4°.
//!
//! The keys built here quantize the **selected** angle, not the snapped one.
//! Not a preference: at the moment a key is built there may be no scan to snap
//! against at all — `retarget_renders_keyed` writes `rendered_for` from the
//! selection before the first dispatch, and on the loop path the snap is a
//! per-frame function of `(target, timestamp)` resolved from *that frame's own*
//! scan, so it cannot be a field of a per-target key.
//!
//! [`RenderParams`]: crate::render_dispatch::RenderParams

use rustdar_radar::types::{RadarProduct, RenderView};
use rustdar_source::id::{LayerId, known};

/// Quantize an elevation angle to tenths of a degree for cache key use.
///
/// Coarser than `rustdar_egui::pane::ELEVATION_TOLERANCE`, deliberately: that is
/// a pairwise comparison, this has to be a hashable bucket, and no exact
/// bucketing agrees with a tolerance at the edges. Tenths is finer than any real
/// sweep spacing, so two selections that compare equal never land in different
/// buckets in practice.
///
/// **This is the only quantization in the key, and it happens once.** Every
/// consumer downstream compares buckets; none of them reads an angle back off a
/// key, and none should start — an angle recovered from a bucket is a different
/// number from the one that was selected.
pub(crate) fn elevation_key(elevation: f32) -> i32 {
    (elevation * 10.0).round() as i32
}

/// Whether the radar layer's cached raster would come out different in the other
/// UI theme — [`OverlayHandler::theme_sensitive`]'s answer for [`known::RADAR`].
///
/// `RadarHandler` declares nothing, so it takes the trait's `false`: a radar
/// picture's palette is the *product's*, not the interface's, and no pixel of it
/// moves when the OS flips to dark.
///
/// **Read, not assumed.** `the_radar_key_is_the_same_in_dark_and_light` asks the
/// live handler registry for this and fails if it ever stops answering `false`
/// while [`render_cache_key`] keeps leaving the part out.
///
/// [`OverlayHandler::theme_sensitive`]: rustdar_overlays::render::overlay_state::OverlayHandler::theme_sensitive
const RADAR_THEME_SENSITIVE: bool = false;

/// The `view_key` every key carries today — see [`RenderKey::view_key`].
const RESERVED_VIEW_KEY: u32 = 0;

/// The parts of a pane's selection that pick *which* picture, quantized.
///
/// # This is the selection's identity, not the selection
///
/// `elevation_tenths` is a **bucket**, not an angle. The pane's live selection
/// stays where it is, as the `f32` the sweep search consumes — see this module's
/// header for why the two must not be merged. Nothing may read an angle back off
/// this field.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SelectKey {
    /// Which radar. The four-letter identifier, owned because the key outlives
    /// every borrow it could have been built from.
    pub site: String,
    /// Which field.
    pub product: RadarProduct,
    /// The selected elevation in tenths of a degree, **present iff the view and
    /// product make the tilt select the picture** —
    /// [`RenderView::elevation_selects_picture`], asked rather than restated,
    /// because the loop path keys its frames on the same question and the two
    /// answering it separately is what let a section loop discard every frame
    /// for a tilt no section can see.
    ///
    /// # Why absent rather than a slot value
    ///
    /// A section cuts across every tilt and a voxel grid resamples all of them,
    /// so the pane's nominal elevation says nothing about the buffer — two
    /// sections of one product at one site are the same render whatever tilt
    /// each pane's selector happens to be parked on, and keying them apart would
    /// store the same picture several times and evict the plan views to do it.
    ///
    /// **A sentinel would have been wrong here.** Any `i32` chosen for "no
    /// elevation" collides with a real tenths bucket — `0` is a genuine 0.0°
    /// plan render. The predecessor of this field was an `i32` with a
    /// `NO_ELEVATION_SLOT` constant, and it was safe only because [`RenderKey`]
    /// also carries the view, so the slot was only ever compared against other
    /// entries of the same view. `None` needs no such argument: there is no
    /// value to collide with.
    pub elevation_tenths: Option<i32>,
    /// The UI theme this picture was baked under, **present iff the owning layer
    /// declares that its raster branches on the theme** — the rule is
    /// [`SelectKey::theme_part`], and the declaration is the layer's own
    /// [`OverlayHandler::theme_sensitive`].
    ///
    /// Absent for every key this cache holds today, because radar is the only
    /// layer in it and radar declares `false`. That absence is load-bearing, not
    /// incidental: entries here are 32 MiB apiece at the base side and 128 MiB
    /// at the long-range one, so a theme term that radar carried "for
    /// consistency" would re-decode and re-render every visible product on an OS
    /// theme flip, for a change that cannot alter one of their pixels.
    ///
    /// [`OverlayHandler::theme_sensitive`]: rustdar_overlays::render::overlay_state::OverlayHandler::theme_sensitive
    pub theme: Option<bool>,
}

impl SelectKey {
    /// The theme part of a key, computed from the owning layer's **own**
    /// declaration.
    ///
    /// Present iff the layer declares itself theme-sensitive; `is_dark` is
    /// reachable only *through* a `true` declaration, and that is the whole
    /// design. A layer whose raster does not branch on the theme cannot acquire
    /// a theme term by accident, however the caller threads its theme reading
    /// around — which is what keeps the radar cache out of a theme flip's way
    /// structurally rather than by discipline.
    ///
    /// The one formula, in one place, so the layers that fill this part when
    /// their rasters join this cache cannot mint a second rule.
    /// [`render_cache_key`] routes through it with radar's declaration and a
    /// `false` it is required to discard — see the note at that call.
    pub fn theme_part(theme_sensitive: bool, is_dark: bool) -> Option<bool> {
        theme_sensitive.then_some(is_dark)
    }
}

/// The identity of one render, and the key its output is cached under.
///
/// # Why the view is in the key
///
/// The cache is shared between panes, and what it shares is a *buffer*. A plan
/// view of reflectivity and a cross-section of reflectivity at the same site are
/// the same `(site, product, elevation)` and completely different shapes — a
/// square of ground against `SECTION_WIDTH × SECTION_HEIGHT` of a vertical
/// plane. Without this axis they collide in the LRU and one pane is handed the
/// other's buffers, which is not a wrong picture: it is a texture of the wrong
/// shape stretched over a pane's geography, with the hover reading a vertical
/// plane's values through a plan view's bounds.
///
/// **The axis being a field is what makes that class unrepresentable.** While
/// the key was a tuple the axis was a positional `i32`-and-friends convention
/// that a second construction site could get half right; the collision was
/// prevented by there being one such site. Now the two pictures cannot be named
/// by one value at all.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RenderKey {
    /// Which layer's render this is. [`known::RADAR`] for everything this cache
    /// holds today; the field exists because the cache is the *shared*
    /// render-output identity and the overlay rasters are the ones that join it.
    pub kind: LayerId,
    /// What was selected, quantized.
    pub select: SelectKey,
    /// Which kind of picture — see this type's doc.
    pub view: RenderView,
    /// **Reserved**, and `0` on every key today. The view-*scoped*
    /// discriminator: the slot for a future view that can show two different
    /// pictures of one selection (a section along two different lines, say). No
    /// view does, so this field discriminates nothing. It is here rather than
    /// added later because adding an axis to a key is the change that silently
    /// re-partitions a cache, and doing it while the answer is a constant is the
    /// cheap moment.
    pub view_key: u32,
}

/// The cache key for one radar render, and the only place one is built.
///
/// Written once rather than at each call site because the rules above — which
/// axis discriminates, when the tilt is part of the identity at all, and which
/// layer's theme declaration applies — are the kind that a second copy gets half
/// right.
pub(crate) fn render_cache_key(
    site: &str,
    product: RadarProduct,
    view: RenderView,
    elevation: f32,
) -> RenderKey {
    RenderKey {
        kind: known::RADAR,
        select: SelectKey {
            site: site.to_string(),
            product,
            elevation_tenths: view
                .elevation_selects_picture(product)
                .then(|| elevation_key(elevation)),
            // Absent, and no theme reading is threaded in to make it so.
            // `SelectKey::theme_part(RADAR_THEME_SENSITIVE, _)` is `None` for
            // both readings, so a theme argument here would be a value this
            // function is required to discard — and a theme reading in reach of
            // the radar key is precisely the mistake
            // `a_theme_flip_never_touches_the_radar_render_cache` exists to
            // catch. The declaration is read by
            // `the_radar_key_is_the_same_in_dark_and_light`, not trusted to this
            // comment.
            theme: SelectKey::theme_part(RADAR_THEME_SENSITIVE, false),
        },
        view,
        view_key: RESERVED_VIEW_KEY,
    }
}

#[cfg(test)]
mod tests;
