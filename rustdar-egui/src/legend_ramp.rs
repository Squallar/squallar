//! The colour bar's baked ramp, and the tick labels written beside it.
//!
//! # What this file is for
//!
//! A colour scale is a ramp sampled once per pixel of its own length. Drawn
//! directly, that is what it cost every frame: a 1080-point pane's vertical bar
//! is ~1032 pixels tall, so the radar bar issued ~1032 `rect_filled` calls
//! backed by ~1032 binary searches of the product's palette, and each stacked
//! overlay bar another ~1032 backed by a **linear** scan of its threshold list.
//! Per pane. Per frame. For a picture that changes when the user picks a
//! different product — which is to say, essentially never.
//!
//! Baked once, the same bar is one `painter.image` over a texture, and the
//! arithmetic happens on the frame the bar changes instead of on all of them.
//!
//! # It does not know what a colour bar is *of*
//!
//! Nothing in this file names a radar product or an overlay layer. A ramp is a
//! slot id, a signature, and a function from `t ∈ [0, 1]` to RGBA; a label list
//! is a slot id, a version key, and a builder. The callers in `ui_map_pane`
//! know what the bars are of and build the slot ids from it, which is what
//! keeps a memoization file out of the business of radar vocabulary.
//!
//! # This crate had no texture-minting path, and now it has exactly one
//!
//! Every other texture `rustdar-egui` draws was minted by the app crate and
//! handed over — a radar frame, an overlay raster, a basemap tile. This is the
//! first thing the UI crate uploads on its own, so the question of who frees it
//! has to be answered rather than assumed.
//!
//! **The answer is: the `egui::Context` does, because that is where the memo
//! lives.** Entries sit in `Context::data` under [`egui::Id`]s built from the
//! key, so a handle cannot outlive the context that owns the texture — the two
//! are the same allocation's lifetime. That matters because a stale handle is
//! precisely the failure `Gui::clear_graphics_state` exists to prevent for
//! pane-held textures, and it is a failure with no symptom: not a panic, not a
//! blank pane, just an id the fresh renderer has never heard of. Both paths
//! that destroy the graphics state — `App::suspended` and the lost-surface arm
//! of `handle_redraw` — drop `AppState`, which owns the `EguiRenderer`, which
//! owns the `Context`. So the memo is empty on the other side of a suspend by
//! construction, with nothing to remember to clear.
//!
//! The cost of never evicting *within* a context's life is bounded and small:
//! [`RAMP_TEXELS`] × 4 bytes per entry, and the callers' key spaces are both
//! fixed — a product list and an orientation, a layer list and an orientation.
//! 120 KiB if one session displays every radar product both ways round, which
//! no session does.

use std::sync::Arc;

/// Texels along a baked ramp.
///
/// Chosen against the bar it replaces rather than for roundness: the ramp used
/// to be sampled once per pixel of the bar's length, and the longest bar this
/// application draws is the height of a full-screen pane. At 1024 the texture
/// is at least as finely sampled as the old per-pixel loop on any display up to
/// that, so `NEAREST` filtering reproduces what the loop drew to within the
/// half-texel the sampling grids differ by. Above it the bar is softer than the
/// old draw by one texel per pixel-and-a-bit, which is a smooth ramp being
/// interpolated slightly differently and not a visible edge.
///
/// It is also 4 KiB, which is what makes never evicting the memo defensible.
const RAMP_TEXELS: usize = 1024;

/// A signature for a ramp whose source cannot change while the process runs.
///
/// The radar palettes are `const` tables behind a `LazyLock`, so a product's
/// ramp is baked once and never re-checked; the slot id carries the whole key.
/// Named rather than written `0` at the call site so that the claim — *this
/// thing does not change* — is the thing being read.
pub(crate) const IMMUTABLE: u64 = 0;

/// A baked ramp, and the number it was baked for.
#[derive(Clone)]
struct Ramp {
    signature: u64,
    texture: egui::TextureHandle,
}

/// A memoized list of labels, and the version key it was built at.
#[derive(Clone)]
struct Labels<K> {
    version: K,
    labels: Arc<Vec<String>>,
}

