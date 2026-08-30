//! MRMS — the Multi-Radar/Multi-Sensor national mosaic, from the
//! `noaa-mrms-pds` open-data bucket.
//!
//! One product per fetch, one decoded grid held whole, painted through
//! `render::gridded` like any other gridded field. What is worth knowing before
//! touching any of it, all measured against the live bucket on 2026-08-21:
//!
//! | Fact | Value |
//! |---|---|
//! | Grid | template **3.0** (plain lat/lon), 7000 × 3500 = **24 500 000** points at 0.01° |
//! | Packing | DRT **5.41** (PNG), 16-bit, decimal scale 1, reference value −999.0 |
//! | Section 6 | `bitmap_indicator = 255` — **there is no bitmap** |
//! | Missing | in-band, and **per product** — see [`MrmsProduct::missing_codes`] |
//! | Files | `.grib2.gz`, ~1.3 MB gzipped, ~1.4 MB of GRIB2 |
//! | Cadence | ~2 min, **timestamps are not clock-aligned** (`000039`, `000242`, `000442`) |
//! | Retention | back to 2020-10-14 |
//!
//! **`CONUS_5KM/` is dead** — it stops at ~2021-02-24. It is still in the
//! bucket listing and reads like a cheaper CONUS; it is not, and
//! [`squallar_source::origins::DataSources::mrms_day_prefix`] never addresses
//! it.
//!
//! ## The two things that would go wrong
//!
//! **No bitmap means nothing produces `NaN` on its own**, and **which numbers
//! mean "missing" is a fact about the product, not about MRMS.** The composite
//! reserves −999 and −99; the precipitation rate reserves **−3** and uses
//! neither of the others. Both travel as ordinary `f32` unless
//! [`decode::to_reading`] stops them, and an unstopped code reaches the hover
//! tooltip, the value range and the blank notice. *Not* the picture, on today's
//! two colour bars — [`decode::to_reading`]'s own doc records what a tamper
//! check actually showed rather than what the failure looks like it should be,
//! and [`MrmsProduct::missing_codes`] records the counts the table was measured
//! from.
//!
//! **98 MB per grid.** `values` is 24.5 M `f32`. The decode streams grib's lazy
//! iterator straight into a pre-sized buffer rather than collecting an
//! intermediate, and the cache is bounded by [`GRID_CACHE_BYTES`] rather than by
//! an entry count — six panes at an entry cap would be 588 MB.

use std::sync::Arc;

use squallar_geo::GeoBounds;

use crate::render::gridded::ResidentGrid;

pub mod decode;
pub mod fetch;
pub mod fields;
/// The 3D stack. Nothing draws it -- see the module doc.
pub mod volume;

/// The MRMS products this layer offers.
///
/// Two, deliberately: the bucket carries well over a hundred, and each one that
/// ships owes a colour bar somebody stands behind (`fields.rs`). A third is a
/// row there and a variant here, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MrmsProduct {
    /// `MergedReflectivityQCComposite_00.50` — the column-maximum
    /// quality-controlled reflectivity mosaic. The layer's reason to exist.
    ReflectivityComposite,
    /// `PrecipRate_00.00` — the instantaneous surface precipitation rate.
    PrecipRate,
}

