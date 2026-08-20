//! The colour bar's baked ramp, and the tick labels written beside it.

use std::sync::Arc;

/// Texels along a baked ramp.
const RAMP_TEXELS: usize = 1024;

/// A signature for a ramp whose source cannot change while the process runs.
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
        assert_ne!(
            across.first(),
            across.last(),
            "the probe ramp is constant, so reversal proves nothing",
        );

        let mismatch = across
            .iter()
            .zip(down.iter().rev())
            .position(|(a, b)| a != b);
        assert_eq!(
            mismatch, None,
            "the vertical ramp must be the horizontal one read bottom-to-top; \
             first disagreement at texel {mismatch:?}",
        );
        assert_eq!(across[0], egui::Color32::from_rgb(0, 0, 0), "low end first");
        assert_eq!(
            down[RAMP_TEXELS - 1],
            egui::Color32::from_rgb(0, 0, 0),
            "a vertical bar's low end is its last row",
        );
    }

    /// A zero-alpha sample stays transparent rather than becoming black.
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
        let _ = ask("inches", &mut builds);
        assert_eq!(
            builds, 3,
            "the memo is one entry per slot — going back rebuilds, by design",
        );
    }
}
