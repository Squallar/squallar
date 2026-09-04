//! MRMS in three dimensions — `MergedReflectivityQC`, **33 CONUS grids** stacked
//! into one field.
//!
//! **Nothing here draws.** There is no [`MrmsProduct`](super::MrmsProduct)
//! variant, no `fields.rs` row, no colour bar and no handler: a stacked national
//! volume is orders of magnitude past every texture budget in the build (see
//! [`CONUS_STACK_BYTES`]), and deciding what to do about that is a separate
//! piece of work that needs this one's measurements to start. What this module
//! is, exactly: list, fetch, decode, congruence-check, stack.
//!
//! ## The level roster, verified against the bucket rather than assumed
//!
//! `MergedReflectivityQC` is published as one CONUS directory **per height**,
//! and the roster is not uniform. Read off `noaa-mrms-pds` on 2026-08-30 with a
//! delimited listing of `CONUS/` (`IsTruncated=false`, so the page is the whole
//! set), the bucket carries exactly **33** `CONUS/MergedReflectivityQC_*/`
//! directories, in three spacings:
//!
//! | Band | Heights | Step | Count |
//! |---|---|---|---:|
//! | low | 0.50 – 3.00 km | 0.25 km | 11 |
//! | middle | 3.50 – 9.00 km | 0.50 km | 12 |
//! | upper | 10.00 – 19.00 km | 1.00 km | 10 |
//!
//! The **middle band is a third spacing**, not part of a smooth widening from
//! 0.25 km to 1 km: heights step by exactly 0.5 km for 12 of the 33 levels.
//! [`LEVELS_KM_MSL`] is that table, and it is checked two ways — against the
//! directory names by `level_prefix_name`, and against **each granule's own
//! section 4 first fixed surface**, which is what stops a renamed or reordered
//! directory being stacked at the wrong height.
//!
//! ## One stamp addresses all 33
//!
//! Every level publishes under the *same* non-clock-aligned timestamp
//! (`085838`, `090042`, `090240` observed across levels 0.50 and 19.00 on
//! 2026-08-30), so a whole timestep is **1 listing per level to find a stamp
//! every level has, then 33 constructed keys** — the keys themselves are a pure
//! function of the stamp, exactly as
//! [`DataSources::mrms_key`](squallar_source::origins::DataSources::mrms_key)
//! states. They do not land simultaneously: at the moment of one listing the
//! 19 km directory was one granule behind the 0.5 km one, which is why
//! [`latest_complete_stamp`] intersects the levels rather than taking the
//! newest stamp of any one of them.
//!
//! ## What a timestep costs, measured
//!
//! **Sampled across the retention, not across a week.** An earlier draft of this
//! module measured seven timesteps inside one late-August week and wrote design
//! conclusions off them. The bucket retains at least 20 months, so that was a
//! choice rather than a constraint, and it was the wrong one: every band below
//! is wider than that week suggested, some by more than an order of magnitude.
//! The caveat was even written down — "not a year… a winter stratiform event is
//! unmeasured here" — and the conclusion was stated anyway. A caveat that names
//! a direction and is then ignored is worse than no caveat.
//!
//! **24 blind draws**, uniform over `2025-09-01` to `2026-08-29` in both date and
//! time of day, seed `20260830`, taken with
//! `the_live_stack_decodes_and_reports_its_own_cost` (`#[ignore]`d because it is
//! network) on a `--release` build, one desktop, one home connection.
//! **Every share has one denominator: 33 × 24 500 000 = 808 500 000 cells of one
//! timestep**, and the ranges below are min/max over the 24 — never a mean,
//! because the extremes are what a residency scheme has to survive.
//!
//! | Figure | Range over 24 draws | Spread |
//! |---|---|---:|
//! | Download, gzipped, all 33 levels | **13.41 – 37.95 MB** | 2.8× |
//! | Fetch + gunzip + decode + stack | **9.7 – 11.1 s** | 1.1× |
//! | `readings` (a value at all) | **1.143 – 4.135 %** | 3.6× |
//! | ≥ 5 dBZ | **1.068 – 3.854 %** | 3.6× |
//! | ≥ 20 dBZ | **0.0715 – 1.2095 %** | 16.9× |
//! | ≥ 40 dBZ | **0.0000 – 0.0467 %** | unbounded — one draw held none |
//!
//! An independent 20-draw sample over the same retention, run by a reviewer with
//! a different seed, found 14.9–44.7 MB, readings 1.291–6.049 %, ≥ 5 dBZ
//! 0.714–4.600 % and ≥ 40 dBZ 0.0000134–0.0504 %. **Two seeds, two samplers,
//! overlapping ranges**: taken together the bands to design against are
//! **13–45 MB**, **readings 1.1–6.0 %**, **≥ 5 dBZ 0.7–4.6 %** — a **6.4×**
//! spread on the number that matters — and **≥ 40 dBZ 0 – 0.05 %**.
//!
//! ### The mechanism, which one August week cannot show
//!
//! Six of the 24 draws, chosen to show the shape rather than to flatter it:
//!
//! | Stamp (UTC) | Download | readings | ≥ 5 dBZ | ≥ 20 dBZ | ≥ 40 dBZ | ≥5 / readings | ≥20 / ≥5 |
//! |---|---:|---:|---:|---:|---:|---:|---:|
//! | 2025-11-28 06:54:35 | 22.78 MB | 2.468 % | 1.126 % | 0.071 % | 0.0003 % | 46 % | 6 % |
//! | 2025-12-20 10:42:37 | 18.88 MB | 2.120 % | 1.244 % | 0.090 % | **0.0000 %** | 59 % | 7 % |
//! | 2026-01-02 07:18:36 | 25.83 MB | 2.830 % | 1.659 % | 0.100 % | 0.0004 % | 59 % | 6 % |
//! | 2026-05-23 21:26:41 | **37.95 MB** | **4.135 %** | **3.854 %** | **1.210 %** | 0.0467 % | 93 % | 31 % |
//! | 2026-07-02 12:24:38 | 15.31 MB | 1.332 % | 1.261 % | 0.406 % | 0.0268 % | 95 % | 32 % |
//! | 2025-09-12 07:46:37 | **13.41 MB** | **1.143 %** | **1.068 %** | 0.321 % | 0.0158 % | 93 % | 30 % |
//!
//! **Cold-season mosaics are broad and weak; warm-season ones are narrow and
//! strong**, and the two last columns are where that is visible. In winter only
//! 46–59 % of readings clear 5 dBZ and only 6–7 % of those clear 20; in summer
//! 93–95 % clear 5 and 30–32 % clear 20. So a winter system maximises *area*
//! while minimising *cores* — and **sparse residency is priced on area**, which
//! is exactly why the summer week understated it. The single densest draw is a
//! late-May convective afternoon that is both broad *and* strong, and it sets
//! every ceiling in the table above.
//!
//! ### What that means for a residency scheme, stated as bands not as a number
//!
//! * **Size for ~5 % of cells carrying a paintable return, not 3 %.** The
//!   measured ceiling is 3.854 % here and 4.600 % in the reviewer's sample; 3 %
//!   is exceeded by 1 of these 24 draws and by 5 of the reviewer's 20.
//! * **A core-following scheme cannot have a fixed budget.** The ≥ 40 dBZ
//!   fraction spans zero to 0.0467 %: on 2025-12-20 the entire CONUS column held
//!   **no cell at all** above 40 dBZ. Any design keyed on cores must degrade to
//!   "there are none" as an ordinary case rather than as an error.
//! * **Download is 13–45 MB per timestep against a ~120 s cadence.** Continuous
//!   ingest is 0.1–0.4 MB/s sustained.
//! * Still **97 to 99 % of the stack is not a reading at all** — the sparsity
//!   the whole track rests on is real. It is *how much* of the remainder to
//!   budget for that the August week got wrong, not whether the field is sparse.
//!
//! ### Occupancy is not uniform in the vertical
//!
//! On the 2026-08-29 20:56:38Z stack the densest levels are 5.5–6.0 km (5.17 %
//! of their own level ≥ 5 dBZ) and the sparsest are the two ends — 0.31 % at
//! 0.50 km, 0.03 % at 19 km. Per-level bytes follow the same curve and peak at
//! 5.5 km, so the top and bottom of the roster are cheap in both dimensions.
//! **Why the bottom level is the emptier end is not something these counts
//! prove**, and two ordinary explanations fit: 0.5 km MSL is below the ground
//! over much of the west, and the beam is above it at any useful range
//! elsewhere. Recorded as an observation, not a mechanism.
//!
//! ### The 2D composite, for scale and not as a comparison
//!
//! The committed 2D composite fixture is **4.8 % readings**: its two reserved
//! codes cover 95.2 % of its 24 500 000 cells, of which `-999` — the
//! no-coverage mask — is **33.96 % of all cells** and 35.68 % of the reserved
//! ones. That is a different granule at a different hour and is **not** a
//! like-for-like comparison with any row above; it is here only so that "the
//! stack is sparser than the column maximum" has a number beside it.
//!
//! ## Wall clock, measured
//!
//! **Two figures, different shapes, never added**, over the same 24 draws:
//!
//! * finding a timestep all 33 levels have: **0.33–0.76 s**, 33 bounded listings
//!   of ~1 KB;
//! * fetch + gunzip + GRIB2 decode + stack: **9.7–11.1 s** at
//!   [`STACK_FETCH_CONCURRENCY`], `--release`.
//!
//! A debug build is several times slower; a wall clock taken there would
//! describe a profile nothing ships.

