//! The radar-internal product registry: one struct literal per product, every
//! field explicit.

use crate::level3::VolumePick;
use crate::sites::RadarNetwork;
use crate::types::{MomentSlot, RadarProduct};
use squallar_units::Quantity;

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
    /// This product's [`squallar_source::product::FieldId`] spelling.
    ///
    /// **Byte-identical to the variant's own `Serialize` output.** That equality
    /// is what makes the move to `FieldId` keys a zero-migration change, and
    /// `the_field_id_is_the_products_own_serde_spelling` checks it rather than
    /// claiming it here.
    pub(crate) field_id: &'static str,
    /// The isosurface slider's travel, in the product's own units — ergonomics,
    /// not physics.
    ///
    /// `Some` for a product with vertical extent; `None` for one the 3D editor
    /// refuses, whose domain is then its own colour scale's span. There is no
    /// wildcard: a product that reaches a slider states the travel it gets.
    pub(crate) value_domain: Option<(f32, f32)>,
    /// The unit string that follows an isosurface threshold, e.g. `" dBZ"`.
    /// Empty where the slider's number carries no unit, and for every product
    /// that has no slider at all.
    pub(crate) domain_suffix: &'static str,
    /// Whether this product exists as a field at individual tilts, as opposed
    /// to being a whole-volume composite or a column integral.
    ///
    /// Not the same question as
    /// [`RadarProduct::tilt_independent_plan_view`](crate::types::RadarProduct::tilt_independent_plan_view),
    /// which asks whether the *plan view's* elevation argument is load-bearing.
    /// The hybrid classification is the pair that separates them: its plan view
    /// is one composited surface, but [`crate::hca`] computes a genuine per-tilt
    /// classification (the RPG's product 165) underneath it.
    pub(crate) tilted: bool,
    /// The unit domain the product's values live in.
    pub(crate) quantity: Quantity,
    /// The networks whose data can produce this product, stated per row.
    ///
    /// **No default and no wildcard**: availability is a fact about an
    /// instrument, so every registration says it. Three reasons a row is
    /// `Wsr88d`-only, all measured:
    ///
    /// * **Level III** — the object is made by an RPG and only the WSR-88D
    ///   network has one. A TDWR is served by the Supplemental Product
    ///   Generator, whose short list contains none of the four AWIPS codes this
    ///   app fetches (`N0K`, `EET`, `DVL`, `DPR`), checked 2026-08-11 against
    ///   `PIT`, `OKC`, `MIA` and `DCA`.
    /// * **Dual-pol moments** — a TDWR is single-pol, so ΦDP, ρHV and Z<sub>DR</sub>
    ///   are not in its radials at all.
    /// * **Dual-pol derived** — hydrometeor classification reads ΦDP and ρHV,
    ///   so it cannot exist where they do not.
    ///
    /// The eight that remain are the eight a terminal radar can draw.
    ///
    /// **Only the Level III arm of `discover_product_elevations` consults
    /// this.** The dual-pol rows are stated because they are true, not because
    /// anything reads them: the volume already excludes those moments by not
    /// carrying them, and duplicating that exclusion here would be a second
    /// implementation of one rule.
    pub(crate) available_networks: &'static [RadarNetwork],
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
            field_id: "Reflectivity",
            value_domain: Some((0.0, 75.0)),
            domain_suffix: " dBZ",
            tilted: true,
            quantity: Quantity::Unitless { label: "dBZ" },
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
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
            field_id: "Velocity",
            value_domain: Some((2.0, 60.0)),
            domain_suffix: " m/s",
            tilted: true,
            quantity: Quantity::SpeedMps,
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
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
            field_id: "SpectrumWidth",
            value_domain: Some((1.0, 20.0)),
            domain_suffix: " m/s",
            tilted: true,
            quantity: Quantity::SpeedMps,
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
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
            field_id: "DifferentialPhase",
            value_domain: Some((10.0, 350.0)),
            domain_suffix: "\u{b0}",
            tilted: true,
            quantity: Quantity::Unitless { label: "\u{00b0}" },
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "CorrelationCoefficient",
            value_domain: Some((0.5, 1.0)),
            domain_suffix: "",
            tilted: true,
            quantity: Quantity::Unitless { label: "CC" },
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "DifferentialReflectivity",
            value_domain: Some((0.5, 6.0)),
            domain_suffix: " dB",
            tilted: true,
            quantity: Quantity::Unitless { label: "dB" },
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "StormRelativeVelocity",
            value_domain: Some((2.0, 60.0)),
            domain_suffix: " m/s",
            tilted: true,
            quantity: Quantity::SpeedMps,
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
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
            field_id: "SpecificDifferentialPhase",
            value_domain: Some((0.25, 8.0)),
            domain_suffix: "\u{b0}/km",
            tilted: true,
            quantity: Quantity::Unitless {
                label: "\u{00b0}/km",
            },
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "EchoTops",
            value_domain: None,
            domain_suffix: "",
            tilted: false,
            quantity: Quantity::HeightKft,
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "EchoTopsInterpolated",
            value_domain: None,
            domain_suffix: "",
            tilted: false,
            quantity: Quantity::HeightKft,
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
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
            field_id: "VerticallyIntegratedLiquid",
            value_domain: None,
            domain_suffix: "",
            tilted: false,
            quantity: Quantity::Unitless {
                label: "kg/m\u{00b2}",
            },
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "VilDensity",
            value_domain: None,
            domain_suffix: "",
            tilted: false,
            quantity: Quantity::Unitless {
                label: "g/m\u{00b3}",
            },
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "ProbabilityOfSevereHail",
            value_domain: None,
            domain_suffix: "",
            tilted: false,
            quantity: Quantity::Unitless { label: "%" },
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
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
            field_id: "MaxExpectedHailSize",
            value_domain: None,
            domain_suffix: "",
            tilted: false,
            quantity: Quantity::HailSizeIn,
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
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
            field_id: "HydrometeorClassification",
            value_domain: None,
            domain_suffix: "",
            tilted: true,
            quantity: Quantity::Unitless { label: "HHC" },
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "PrecipitationRate",
            value_domain: None,
            domain_suffix: "",
            tilted: false,
            quantity: Quantity::PrecipRateInPerHr,
            available_networks: &[RadarNetwork::Wsr88d],
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
            field_id: "NormalizedRotation",
            value_domain: Some((0.25, 3.0)),
            domain_suffix: "",
            tilted: true,
            quantity: Quantity::Unitless { label: "NROT" },
            available_networks: &[RadarNetwork::Wsr88d, RadarNetwork::Tdwr],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every registration answers for every network, and WSR-88D is
    /// true for all seventeen — the behaviour-neutrality pin.**
    ///
    /// The walk is over `RadarProduct::all()`, so a product added without an
    /// `available_networks` row cannot compile, and one added with an empty row
    /// fails here rather than quietly becoming unavailable everywhere.
    #[test]
    fn every_product_states_its_availability_and_wsr88d_gets_all_of_them() {
        let all = RadarProduct::all();
        assert_eq!(all.len(), 17, "the registry's size moved");

        for product in all {
            let networks = spec(*product).available_networks;
            assert!(
                !networks.is_empty(),
                "{product:?} states no network at all, so it is available nowhere",
            );
            assert!(
                product.available_for(RadarNetwork::Wsr88d),
                "{product:?} left the WSR-88D network -- this campaign's seam is \
                 behaviour-neutral for WSR-88D and that is a pin, not a preference",
            );
        }
    }

    /// **The eight a terminal radar can draw**, as a literal list checked
    /// against the registry — the second spelling, so neither side can rot
    /// alone. Nine products are WSR-88D-only: five Level III objects a TDWR's
    /// generator does not publish, three dual-pol moments a single-pol radar
    /// does not measure, and the classification derived from two of them.
    #[test]
    fn a_terminal_radar_offers_the_eight_it_can_draw() {
        const TDWR_CAN_DRAW: [&str; 8] = ["ref", "vel", "sw", "srv", "eti", "posh", "mehs", "nrot"];

        let offered: Vec<&str> = RadarProduct::all()
            .iter()
            .filter(|p| p.available_for(RadarNetwork::Tdwr))
            .map(|p| p.code())
            .collect();

        assert_eq!(offered, TDWR_CAN_DRAW, "the terminal radar's list moved");

        // The nine that are not, split by the reason each is excluded, so a
        // row moving between reasons is visible rather than absorbed.
        let withheld: Vec<&str> = RadarProduct::all()
            .iter()
            .filter(|p| !p.available_for(RadarNetwork::Tdwr))
            .map(|p| p.code())
            .collect();
        assert_eq!(withheld.len(), 9, "{withheld:?}");
        assert_eq!(
            RadarProduct::all()
                .iter()
                .filter(|p| p.is_level3() && !p.available_for(RadarNetwork::Tdwr))
                .count(),
            5,
            "the five Level III objects are the half the L3 arm applies",
        );
    }
    use squallar_units::{HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, UserPreferences};
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
