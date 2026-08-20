//! The Volume Alpha curve: a per-product opacity profile the user draws over
//! the palette, GR2Analyst-style.

use std::collections::HashMap;
use std::sync::Arc;

use rustdar_source::product::FieldId;

/// Palette entries a curve spans — one alpha per LUT index.
pub const CURVE_LEN: usize = 256;

/// A user-drawn alpha curve over the 256-index value axis.
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

    /// A grid table's alpha channel as a curve — what an untouched editor
    /// shows, and what a first stroke starts from. `None` unless `lut` is the
    /// exact 1024 bytes a `VolumeGrid::lut()` hands over.
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
    /// transparent — `TransferTable::fade_band`'s rule, over the curve instead
    /// of the palette, spelled identically on purpose: the raymarch's skip
    /// threshold is anchored at `(band + 0.5) / 255`, and the two producers of
    /// `band` must agree about what it counts or the march skips visible data
    /// on one path and pays for invisible shells on the other.
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
pub fn apply_stroke(alphas: &mut [u8; CURVE_LEN], from: (f32, f32), to: (f32, f32)) {
    if ![from.0, from.1, to.0, to.1].iter().all(|v| v.is_finite()) {
        return;
    }
    let (left, right) = if from.0 <= to.0 {
        (from, to)
    } else {
        (to, from)
    };
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
#[derive(Default)]
pub struct AlphaCurves {
    curves: HashMap<FieldId, AlphaCurve>,
}

impl AlphaCurves {
    /// The curve for `field`, or `None` for an untouched editor.
    pub fn get(&self, field: &FieldId) -> Option<AlphaCurve> {
        self.curves.get(field).cloned()
    }

    /// Store `field`'s curve. Live during a drag: the editor writes every
    /// frame of the stroke, and the frontend re-uploads the 1 KiB LUT only
    /// when the bytes actually changed.
    ///
    /// **The one door, for the editor and for the config alike.** A curve
    /// saved under a field this build does not register is kept here verbatim
    /// under the open-id doctrine: it applies to nothing — no pane can select
    /// a field the registry does not offer — and it survives to be written
    /// back, so a newer build's curve is not destroyed by a session under this
    /// one.
    pub fn set(&mut self, field: &FieldId, curve: AlphaCurve) {
        self.curves.insert(field.clone(), curve);
    }

    /// Forget `field`'s curve — the reset, back to the grid table's own
    /// alpha. See [`AlphaCurve::from_palette`] for why that is not the same
    /// thing as the palette's.
    pub fn reset(&mut self, field: &FieldId) {
        self.curves.remove(field);
    }

    /// Whether `field` has a user curve at all.
    pub fn is_edited(&self, field: &FieldId) -> bool {
        self.curves.contains_key(field)
    }

    /// Every edited field and its curve, in an arbitrary order — the save
    /// path sorts so the config file is deterministic.
    pub fn entries(&self) -> impl Iterator<Item = (&FieldId, &AlphaCurve)> {
        self.curves.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_radar::fields as radar_fields;
    use rustdar_source::product::FieldId;

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

    /// The seeded default is the grid table's own alpha, entry for entry —
    /// which is what makes "open the editor and touch nothing" a no-op by
    /// construction rather than by luck.
    #[test]
    fn the_default_curve_is_the_grid_tables_own_alpha() {
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
        assert_eq!(
            alphas[0], 0,
            "a stroke over the left edge must not paint entry 0"
        );
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

        assert_eq!(
            &alphas[..100],
            &before[..100],
            "left of the stroke is untouched"
        );
        assert_eq!(
            &alphas[111..],
            &before[111..],
            "right of the stroke is untouched"
        );
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
        assert_eq!(
            alphas, before,
            "a non-finite sample must be refused, not laundered"
        );
    }

    /// The curve's `fade_band` follows `TransferTable::fade_band`'s exact rule:
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

    fn field(product: &FieldId) -> FieldId {
        product.clone()
    }

    /// The store is per-field: a curve set for one field neither answers
    /// nor resets another's.
    #[test]
    fn curves_are_stored_per_field() {
        let mut curves = AlphaCurves::default();
        let mut alphas = [0u8; CURVE_LEN];
        alphas[200] = 99;
        let curve = AlphaCurve::from_alphas(alphas);
        let reflectivity = field(&radar_fields::known::REFLECTIVITY);
        let velocity = field(&radar_fields::known::VELOCITY);

        curves.set(&reflectivity, curve.clone());
        assert_eq!(curves.get(&reflectivity), Some(curve));
        assert_eq!(
            curves.get(&velocity),
            None,
            "another field's editor is untouched",
        );
        assert!(curves.is_edited(&reflectivity));
        assert!(!curves.is_edited(&velocity));

        curves.reset(&velocity);
        assert!(
            curves.is_edited(&reflectivity),
            "resetting one field must not reset another",
        );
        curves.reset(&reflectivity);
        assert_eq!(
            curves.get(&reflectivity),
            None,
            "reset restores the untouched state, which is what makes it bit-exact",
        );
    }

    /// **The open-id doctrine on the curve store.** A curve saved under a
    /// field this build does not register is kept verbatim and applies to
    /// nothing — which is the guarantee `known_product_or_none`'s
    /// drop-on-load used to buy, without destroying the entry.
    #[test]
    fn a_curve_for_a_field_this_build_does_not_register_is_kept_inert() {
        let mut curves = AlphaCurves::default();
        let mut alphas = [0u8; CURVE_LEN];
        alphas[200] = 99;
        let unknown = FieldId::new("NoBuildRegistersThisField");
        curves.set(&unknown, AlphaCurve::from_alphas(alphas));
        assert!(curves.is_edited(&unknown));
        for product in radar_fields::known::ALL.iter() {
            assert!(
                !curves.is_edited(&field(product)),
                "an unknown id leaked onto {product:?}",
            );
        }
    }
}
