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