use chrono::NaiveDateTime;
use futures::stream::StreamExt;
use squallar_geo::GeoBounds;
use squallar_source::origins::DataSources;

use super::decode::RawGrid;
use crate::fetch_policy::{FetchError, NotFound};
use crate::hrrr::GridCoords;

/// How many height levels `MergedReflectivityQC` publishes.
pub const LEVEL_COUNT: usize = 33;

/// Every published height, **km above mean sea level**, ascending.
///
/// MSL and not above-ground: every granule states surface type
/// [`SURFACE_TYPE_ALTITUDE_MSL`] in its section 4, which is code table 4.5's
/// "specific altitude above mean sea level" and *not* 103, "specified height
/// level above ground". The distinction is the whole difference between a stack
/// that sits on the terrain and one that sits on the geoid, and it is checked
/// against the granule rather than taken from the directory name — see
/// [`check_granule_is_this_level`].
pub const LEVELS_KM_MSL: [f64; LEVEL_COUNT] = [
    0.50, 0.75, 1.00, 1.25, 1.50, 1.75, 2.00, 2.25, 2.50, 2.75, 3.00, // 0.25 km
    3.50, 4.00, 4.50, 5.00, 5.50, 6.00, 6.50, 7.00, 7.50, 8.00, 8.50, 9.00, // 0.50 km
    10.00, 11.00, 12.00, 13.00, 14.00, 15.00, 16.00, 17.00, 18.00, 19.00, // 1.00 km
];

/// Section 4's `(parameter category, parameter number)` for
/// `MergedReflectivityQC`.
///
/// **This, not the height, is what tells a level from the 2D composite.** Read
/// off the committed granules: the 3D levels declare `(9, 0)` and the 2D
/// `MergedReflectivityQCComposite_00.50` declares `(10, 0)` — and the two are
/// otherwise indistinguishable to this decoder. Both state first fixed surface
/// `(102, 500 m)`, both are 7000 × 3500 at 0.01°, both use the same packing and
/// the same reserved codes. A review substituted the composite granule into the
/// 0.50 km level's slot and **every offline test passed**, because the check
/// here only proved the *height* and the height is genuinely identical.
///
/// The number is `0` for both, so it discriminates nothing on its own; it is
/// carried and asserted anyway, because a product that changed its parameter
/// *number* while keeping its category is exactly the substitution this pair
/// exists to refuse.
pub const PARAMETER: (u8, u8) = (9, 0);

/// The 2D composite's `(category, number)`, which is **not** a level's.
///
/// Named rather than left implicit so the error message can say what the
/// granule looks like it actually is, and so
/// `tests::the_level_check_refuses_the_two_dimensional_composite` reads as the
/// statement it is.
pub const COMPOSITE_PARAMETER: (u8, u8) = (10, 0);

/// Code table 4.5's "specific altitude above mean sea level".
pub const SURFACE_TYPE_ALTITUDE_MSL: u8 = 102;

