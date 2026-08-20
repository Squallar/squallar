//! Per-product isosurface thresholds — the session state the sidebar slider edits,
//! the frame reads, and the config persists.

use std::collections::HashMap;

use rustdar_radar::types::RadarProduct;
use rustdar_radar::voxel::default_iso_threshold;

/// Every product's user-set isosurface threshold, in the product's own units.
#[derive(Default)]
pub struct IsoThresholds {
    thresholds: HashMap<RadarProduct, f32>,
}

impl IsoThresholds {
    /// The threshold for `product`: the user's where one is set, else the argued
    /// default.
    pub fn get(&self, product: RadarProduct) -> f32 {
        self.thresholds
            .get(&product)
            .copied()
            .unwrap_or_else(|| default_iso_threshold(product))
    }

    pub fn set(&mut self, product: RadarProduct, threshold: f32) {
        if !threshold.is_finite() {
            return;
        }
        if threshold == default_iso_threshold(product) {
            self.thresholds.remove(&product);
        } else {
            self.thresholds.insert(product, threshold);
        }
    }

    pub fn is_edited(&self, product: RadarProduct) -> bool {
        self.thresholds.contains_key(&product)
    }

    pub fn entries(&self) -> impl Iterator<Item = (RadarProduct, f32)> + '_ {
        self.thresholds.iter().map(|(&p, &t)| (p, t))
    }
}

/// The slider's travel for a product, in its own units — ergonomics, not physics.
pub fn slider_range(product: RadarProduct) -> std::ops::RangeInclusive<f32> {
    match product {
        RadarProduct::Reflectivity => 0.0..=75.0,
        RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => 2.0..=60.0,
        RadarProduct::SpectrumWidth => 1.0..=20.0,
        RadarProduct::DifferentialReflectivity => 0.5..=6.0,
        RadarProduct::DifferentialPhase => 10.0..=350.0,
        RadarProduct::CorrelationCoefficient => 0.5..=1.0,
        RadarProduct::SpecificDifferentialPhase => 0.25..=8.0,
        RadarProduct::NormalizedRotation => 0.25..=3.0,
        _ => 0.0..=1.0,
    }
}

/// What the slider's number means, as a short prefix — "≥" for a value, "|±| ≥" for
/// a deviation, "≤" for ρHV's bound — plus the unit suffix.
pub fn slider_labels(product: RadarProduct) -> (&'static str, &'static str) {
    use rustdar_radar::voxel::{IsoShape, iso_shape};
    let prefix = match iso_shape(product) {
        IsoShape::Sequential => "\u{2265}",
        IsoShape::DeviationFrom { .. } => "|\u{b1}| \u{2265}",
        IsoShape::AtOrBelow => "\u{2264}",
    };
    let suffix = match product {
        RadarProduct::Reflectivity => " dBZ",
        RadarProduct::Velocity
        | RadarProduct::StormRelativeVelocity
        | RadarProduct::SpectrumWidth => " m/s",
        RadarProduct::DifferentialReflectivity => " dB",
        RadarProduct::DifferentialPhase => "\u{b0}",
        RadarProduct::SpecificDifferentialPhase => "\u{b0}/km",
        _ => "",
    };
    (prefix, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store holds exceptions: setting a product back to its default erases it.
    #[test]
    fn thresholds_are_stored_per_product_as_exceptions() {
        let mut store = IsoThresholds::default();
        assert_eq!(
            store.get(RadarProduct::Reflectivity),
            default_iso_threshold(RadarProduct::Reflectivity),
        );
        assert!(!store.is_edited(RadarProduct::Reflectivity));

        store.set(RadarProduct::Reflectivity, 35.0);
        assert_eq!(store.get(RadarProduct::Reflectivity), 35.0);
        assert!(store.is_edited(RadarProduct::Reflectivity));
        assert_eq!(
            store.get(RadarProduct::Velocity),
            default_iso_threshold(RadarProduct::Velocity),
            "one product's threshold must never bleed into another's",
        );

        store.set(
            RadarProduct::Reflectivity,
            default_iso_threshold(RadarProduct::Reflectivity),
        );
        assert!(
            !store.is_edited(RadarProduct::Reflectivity),
            "back at the default is the same as never touched",
        );
    }

    /// A non-finite threshold is refused at the door, like every persisted float in
    /// this codebase.
    #[test]
    fn a_non_finite_threshold_is_refused() {
        let mut store = IsoThresholds::default();
        store.set(RadarProduct::Reflectivity, f32::NAN);
        store.set(RadarProduct::Reflectivity, f32::INFINITY);
        assert!(!store.is_edited(RadarProduct::Reflectivity));
    }
}
