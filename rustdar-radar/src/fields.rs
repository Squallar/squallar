//! The radar crate's products projected into the substrate's read contract:
//! [`rustdar_source::product::ProductSpec`].
//!
//! This is a **projection, not a second table**. Every value below is read from
//! [`crate::product_spec::spec`] — the one registration — or computed from the
//! physics predicates that already answer the question
//! ([`crate::derive::volume_slot`], [`crate::voxel::iso_shape`],
//! [`crate::palette`]'s built-once colour scales). Nothing here is a fact this
//! crate does not already state somewhere else.
//!
//! **The wire code deliberately does not cross.** `VoxelGrid::to_bytes` writes
//! `product.wire_code()` as a `u16` at byte offset 6, so the `FieldId` ↔ wire
//! code map has to stay private to this crate: a consumer that could read it
//! could pin a byte layout it does not own.

use std::sync::LazyLock;

use rustdar_source::product::{FieldId, ProductSpec};

use crate::product_spec::spec as row;
use crate::types::RadarProduct;
use crate::voxel::IsoShape;

/// The group label every radar product files under.
pub const GROUP: &str = "Radar products";

/// The threshold prefix a product's isosurface shape implies: `≥` for a value,
/// `|±| ≥` for a deviation, `≤` for ρHV's bound.
fn domain_prefix(product: RadarProduct) -> &'static str {
    match crate::voxel::iso_shape(product) {
        IsoShape::Sequential => "\u{2265}",
        IsoShape::DeviationFrom { .. } => "|\u{b1}| \u{2265}",
        IsoShape::AtOrBelow => "\u{2264}",
    }
}

/// Every radar product's registration, projected once.
///
/// `LazyLock` rather than `const`: `scale` borrows
/// [`crate::palette`]'s built-once table, which is itself a `LazyLock`.
static FIELDS: LazyLock<Vec<ProductSpec>> = LazyLock::new(|| {
    RadarProduct::all()
        .iter()
        .map(|&p| {
            let r = row(p);
            let scale = crate::palette::legend_scale_static(p);
            ProductSpec {
                id: FieldId::from_static(r.field_id),
                name: r.name,
                code: r.code,
                sort_order: r.sort_order,
                group: GROUP,
                quantity: r.quantity,
                scale,
                // A product with no 3D editor has no ergonomic slider travel;
                // its domain is the span its own colour scale covers. There is
                // no fabricated `0..=1` here — that wildcard died with WO-E9a.
                value_domain: r.value_domain.unwrap_or((scale.min_value, scale.max_value)),
                domain_label_ends: (domain_prefix(p), r.domain_suffix),
                vertical: crate::derive::volume_slot(p).is_some(),
                tilted: r.tilted,
            }
        })
        .collect()
});

/// Every radar product, in [`RadarProduct::all`]'s order.
pub fn products() -> &'static [ProductSpec] {
    &FIELDS
}

/// The registration for `product`.
pub fn spec(product: RadarProduct) -> &'static ProductSpec {
    // Indexing by discriminant is what `palette::legend_scale_static` already
    // does; `the_all_list_is_in_discriminant_order` is the pin that keeps it
    // honest.
    &FIELDS[product as usize]
}

/// The product a `FieldId` names, or `None` for an id this build does not
/// register — an open-string id may name a field from another build.
pub fn product_for(id: &FieldId) -> Option<RadarProduct> {
    RadarProduct::all()
        .iter()
        .copied()
        .find(|&p| spec(p).id == *id)
}

#[cfg(test)]
mod tests;

/// The seventeen field ids this crate registers, as `const` items.
///
/// The exact model of [`rustdar_source::id::known`], and for the same reason:
/// an open string has no compiler to catch a typo, so the spellings live in one
/// place and everything else refers to them. **These are the bytes already in
/// every user's config file** — the product enum's own `Serialize` output — so
/// this module is as append-only as the layer ledger is.
///
/// `every_known_field_is_registered` is what stops one of these drifting away
/// from the registration it names.
pub mod known {
    use rustdar_source::product::FieldId;

    pub const REFLECTIVITY: FieldId = FieldId::from_static("Reflectivity");
    pub const VELOCITY: FieldId = FieldId::from_static("Velocity");
    pub const SPECTRUM_WIDTH: FieldId = FieldId::from_static("SpectrumWidth");
    pub const DIFFERENTIAL_PHASE: FieldId = FieldId::from_static("DifferentialPhase");
    pub const CORRELATION_COEFFICIENT: FieldId = FieldId::from_static("CorrelationCoefficient");
    pub const DIFFERENTIAL_REFLECTIVITY: FieldId = FieldId::from_static("DifferentialReflectivity");
    pub const STORM_RELATIVE_VELOCITY: FieldId = FieldId::from_static("StormRelativeVelocity");
    pub const SPECIFIC_DIFFERENTIAL_PHASE: FieldId =
        FieldId::from_static("SpecificDifferentialPhase");
    pub const ECHO_TOPS: FieldId = FieldId::from_static("EchoTops");
    pub const ECHO_TOPS_INTERPOLATED: FieldId = FieldId::from_static("EchoTopsInterpolated");
    pub const VERTICALLY_INTEGRATED_LIQUID: FieldId =
        FieldId::from_static("VerticallyIntegratedLiquid");
    pub const VIL_DENSITY: FieldId = FieldId::from_static("VilDensity");
    pub const PROBABILITY_OF_SEVERE_HAIL: FieldId = FieldId::from_static("ProbabilityOfSevereHail");
    pub const MAX_EXPECTED_HAIL_SIZE: FieldId = FieldId::from_static("MaxExpectedHailSize");
    pub const HYDROMETEOR_CLASSIFICATION: FieldId =
        FieldId::from_static("HydrometeorClassification");
    pub const PRECIPITATION_RATE: FieldId = FieldId::from_static("PrecipitationRate");
    pub const NORMALIZED_ROTATION: FieldId = FieldId::from_static("NormalizedRotation");

    /// Every const above, for the sweeps that have to cover all of them.
    pub const ALL: [FieldId; 17] = [
        REFLECTIVITY,
        VELOCITY,
        SPECTRUM_WIDTH,
        DIFFERENTIAL_PHASE,
        CORRELATION_COEFFICIENT,
        DIFFERENTIAL_REFLECTIVITY,
        STORM_RELATIVE_VELOCITY,
        SPECIFIC_DIFFERENTIAL_PHASE,
        ECHO_TOPS,
        ECHO_TOPS_INTERPOLATED,
        VERTICALLY_INTEGRATED_LIQUID,
        VIL_DENSITY,
        PROBABILITY_OF_SEVERE_HAIL,
        MAX_EXPECTED_HAIL_SIZE,
        HYDROMETEOR_CLASSIFICATION,
        PRECIPITATION_RATE,
        NORMALIZED_ROTATION,
    ];
}