/// Look a ramp up, baking it if the slot is empty or holds a different
/// signature.
///
/// `slot` is the whole identity of the ramp — what it is *of* — and
/// `signature` is what would make the same ramp come out differently. A caller
/// whose ramp is a compile-time table passes [`IMMUTABLE`]; one whose ramp is a
/// handler's legend passes that legend's own signature (see
/// `rustdar_overlays::render::overlay_state::Signed`).
///
/// `sample` answers the ramp's RGBA at `t ∈ [0, 1]`, where `t = 0` is the
/// scale's minimum. It is called [`RAMP_TEXELS`] times on a miss and not at all
/// on a hit — the whole point.
///
/// Nothing here knows what a radar product or an overlay layer is, deliberately:
/// this file is about baking a gradient once and keeping it, and the two
/// callers that have such things build the `slot` from them.
pub(crate) fn ramp(
    ctx: &egui::Context,
    slot: egui::Id,
    signature: u64,
    name: &str,
    horizontal: bool,
    sample: impl Fn(f32) -> [u8; 4],
) -> egui::TextureHandle {
    if let Some(memo) = ctx.data(|d| d.get_temp::<Ramp>(slot))
        && memo.signature == signature
    {
        return memo.texture;
    }

    // Outside the `data` borrow, deliberately: `Context::load_texture` takes
    // the context's own lock, and minting inside `data_mut`'s closure would be
    // that lock taken twice by one thread.
    let texture = bake(ctx, name, horizontal, sample);
    ctx.data_mut(|d| {
        d.insert_temp(
            slot,
            Ramp {
                signature,
                texture: texture.clone(),
            },
        );
    });
    texture
}

/// Upload one ramp.
///
/// Horizontal ramps are a `RAMP_TEXELS × 1` row running low value → high value
/// left to right; vertical ramps are a `1 × RAMP_TEXELS` column running high →
/// low **top to bottom**, because a vertical bar's minimum is at its bottom and
/// an image's first row is at its top. Two textures rather than one rotated by
/// its UVs: `Painter::image` maps `uv.min` to `rect.min` and `uv.max` to
/// `rect.max` and cannot transpose, so one texture would mean hand-building a
/// mesh — a second way to draw a bar, to save 4 KiB.
///
/// `a == 0` is carried through as a fully transparent texel rather than skipped.
/// The draw it replaces skipped those samples and let the map show through, and
/// a transparent texel is what that looks like once the strips are a texture.
fn bake(
    ctx: &egui::Context,
    name: &str,
    horizontal: bool,
    sample: impl Fn(f32) -> [u8; 4],
) -> egui::TextureHandle {
    let size = if horizontal {
        [RAMP_TEXELS, 1]
    } else {
        [1, RAMP_TEXELS]
    };
    ctx.load_texture(
        name,
        egui::ColorImage::new(size, ramp_pixels(horizontal, sample)),
        egui::TextureOptions::NEAREST,
    )
}

/// [`bake`]'s texels, in image order. Split out so the orientation flip — the
/// one thing here that can be silently backwards — is assertable without a GPU.
///
/// The vertical ramp is the horizontal one **reversed** rather than sampled at
/// `1 − t`. Those are the same sequence in exact arithmetic and not in `f32`:
/// `1.0 − i/1023.0` and `(1023 − i)/1023.0` differ in the last bit for some `i`,
/// which put a one-step colour difference between the two orientations of the
/// same bar at about three texels in a thousand. Nobody would ever have seen it;
/// it is reversed because a property that can be exact should be.
fn ramp_pixels(horizontal: bool, sample: impl Fn(f32) -> [u8; 4]) -> Vec<egui::Color32> {
    let mut texels: Vec<egui::Color32> = (0..RAMP_TEXELS)
        .map(|i| {
            let t = i as f32 / (RAMP_TEXELS - 1) as f32;
            let [r, g, b, a] = sample(t);
            egui::Color32::from_rgba_unmultiplied(r, g, b, a)
        })
        .collect();
    if !horizontal {
        texels.reverse();
    }
    texels
}

