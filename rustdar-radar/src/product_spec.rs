//! The radar-internal product registry: one struct literal per product, every
//! field explicit.

use crate::level3::VolumePick;
use crate::types::{MomentSlot, RadarProduct};
use rustdar_units::Quantity;

/// Everything the crate states about one product, in one row.
pub(crate) struct RadarProductSpec {
    /// Short lowercase identifier (`"ref"`, `"vel"`, …).
    pub(crate) code: &'static str,
    /// Display name.
    pub(crate) name: &'static str,
    /// Order products are listed in the UI.
    pub(crate) sort_order: u8,
    /// Whether the pixels come from the RPG's own Level III object rather
    /// than a Level II tilt.
    pub(crate) is_level3: bool,
    /// The AWIPS product IDs to fetch for this product.
    pub(crate) level3_codes: Option<&'static [&'static str]>,
    /// Which object of a paired volume this product's Level III rendition is.
    pub(crate) level3_volume_pick: Option<VolumePick>,
    /// A stable identifier for this product on a wire.
    pub(crate) wire_code: u16,
    /// Which of a radial's moment fields this product reads.
    pub(crate) moment_slot: Option<MomentSlot>,
    /// Whether this product reads every tilt carrying its moment, rather than
    /// the one sweep `crate::render::find_sweep` picks.
    pub(crate) reads_whole_volume: bool,
    /// Whether this product's picture is a function of the environmental 0 °C / −20 °C
    /// heights ([`crate::sounding`]'s per-site pair).
    pub(crate) reads_env_heights: bool,
    /// The unit domain the product's values live in.
    pub(crate) quantity: Quantity,
}

