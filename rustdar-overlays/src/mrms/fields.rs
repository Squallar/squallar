//! The MRMS products projected into the substrate's read contract:
//! [`rustdar_source::product::ProductSpec`].
//!
//! **This crate cannot reuse `rustdar-radar`'s palette, and that is a rule
//! rather than an oversight.** The overlays→radar edge is cut and pinned by
//! `rustdar-source/tests/charter.rs::the_overlays_to_radar_edge_stays_cut`; a
//! shared colour table would have to move into `rustdar-source`, which is
//! contract and vocabulary only. Duplicating a reflectivity ramp is the correct
//! cost of the band; a band violation is not.
//!
//! The stops below are the classic NWS reflectivity ladder in **5 dBZ bands**,
//! `is_gradient: false` because reflectivity is read as bands and not as a
//! continuous ramp — the number a reader takes off the bar is the band's floor.
//!
//! A composite is a column maximum, so it has no tilt and no vertical extent:
//! `vertical: false`, `tilted: false`.

use std::sync::LazyLock;

use rustdar_source::product::{FieldId, LegendScale, ProductSpec};
use rustdar_units::Quantity;

use super::MrmsProduct;

/// The group label every MRMS product files under.
pub const GROUP: &str = "MRMS national mosaic";

/// Reflectivity in dBZ, in 5 dBZ bands from 5 up.
///
/// **The first stop is load-bearing beyond ergonomics.**
/// [`crate::render::gridded::color_for`] paints nothing below it, so clear air,
/// genuine −30 dBZ returns and — on this bar, today — an unmapped −999 all come
/// out transparent. That is what makes `crate::mrms::decode::to_reading`'s
/// effect invisible on screen and visible only in the reading a hover reports;
/// `to_reading`'s own doc records the tamper check.
///
/// **A bar whose first stop went below −99 would change that**, and the person
/// registering it would be editing this file rather than the decoder. Say so
/// here, where they will be looking.
static REFLECTIVITY: LazyLock<LegendScale> = LazyLock::new(|| {
    let thresholds = vec![
        (5.0f32, [0x40u8, 0xe8, 0xe3]),
        (10.0, [0x26, 0xa4, 0xfa]),
        (15.0, [0x0a, 0x2f, 0xc0]),
        (20.0, [0x00, 0xff, 0x00]),
        (25.0, [0x00, 0xc8, 0x00]),
        (30.0, [0x00, 0x90, 0x00]),
        (35.0, [0xff, 0xff, 0x00]),
        (40.0, [0xe7, 0xc0, 0x00]),
        (45.0, [0xff, 0x90, 0x00]),
        (50.0, [0xff, 0x00, 0x00]),
        (55.0, [0xd6, 0x00, 0x00]),
        (60.0, [0xc0, 0x00, 0x00]),
        (65.0, [0xff, 0x00, 0xff]),
        (70.0, [0x99, 0x55, 0xc9]),
        (75.0, [0xff, 0xff, 0xff]),
    ];
    let min_value = thresholds.first().map_or(0.0, |e| e.0);
    let max_value = thresholds.last().map_or(1.0, |e| e.0);
    LegendScale {
        thresholds,
        // Bands, not a gradient: a reflectivity bar is read by which band a
        // pixel is in, and interpolating between 45 and 50 dBZ invents a colour
        // that names no band.
        is_gradient: false,
        min_value,
        max_value,
    }
});

/// Precipitation rate in mm/h.
///
/// Stated in **mm/h, which is the unit the grid carries** — that is the whole
/// condition on [`crate::render::gridded::FieldPaint::over_scale`], and it is
/// why MRMS may use the generic path where the HRRR parameters may not.
static PRECIP_RATE: LazyLock<LegendScale> = LazyLock::new(|| {
    let thresholds = vec![
        (0.1f32, [0xa0u8, 0xe0, 0xff]),
        (0.5, [0x4d, 0xa6, 0xff]),
        (1.0, [0x1a, 0x5c, 0xe6]),
        (2.5, [0x00, 0xc0, 0x40]),
        (5.0, [0xa8, 0xe0, 0x00]),
        (10.0, [0xff, 0xd0, 0x00]),
        (20.0, [0xff, 0x80, 0x00]),
        (35.0, [0xff, 0x20, 0x00]),
        (50.0, [0xc0, 0x00, 0x40]),
        (75.0, [0xff, 0x00, 0xff]),
        (100.0, [0xff, 0xff, 0xff]),
    ];
    let min_value = thresholds.first().map_or(0.0, |e| e.0);
    let max_value = thresholds.last().map_or(1.0, |e| e.0);
    LegendScale {
        thresholds,
        is_gradient: true,
        min_value,
        max_value,
    }
});