/// A memoized label list, rebuilt when `version` stops matching.
///
/// `build` is the formatting the memo exists to avoid: a `Vec<String>` laid out
/// per threshold, in whichever unit the user asked for. On the radar bar three
/// callers want the same list on the same frame — the painter, the gutter that
/// reserves room for what the painter writes, and the overlay stack behind
/// them — so even within one frame there was more than one build to save.
///
/// `version` is compared rather than folded into `slot` so the memo stays one
/// entry per bar however often it changes: a mismatch overwrites in place. It
/// is a value and not a hash because a hash collision here shows as a bar
/// labelled in the wrong unit, and there is nothing to gain by risking it —
/// callers pass the preferences themselves, or a signature they already have.
///
/// The `Arc` is what makes the hit free: a caller gets a refcount, not a
/// re-allocated list of strings.
pub(crate) fn labels<K>(
    ctx: &egui::Context,
    slot: egui::Id,
    version: K,
    build: impl FnOnce() -> Vec<String>,
) -> Arc<Vec<String>>
where
    K: PartialEq + Clone + Send + Sync + 'static,
{
    if let Some(memo) = ctx.data(|d| d.get_temp::<Labels<K>>(slot))
        && memo.version == version
    {
        return memo.labels;
    }
    let built = Arc::new(build());
    ctx.data_mut(|d| {
        d.insert_temp(
            slot,
            Labels {
                version,
                labels: Arc::clone(&built),
            },
        );
    });
    built
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slot id for a test, distinct per test so two tests cannot share a memo.
    fn slot(what: &str) -> egui::Id {
        egui::Id::new(("legend_ramp::tests", what))
    }

    /// A vertical bar's minimum is at its **bottom**; an image's first row is
    /// at its top. So the vertical ramp is the horizontal one reversed, and
    /// getting that backwards would flip every stacked colour bar in the app
    /// upside down while leaving it looking exactly like a colour bar.
    #[test]
    fn the_vertical_ramp_runs_the_other_way() {
        let sample = |t: f32| [(t * 255.0) as u8, 0, 0, 255];
        let across = ramp_pixels(true, sample);
        let down = ramp_pixels(false, sample);

        assert_eq!(across.len(), RAMP_TEXELS);
        assert_eq!(down.len(), RAMP_TEXELS);
        // Non-vacuity: a constant ramp would satisfy any reversal claim.
        assert_ne!(
            across.first(),
            across.last(),
            "the probe ramp is constant, so reversal proves nothing",
        );

        // Reported as a first mismatch rather than by comparing the vectors:
        // an `assert_eq!` on two 1024-element colour lists prints both.
        let mismatch = across
            .iter()
            .zip(down.iter().rev())
            .position(|(a, b)| a != b);
        assert_eq!(
            mismatch, None,
            "the vertical ramp must be the horizontal one read bottom-to-top; \
             first disagreement at texel {mismatch:?}",
        );
        // And the ends are where the scale's ends are.
        assert_eq!(across[0], egui::Color32::from_rgb(0, 0, 0), "low end first");
        assert_eq!(
            down[RAMP_TEXELS - 1],
            egui::Color32::from_rgb(0, 0, 0),
            "a vertical bar's low end is its last row",
        );
    }

    /// A zero-alpha sample stays transparent rather than becoming black.
    ///
    /// The draw this replaced *skipped* those samples and let the map show
    /// through; a texel painted opaque black would put a bar across the map
    /// wherever a palette declares nothing.
    #[test]
    fn a_transparent_sample_stays_transparent() {
        let pixels = ramp_pixels(true, |t| {
            if t < 0.5 {
                [9, 9, 9, 0]
            } else {
                [9, 9, 9, 255]
            }
        });
        assert_eq!(pixels[0], egui::Color32::TRANSPARENT);
        assert_eq!(pixels[RAMP_TEXELS - 1], egui::Color32::from_rgb(9, 9, 9));
    }

    /// An immutable ramp is baked once per slot and handed back on every later
    /// frame, and two slots are two ramps.
    ///
    /// The memo is the whole point: a second bake would mean the per-frame cost
    /// moved from `rect_filled` to `load_texture`, which is worse.
    #[test]
    fn an_immutable_ramp_is_baked_once_per_slot() {
        let ctx = egui::Context::default();
        let flat = |_: f32| [1u8, 2, 3, 255];

        let a = ramp(&ctx, slot("a"), IMMUTABLE, "a", false, flat);
        assert_eq!(
            a.id(),
            ramp(&ctx, slot("a"), IMMUTABLE, "a", false, flat).id(),
            "the second ask must be the first's texture",
        );
        assert_ne!(
            a.id(),
            ramp(&ctx, slot("b"), IMMUTABLE, "b", false, flat).id(),
            "two slots must not share one ramp",
        );
    }

    /// A signed ramp is re-baked when, and only when, its signature moves.
    #[test]
    fn a_signed_ramp_follows_its_signature() {
        let ctx = egui::Context::default();
        let flat = |_: f32| [1u8, 2, 3, 255];

        let first = ramp(&ctx, slot("signed"), 7, "signed", false, flat);
        assert_eq!(
            first.id(),
            ramp(&ctx, slot("signed"), 7, "signed", false, flat).id(),
            "an unchanged signature must not re-bake",
        );
        assert_ne!(
            first.id(),
            ramp(&ctx, slot("signed"), 8, "signed", false, flat).id(),
            "a moved signature must re-bake",
        );
    }

    /// Labels are built once until the version key moves.
    #[test]
    fn labels_are_built_once_until_the_version_moves() {
        let ctx = egui::Context::default();
        let mut builds = 0;
        let ask = |version: &str, builds: &mut i32| {
            labels(&ctx, slot("labels"), version.to_owned(), || {
                *builds += 1;
                vec!["one".to_owned(), "two".to_owned()]
            })
        };

        let first = ask("inches", &mut builds);
        let again = ask("inches", &mut builds);
        assert_eq!(builds, 1, "the second ask must be answered from the memo");
        assert!(
            Arc::ptr_eq(&first, &again),
            "the memo hit must hand back the same allocation, not an equal one",
        );

        let _ = ask("millimetres", &mut builds);
        assert_eq!(builds, 2, "a version change must rebuild");
        let _ = ask("millimetres", &mut builds);
        assert_eq!(builds, 2, "and then be memoized in its turn");
        // Back again: the slot holds one entry, so this is a miss.
        let _ = ask("inches", &mut builds);
        assert_eq!(
            builds, 3,
            "the memo is one entry per slot — going back rebuilds, by design",
        );
    }
}