/// How far a granule's own declared height may sit from [`LEVELS_KM_MSL`]
/// before the level is refused, in metres.
///
/// The scaled value is an integer number of metres on this product, so any
/// disagreement at all is a renamed or re-levelled directory rather than
/// rounding. One metre is slack for a scale-factor change that still means the
/// same height.
const LEVEL_TOLERANCE_M: f64 = 1.0;

/// **The reserved codes `MergedReflectivityQC` uses**, which are the composite's
/// and not the rate's.
///
/// Declared here rather than reached for through
/// [`MrmsProduct::missing_codes`](super::MrmsProduct::missing_codes) because
/// this product is not an `MrmsProduct` — and because that doc's whole point is
/// that a code set is a fact about one product, measured, never inherited from a
/// neighbour.
///
/// **What actually holds this set, and how far it reaches.** Offline,
/// `tests::the_fixtures_carry_the_codes_this_product_declares` runs on the
/// default `cargo test` row over the two committed granules: it fails if either
/// declared code is absent — a code nothing exercises is a code nothing tests —
/// and fails if the rate's `-3`, the one
/// [`known_reserved_codes`](super::MrmsProduct::known_reserved_codes) value this
/// product does *not* declare, occurs at coverage-mask scale. Two granules is
/// two of 33 levels, so the same sweep runs across a whole live stack in
/// `tests::the_live_stack_decodes_and_reports_its_own_cost`, which is
/// **`#[ignore]`d** — `cargo test -p squallar-overlays --release -- --ignored
/// the_live_stack`.
///
/// **The `-3` count is seasonal, so it is quoted as a range and not as one
/// number.** Over the 24 draws above it ran from **2 430** (2026-08-13) to
/// **267 092** (2025-11-28) of 808 500 000 cells — a 110x spread, tracking the
/// cold-season weak returns that table describes. The conclusion is unchanged
/// and the margin is large: the worst case is **0.033 %** against a 1 % bar, so
/// these are genuine -3.0 dBZ returns and not a coverage mask. An earlier draft
/// quoted 5 231 from a single August stamp as though it were the figure.
pub const MISSING_CODES: [f32; 2] = [-999.0, -99.0];

/// One stacked timestep's values, in bytes: 33 × 7000 × 3500 `f32` =
/// **3 234 000 000**.
///
/// Stated so that nothing has to derive it in a hurry. It is **66× the whole
/// wasm arm of [`GRID_CACHE_BYTES`](super::GRID_CACHE_BYTES)**, and it does not
/// fit in a wasm32 address space at all. Nothing in this module is reachable
/// from a render path, and this constant is the reason.
///
/// **Priced in `f32` and NOT off [`CONUS_GRID_BYTES`](super::CONUS_GRID_BYTES),
/// which is a `u16` figure.** [`MrmsVolume::values`] really is a `Vec<f32>` —
/// the stack widens each level at `push` and keeps one flat buffer, because
/// nothing draws it and a second narrow store would be carried for no reader.
/// Deriving this from the grid constant is what it used to do, and when that
/// constant followed MRMS down to 16 bits this figure silently **halved while
/// the allocation did not**: a budget that understates a 3.2 GB buffer by 2×
/// is worse than no budget, because it reads as headroom. The doc above states
/// a count and the value must keep honouring it.
pub const CONUS_STACK_BYTES: usize = LEVEL_COUNT * 7000 * 3500 * std::mem::size_of::<f32>();

// The stack's own width, restated as a build failure: `resident_bytes` counts
// `values.len() * size_of::<f32>()`, and this constant is what that figure is
// checked against.
const _: () = assert!(CONUS_STACK_BYTES == 3_234_000_000);

/// How many level GETs are in flight at once.
///
/// A ceiling on **peak memory**, not on politeness. Every level decode needs a
/// 49 MB values vector and `super::staging`'s slot holds exactly one, so all
/// but one of the concurrent decodes allocates its own; the stack being filled
/// is already [`CONUS_STACK_BYTES`], so the peak is
/// `CONUS_STACK_BYTES + STACK_FETCH_CONCURRENCY × 49 MB` — ~3.4 GB at four,
/// ~6.4 GB if all 33 ran at once.
///
/// The figure was `× 147 MB` while `super::decode` also held grib's 49 MB PNG
/// image buffer for the length of a decode. The row walk removed that half; it
/// did **not** remove the values vector, which is what this constant really
/// bounds.
pub const STACK_FETCH_CONCURRENCY: usize = 4;

/// How many level *listings* are in flight at once.
///
/// Higher than [`STACK_FETCH_CONCURRENCY`] because a bounded listing is ~1 KB
/// and holds nothing: the 33 of them are ~33 KB in total, against the ~40 MB of
/// granules they decide the stamp for.
const LISTING_CONCURRENCY: usize = 8;

/// The bucket's directory name for one level — `MergedReflectivityQC_04.50`.
///
/// Two decimals, zero-padded to five characters, which is what the bucket
/// spells: `00.50` at the bottom and `19.00` at the top.
///
/// # Panics
/// If `level` is not a valid index into [`LEVELS_KM_MSL`].
pub fn level_prefix_name(level: usize) -> String {
    format!("MergedReflectivityQC_{:05.2}", LEVELS_KM_MSL[level])
}

/// The object key one level publishes `stamp` under.
pub fn level_key(level: usize, stamp: &NaiveDateTime) -> String {
    DataSources::mrms_key(&level_prefix_name(level), stamp)
}

