//! The Volume Alpha curve: a per-product opacity profile the user draws over
//! the palette, GR2Analyst-style.
//!
//! # What this is
//!
//! The 3D raymarch colours a sample by fetching the grid's 256-entry palette
//! LUT, and the entry's **alpha** is what decides how much of the storm that
//! density hides. GR2Analyst's "Volume Alpha" window lets the user redraw that
//! alpha as a freehand curve over the palette strip — strip the low dBZ haze
//! off a supercell, or thin the mid-range until the hail core shows through.
//! This module is that curve: the value model, the freehand-stroke editing
//! rule, and the per-product store the UI and the config both read.
//!
//! # Where it applies, and where it deliberately does not
//!
//! The curve is applied at exactly one seam: the frontend's LUT upload, where
//! the grid's own table is about to go to the GPU. Colour channels stay the
//! palette's; only the alpha channel is replaced. Nothing upstream changes —
//! not the wire, not the worker, not `rustdar-radar`'s tables — and nothing
//! reaches the GPU-test instruments, which upload `VoxelGrid::lut()` directly
//! and never see a `VolumeFrameState`.
//!
//! **An untouched editor is bit-exact.** "No curve stored" is a real state
//! ([`AlphaCurves::get`] answers `None`), and the frontend uploads the grid's
//! own LUT bytes unmodified in that state. The default curve the editor
//! *shows* is seeded from the palette's own alpha, so drawing over one region
//! and leaving the rest alone keeps the rest at the palette's values — which
//! is the "per region of the value axis" behaviour the feature exists for.
//!
//! # Index 0 is no-data and cannot be made visible
//!
//! Palette entry 0 is the no-data index: `build_voxels` forces it transparent
//! so unmeasured air draws nothing, and the raymarch's skip threshold sits
//! above it. A curve that painted alpha onto entry 0 would resurrect
//! unmeasured air as a visible shell around every storm — a fabricated
//! picture. So [`AlphaCurve::from_alphas`] clamps entry 0 to zero at the only
//! constructor, [`apply_stroke`] re-clamps after every edit, and the frontend
//! forces it a third time at the upload. The editor says so in its UI text
//! rather than silently eating the user's stroke.

use std::collections::HashMap;
use std::sync::Arc;

use rustdar_radar::types::RadarProduct;

/// Palette entries a curve spans — one alpha per LUT index.
pub const CURVE_LEN: usize = 256;

/// A user-drawn alpha curve over the 256-index value axis.
///
/// Cheap to clone and to compare: the alphas live behind an `Arc`, and
/// equality takes the pointer fast path before it compares bytes — the
/// frontend compares last frame's curve against this frame's on every frame,
/// and re-uploads the LUT only when they differ.
///
/// Entry 0 is always 0. See the module doc: index 0 is no-data by design, and
/// the constructor is where that is enforced.
#[derive(Clone, Debug)]
pub struct AlphaCurve(Arc<[u8; CURVE_LEN]>);

impl PartialEq for AlphaCurve {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Eq for AlphaCurve {}

impl AlphaCurve {
    /// The one constructor. Clamps entry 0 to zero, whatever the caller says —
    /// index 0 is the no-data index and a curve must not be able to make
    /// unmeasured air visible.
    pub fn from_alphas(mut alphas: [u8; CURVE_LEN]) -> Self {
        alphas[0] = 0;
        Self(Arc::new(alphas))
    }

    /// The palette's own alpha channel as a curve — what an untouched editor
    /// shows, and what a first stroke starts from. `None` unless `lut` is the
    /// exact 1024 bytes a `VoxelGrid::lut()` hands over.
    pub fn from_palette(lut: &[u8]) -> Option<Self> {
        if lut.len() != CURVE_LEN * 4 {
            return None;
        }
        let mut alphas = [0u8; CURVE_LEN];
        for (slot, entry) in alphas.iter_mut().zip(lut.chunks_exact(4)) {
            *slot = entry[3];
        }
        Some(Self::from_alphas(alphas))
    }

    /// One alpha per palette entry, entry 0 first.
    pub fn alphas(&self) -> &[u8; CURVE_LEN] {
        &self.0
    }