impl MrmsProduct {
    pub fn all() -> &'static [MrmsProduct] {
        &[MrmsProduct::ReflectivityComposite, MrmsProduct::PrecipRate]
    }

    /// The persisted spelling **and** the `FieldId`: `serialize_pane_state`
    /// writes this, `deserialize_pane_state` reads it, and
    /// `GriddedJob::encode` puts it on the wire. Never change one of them.
    pub fn as_str(self) -> &'static str {
        match self {
            MrmsProduct::ReflectivityComposite => "mrms_reflectivity",
            MrmsProduct::PrecipRate => "mrms_preciprate",
        }
    }

    /// The bucket's own directory name for this product — a *different* string
    /// from [`Self::as_str`] on purpose. That one is a persistence key this
    /// build owns; this one is NOAA's, and NOAA may rename a directory without
    /// asking us to migrate every saved config.
    pub fn prefix_name(self) -> &'static str {
        match self {
            MrmsProduct::ReflectivityComposite => "MergedReflectivityQCComposite_00.50",
            MrmsProduct::PrecipRate => "PrecipRate_00.00",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            MrmsProduct::ReflectivityComposite => "Composite Reflectivity",
            MrmsProduct::PrecipRate => "Precipitation Rate",
        }
    }

    /// Short form for a status line, where the layer's own name is already
    /// beside it.
    pub fn short_name(self) -> &'static str {
        match self {
            MrmsProduct::ReflectivityComposite => "CREF",
            MrmsProduct::PrecipRate => "Rate",
        }
    }

    pub fn unit_label(self) -> &'static str {
        match self {
            MrmsProduct::ReflectivityComposite => "dBZ",
            MrmsProduct::PrecipRate => "mm/h",
        }
    }

    /// **The in-band codes this product uses for "not a reading", exactly.**
    ///
    /// MRMS has no bitmap (section 6 `bitmap_indicator = 255`), so missing data
    /// arrives as ordinary values and there is no way to recognise one except by
    /// knowing which numbers the product reserves. **The set is per product and
    /// is not a detail** — it was measured on live granules, and the two
    /// products disagree:
    ///
    /// Counted over the two committed granules, 24 500 000 points each:
    ///
    /// | Product | Codes | Occurrences |
    /// |---|---|---|
    /// | Composite reflectivity | −999, −99 | 8 319 161 (34.0 %) and 14 999 005 (61.2 %) |
    /// | Precipitation rate | **−3** | 8 435 577 (34.4 %) |
    ///
    /// **The two sets are disjoint apart from nothing at all**, and every
    /// crossing is a trap:
    ///
    /// * the rate's no-coverage code is **−3**, not −99. Taking the composite's
    ///   set for the rate leaves a third of that mosaic reporting −3 mm/h as a
    ///   measurement, in the hover tooltip and in `value_range`. That is not a
    ///   thought experiment — it is what the first live fetch printed.
    /// * the rate carries **no −999 and no −99 whatsoever**, though −999 is the
    ///   packing's own reference value. Listing them anyway would be a claim
    ///   nothing checks.
    /// * and it does not generalise the other way either. A blanket "negative
    ///   means missing" rule would be right for a rain rate and **wrong for
    ///   reflectivity**, where the same fixture carries 8 045 genuine returns
    ///   below 0 dBZ — **347 of them at exactly −3.0**. That is why this is a
    ///   table of reserved numbers and not a sign test.
    ///
    /// A product added here owes a granule in `testdata/`:
    /// `decode::tests::every_fixture_really_contains_the_codes_it_is_here_to_prove`
    /// refuses a code nothing exercises, and
    /// `no_undeclared_reserved_code_hides_in_a_fixture` refuses a code that
    /// occurs at coverage-mask scale without being declared.
    pub fn missing_codes(self) -> &'static [f32] {
        match self {
            MrmsProduct::ReflectivityComposite => &[-999.0, -99.0],
            MrmsProduct::PrecipRate => &[-3.0],
        }
    }

    /// Every value MRMS is known to reserve, across **all** products here.
    ///
    /// Not what any one product uses — [`Self::missing_codes`] is that. This is
    /// the search space the fixtures are swept over, so a product that started
    /// using a neighbour's code is caught rather than silently reported as a
    /// third of a mosaic's worth of readings.
    pub fn known_reserved_codes() -> &'static [f32] {
        &[-999.0, -99.0, -3.0]
    }

    /// One reading, formatted for a hover tooltip. Empty for a missing point,
    /// which is what stops the tooltip claiming a value where there is none.
    pub fn format_value(self, value: f32) -> String {
        if !value.is_finite() {
            return String::new();
        }
        match self {
            MrmsProduct::ReflectivityComposite => format!("{}: {value:.1} dBZ", self.short_name()),
            MrmsProduct::PrecipRate => format!("{}: {value:.2} mm/h", self.short_name()),
        }
    }
}

impl std::str::FromStr for MrmsProduct {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        MrmsProduct::all()
            .iter()
            .copied()
            .find(|p| p.as_str() == s)
            .ok_or(())
    }
}

/// The longitude envelope MRMS declares to
/// [`crate::hrrr::fetch::check_domain_longitude`].
///
/// The CONUS mosaic measures -129.995..-60.005 — read off section 3 of a live
/// granule and re-derived by `decode::tests` from the committed fixture. This is
/// that with margin either side, and it is deliberately **narrower** than a
/// union with HRRR's: an envelope is the narrowest claim a source can stand
/// behind, not the widest anyone has measured.
pub const MRMS_DOMAIN_LON: std::ops::RangeInclusive<f64> = -130.0..=-60.0;

/// One CONUS mosaic's values, in bytes: 7000 × 3500 `f32` = **98 MB**.
///
/// The budget below is stated as a multiple of this rather than as a round
/// number of megabytes, so "one resident grid" stays one resident grid if the
/// product's grid ever changes shape.
pub const CONUS_GRID_BYTES: usize = 7000 * 3500 * std::mem::size_of::<f32>();