/// Refuse a granule that is not `level` of this product, on its own section 4.
///
/// **What makes the level table evidence rather than a belief**, and it takes
/// two facts rather than one:
///
/// * the **parameter category** ([`PARAMETER`]) says it is `MergedReflectivityQC`
///   at all. An earlier version of this function checked only the height, and a
///   review proved the gap by substituting the 2D composite granule into the
///   0.50 km slot: identical surface, identical grid, every test green.
/// * the **first fixed surface** says which level. A directory NOAA renamed, a
///   roster edited out of order, or a level re-published at another altitude all
///   produce a stack that decodes perfectly and is wrong in the vertical — a
///   failure with no visible symptom until something is measured against it.
fn check_granule_is_this_level(level: usize, raw: &RawGrid) -> Result<(), String> {
    let expected_m = LEVELS_KM_MSL[level] * 1000.0;
    match raw.parameter {
        None => {
            return Err(format!(
                "MRMS {}: the granule states no parameter category, so nothing \
                 says it is MergedReflectivityQC rather than a mosaic that \
                 happens to share its grid",
                level_prefix_name(level),
            ));
        }
        Some(p) if p == PARAMETER => {}
        Some(p) => {
            let looks_like = if p == COMPOSITE_PARAMETER {
                " - that is the 2D column-max composite, which declares the same \
                 first fixed surface as the 0.50 km level and is otherwise \
                 indistinguishable here"
            } else {
                ""
            };
            return Err(format!(
                "MRMS {}: parameter is {p:?}, not {PARAMETER:?}{looks_like}",
                level_prefix_name(level),
            ));
        }
    }
    let Some((surface_type, value_m)) = raw.first_fixed_surface else {
        return Err(format!(
            "MRMS {}: the granule states no first fixed surface, so its height \
             cannot be checked against the {expected_m} m its directory claims",
            level_prefix_name(level),
        ));
    };
    if surface_type != SURFACE_TYPE_ALTITUDE_MSL {
        return Err(format!(
            "MRMS {}: first fixed surface type is {surface_type}, not \
             {SURFACE_TYPE_ALTITUDE_MSL} (altitude above mean sea level); the \
             stack's vertical axis is MSL and a height above ground cannot be \
             stacked into it",
            level_prefix_name(level),
        ));
    }
    if !value_m.is_finite() || (value_m - expected_m).abs() > LEVEL_TOLERANCE_M {
        return Err(format!(
            "MRMS {}: the granule declares {value_m} m where its directory \
             names {expected_m} m",
            level_prefix_name(level),
        ));
    }
    Ok(())
}

/// One level's granule, as it came off the wire and out of the decoder.
pub struct LevelFetch {
    pub level: usize,
    /// `.grib2.gz` bytes, exactly as the bucket served them — the figure a
    /// per-timestep download budget is made of.
    pub compressed_bytes: usize,
    /// GRIB2 bytes after gunzip.
    pub grib_bytes: usize,
    pub grid: RawGrid,
}

/// **One timestep of MRMS 3D reflectivity**, all 33 levels, stacked.
pub struct MrmsVolume {
    /// **The latest of the 33 levels' section 1 reference times.**
    ///
    /// The latest and not "the one they agree on", because 2–6 % of timesteps
    /// are published one second apart across a partition of the levels — see
    /// [`STAMP_TOLERANCE_SECONDS`]. [`Self::valid_span_seconds`] is how far they
    /// actually spread, and it is 0 for the common case.
    pub valid: NaiveDateTime,
    /// Seconds between the earliest and latest level's reference time: **0 when
    /// the levels agree**, at most [`STAMP_TOLERANCE_SECONDS`] otherwise.
    ///
    /// Carried rather than discarded so a caller can see that a timestep was
    /// assembled across a split rather than having to trust that it was not.
    pub valid_span_seconds: i64,
    pub bounds: GeoBounds,
    /// The horizontal grid, shared by every level by construction: the
    /// assembler refuses a level whose coordinates differ.
    pub coords: GridCoords,
    pub ni: usize,
    pub nj: usize,
    /// **Level-major**: level `l`'s grid is `values[l * ni * nj ..][.. ni * nj]`,
    /// and within a level the order is the granule's own scanning order — the
    /// same order [`GridCoords::at`](crate::hrrr::GridCoords::at) indexes.
    ///
    /// Level-major and not cell-major because that is the order the levels
    /// arrive in and the order a level-of-detail scheme drops levels in; a
    /// column-major reshuffle would be 3.2 GB of gather for a layout nothing has
    /// yet chosen.
    pub values: Vec<f32>,
    /// Gzipped bytes downloaded for this timestep, **per level** — the shape of
    /// the download and not just its total, because the levels are nowhere near
    /// equal and which ones a residency scheme could drop is exactly the
    /// question E2 asks.
    pub compressed_bytes_by_level: [usize; LEVEL_COUNT],
    /// GRIB2 bytes after gunzip, per level.
    pub grib_bytes_by_level: [usize; LEVEL_COUNT],
}

/// Shape and cost, never the 3.2 GB of values -- a derived `Debug` would print
/// 808 million floats into a test failure message.
impl std::fmt::Debug for MrmsVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MrmsVolume")
            .field("valid", &self.valid)
            .field("valid_span_seconds", &self.valid_span_seconds)
            .field("levels", &LEVEL_COUNT)
            .field("ni", &self.ni)
            .field("nj", &self.nj)
            .field("cells", &self.cells())
            .field("compressed_bytes", &self.compressed_bytes())
            .finish_non_exhaustive()
    }
}

impl MrmsVolume {
    /// Gzipped bytes for the whole timestep — **all 33 levels**, which is the
    /// only denominator a per-timestep download budget may be stated against.
    pub fn compressed_bytes(&self) -> usize {
        self.compressed_bytes_by_level.iter().sum()
    }

    /// GRIB2 bytes after gunzip, all 33 levels.
    pub fn grib_bytes(&self) -> usize {
        self.grib_bytes_by_level.iter().sum()
    }

    /// Points per level.
    pub fn points_per_level(&self) -> usize {
        self.ni * self.nj
    }

    /// **The denominator every occupancy figure is stated against**: 33 ×
    /// `ni` × `nj`.
    pub fn cells(&self) -> usize {
        LEVEL_COUNT * self.points_per_level()
    }

    /// Bytes of stacked values held.
    pub fn resident_bytes(&self) -> usize {
        self.values.len() * std::mem::size_of::<f32>()
    }

    /// One level's grid.
    pub fn level_values(&self, level: usize) -> &[f32] {
        let n = self.points_per_level();
        &self.values[level * n..(level + 1) * n]
    }

    /// Per-level occupancy, in [`LEVELS_KM_MSL`]'s order.
    pub fn per_level_occupancy(&self) -> Vec<Occupancy> {
        (0..LEVEL_COUNT)
            .map(|l| Occupancy::of(self.level_values(l)))
            .collect()
    }

