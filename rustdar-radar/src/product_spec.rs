//! The radar-internal product registry: one struct literal per product, every
//! field explicit.
//!
//! [`spec`] is the single statement of a product's metadata. The match-backed
//! methods on [`RadarProduct`] — `code`, `name`, `sort_order`, `is_level3`,
//! `level3_products`, `level3_volume_pick`, `wire_code`, `from_wire_code`,
//! `moment_slot`, `reads_whole_volume`, `reads_env_heights`, `unit_label` —
//! are one-line delegates into it, so their signatures (and every caller)
//! stay put while the tables live in one place.
//!
//! **There is deliberately no `Default` impl and no `..` spread anywhere in
//! this file.** A registration that omits a field must not compile: adding an
//! eighteenth product forces its author to answer every column — wire code,
//! moment slot, whole-volume posture, environmental-heights posture, unit
//! domain — instead of inheriting a default nobody chose. That property is
//! the deliverable; keeping the literals fully explicit is what a review of
//! this file checks.
//!
//! The derived methods stay derived and stay in `types.rs`: `all`,
//! `level3_readers`, `level3_codes_for`, `get_moment`, `is_wire_moment`,
//! `tilt_independent_plan_view` compute over these tables rather than
//! restating them, and `format_value` deliberately keeps its per-product
//! string shapes (its own comment says why).
//!
//! The colour tables are deliberately **not** fields. A `const fn` cannot
//! read a `static` (E0013), and both tables allocate (`Vec`-backed), so they
//! live as `LazyLock` companion functions — `palette::legend_scale_static`
//! and `voxel::volume_lut_static` — indexed by `product as usize` under the
//! declaration-order law `all_lists_every_variant_in_declaration_order`
//! holds below.

use crate::level3::VolumePick;
use crate::types::{MomentSlot, RadarProduct};
use rustdar_units::Quantity;

/// Everything the crate states about one product, in one row.
///
/// Field-level rules that used to live on the collapsed matches:
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
    /// The AWIPS product IDs to fetch for this product. These key the
    /// `unidata-nexrad-level3` bucket (`TLX_N0S_2026_07_25_...`). `None` for
    /// Level II products — [`RadarProduct::level3_products`] documents the
    /// deliberate absences (SRV's five-object fetch is gone; the RPG vector
    /// still arrives via `N0S` on the round the app already makes).
    pub(crate) level3_codes: Option<&'static [&'static str]>,
    /// Which object of a paired volume this product's Level III rendition is.
    /// `None` for Level II products — meaningless there, and it says so
    /// rather than defaulting. **Every product naming a given code must
    /// answer the same pick** (objects are cached per code, shared by every
    /// reader); `every_shared_level3_code_agrees_on_its_volume_pick` in
    /// [`crate::level3`] holds that.
    pub(crate) level3_volume_pick: Option<VolumePick>,
    /// A stable identifier for this product on a wire. Deliberately not the
    /// enum's declaration order and not the serde representation: reordering
    /// or renaming the variants must not silently change what an
    /// already-encoded message means. Both message formats that cross the
    /// browser's worker boundary — [`crate::render_input`]'s payload and
    /// `rustdar_frontend::offload`'s job framing — read this one column, and
    /// the registration is exhaustive with every field explicit, so a new
    /// variant fails to compile until it is given a code.
    pub(crate) wire_code: u16,
    /// Which of a radial's moment fields this product reads. `None` for the
    /// five Level III products: no Level II moment stands behind them.
    pub(crate) moment_slot: Option<MomentSlot>,
    /// Whether this product reads every tilt carrying its moment, rather
    /// than the one sweep `crate::render::find_sweep` picks. `false` for the
    /// six wire moments (one sweep: the rasterizer touches this product's
    /// own moment on the sweep `find_sweep` chose and nothing else in the
    /// volume) and for the Level III products (their pixels come from the
    /// RPG's own object, not Level II tilts). See
    /// [`RadarProduct::reads_whole_volume`] for the SRV chunk-feed
    /// regression this column exists to prevent.
    pub(crate) reads_whole_volume: bool,
    /// Whether this product's picture is a function of the environmental
    /// 0 °C / −20 °C heights ([`crate::sounding`]'s per-site pair). Every
    /// other product must never carry the pair, or the byte identity of its
    /// payload would depend on an unrelated cache. See
    /// [`RadarProduct::reads_env_heights`].
    pub(crate) reads_env_heights: bool,
    /// The unit domain the product's values live in. `unit_label` derives
    /// from it: a `Unitless` label prints as itself, every other quantity
    /// takes the preferred unit's suffix.
    pub(crate) quantity: Quantity,
}