fn scale(product: MrmsProduct) -> &'static LegendScale {
    match product {
        MrmsProduct::ReflectivityComposite => &REFLECTIVITY,
        MrmsProduct::PrecipRate => &PRECIP_RATE,
    }
}

/// Every MRMS product's registration, projected once.
static FIELDS: LazyLock<Vec<ProductSpec>> = LazyLock::new(|| {
    MrmsProduct::all()
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let scale = scale(p);
            ProductSpec {
                // The product's persisted spelling: `serialize_pane_state`
                // writes `as_str()` and `deserialize_pane_state` reads it, so
                // this is the key that ends up in a user's config file, and it
                // is also what `GriddedJob::encode` puts on the wire.
                id: FieldId::from_static(p.as_str()),
                name: p.display_name(),
                code: p.as_str(),
                sort_order: u8::try_from(i).expect("two products fit in a u8"),
                group: GROUP,
                // `Unitless`: dBZ is not a convertible quantity, and a rain
                // rate the mosaic states in mm/h is already the number the
                // scale's stops are written in — converting it at display would
                // move the value away from the bar that explains it.
                quantity: Quantity::Unitless {
                    label: p.unit_label(),
                },
                scale,
                value_domain: (scale.min_value, scale.max_value),
                domain_label_ends: ("\u{2265}", p.unit_label()),
                // A composite is a column maximum and a rate is a surface
                // field: neither has vertical extent, so neither reaches an
                // isosurface slider or the 3D stack.
                vertical: false,
                tilted: false,
            }
        })
        .collect()
});

/// Every MRMS product, in [`MrmsProduct::all`]'s order.
pub fn products() -> &'static [ProductSpec] {
    &FIELDS
}

/// The registration for `product`.
pub fn spec(product: MrmsProduct) -> &'static ProductSpec {
    let i = MrmsProduct::all()
        .iter()
        .position(|&p| p == product)
        .expect("every product is in `all()`");
    &FIELDS[i]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_product_registers_a_row_that_names_it_back() {
        assert_eq!(products().len(), MrmsProduct::all().len());
        for (i, &p) in MrmsProduct::all().iter().enumerate() {
            let row = spec(p);
            assert_eq!(row.id.as_str(), p.as_str());
            assert_eq!(row.group, GROUP);
            assert_eq!(row.sort_order as usize, i);
            assert!(!row.vertical && !row.tilted);
        }
    }

    /// Ascending stops are what `color_for`'s `partition_point` bracket rests
    /// on; a descending pair would silently pick the wrong band.
    #[test]
    fn every_scale_is_ascending_and_matches_its_declared_ends() {
        for &p in MrmsProduct::all() {
            let s = scale(p);
            assert!(s.thresholds.len() > 4, "{} has a stub bar", p.as_str());
            for pair in s.thresholds.windows(2) {
                assert!(
                    pair[1].0 > pair[0].0,
                    "{}'s stops are not ascending at {:?}",
                    p.as_str(),
                    pair,
                );
            }
            assert_eq!(s.min_value, s.thresholds[0].0);
            assert_eq!(s.max_value, s.thresholds.last().unwrap().0);
        }
    }

    /// The condition [`crate::render::gridded::FieldPaint::over_scale`] is
    /// documented against: the stops are in the units the grid carries, so no
    /// display conversion sits between the value and the bar.
    #[test]
    fn no_product_converts_for_display() {
        for &p in MrmsProduct::all() {
            assert!(
                matches!(spec(p).quantity, Quantity::Unitless { .. }),
                "{} declares a convertible quantity, so the raster would \
                 compare raw values against converted stops — the exact defect \
                 that keeps the sixteen HRRR parameters off the generic ramp",
                p.as_str(),
            );
        }
    }

    /// Reflectivity's first stop is above zero, which is what makes clear air
    /// transparent even for a value that survived the NaN mapping.
    #[test]
    fn the_reflectivity_bar_starts_above_clear_air() {
        assert!(REFLECTIVITY.thresholds[0].0 >= 5.0);
        assert!(!REFLECTIVITY.is_gradient, "reflectivity is banded");
    }
}