    /// Occupancy over the whole stack — the sum of [`Self::per_level_occupancy`],
    /// so the two can never disagree.
    pub fn occupancy(&self) -> Occupancy {
        self.per_level_occupancy()
            .into_iter()
            .fold(Occupancy::default(), Occupancy::merged)
    }
}

/// The reflectivity bars occupancy is counted against.
///
/// Four, because "non-zero" is not one number and reporting it as one is what
/// would make a residency decision on the wrong quantity:
///
/// * **0 dBZ** — any return at all, including the returns below every colour
///   bar's first stop;
/// * **5 dBZ** — the overlay reflectivity ladder's own floor
///   (`squallar_source::product::REFLECTIVITY_OVERLAY_STOPS`), so this is the
///   fraction that would paint *anything* on today's bar;
/// * **20 dBZ** — light precipitation and up;
/// * **40 dBZ** — the convective core, the fraction a storm-following scheme
///   would resolve at full detail.
pub const OCCUPANCY_THRESHOLDS_DBZ: [f32; 4] = [0.0, 5.0, 20.0, 40.0];

/// How much of a field is anything at all.
///
/// Every field is a **count**, never a ratio, so that counts from different
/// scopes add and a caller states its own denominator. [`Occupancy::fraction`]
/// is where a ratio is taken, and it takes one against `cells`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Occupancy {
    /// Values examined — the denominator.
    pub cells: usize,
    /// Values that are a reading at all: finite, so neither of
    /// [`MISSING_CODES`] and nothing the decoder rejected.
    pub readings: usize,
    /// Readings at or above each of [`OCCUPANCY_THRESHOLDS_DBZ`], in that order.
    pub at_or_above: [usize; OCCUPANCY_THRESHOLDS_DBZ.len()],
}

impl Occupancy {
    /// Count one slab.
    ///
    /// A `NaN` compares false against every threshold, so the missing codes the
    /// decoder mapped are excluded from every count without a second test.
    pub fn of(values: &[f32]) -> Self {
        let mut out = Occupancy {
            cells: values.len(),
            ..Occupancy::default()
        };
        for &v in values {
            if !v.is_finite() {
                continue;
            }
            out.readings += 1;
            for (slot, &threshold) in out.at_or_above.iter_mut().zip(&OCCUPANCY_THRESHOLDS_DBZ) {
                if v >= threshold {
                    *slot += 1;
                }
            }
        }
        out
    }

    /// Two scopes' counts, added.
    pub fn merged(self, other: Self) -> Self {
        let mut out = Occupancy {
            cells: self.cells + other.cells,
            readings: self.readings + other.readings,
            at_or_above: self.at_or_above,
        };
        for (slot, add) in out.at_or_above.iter_mut().zip(other.at_or_above) {
            *slot += add;
        }
        out
    }

    /// `count / cells`, or 0 for an empty scope.
    pub fn fraction(&self, count: usize) -> f64 {
        if self.cells == 0 {
            0.0
        } else {
            count as f64 / self.cells as f64
        }
    }
}

/// Levels in, one [`MrmsVolume`] out — with every congruence check on the way.
///
/// The buffer is allocated **once**, at [`CONUS_STACK_BYTES`], the first time a
/// level arrives and its shape is known; each level is written into its own
/// slice. Growing a `Vec` level by level would hold the old and the new
/// allocation at once, which at this size is another 3.2 GB.
pub struct VolumeAssembler {
    shape: Option<(usize, usize)>,
    valid: Option<(NaiveDateTime, NaiveDateTime)>,
    coords: Option<GridCoords>,
    bounds: Option<GeoBounds>,
    values: Vec<f32>,
    filled: [bool; LEVEL_COUNT],
    compressed_bytes: [usize; LEVEL_COUNT],
    grib_bytes: [usize; LEVEL_COUNT],
}

impl Default for VolumeAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl VolumeAssembler {
    pub fn new() -> Self {
        VolumeAssembler {
            shape: None,
            valid: None,
            coords: None,
            bounds: None,
            values: Vec::new(),
            filled: [false; LEVEL_COUNT],
            compressed_bytes: [0; LEVEL_COUNT],
            grib_bytes: [0; LEVEL_COUNT],
        }
    }

    /// Add one level.
    ///
    /// **Every disagreement is an error, never a reconciliation.** A stack whose
    /// levels do not share a horizontal grid, a valid time and a scanning order
    /// is not a volume, and a resampled or reindexed level would be a silent
    /// vertical smear rather than a visible fault.
    pub fn push(&mut self, fetch: LevelFetch) -> Result<(), String> {
        let LevelFetch {
            level,
            compressed_bytes,
            grib_bytes,
            grid,
        } = fetch;
        if level >= LEVEL_COUNT {
            return Err(format!("MRMS 3D: level {level} is past the {LEVEL_COUNT}"));
        }
        if self.filled[level] {
            return Err(format!(
                "MRMS 3D: {} arrived twice",
                level_prefix_name(level),
            ));
        }
        check_granule_is_this_level(level, &grid)?;

        let name = level_prefix_name(level);
        let shape = *self.shape.get_or_insert((grid.ni, grid.nj));
        if shape != (grid.ni, grid.nj) {
            return Err(format!(
                "MRMS 3D: {name} is {}×{} where the stack is {}×{}",
                grid.ni, grid.nj, shape.0, shape.1,
            ));
        }
        if grid.values.len() != shape.0 * shape.1 {
            return Err(format!(
                "MRMS 3D: {name} decoded {} values for a {}×{} grid",
                grid.values.len(),
                shape.0,
                shape.1,
            ));
        }
        // **Within the tolerance, not equal.** 1-6 % of timesteps are
        // published a few seconds apart across a partition of the levels
        // ([`STAMP_TOLERANCE_SECONDS`]), so demanding equality here would
        // refuse every timestep the tolerant matcher exists to recover. The
        // window is 8.3 % of the ~120 s cadence, so the neighbouring scan is
        // still 12x away and "a stack of two timesteps" is still refused.
        let span = self.valid.get_or_insert((grid.valid, grid.valid));
        span.0 = span.0.min(grid.valid);
        span.1 = span.1.max(grid.valid);
        if (span.1 - span.0).num_seconds() > STAMP_TOLERANCE_SECONDS {
            return Err(format!(
                "MRMS 3D: {name} is valid {} and the stack already spans {} to \
                 {}, more than the {STAMP_TOLERANCE_SECONDS} s one scan may \
                 spread; a stack of two timesteps is not a timestep",
                grid.valid, span.0, span.1,
            ));
        }
        let coords = self.coords.get_or_insert_with(|| grid.coords.clone());
        if *coords != grid.coords {
            return Err(format!(
                "MRMS 3D: {name}'s horizontal grid differs from the stack's, so \
                 its points do not stand above the others",
            ));
        }
        let bounds = *self.bounds.get_or_insert(grid.bounds);
        if bounds != grid.bounds {
            return Err(format!(
                "MRMS 3D: {name}'s envelope differs from the stack's"
            ));
        }

        let n = shape.0 * shape.1;
        if self.values.is_empty() {
            // **`NAN` and not `0.0`**, which is the second line of defence
            // behind `finish`'s refusal of a partial stack: 0 dBZ is a
            // *reading* and would occupy a hole rather than mark it, so a level
            // that never arrived would read downstream as a real, weak echo at
            // that height. `Occupancy` counts only finite values, so a leak
            // shows up as a missing count rather than as inflated coverage.
            // Pinned by `tests::an_unfilled_level_is_nan_and_never_zero`.
            self.values = vec![f32::NAN; LEVEL_COUNT * n];
        }
        // **Widened here, deliberately.** The stack is a `Vec<f32>` of all
        // 33 levels and nothing draws it, so it is not worth a second narrow
        // store; the granule's own values are read back through
        // `GridValues::iter` one at a time, which allocates nothing.
        for (slot, value) in self.values[level * n..(level + 1) * n]
            .iter_mut()
            .zip(grid.values.iter())
        {
            *slot = value;
        }
        self.filled[level] = true;
        self.compressed_bytes[level] = compressed_bytes;
        self.grib_bytes[level] = grib_bytes;
        Ok(())
    }