/// How many bytes of decoded MRMS grid may stay resident at once: **one grid on
/// wasm, two on mobile, four on desktop**.
///
/// **A byte budget, not an entry count.** At 98 MB apiece, the six-entry shape
/// the model cache uses would be 588 MB — more than the whole wasm32 address
/// space has to spare once a `px_coords` buffer and a texture are in it.
///
/// Spelled as a `cfg` cascade rather than resolved from `squallar-device-profile`
/// because that crate sits **above** this one in the crate graph
/// (`ARCHITECTURE.md` §1: device-profile is band 3, `squallar-overlays` band 2),
/// so the dependency cannot run back. `target_os` rather than device-profile's
/// own `mobile` cfg for the same reason — that cfg is emitted by *its* build
/// script and is invisible here. The rule is `is_mobile_target`'s, restated;
/// the model cache spells `MAX_PANES_DESKTOP` locally on the same reasoning.
///
/// Below the pane count a miss costs a **picture** rather than a refetch —
/// `prepare_job` answers `None` and the pane goes on drawing its last texture
/// with nothing that will re-ask — so the cache's pin on every pane's selected
/// product is what keeps a visible pane drawn regardless.
#[cfg(target_arch = "wasm32")]
pub const GRID_CACHE_BYTES: usize = CONUS_GRID_BYTES;
/// See the wasm arm.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub const GRID_CACHE_BYTES: usize = 2 * CONUS_GRID_BYTES;
/// See the wasm arm.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub const GRID_CACHE_BYTES: usize = 4 * CONUS_GRID_BYTES;

// **A build failure, not a test failure.** Every term here is a compile-time
// constant, so a runtime assertion over them is one that cannot fail on a build
// that got as far as running tests; the arm that would be wrong is one that a
// *different* target selects, and only the compiler ever sees that. The model
// cache's `MODEL_GRID_CACHE_ENTRIES >= 2` is spelled the same way.
//
// A budget under one grid settles the cache empty: the arrival evicts itself,
// `prepare_job` answers `None` for ever and every pane draws its last texture.
const _: () = assert!(GRID_CACHE_BYTES >= CONUS_GRID_BYTES);
// And a whole number of grids, which is what the doc above claims.
const _: () = assert!(GRID_CACHE_BYTES.is_multiple_of(CONUS_GRID_BYTES));
const _: () = assert!(CONUS_GRID_BYTES == 98_000_000);

/// **How many bytes of loop-frame granule may stage at once: one mosaic, on
/// every arm.**
///
/// Not a per-arm cascade, because the figure is not a guess about the device —
/// it is what the pipeline needs. A loop frame's storage is its **texture**,
/// held by the pane; the granule is a 98,000,000 B staging buffer a frame
/// passes through on its way to one. A described job takes its own refcount on
/// the raster, so the slot is free again the moment `prepare_job` has run, and
/// the handler's frame gate admits one fetch at a time so nothing else can ask
/// for the slot in the meantime.
///
/// The gate matters even more here than at GMGSI: one MRMS decode **peaks at
/// ~147 MB transient** (`decode`'s own arithmetic — grib's PNG stage holds the
/// whole 49 MB image buffer while the 98 MB values vector fills), so N
/// concurrent frame fetches would hold N x 147 MB inside the futures before
/// any cache saw a byte. Thirty unthrottled fetches — one slider-default hour
/// at the ~2-minute cadence — would be ~4.4 GB in flight.
pub const FRAME_STAGING_BYTES: usize = CONUS_GRID_BYTES;

// The pipeline advances one granule at a time, so a staging area under one
// grid settles empty and no frame is ever rasterized. Same reason the live
// cache carries the same floor, and the same reason it is a **build** failure.
const _: () = assert!(FRAME_STAGING_BYTES >= CONUS_GRID_BYTES);

/// **What one frame listing found**, carried back to `apply_frame_listing` as
/// its scope.
///
/// The product is captured at dispatch, not read back off the arriving pane:
/// the `PaneRef` a listing lands with is the union across panes and its config
/// is null by construction, so a listing taken for the composite would
/// otherwise be filed under whatever product the pane holds by then.
///
/// `keys` is the whole of what the listing bought — a stamp and the object
/// name carrying it — because MRMS **timestamps are not clock-aligned**
/// (`000039`, `000242`, `000442`), so a stamp cannot be rounded back into a
/// key and a frame fetched later would otherwise re-list its day.
///
/// **Public so a test can drive the real handler**, for the reason
/// `gmgsi::GmgsiListing` states: a double cannot catch a layer that files its
/// frames wrongly.
pub struct MrmsListing {
    pub product: MrmsProduct,
    pub range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    pub keys: Vec<(chrono::NaiveDateTime, String)>,
    /// Whether the days listed were every day the range touches, all answered.
    pub complete: bool,
}