    /// How many indices above the no-data index this curve keeps fully
    /// transparent — [`VoxelGrid::fade_band`]'s rule, over the curve instead
    /// of the palette, spelled identically on purpose: the raymarch's skip
    /// threshold is anchored at `(band + 0.5) / 255`, and the two producers of
    /// `band` must agree about what it counts or the march skips visible data
    /// on one path and pays for invisible shells on the other.
    ///
    /// All-transparent answers `u8::MAX`, which puts the threshold above every
    /// representable index: the march skips everything and the pane is
    /// honestly empty — the picture a fully-zeroed curve asks for.
    ///
    /// [`VoxelGrid::fade_band`]: rustdar_radar::voxel::VoxelGrid::fade_band
    pub fn fade_band(&self) -> u8 {
        match self.0.iter().position(|alpha| *alpha != 0) {
            // Entry 0 is clamped transparent, so the first visible entry is at
            // index 1 or above and the band under it is `n - 1` wide. The
            // `saturating_sub` mirrors the voxel-side spelling; entry 0 being
            // nonzero is unreachable through `from_alphas`.
            Some(n) => n.saturating_sub(1) as u8,
            None => u8::MAX,
        }
    }
}

/// One freehand stroke segment: rewrite the curve between two pointer samples.
///
/// `from` and `to` are `(index, alpha)` in curve units — index `0..=255`
/// (fractional, straight off the pointer's x), alpha `0..=1`. Every integer
/// index the segment crosses gets the segment's linearly interpolated alpha;
/// **every index outside the crossed range is untouched**, which is the whole
/// "redraw one region, keep the rest" contract. Order does not matter: a
/// right-to-left drag is the same stroke.
///
/// The pointer path arrives as one segment per frame, so a whole drag is a
/// chain of these calls — freehand, monotone in x within each segment, later
/// segments overwriting earlier ones where the pointer doubles back.
///
/// Non-finite input writes nothing: the pointer cannot produce it, so
/// anything non-finite here is a bug upstream, and a NaN must not be laundered
/// into a `u8` by `as` saturation.
pub fn apply_stroke(alphas: &mut [u8; CURVE_LEN], from: (f32, f32), to: (f32, f32)) {
    if ![from.0, from.1, to.0, to.1].iter().all(|v| v.is_finite()) {
        return;
    }
    let (left, right) = if from.0 <= to.0 { (from, to) } else { (to, from) };
    let lo = (left.0.round().clamp(0.0, 255.0)) as usize;
    let hi = (right.0.round().clamp(0.0, 255.0)) as usize;
    let span = right.0 - left.0;
    for (i, slot) in alphas.iter_mut().enumerate().take(hi + 1).skip(lo) {
        // The segment's alpha at this index. A zero-width segment (a click,
        // or a vertical pointer move) takes the newest sample's alpha rather
        // than dividing by zero.
        let alpha = if span <= f32::EPSILON {
            to.1
        } else {
            let t = ((i as f32 - left.0) / span).clamp(0.0, 1.0);
            left.1 + (right.1 - left.1) * t
        };
        *slot = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    // The no-data clamp, re-asserted after every edit — the same rule as
    // `AlphaCurve::from_alphas`, enforced here too so a stroke dragged into
    // the left edge cannot make index 0 visible even transiently.
    alphas[0] = 0;
}

/// Every product's user-drawn curve — the session state the editor edits, the
/// frame reads, and the config persists.
///
/// Keyed by product from day one: only reflectivity renders in 3D today, but
/// the products WP is next and a curve drawn for one moment must never apply
/// to another. Absence is the meaningful default — a product with no entry
/// renders through its palette's own alpha, bit-exactly.
#[derive(Default)]
pub struct AlphaCurves {
    curves: HashMap<RadarProduct, AlphaCurve>,
}

impl AlphaCurves {
    /// The curve for `product`, or `None` for an untouched editor.
    pub fn get(&self, product: RadarProduct) -> Option<AlphaCurve> {
        self.curves.get(&product).cloned()
    }

    /// Store `product`'s curve. Live during a drag: the editor writes every
    /// frame of the stroke, and the frontend re-uploads the 1 KiB LUT only
    /// when the bytes actually changed.
    pub fn set(&mut self, product: RadarProduct, curve: AlphaCurve) {
        self.curves.insert(product, curve);
    }

    /// Forget `product`'s curve — the reset, back to the palette's own alpha.
    pub fn reset(&mut self, product: RadarProduct) {
        self.curves.remove(&product);
    }

    /// Whether `product` has a user curve at all.
    pub fn is_edited(&self, product: RadarProduct) -> bool {
        self.curves.contains_key(&product)
    }

    /// Every edited product and its curve, in an arbitrary order — the save
    /// path sorts by product code so the config file is deterministic.
    pub fn entries(&self) -> impl Iterator<Item = (RadarProduct, &AlphaCurve)> {
        self.curves.iter().map(|(product, curve)| (*product, curve))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A palette shaped like reflectivity's: entry 0 transparent, a 64-index
    /// fade band, then a ramp of visible entries.
    fn reflectivity_shaped_lut() -> Vec<u8> {
        let mut lut = Vec::with_capacity(CURVE_LEN * 4);
        for i in 0..CURVE_LEN {
            let alpha = if i <= 64 { 0 } else { 200 };
            lut.extend_from_slice(&[10, 20, 30, alpha]);
        }
        lut
    }

    /// The seeded default is the palette's own alpha, entry for entry — which
    /// is what makes "open the editor and touch nothing" a no-op by
    /// construction rather than by luck.
    #[test]
    fn the_default_curve_is_the_palettes_own_alpha() {
        let lut = reflectivity_shaped_lut();
        let curve = AlphaCurve::from_palette(&lut).expect("a 1024-byte palette seeds");
        for (i, (alpha, entry)) in curve.alphas().iter().zip(lut.chunks_exact(4)).enumerate() {
            assert_eq!(
                *alpha, entry[3],
                "entry {i}: the seeded curve must be the palette's alpha",
            );
        }
        assert!(
            AlphaCurve::from_palette(&lut[..1020]).is_none(),
            "a short palette must not seed a curve",
        );
    }

    /// Index 0 is no-data and stays transparent through every door: the
    /// constructor, the palette seed, and a stroke dragged into the left edge.
    #[test]
    fn the_no_data_index_cannot_be_made_visible() {
        assert_eq!(
            AlphaCurve::from_alphas([255; CURVE_LEN]).alphas()[0],
            0,
            "the constructor must clamp entry 0",
        );

        let mut hostile = reflectivity_shaped_lut();
        hostile[3] = 255; // a palette claiming a visible no-data entry
        assert_eq!(
            AlphaCurve::from_palette(&hostile)
                .expect("well-sized palette")
                .alphas()[0],
            0,
            "the palette seed must clamp entry 0 even against a hostile table",
        );

        let mut alphas = [0u8; CURVE_LEN];
        apply_stroke(&mut alphas, (-3.0, 1.0), (5.0, 1.0));
        assert_eq!(alphas[0], 0, "a stroke over the left edge must not paint entry 0");
        assert!(
            alphas[1..=5].iter().all(|a| *a == 255),
            "the rest of the stroke must land: {:?}",
            &alphas[..8],
        );
    }

    /// **The per-region contract.** A stroke rewrites exactly the indices the
    /// pointer crossed, interpolates linearly between its endpoints, and
    /// leaves every other index bit-identical. The mutation this exists to
    /// kill is any "apply to the whole curve" rewrite — a constant alpha, a
    /// global scale — which fails the untouched-outside half instantly.
    #[test]
    fn a_stroke_rewrites_only_the_indices_it_crossed() {
        let mut alphas = [17u8; CURVE_LEN];
        alphas[0] = 0;
        let before = alphas;
        apply_stroke(&mut alphas, (100.0, 1.0), (110.0, 0.0));

        assert_eq!(&alphas[..100], &before[..100], "left of the stroke is untouched");
        assert_eq!(&alphas[111..], &before[111..], "right of the stroke is untouched");
        assert_eq!(alphas[100], 255, "the stroke's start takes its alpha");
        assert_eq!(alphas[110], 0, "the stroke's end takes its alpha");
        assert_eq!(alphas[105], 128, "the midpoint interpolates");
        for window in alphas[100..=110].windows(2) {
            assert!(
                window[0] >= window[1],
                "a falling stroke must fall monotonically: {:?}",
                &alphas[100..=110],
            );
        }
    }

    /// A right-to-left drag is the same stroke as its mirror — the user's
    /// hand direction is not part of the curve.
    #[test]
    fn a_stroke_drawn_right_to_left_is_the_same_stroke() {
        let mut forward = [80u8; CURVE_LEN];
        let mut backward = [80u8; CURVE_LEN];
        apply_stroke(&mut forward, (40.2, 0.9), (61.7, 0.1));
        apply_stroke(&mut backward, (61.7, 0.1), (40.2, 0.9));
        assert_eq!(forward, backward);
    }

    /// A click (zero-width segment) writes one index; a non-finite sample
    /// writes nothing at all.
    #[test]
    fn degenerate_strokes_are_a_point_or_nothing() {
        let mut alphas = [10u8; CURVE_LEN];
        apply_stroke(&mut alphas, (50.0, 0.5), (50.0, 0.5));
        assert_eq!(alphas[50], 128);
        assert_eq!(alphas[49], 10);
        assert_eq!(alphas[51], 10);

        let before = alphas;
        apply_stroke(&mut alphas, (f32::NAN, 0.5), (60.0, 0.5));
        apply_stroke(&mut alphas, (60.0, f32::INFINITY), (70.0, 0.5));
        assert_eq!(alphas, before, "a non-finite sample must be refused, not laundered");
    }

    /// The curve's `fade_band` follows `VoxelGrid::fade_band`'s exact rule:
    /// the first nonzero-alpha entry at index `n` means a band of `n - 1`, and
    /// an all-transparent curve answers `u8::MAX`. The skip threshold
    /// `(band + 0.5) / 255` then sits strictly between the last transparent
    /// entry and the first visible one — the "never skip visible data, never
    /// pay for invisible shells" anchor — for every curve, not just palettes.
    #[test]
    fn the_curves_fade_band_matches_the_palettes_rule() {
        let mut alphas = [0u8; CURVE_LEN];
        alphas[65] = 1;
        assert_eq!(AlphaCurve::from_alphas(alphas).fade_band(), 64);

        let mut low = [0u8; CURVE_LEN];
        low[1] = 200;
        assert_eq!(
            AlphaCurve::from_alphas(low).fade_band(),
            0,
            "alpha at index 1 means no transparent band at all",
        );

        assert_eq!(
            AlphaCurve::from_alphas([0; CURVE_LEN]).fade_band(),
            u8::MAX,
            "an all-transparent curve reports the band that skips everything",
        );

        // The anchor property, over user curves: the threshold separates the
        // last transparent entry from the first visible one.
        for first_visible in [1usize, 2, 65, 100, 255] {
            let mut alphas = [0u8; CURVE_LEN];
            alphas[first_visible] = 128;
            let band = AlphaCurve::from_alphas(alphas).fade_band();
            let threshold = (f32::from(band) + 0.5) / 255.0;
            assert!(
                (first_visible - 1) as f32 / 255.0 <= threshold,
                "first visible at {first_visible}: the last transparent entry \
                 must not clear the threshold",
            );
            assert!(
                first_visible as f32 / 255.0 > threshold,
                "first visible at {first_visible}: the first visible entry \
                 must clear the threshold",
            );
        }
    }

    /// The store is per-product: a curve set for one product neither answers
    /// nor resets another's.
    #[test]
    fn curves_are_stored_per_product() {
        let mut curves = AlphaCurves::default();
        let mut alphas = [0u8; CURVE_LEN];
        alphas[200] = 99;
        let curve = AlphaCurve::from_alphas(alphas);

        curves.set(RadarProduct::Reflectivity, curve.clone());
        assert_eq!(curves.get(RadarProduct::Reflectivity), Some(curve));
        assert_eq!(
            curves.get(RadarProduct::Velocity),
            None,
            "another product's editor is untouched",
        );
        assert!(curves.is_edited(RadarProduct::Reflectivity));
        assert!(!curves.is_edited(RadarProduct::Velocity));

        curves.reset(RadarProduct::Velocity);
        assert!(
            curves.is_edited(RadarProduct::Reflectivity),
            "resetting one product must not reset another",
        );
        curves.reset(RadarProduct::Reflectivity);
        assert_eq!(
            curves.get(RadarProduct::Reflectivity),
            None,
            "reset restores the untouched state, which is what makes it bit-exact",
        );
    }
}