    /// The buffer as filled so far — every unfilled level still `NaN`.
    ///
    /// Exists for `tests::an_unfilled_level_is_nan_and_never_zero`: the fill is
    /// unobservable through [`Self::finish`], which refuses a partial stack, and
    /// an unobservable second line of defence is one nothing holds.
    pub fn partial_values(&self) -> &[f32] {
        &self.values
    }

    /// Which levels have not arrived.
    pub fn missing_levels(&self) -> Vec<usize> {
        (0..LEVEL_COUNT).filter(|&l| !self.filled[l]).collect()
    }

    /// **All 33 or nothing.**
    ///
    /// A partial stack is refused rather than returned short: a hole at one
    /// height reads downstream as clear air at that height, which is the
    /// "silent partial success" shape — a round that returns `Ok` while the
    /// picture under-draws.
    pub fn finish(self) -> Result<MrmsVolume, String> {
        let missing = self.missing_levels();
        if !missing.is_empty() {
            return Err(format!(
                "MRMS 3D: {} of {LEVEL_COUNT} levels never arrived, starting \
                 with {}",
                missing.len(),
                level_prefix_name(missing[0]),
            ));
        }
        let (ni, nj) = self.shape.expect("a filled stack has a shape");
        let (earliest, latest) = self.valid.expect("a filled stack has a valid time");
        Ok(MrmsVolume {
            valid: latest,
            valid_span_seconds: (latest - earliest).num_seconds(),
            bounds: self.bounds.expect("a filled stack has an envelope"),
            coords: self.coords.expect("a filled stack has coordinates"),
            ni,
            nj,
            values: self.values,
            compressed_bytes_by_level: self.compressed_bytes,
            grib_bytes_by_level: self.grib_bytes,
        })
    }
}

/// **How far apart two levels' stamps may be and still be one scan**, in
/// seconds.
///
/// **Because "every level publishes under the same stamp" is false**, and it is
/// false often enough to matter. The mechanism is an exact partition rather
/// than a race: on `20260829` at `003242` the levels 00.50 through 01.75
/// published and at `003243` the other 27 did, with **zero overlap** — the same
/// scan, spelled one second apart.
///
/// Measured over six whole UTC days spread across 12 months, every one of which
/// published exactly **720 granules per level**
/// (`tests::the_levels_do_not_always_share_a_stamp_and_the_tolerance_recovers_them`,
/// `#[ignore]`d because it is network):
///
/// | Day | Exact intersection | Lost to a single stamp | Largest hole, exact | Tolerance that recovers all 720 |
/// |---|---:|---:|---:|---:|
/// | 2025-09-15 | 679 | 41 (5.7 %) | 481 s | 8 s |
/// | 2025-12-01 | 712 | 8 (1.1 %) | 241 s | 6 s |
/// | 2026-02-14 | 705 | 15 (2.1 %) | 246 s | 4 s |
/// | 2026-03-15 | 705 | 15 (2.1 %) | 363 s | **10 s** |
/// | 2026-06-01 | 697 | 23 (3.2 %) | 362 s | 6 s |
/// | 2026-08-29 | 680 | 40 (5.6 %) | 361 s | 5 s |
///
/// So an exact match loses **1.1 % to 5.7 %** of timesteps and opens holes of
/// **241 s to 481 s against a ~120 s cadence, on days with no outage at all**.
///
/// **The residual, after the fix: zero.** At 10 s every one of the six days
/// resolves **720 of 720** timesteps — a 0.0 % unfetchable rate, down from
/// 1.1-5.7 % — and the largest hole in the fetchable series falls to 126-137 s,
/// which *is* one cadence. There is no remaining hole to report.
///
/// **Ten is read off that curve, not guessed.** Every one of the six days
/// reaches 720 of 720 at 10 s and the worst of them needs exactly 10; past it
/// the curve is flat out to 30 s. An earlier draft used 3 s on the strength of
/// two days — as did the review that found the split — and that left 2 to 5
/// timesteps a day still unfetchable. The six-day sweep is what moved it.
///
/// It is safe at the top end for the same reason it is chosen: 10 s is **8.3 %
/// of the ~120 s cadence**, so the neighbouring scan is 12× the window away and
/// no clustering can merge two of them. [`timesteps_within`] also caps a cluster
/// at this many seconds from its *first* stamp rather than between consecutive
/// pairs, so a chain of 1 s steps cannot drift one wider.
pub const STAMP_TOLERANCE_SECONDS: i64 = 10;

