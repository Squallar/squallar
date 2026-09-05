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
//! **49 MB per grid.** `values` is 24.5 M `u16` codes, the width section 5
//! actually declares; it was 24.5 M `f32` until the store followed the source.
//! The decode streams grib's lazy iterator straight into a pre-sized buffer
//! rather than collecting an intermediate, and the cache is bounded by
//! [`GRID_CACHE_BYTES`] rather than by an entry count — six panes at an entry
//! cap would be 294 MB.

use std::sync::Arc;

use squallar_geo::GeoBounds;

use crate::render::gridded::ResidentGrid;

pub mod decode;
pub mod fetch;
pub mod fields;
pub mod staging;
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

// **The key space's tripwire.** [`GRID_CACHE_BYTES`]'s `const _` prices one
// grid per entry of [`MrmsProduct::all`], and the cache is a `HashMap` keyed by
// this enum — so an `all()` that under-counts the enum is a budget under the
// real key space with every assert green. `all()` is hand-written. `index` is
// an exhaustive `match` with no wildcard arm, which makes adding a variant a
// **compile error** rather than a silent under-count, and this block checks
// that `all()` is dense and ordered under it: a duplicate, a reordering, or an
// entry dropped from the list all fail the build here.
//
// **What it does not prove is completeness.** "Every variant is listed" needs
// an enumeration of the variants, which is the very thing `all()` is, so the
// check is circular without a derive (`strum::EnumCount`) or the unstable
// `mem::variant_count` — and a band-2 crate standing on exactly
// {squallar-geo, squallar-source, squallar-units} does not buy a dependency for
// it. This is a tripwire at the point of change, not a proof; if one is ever
// walked past, the byte ceiling is what still bounds the heap.
const _: () = {
    let all = MrmsProduct::all();
    let mut i = 0;
    while i < all.len() {
        assert!(all[i].index() == i);
        i += 1;
    }
};