/// **One loop frame's granule**, as its fetch hands it back.
///
/// `product` and `valid` come off the dispatch rather than off the decoded
/// granule for the reason [`MrmsListing`] states, and `grid: None` is a fetch
/// that failed: the frame is left without a picture rather than being given
/// another stamp's.
pub struct MrmsFrameFetch {
    pub product: MrmsProduct,
    pub valid: chrono::NaiveDateTime,
    pub grid: Option<MrmsGrid>,
}

/// One decoded MRMS mosaic.
///
/// The grid itself is behind an `Arc` because that is what `prepare_job` hands
/// to the raster: describing a job must cost a refcount, never a 98 MB copy.
#[derive(Debug, Clone, PartialEq)]
pub struct MrmsGrid {
    pub product: MrmsProduct,
    /// Field identity, shape, coordinates and values — the source-agnostic
    /// carry `rasterize_gridded` reads.
    pub grid: Arc<ResidentGrid>,
    pub bounds: GeoBounds,
    /// The granule's own valid time, from GRIB2 section 1.
    pub valid: chrono::NaiveDateTime,
    /// How many grid points map to a non-transparent colour, computed once on
    /// the fetch path. See [`Self::blank_notice`].
    pub visible_points: usize,
    pub value_range: Option<(f32, f32)>,
}

impl MrmsGrid {
    /// Bytes of decoded values this grid holds — what [`GRID_CACHE_BYTES`]
    /// counts. Coordinates are not in it: a
    /// [`Regular`](crate::hrrr::GridCoords::Regular) grid is seven scalars
    /// whether it covers 1.9 M points or 24.5 M, which is the whole reason MRMS
    /// never calls `latlons()`.
    pub fn resident_bytes(&self) -> usize {
        self.grid.values.len() * std::mem::size_of::<f32>()
    }

    /// Explain why this mosaic will render as nothing, when it will.
    ///
    /// A quiet night is the *ordinary* case for a national mosaic, so this is
    /// not a warning: it distinguishes "decoded fine, nothing above 5 dBZ" from
    /// "the fetch never happened", which look identical on screen.
    pub fn blank_notice(&self) -> Option<String> {
        if self.visible_points > 0 {
            return None;
        }
        let name = self.product.short_name();
        Some(match self.value_range {
            None => format!("{name}: no coverage anywhere in the mosaic"),
            Some((lo, hi)) => format!(
                "{name}: nothing above the lowest colour band (range {lo:.1} to {hi:.1} {})",
                self.product.unit_label(),
            ),
        })
    }
}

pub struct MrmsFetchResult(pub Result<MrmsGrid, crate::fetch_policy::FetchError>);

/// **[`Whole`], not assembled**: one product, one granule, one request that
/// either answered or did not.
///
/// [`Whole`]: crate::fetch_policy::Whole
impl crate::fetch_policy::FetchRound for MrmsFetchResult {
    type Shape = crate::fetch_policy::Whole;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_product_round_trips_through_its_config_key() {
        for &p in MrmsProduct::all() {
            let parsed: MrmsProduct = p.as_str().parse().unwrap();
            assert_eq!(parsed, p, "key {:?} did not round-trip", p.as_str());
        }
        assert!("not_a_product".parse::<MrmsProduct>().is_err());
    }

    /// The persistence key and the bucket's directory name are different
    /// strings, and must stay different: one is ours to keep stable, the other
    /// is NOAA's to rename.
    #[test]
    fn a_persisted_key_is_never_the_buckets_directory_name() {
        for &p in MrmsProduct::all() {
            assert_ne!(p.as_str(), p.prefix_name());
            assert!(!p.as_str().contains('.'), "{:?}", p.as_str());
        }
    }

    #[test]
    fn a_missing_reading_formats_as_nothing_at_all() {
        for &p in MrmsProduct::all() {
            assert_eq!(p.format_value(f32::NAN), "");
            assert_eq!(p.format_value(f32::INFINITY), "");
            assert!(!p.format_value(30.0).is_empty());
        }
    }

    #[test]
    fn the_declared_envelope_brackets_the_conus_mosaic() {
        // The measured section-3 extent, with the margin this const adds.
        assert!(MRMS_DOMAIN_LON.contains(&-129.995));
        assert!(MRMS_DOMAIN_LON.contains(&-60.005));
        // Narrower than HRRR's on both ends: an envelope is a claim about one
        // source, not a union.
        assert!(*MRMS_DOMAIN_LON.start() > *crate::hrrr::fetch::HRRR_DOMAIN_LON.start());
        assert!(*MRMS_DOMAIN_LON.end() < *crate::hrrr::fetch::HRRR_DOMAIN_LON.end());
    }
}