/// The registration for `p`: seventeen struct literals, every field written
/// out, no `Default`, no `..` — a new variant, or a new field, fails to
/// compile until every row answers it.
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
            // Storm-relative velocity is derived from velocity — every
            // velocity tilt lists, an upgrade over the four fixed Level III
            // tilts the product used to fetch. See `crate::srv`.
            moment_slot: Some(MomentSlot::Velocity),
            // The selected sweep is what rasterizes, but
            // `crate::velocity::volume_wind_profile` fits the dealias-seeding
            // profile from every velocity tilt of the volume — and the
            // profile is also where SRV's default Bunkers vector comes from
            // (`crate::srv`). A user's override does not shrink this:
            // dealias seeding still wants the profile, or render quality
            // would silently vary with whether a vector was typed in.
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
            // Level III: no Level II moment stands behind it, its pixels
            // come from the RPG's own object, and it reads no Level II tilt.
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
            // Level III: no Level II moment stands behind it, its pixels
            // come from the RPG's own object, and it reads no Level II tilt.
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
            // Interpolated echo tops integrate the whole reflectivity
            // volume; tying availability to the reflectivity moment lists it
            // alongside the reflectivity tilts (the rendered field is
            // tilt-independent).
            moment_slot: Some(MomentSlot::Reflectivity),
            // `volumetric::compute_echo_tops` integrates the whole
            // reflectivity volume. `VolumeCube::build` dedups same-elevation
            // cuts in encounter order, so the tilts have to arrive in scan
            // order as well as all arrive.
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
            // Level III: no Level II moment stands behind it, its pixels
            // come from the RPG's own object, and it reads no Level II tilt.
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
            // Derived from **two objects**, `DVL` over `EET` for the same
            // volume ([`crate::vild`]), so it names both — the only product
            // whose codes are inputs to a computation rather than tilts of
            // itself, and the only one that reuses codes another product
            // also fetches.
            level3_codes: Some(&["DVL", "EET"]),
            level3_volume_pick: Some(VolumePick::NEAREST),
            wire_code: 15,
            // Level III rather than on reflectivity: it used to be a local
            // quotient of two whole-volume integrals, and is now the RPG's
            // own `DVL` over its own `EET` ([`crate::vild`]) because the
            // local version was measured mute at the thresholds it is read
            // for (see [`crate::vil`]'s validation section). It left the
            // whole-volume set along with the integrals.
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
            // The hail pair integrates the whole reflectivity volume
            // (`crate::hail`); the environmental heights it also needs ride
            // the render parameters, not a moment.
            moment_slot: Some(MomentSlot::Reflectivity),
            // The SHI column integral reads every reflectivity tilt, over
            // the same local VIL machinery echo tops uses (`crate::hail`).
            reads_whole_volume: true,
            // The SHI-to-size mapping has no field at all without the pair:
            // the warning-threshold integral starts at the 0 °C height and
            // is fully weighted above −20 °C, so without them `crate::hail`
            // renders nothing rather than guessing.
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
            // The hail pair integrates the whole reflectivity volume
            // (`crate::hail`); the environmental heights it also needs ride
            // the render parameters, not a moment.
            moment_slot: Some(MomentSlot::Reflectivity),
            // The SHI column integral reads every reflectivity tilt, over
            // the same local VIL machinery echo tops uses (`crate::hail`).
            reads_whole_volume: true,
            // The SHI-to-size mapping has no field at all without the pair —
            // see `ProbabilityOfSevereHail`'s row.
            reads_env_heights: true,
            // The field computes in mm (`crate::hail`) and the render seam
            // converts to inches, the unit US hail sizes are reported in;
            // `Quantity::suffix` carries the Inches→"in" colour-bar rule.
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
            // The hybrid hydrometeor classification composites every
            // dual-pol tilt of the volume (`crate::hhc`); listing on
            // reflectivity puts the tilt-independent volume product
            // alongside the reflectivity tilts, the same convention as ETI
            // and VIL density. The render payload carries the rest of the
            // moments (`crate::render_input`'s extras).
            moment_slot: Some(MomentSlot::Reflectivity),
            // The hybrid classification composites every dual-pol tilt down
            // the hybrid scan, and reads every *moment* of them too
            // (`crate::hhc`).
            reads_whole_volume: true,
            // Picks `HsdaHeights::from_env_heights` over
            // `operational_defaults`, and the pair's 0 °C height is the
            // third rung of `crate::hca::resolve_melting_layer`, so every
            // class code downstream of the layer moves with it
            // (`crate::render::render_hhc_to_image`). Absent a sounding it
            // falls back to the adaptation defaults, exactly as the RPG runs
            // without environmental data. The RPG's own melting layer object
            // rides `RenderInput::with_melting_layer_product` rather than
            // this flag: it is per *volume*, not per site.
            reads_env_heights: true,
            quantity: Quantity::Unitless { label: "HHC" },
        },
        RadarProduct::PrecipitationRate => RadarProductSpec {
            code: "dpr",
            name: "Precipitation Rate",
            sort_order: 16,
            is_level3: true,
            level3_codes: Some(&["DPR"]),
            // `Latest` for the QPE family: it emits an end-of-volume
            // composite *plus* a partial intermediate per SAILS/MRLE scan
            // under the same volume start, so the nearest-to-start candidate
            // is an intermediate, and a loop paired that way would animate
            // partial accumulations. Everything else publishes once per
            // volume and takes `NEAREST`.
            level3_volume_pick: Some(VolumePick::Latest),
            wire_code: 13,
            // Level III: no Level II moment stands behind it, its pixels
            // come from the RPG's own object, and it reads no Level II tilt.
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
            // The selected sweep is what rasterizes, but
            // `crate::velocity::volume_wind_profile` fits the dealias-seeding
            // profile from every velocity tilt of the volume — the only wind
            // source since the NVW fetch left (`crate::nrot`).
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

    /// `palette::legend_scale_static` and `voxel::volume_lut_static` index
    /// their `LazyLock` tables by `product as usize`, which is sound only
    /// while `all()` lists every variant in declaration order — the law this
    /// holds. The count is a **literal**, not `all().len()`: a floor written
    /// in terms of the registry is satisfied by a shrunken registry.
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

    /// Every product's registration is complete and self-consistent: codes
    /// and names non-empty, wire codes unique, sort orders unique, and the
    /// Level III columns move together — in today's tables (checked arm by
    /// arm while transcribing) exactly the five Level III products carry
    /// fetch codes and a volume pick, and nothing else does.
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

    /// `from_wire_code` is the inverse of the registry's wire codes, and an
    /// unknown code is a clean `None` — the debug assertion the old
    /// hand-written inverse match carried, now held for every build.
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

    /// `unit_label` through the quantity column prints character for
    /// character what the pre-M4 match arms printed. The expected strings
    /// are **hand-pinned copies of the old arms**, not re-derived through
    /// `Quantity` — under default preferences for all seventeen products,
    /// then one non-default per preference-backed unit, including the
    /// Inches→"in" colour-bar rule (the default hail row) and its
    /// take-your-own-suffix complement.
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