/// **The 33 stamps that together are one timestep** — one per level, all within
/// [`STAMP_TOLERANCE_SECONDS`] of each other.
///
/// Not a single stamp, because a single stamp cannot address 2–6 % of
/// timesteps. Each level is fetched at *its own* published stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackStamps {
    /// Indexed by level, in [`LEVELS_KM_MSL`]'s order.
    pub stamps: [NaiveDateTime; LEVEL_COUNT],
}

impl StackStamps {
    /// The timestep's own identity: the **latest** of the 33.
    ///
    /// The latest and not the earliest or a mean, so that a timestep is named by
    /// the instant every one of its levels had been published by — which is the
    /// only one of the three a "newest at or before `at`" question can be
    /// answered against without lying by up to the tolerance.
    pub fn valid(&self) -> NaiveDateTime {
        self.stamps.iter().copied().max().expect("33 stamps")
    }

    /// How far the 33 stamps spread, in seconds — 0 when they agree.
    pub fn span_seconds(&self) -> i64 {
        let lo = self.stamps.iter().min().expect("33 stamps");
        let hi = self.stamps.iter().max().expect("33 stamps");
        (*hi - *lo).num_seconds()
    }

    /// Whether every level published the same second, which is the common case
    /// and **not** the invariant.
    pub fn is_aligned(&self) -> bool {
        self.span_seconds() == 0
    }
}

/// **Every timestep all 33 levels published, from each level's own stamp list.**
///
/// Pure, and deliberately so: this is the whole of the "the stamp is the
/// intersection, never one level's newest" claim, and a network-free function is
/// one a default `cargo test` row can hold. `per_level` must have
/// [`LEVEL_COUNT`] entries, each sorted ascending; anything else answers empty
/// rather than guessing.
///
/// The algorithm is a clustering, not a set intersection. The union is walked in
/// order and cut wherever the next stamp is more than
/// [`STAMP_TOLERANCE_SECONDS`] past the cluster's **first** — bounding the whole
/// cluster rather than each consecutive pair, so a chain of 1 s steps cannot
/// drift a cluster arbitrarily wide. A cluster becomes a timestep only when
/// **every** level has a stamp inside it; a level with two stamps in one cluster
/// contributes its earliest, which cannot happen at a 120 s cadence and is
/// resolved rather than left to ordering.
pub fn timesteps(per_level: &[Vec<NaiveDateTime>]) -> Vec<StackStamps> {
    timesteps_within(per_level, STAMP_TOLERANCE_SECONDS)
}

/// [`timesteps`] at an arbitrary tolerance.
///
/// **The tolerance is a parameter here so that its value can be chosen from a
/// curve rather than asserted.** `tests::the_levels_do_not_always_share_a_stamp_
/// and_the_tolerance_recovers_them` (`#[ignore]`d, network) sweeps it over a
/// whole day and prints what each second buys; [`STAMP_TOLERANCE_SECONDS`]
/// records where that curve goes flat.
pub fn timesteps_within(per_level: &[Vec<NaiveDateTime>], tolerance: i64) -> Vec<StackStamps> {
    // **The length invariant is carried by the `try_from` below and nowhere
    // else.** An explicit `per_level.len() != LEVEL_COUNT` guard used to sit
    // here; a mutation sweep showed it could be deleted with every test still
    // green, because a short or long input already fails to fill a
    // `[NaiveDateTime; LEVEL_COUNT]` and yields no timestep. A branch no test
    // can distinguish is a branch that should not exist.
    let mut union: Vec<NaiveDateTime> = per_level.iter().flatten().copied().collect();
    union.sort_unstable();
    union.dedup();

    let mut out = Vec::new();
    let mut i = 0;
    while i < union.len() {
        let first = union[i];
        let mut j = i;
        while j < union.len() && (union[j] - first).num_seconds() <= tolerance {
            j += 1;
        }
        let last = union[j - 1];
        let picked: Vec<NaiveDateTime> = per_level
            .iter()
            .filter_map(|stamps| {
                stamps
                    .iter()
                    .copied()
                    .filter(|s| *s >= first && *s <= last)
                    .min()
            })
            .collect();
        if let Ok(stamps) = <[NaiveDateTime; LEVEL_COUNT]>::try_from(picked) {
            out.push(StackStamps { stamps });
        }
        i = j;
    }
    out
}

/// The stamps a listing's keys carry, **at or before `at`**, sorted and deduped.
///
/// Pure, so the `at` filter has a home a default test row can reach. The filter
/// is load-bearing whenever `at` is not now: [`super::fetch::listing_attempts`]
/// bounds where a listing *starts* and never where it ends, so a day prefix
/// asked about 15:00Z still answers with that whole UTC day and an unfiltered
/// maximum would be this evening's scan drawn over a mid-afternoon instant. A
/// key with no decodable stamp is dropped rather than kept: an undatable key
/// cannot be shown to be at or before anything.
pub fn stamps_at_or_before(keys: &[String], at: NaiveDateTime) -> Vec<NaiveDateTime> {
    let mut stamps: Vec<NaiveDateTime> = keys
        .iter()
        .filter_map(|k| super::fetch::key_valid_time(k))
        .filter(|valid| *valid <= at)
        .collect();
    stamps.sort_unstable();
    stamps.dedup();
    stamps
}

/// The stamps `level` published in the two UTC days up to `at`.
///
/// Reuses [`super::fetch::listing_attempts`]' ladder whole — bounded, unbounded,
/// yesterday — so the 3D path inherits the midnight-boundary and stalled-feed
/// handling rather than restating it, and takes `at` as a parameter for the same
/// reason [`super::fetch::latest_key`] does.
async fn level_stamps(
    client: &reqwest::Client,
    sources: &DataSources,
    level: usize,
    at: NaiveDateTime,
) -> Result<Vec<NaiveDateTime>, FetchError> {
    let name = level_prefix_name(level);
    let mut last_error: Option<FetchError> = None;
    for (date, start_after) in super::fetch::listing_attempts(at) {
        match super::fetch::list_day(client, sources, &name, date, start_after).await {
            Ok(keys) => {
                let stamps = stamps_at_or_before(&keys, at);
                if !stamps.is_empty() {
                    return Ok(stamps);
                }
            }
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        FetchError::absent(format!(
            "MRMS 3D: {name} published nothing in the two UTC days up to {at}"
        ))
    }))
}