impl MrmsProduct {
    pub const fn all() -> &'static [MrmsProduct] {
        &[MrmsProduct::ReflectivityComposite, MrmsProduct::PrecipRate]
    }

    /// A dense index over the variants, by a `match` with **no wildcard arm**.
    ///
    /// Its only caller is the `const _` above, and that is the point: a variant
    /// added to [`MrmsProduct`] fails to compile *here*, one line from
    /// [`Self::all`], which is the list the grid cache's key space is counted
    /// from.
    const fn index(self) -> usize {
        match self {
            MrmsProduct::ReflectivityComposite => 0,
            MrmsProduct::PrecipRate => 1,
        }
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

/// One CONUS mosaic's values, in bytes: 7000 × 3500 `u16` = **49 MB**.
///
/// The budget below is stated as a multiple of this rather than as a round
/// number of megabytes, so "one resident grid" stays one resident grid if the
/// product's grid ever changes shape.
///
/// **`u16`, because that is the width MRMS publishes.** Section 5 declares a
/// 16-bit code and the value is `(ref_val + code * 2^exp) * 10^-dec`, so the
/// `f32` this used to count was a widening of the source's own width and the
/// mosaic was 98,000,000 B to hold 49,000,000 B of information. The store is
/// [`GridValues::Scaled`](crate::render::gridded::GridValues::Scaled) now and
/// this figure follows it.
pub const CONUS_GRID_BYTES: usize = 7000 * 3500 * crate::render::gridded::ScaledU16::ELEMENT_BYTES;

/// How many bytes of decoded MRMS grid may stay resident at once: **two grids
/// on every arm** — never fewer than the layer has products, which the
/// `const _` below holds as a build failure, and no more than it has products,
/// which is a decision recorded below rather than a floor.
///
/// **A byte budget, not an entry count.** At 49 MB apiece, the six-entry shape
/// the model cache uses would be 294 MB — and at the `f32` width this layer
/// shipped with, 588 MB, more than the whole wasm32 address space has to spare
/// once a `px_coords` buffer and a texture are in it.
///
/// **The narrowing was BANKED, and the desktop arm is now read down to what
/// the key space can use.** Stated as a multiple of [`CONUS_GRID_BYTES`], this
/// budget halved with the `u16` store — desktop went 392,000,000 B to
/// 196,000,000 B and stayed *four* grids rather than becoming eight, on the
/// argument that capacity a user has been given cannot be taken away without
/// removing something they can see. Two of those four grids were never
/// capacity: the cache keys by product and there are two, so the desktop arm
/// priced 98 MB of headroom no insert could reach and nobody could see. It is
/// two grids now, 98,000,000 B, and nothing resident changes — the cache's
/// peak was two grids before and is two grids after. A third product added to
/// `all()` moves this arm through the `const _` below, not by finding room in
/// it.
///
/// Spelled as a `cfg` cascade rather than resolved from `squallar-device-profile`
/// because that crate sits **above** this one in the crate graph
/// (`ARCHITECTURE.md` §1: device-profile is band 3, `squallar-overlays` band 2),
/// so the dependency cannot run back. `target_os` rather than device-profile's
/// own `mobile` cfg for the same reason — that cfg is emitted by *its* build
/// script and is invisible here. The rule is `is_mobile_target`'s, restated;
/// the model cache spells `MAX_PANES_DESKTOP` locally on the same reasoning.
///
/// **Never below the key space.** The cache is keyed by product and holds one
/// grid per *distinct* product some pane has selected; the pin set
/// `MrmsGridCache::insert` is handed is the union of every pane's selection,
/// so with the budget at the key space every entry can be pinned and no pinned
/// entry is ever a victim. A budget below the key space does not make the
/// cache hold less: `insert` runs out of unpinned victims and takes its
/// `break` arm, holding the key space anyway while this constant says
/// otherwise. That is what the wasm arm did at one grid against two products —
/// two panes held 98 MB under a figure that said 49. Stating the key space
/// moved nothing at that peak. Whether a pane that has switched products keeps
/// the one it left resident is not this ceiling's decision but
/// [`GRID_HISTORY_ENTRIES`]'s. The `const _` below is what keeps every arm
/// here.
#[cfg(target_arch = "wasm32")]
pub const GRID_CACHE_BYTES: usize = 2 * CONUS_GRID_BYTES;
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
pub const GRID_CACHE_BYTES: usize = 2 * CONUS_GRID_BYTES;

// **A build failure, not a test failure.** Every term here is a compile-time
// constant, so a runtime assertion over them is one that cannot fail on a build
// that got as far as running tests; the arm that would be wrong is one that a
// *different* target selects, and only the compiler ever sees that. The model
// cache's `const _` over `WASM_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES
// >= MAX_PANES_DESKTOP` is spelled the same way.
//
// **The key space.** One grid per product any pane can select, because that is
// what the cache holds when every product is on some pane and the pin set —
// the union of every pane's selection — covers every entry. Below this figure
// `MrmsGridCache::insert` does not evict; it runs out of unpinned victims and
// takes its `break` arm, so the cache overruns the budget silently and the
// constant under-reports what the heap is carrying. The wasm arm sat one grid
// under this for as long as the layer had two products.
const _: () = assert!(GRID_CACHE_BYTES >= MrmsProduct::all().len() * CONUS_GRID_BYTES);
// At least one grid — implied by the key space, kept as the plainer statement.
// (Not "or the cache settles empty": the arrival is never its own victim, so a
// budget under one grid overruns exactly as one under the key space does.)
const _: () = assert!(GRID_CACHE_BYTES >= CONUS_GRID_BYTES);
// And a whole number of grids, which is what the doc above claims.
const _: () = assert!(GRID_CACHE_BYTES.is_multiple_of(CONUS_GRID_BYTES));
// **The width gate, which until this commit gated nothing.** It claimed a store
// that went back to `f32` "fails the BUILD here"; it did not, and could not.
// The constant was `7000 * 3500 * size_of::<u16>()` — a literal width, not the
// store's — so its value did not depend on the store at all. Measured on this
// tree with `ScaledCode` narrowed to a byte and this spelling left in place:
// every other width pin in the crate fired and THIS ONE STAYED GREEN, holding
// 49,000,000 for a 24,500,000 B grid. That is the same defect, in the same
// shape, as `GLOBAL_GRID_BYTES` pricing four bytes a point after GMGSI narrowed
// to one.
//
// Read through `ScaledU16::ELEMENT_BYTES` the gate is real, and the two terms
// are pinned APART so a failure names which one moved: the store's element, or
// the product of it and the shape.
const _: () = assert!(crate::render::gridded::ScaledU16::ELEMENT_BYTES == 2);
const _: () = assert!(CONUS_GRID_BYTES == 49_000_000);

/// How many grids a single pane that cycles selections keeps warm — the
/// **unpinned history** the cache retains beyond the pinned set: **none on
/// wasm, one on mobile, every product but the one showing on desktop**.
///
/// [`GRID_CACHE_BYTES`] is the ceiling and it is held at the key space, so on
/// its own it lets one pane that has cycled through every product keep every
/// product resident — 98 MB of MRMS behind a pane that is showing 49. Those
/// grids are the second tier of the memory model: switching back is faster
/// with them, nothing is lost without them, and how many to keep is a
/// governor's decision to lower and restore, not a constant's. This is that
/// governor's lever — `MrmsGridCache::set_history` — at its opening position.
/// Retention is `max(pinned set, history)`: the pin set, the union of every
/// visible pane's selection, is never evicted whatever this says; beyond it the
/// least-recently-used unpinned grids go until at most this many remain.
///
/// Each arm is what that arm did before the byte budgets were raised to the
/// key space, so the arms move nothing today: wasm held one grid, so a pane
/// that switched refetched on the way back (history 0); mobile held two, one
/// showing and one warm (1); desktop held four against two products,
/// everything warm (`all().len() - 1`). Named per arm, like the model cache's
/// byte budgets, so the `const _` below holds on every build and a host test
/// can state the wasm arm by name.
pub const WASM_GRID_HISTORY_ENTRIES: usize = 0;
/// See [`WASM_GRID_HISTORY_ENTRIES`].
pub const MOBILE_GRID_HISTORY_ENTRIES: usize = 1;
/// See [`WASM_GRID_HISTORY_ENTRIES`]. `all().len() - 1` is the value at which
/// the history never binds: at least one product is always pinned, so that is
/// the most unpinned grids the cache can ever hold.
pub const DESKTOP_GRID_HISTORY_ENTRIES: usize = MrmsProduct::all().len() - 1;

/// The arm this build selects — see [`WASM_GRID_HISTORY_ENTRIES`]. The same
/// `cfg` cascade as [`GRID_CACHE_BYTES`], for the same reason.
#[cfg(target_arch = "wasm32")]
pub const GRID_HISTORY_ENTRIES: usize = WASM_GRID_HISTORY_ENTRIES;
/// See the wasm arm.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub const GRID_HISTORY_ENTRIES: usize = MOBILE_GRID_HISTORY_ENTRIES;
/// See the wasm arm.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub const GRID_HISTORY_ENTRIES: usize = DESKTOP_GRID_HISTORY_ENTRIES;

// **Below the key space, on every arm.** `pinned_products` never answers an
// empty set (it falls back to the default product), so the most unpinned grids
// the cache can hold is `all().len() - 1`; a history at or above the key space
// is a lever connected to nothing, and it would also price the pinned set plus
// the history above the byte ceiling. Over the named arms rather than the
// selected one, so every build checks all three.
const _: () = {
    assert!(WASM_GRID_HISTORY_ENTRIES < MrmsProduct::all().len());
    assert!(MOBILE_GRID_HISTORY_ENTRIES < MrmsProduct::all().len());
    assert!(DESKTOP_GRID_HISTORY_ENTRIES < MrmsProduct::all().len());
};

/// **How many bytes of loop-frame granule may stage at once: one mosaic, on
/// every arm.**
///
/// Not a per-arm cascade, because the figure is not a guess about the device —
/// it is what the pipeline needs. A loop frame's storage is its **texture**,
/// held by the pane; the granule is a 49,000,000 B staging buffer a frame
/// passes through on its way to one. A described job takes its own refcount on
/// the raster, so the slot is free again the moment `prepare_job` has run, and
/// the handler's frame gate admits one fetch at a time so nothing else can ask
/// for the slot in the meantime.
///
/// The gate matters even more here than at GMGSI: **the slot is one grid, and
/// only one decode can be handed it.** [`staging::StagingPool::take`] answers
/// the retained buffer to whoever asks first and every other caller allocates
/// its own 49 MB, so N concurrent frame fetches hold N x 49 MB inside the
/// futures before any cache sees a byte. Thirty unthrottled fetches — one
/// slider-default hour at the ~2-minute cadence — would be ~1.5 GB in flight.
/// **And since 2026-08-31 it is one *allocation* as well as one slot.** The
/// figure named a slot and nothing enforced the rest of what it says: each
/// granule built a fresh 98 MB vector (`f32` then) and freed the last one, so a
/// playing loop
/// put ~147 MB of large-block churn on a heap that only grows. [`staging`] holds
/// the buffer between granules and refills it, which is what makes the code
/// match this constant's claim; the freeze it fixes and why a wider budget
/// would not have is written up there.
///
/// The other 49 MB of that churn was `grib`'s whole-image PNG buffer, and
/// since the row walk landed (`decode::decode_png_into`) there is no
/// per-granule block left above 1 MiB at all: **measured, one warm decode peaks
/// at 0.43 MB**.
pub const FRAME_STAGING_BYTES: usize = CONUS_GRID_BYTES;

// The pipeline advances one granule at a time, so a staging area under one
// grid settles empty and no frame is ever rasterized. Same reason the live
// cache carries the same floor, and the same reason it is a **build** failure.
const _: () = assert!(FRAME_STAGING_BYTES >= CONUS_GRID_BYTES);
// The retained buffer is sized in points off this budget, so a budget that is
// not a whole number of STORED CODES would silently round the staging grid down
// and make every mosaic decode miss the pool. The reason said `f32` while the
// check said `u16`; both now name the store's own element, which is the only
// width that makes the sentence true.
const _: () =
    assert!(FRAME_STAGING_BYTES.is_multiple_of(crate::render::gridded::ScaledU16::ELEMENT_BYTES));

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
/// to the raster: describing a job must cost a refcount, never a 49 MB copy.
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
        self.grid.values.resident_bytes()
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