/// The registration for `p`.
pub(crate) const fn spec(p: RadarProduct) -> RadarProductSpec {
    match p {
        RadarProduct::Reflectivity => RadarProductSpec {
            code: "ref",
            name: "Reflectivity",
            sort_order: 0,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 1,
            moment_slot: Some(MomentSlot::Reflectivity),
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::Unitless { label: "dBZ" },
        },
        RadarProduct::Velocity => RadarProductSpec {
            code: "vel",
            name: "Velocity",
            sort_order: 1,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 2,
            moment_slot: Some(MomentSlot::Velocity),
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::SpeedMps,
        },
        RadarProduct::SpectrumWidth => RadarProductSpec {
            code: "sw",
            name: "Spectrum Width",
            sort_order: 2,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 3,
            moment_slot: Some(MomentSlot::SpectrumWidth),
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::SpeedMps,
        },
        RadarProduct::DifferentialPhase => RadarProductSpec {
            code: "phi",
            name: "Differential Phase",
            sort_order: 5,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 4,
            moment_slot: Some(MomentSlot::DifferentialPhase),
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::Unitless { label: "\u{00b0}" },
        },
        RadarProduct::CorrelationCoefficient => RadarProductSpec {
            code: "rho",
            name: "Correlation Coefficient",
            sort_order: 4,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 5,
            moment_slot: Some(MomentSlot::CorrelationCoefficient),
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::Unitless { label: "CC" },
        },
        RadarProduct::DifferentialReflectivity => RadarProductSpec {
            code: "zdr",
            name: "Differential Reflectivity",
            sort_order: 3,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 6,
            moment_slot: Some(MomentSlot::DifferentialReflectivity),
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::Unitless { label: "dB" },
        },
        RadarProduct::StormRelativeVelocity => RadarProductSpec {
            code: "srv",
            name: "Storm-Relative Velocity",
            sort_order: 7,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 7,
            // Derived from velocity, so every velocity tilt lists.
            moment_slot: Some(MomentSlot::Velocity),
            // `crate::velocity::volume_wind_profile` fits the dealias-seeding
            // profile from every velocity tilt of the volume.
            reads_whole_volume: true,
            reads_env_heights: false,
            quantity: Quantity::SpeedMps,
        },
        RadarProduct::SpecificDifferentialPhase => RadarProductSpec {
            code: "kdp",
            name: "Specific Differential Phase",
            sort_order: 8,
            is_level3: true,
            level3_codes: Some(&["N0K"]),
            level3_volume_pick: Some(VolumePick::NEAREST),
            wire_code: 8,
            moment_slot: None,
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::Unitless {
                label: "\u{00b0}/km",
            },
        },
        RadarProduct::EchoTops => RadarProductSpec {
            code: "eet",
            name: "Echo Tops",
            sort_order: 9,
            is_level3: true,
            level3_codes: Some(&["EET"]),
            level3_volume_pick: Some(VolumePick::NEAREST),
            wire_code: 9,
            moment_slot: None,
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::HeightKft,
        },
        RadarProduct::EchoTopsInterpolated => RadarProductSpec {
            code: "eti",
            name: "Echo Tops (Interp)",
            sort_order: 10,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 10,
            // Integrates the whole reflectivity volume; the rendered field is
            // tilt-independent.
            moment_slot: Some(MomentSlot::Reflectivity),
            // `volumetric::compute_echo_tops` integrates the whole reflectivity volume.
            reads_whole_volume: true,
            reads_env_heights: false,
            quantity: Quantity::HeightKft,
        },
        RadarProduct::VerticallyIntegratedLiquid => RadarProductSpec {
            code: "vil",
            name: "Vertically Integrated Liquid",
            sort_order: 11,
            is_level3: true,
            level3_codes: Some(&["DVL"]),
            level3_volume_pick: Some(VolumePick::NEAREST),
            wire_code: 11,
            moment_slot: None,
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::Unitless {
                label: "kg/m\u{00b2}",
            },
        },
        RadarProduct::VilDensity => RadarProductSpec {
            code: "vild",
            name: "VIL Density",
            sort_order: 12,
            is_level3: true,
            // Derived from two objects, `DVL` over `EET` for the same volume
            // ([`crate::vild`]), so it names both.
            level3_codes: Some(&["DVL", "EET"]),
            level3_volume_pick: Some(VolumePick::NEAREST),
            wire_code: 15,
            // The RPG's own `DVL` over its own `EET` ([`crate::vild`]).
            moment_slot: None,
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::Unitless {
                label: "g/m\u{00b3}",
            },
        },
        RadarProduct::ProbabilityOfSevereHail => RadarProductSpec {
            code: "posh",
            name: "Prob. of Severe Hail",
            sort_order: 13,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 16,
            moment_slot: Some(MomentSlot::Reflectivity),
            reads_whole_volume: true,
            // The SHI-to-size mapping has no field at all without the 0 °C /
            // −20 °C pair: `crate::hail` renders nothing rather than guessing.
            reads_env_heights: true,
            quantity: Quantity::Unitless { label: "%" },
        },
        RadarProduct::MaxExpectedHailSize => RadarProductSpec {
            code: "mehs",
            name: "Max Expected Hail Size",
            sort_order: 14,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 17,
            moment_slot: Some(MomentSlot::Reflectivity),
            reads_whole_volume: true,
            reads_env_heights: true,
            // The field computes in mm (`crate::hail`); the render seam
            // converts to inches.
            quantity: Quantity::HailSizeIn,
        },
        RadarProduct::HydrometeorClassification => RadarProductSpec {
            code: "hhc",
            name: "Hydrometeor Classification",
            sort_order: 15,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 12,
            // Composites every dual-pol tilt of the volume (`crate::hhc`);
            // listed on reflectivity as ETI and VIL density are.
            moment_slot: Some(MomentSlot::Reflectivity),
            reads_whole_volume: true,
            // The pair's 0 °C height is the third rung of
            // `crate::hca::resolve_melting_layer`, so every class code
            // downstream of the layer moves with it.
            reads_env_heights: true,
            quantity: Quantity::Unitless { label: "HHC" },
        },
        RadarProduct::PrecipitationRate => RadarProductSpec {
            code: "dpr",
            name: "Precipitation Rate",
            sort_order: 16,
            is_level3: true,
            level3_codes: Some(&["DPR"]),
            // `Latest` for the QPE family: it emits an end-of-volume composite
            // plus a partial intermediate per SAILS/MRLE scan under the same
            // volume start, and a loop must not animate partial accumulations.
            level3_volume_pick: Some(VolumePick::Latest),
            wire_code: 13,
            moment_slot: None,
            reads_whole_volume: false,
            reads_env_heights: false,
            quantity: Quantity::PrecipRateInPerHr,
        },
        RadarProduct::NormalizedRotation => RadarProductSpec {
            code: "nrot",
            name: "Normalized Rotation",
            sort_order: 6,
            is_level3: false,
            level3_codes: None,
            level3_volume_pick: None,
            wire_code: 14,
            // NROT is derived from velocity.
            moment_slot: Some(MomentSlot::Velocity),
            // `crate::velocity::volume_wind_profile` fits the dealias-seeding
            // profile from every velocity tilt of the volume.
            reads_whole_volume: true,
            reads_env_heights: false,
            quantity: Quantity::Unitless { label: "NROT" },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_units::{HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, UserPreferences};
    use std::collections::HashSet;

    fn prefs_with(mutate: impl FnOnce(&mut UserPreferences)) -> UserPreferences {
        let mut prefs = UserPreferences::default();
        mutate(&mut prefs);
        prefs
    }

    #[test]
    fn all_lists_every_variant_in_declaration_order() {
        assert_eq!(RadarProduct::all().len(), 17);
        for (i, &p) in RadarProduct::all().iter().enumerate() {
            assert_eq!(
                p as usize, i,
                "{p:?} sits at position {i} of all() but has discriminant {} — \
                 `product as usize` indexing is broken",
                p as usize,
            );
        }
    }

    #[test]
    fn every_product_has_a_registration() {
        let mut wire_codes = HashSet::new();
        let mut sort_orders = HashSet::new();
        for &p in RadarProduct::all() {
            let s = spec(p);
            assert!(!s.code.is_empty(), "{p:?} registers an empty code");
            assert!(!s.name.is_empty(), "{p:?} registers an empty name");
            assert!(
                wire_codes.insert(s.wire_code),
                "{p:?} reuses wire code {}",
                s.wire_code,
            );
            assert!(
                sort_orders.insert(s.sort_order),
                "{p:?} reuses sort order {}",
                s.sort_order,
            );
            assert_eq!(
                s.level3_codes.is_some(),
                s.is_level3,
                "{p:?}: level3_codes and is_level3 disagree — a Level III \
                 product with nothing to fetch, or fetch codes on a Level II \
                 product",
            );
            assert_eq!(
                s.level3_volume_pick.is_some(),
                s.is_level3,
                "{p:?}: level3_volume_pick and is_level3 disagree — the old \
                 method derived the pick's presence from is_level3, so the \
                 registry must not drift from it",
            );
        }
    }

    #[test]
    fn wire_codes_round_trip() {
        for &p in RadarProduct::all() {
            assert_eq!(
                RadarProduct::from_wire_code(spec(p).wire_code),
                Some(p),
                "{p:?} does not round-trip through its wire code",
            );
        }
        for unknown in [0u16, 18, u16::MAX] {
            assert_eq!(
                RadarProduct::from_wire_code(unknown),
                None,
                "wire code {unknown} is not registered and must decode to None",
            );
        }
    }

    #[test]
    fn unit_label_via_quantity_matches_the_old_table() {
        let defaults = UserPreferences::default();
        let expected: [(RadarProduct, &str); 17] = [
            (RadarProduct::Reflectivity, "dBZ"),
            (RadarProduct::Velocity, "mph"),
            (RadarProduct::SpectrumWidth, "mph"),
            (RadarProduct::DifferentialPhase, "\u{00b0}"),
            (RadarProduct::CorrelationCoefficient, "CC"),
            (RadarProduct::DifferentialReflectivity, "dB"),
            (RadarProduct::StormRelativeVelocity, "mph"),
            (RadarProduct::SpecificDifferentialPhase, "\u{00b0}/km"),
            (RadarProduct::EchoTops, "kft"),
            (RadarProduct::EchoTopsInterpolated, "kft"),
            (RadarProduct::VerticallyIntegratedLiquid, "kg/m\u{00b2}"),
            (RadarProduct::VilDensity, "g/m\u{00b3}"),
            (RadarProduct::ProbabilityOfSevereHail, "%"),
            (RadarProduct::MaxExpectedHailSize, "in"),
            (RadarProduct::HydrometeorClassification, "HHC"),
            (RadarProduct::PrecipitationRate, "in/hr"),
            (RadarProduct::NormalizedRotation, "NROT"),
        ];
        assert_eq!(expected.len(), RadarProduct::all().len());
        for (p, label) in expected {
            assert_eq!(p.unit_label(&defaults), label, "{p:?} under default prefs");
        }

        let metric_speed = prefs_with(|p| p.speed = SpeedUnit::MetersPerSec);
        for p in [
            RadarProduct::Velocity,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpectrumWidth,
        ] {
            assert_eq!(p.unit_label(&metric_speed), "m/s", "{p:?} in m/s");
        }
        let metric_height = prefs_with(|p| p.height = HeightUnit::Meters);
        for p in [RadarProduct::EchoTops, RadarProduct::EchoTopsInterpolated] {
            assert_eq!(p.unit_label(&metric_height), "km", "{p:?} metric");
        }
        let mm = prefs_with(|p| p.precip_rate = PrecipRateUnit::MillimetersPerHour);
        assert_eq!(RadarProduct::PrecipitationRate.unit_label(&mm), "mm/hr");
        let cm = prefs_with(|p| p.hail_size = HailSizeUnit::Centimeters);
        assert_eq!(RadarProduct::MaxExpectedHailSize.unit_label(&cm), "cm");
    }
}