/// Each level's stamps, or the first listing failure.
///
/// **A level that could not be listed is an error and never an empty set.**
/// Folding an outage into the clustering would silently drop every timestep
/// instead of saying the feed was unreachable.
async fn all_level_stamps(
    client: &reqwest::Client,
    sources: &DataSources,
    at: NaiveDateTime,
) -> Result<Vec<Vec<NaiveDateTime>>, FetchError> {
    let per_level: Vec<Result<Vec<NaiveDateTime>, FetchError>> =
        futures::stream::iter(0..LEVEL_COUNT)
            .map(|level| level_stamps(client, sources, level, at))
            .buffered(LISTING_CONCURRENCY)
            .collect()
            .await;
    per_level
        .into_iter()
        .enumerate()
        .map(|(level, stamps)| {
            stamps.map_err(|e| {
                FetchError::transient(format!(
                    "MRMS 3D: {} could not be listed, so no timestep is known to \
                     be complete: {e}",
                    level_prefix_name(level),
                ))
            })
        })
        .collect()
}

/// **The newest timestep at or before `at` that every one of the 33 levels has
/// published.**
///
/// 33 bounded listings, ~1 KB apiece. `at` is a parameter and not a clock read,
/// matching [`super::fetch::latest_key`]: the instant the caller depicts is the
/// caller's to state, and it is what makes this drivable without a wall clock.
pub async fn latest_timestep(
    client: &reqwest::Client,
    sources: &DataSources,
    at: NaiveDateTime,
) -> Result<StackStamps, FetchError> {
    let per_level = all_level_stamps(client, sources, at).await?;
    timesteps(&per_level)
        .into_iter()
        .max_by_key(StackStamps::valid)
        .ok_or_else(|| {
            FetchError::absent(format!(
                "MRMS 3D: no timestep in the two UTC days up to {at} is published \
                 by all {LEVEL_COUNT} levels"
            ))
        })
}

/// **Every timestep of one whole UTC day**, ascending.
///
/// The instrument behind [`STAMP_TOLERANCE_SECONDS`]' figures, and the enumeration
/// a loop over a national volume would need. **The cost, with its denominator**:
/// 33 *unbounded* day listings — one per level, ~720 keys and ~90 KB of XML each,
/// so ~3 MB for the day — against the ~25 MB one timestep of granules costs.
pub async fn day_timesteps(
    client: &reqwest::Client,
    sources: &DataSources,
    day: chrono::NaiveDate,
) -> Result<Vec<StackStamps>, FetchError> {
    let end = day
        .and_hms_opt(23, 59, 59)
        .expect("23:59:59 is a time of day");
    let per_level: Vec<Result<Vec<NaiveDateTime>, FetchError>> =
        futures::stream::iter(0..LEVEL_COUNT)
            .map(|level| async move {
                let name = level_prefix_name(level);
                let keys = super::fetch::list_day(client, sources, &name, day, None).await?;
                Ok(stamps_at_or_before(&keys, end))
            })
            .buffered(LISTING_CONCURRENCY)
            .collect()
            .await;
    let per_level: Vec<Vec<NaiveDateTime>> = per_level.into_iter().collect::<Result<_, _>>()?;
    Ok(timesteps(&per_level))
}

/// Download and decode one level of one timestep.
pub async fn fetch_level(
    client: &reqwest::Client,
    sources: &DataSources,
    level: usize,
    stamp: &NaiveDateTime,
) -> Result<LevelFetch, FetchError> {
    let key = level_key(level, stamp);
    let url = sources.s3_object_url(&sources.mrms_bucket, &key);
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("MRMS 3D request failed: {e}")))?;
    if !resp.status().is_success() {
        // `IsRoutine`, as `super::fetch::fetch_key` uses: the stamp came out of
        // a listing of this very level, so a 404 is an object expired or
        // replaced between the two requests.
        return Err(FetchError::from_status(
            resp.status(),
            NotFound::IsRoutine,
            format!("MRMS 3D {key}: HTTP {}", resp.status()),
        ));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("MRMS 3D body read failed: {e}")))?;
    let compressed_bytes = body.len();
    let grib = super::decode::gunzip(&body).map_err(FetchError::transient)?;
    let grib_bytes = grib.len();
    let grid =
        super::decode::parse_grib2_raw(&grib, &MISSING_CODES).map_err(FetchError::transient)?;
    Ok(LevelFetch {
        level,
        compressed_bytes,
        grib_bytes,
        grid,
    })
}

/// **One timestep, all 33 levels, stacked.**
///
/// 33 GETs at [`STACK_FETCH_CONCURRENCY`] in flight, **each level at its own
/// stamp** — which is why the argument is a [`StackStamps`] and not one instant.
pub async fn fetch_stack(
    client: &reqwest::Client,
    sources: &DataSources,
    timestep: &StackStamps,
) -> Result<MrmsVolume, FetchError> {
    let mut assembler = VolumeAssembler::new();
    let mut levels = futures::stream::iter(0..LEVEL_COUNT)
        .map(|level| fetch_level(client, sources, level, &timestep.stamps[level]))
        .buffer_unordered(STACK_FETCH_CONCURRENCY);
    while let Some(fetched) = levels.next().await {
        assembler.push(fetched?).map_err(FetchError::transient)?;
    }
    assembler.finish().map_err(FetchError::transient)
}

/// The newest timestep at or before `at` that every level has published,
/// stacked.
pub async fn fetch_latest_stack(
    client: &reqwest::Client,
    sources: &DataSources,
    at: NaiveDateTime,
) -> Result<MrmsVolume, FetchError> {
    let timestep = latest_timestep(client, sources, at).await?;
    log::info!(
        "MRMS 3D: stacking {LEVEL_COUNT} levels of {} (stamp span {} s)",
        timestep.valid(),
        timestep.span_seconds(),
    );
    fetch_stack(client, sources, &timestep).await
}

#[cfg(test)]
mod tests;
