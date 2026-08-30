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
//! Seven timesteps, 2026-08-27 to 2026-08-30, taken with
//! `the_live_stack_decodes_and_reports_its_own_cost` (`#[ignore]`d because it is
//! network) on a `--release` build, one desktop, one home connection.
//! **Every share below has one denominator: 33 × 24 500 000 = 808 500 000 cells
//! of that one timestep.** Nothing here is averaged across rows — see the
//! spread, which is the point.
//!
//! | Stamp (UTC) | Download (gz) | readings | ≥ 5 dBZ | ≥ 20 dBZ | ≥ 40 dBZ |
//! |---|---:|---:|---:|---:|---:|
//! | 2026-08-27 20:58:43 | 30 916 431 B | 2.851 % | 2.714 % | 0.738 % | 0.0489 % |
//! | 2026-08-28 08:58:41 | 18 392 787 B | 1.543 % | 1.433 % | 0.357 % | 0.0157 % |
//! | 2026-08-28 20:58:40 | 29 836 909 B | 2.767 % | 2.610 % | 0.608 % | 0.0333 % |
//! | 2026-08-29 09:58:42 | 19 899 508 B | 1.843 % | 1.731 % | 0.578 % | 0.0197 % |
//! | 2026-08-29 20:56:38 | 28 242 562 B | 2.665 % | 2.513 % | 0.618 % | 0.0249 % |
//! | 2026-08-30 01:58:42 | 25 439 250 B | 2.501 % | 2.338 % | 0.603 % | 0.0134 % |
//! | 2026-08-30 09:24:42 | 25 435 169 B | 2.532 % | 2.363 % | 0.545 % | 0.0133 % |
//!
//! **A timestep is 18–31 MB gzipped**, against MRMS's ~2-minute cadence.
//!
//! **97.1 to 98.5 % of the stack is not a reading at all.** `readings` counts
//! finite values — everything that is neither of [`MISSING_CODES`] — and over
//! seven timesteps it never reached 3 %. For scale, and **not as a like-for-like
//! comparison** (a different granule at a different hour): the committed 2D
//! composite fixture is 4.8 % readings, its reserved codes covering 95.2 % of
//! which only 34 % is the no-coverage mask. So the stack is roughly half as
//! dense per cell as the column maximum it would replace, and what dominates it
//! is `-99` — "no echo at this height" — because most of the atmosphere above
//! a covered point holds nothing at any given moment.
//!
//! **The quiet-to-active spread is a small multiple, not orders of magnitude.**
//! The diurnal peak (~21:00Z, mid-afternoon over the plains) and the overnight
//! minimum (~09:00Z) differ by **1.9×** in the ≥ 5 dBZ share and **3.7×** in the
//! ≥ 40 dBZ share, over seven timesteps spanning three days. A scheme sized for
//! 3 % occupancy is not routinely surprised; one sized for the mean of these
//! rows would be, which is why they are stated separately. Seven timesteps in
//! one late-August week is also **not a year**: a winter stratiform event or a
//! landfalling tropical system are unmeasured here.
//!
//! **Occupancy peaks in the middle of the column, not at the bottom.** The
//! densest levels are 5.5–6.0 km (5.17 % of their own level ≥ 5 dBZ on the
//! 2026-08-29 20:56:38Z stack) and the sparsest are the two ends — 0.31 % at
//! 0.50 km, 0.03 % at 19 km. Per-level bytes follow the same curve and peak at
//! 5.5 km, so the top and bottom of the roster are cheap in both dimensions.
//! **Why the bottom level is the emptier end is not something these counts
//! prove**, and two ordinary explanations fit: 0.5 km MSL is below the ground
//! over much of the west, and the beam is above it at any useful range
//! elsewhere. It is recorded as an observation, not a mechanism.
//!
//! ## Wall clock, measured
//!
//! On the same seven runs, and **stated as two figures because they have
//! different shapes**:
//!
//! * finding a stamp all 33 levels have: **0.50–0.65 s**, 33 bounded listings
//!   of ~1 KB;
//! * fetch + gunzip + GRIB2 decode + stack: **10.6–14.1 s** at
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
/// [`check_declared_level`].
pub const LEVELS_KM_MSL: [f64; LEVEL_COUNT] = [
    0.50, 0.75, 1.00, 1.25, 1.50, 1.75, 2.00, 2.25, 2.50, 2.75, 3.00, // 0.25 km
    3.50, 4.00, 4.50, 5.00, 5.50, 6.00, 6.50, 7.00, 7.50, 8.00, 8.50, 9.00, // 0.50 km
    10.00, 11.00, 12.00, 13.00, 14.00, 15.00, 16.00, 17.00, 18.00, 19.00, // 1.00 km
];

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
pub const MISSING_CODES: [f32; 2] = [-999.0, -99.0];

/// One stacked timestep's values, in bytes: 33 × 7000 × 3500 `f32` =
/// **3 234 000 000**.
///
/// Stated so that nothing has to derive it in a hurry. It is **33× the whole
/// wasm arm of [`GRID_CACHE_BYTES`](super::GRID_CACHE_BYTES)**, and it does not
/// fit in a wasm32 address space at all. Nothing in this module is reachable
/// from a render path, and this constant is the reason.
pub const CONUS_STACK_BYTES: usize = LEVEL_COUNT * super::CONUS_GRID_BYTES;

/// How many level GETs are in flight at once.
///
/// A ceiling on **peak memory**, not on politeness. One decode peaks at ~147 MB
/// transient (`super::decode`'s own arithmetic: grib's 49 MB PNG image buffer
/// held while a 98 MB values vector fills), and the stack being filled is
/// already [`CONUS_STACK_BYTES`], so the peak is
/// `CONUS_STACK_BYTES + STACK_FETCH_CONCURRENCY × 147 MB` — ~3.8 GB at four,
/// ~8.1 GB if all 33 ran at once.
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

/// Refuse a granule whose own section 4 does not agree with the height its
/// directory name claims.
///
/// **The one check that makes the level table evidence rather than a belief.**
/// A directory NOAA renamed, a roster edited out of order, or a level silently
/// re-published at a different altitude all produce a stack that decodes
/// perfectly and is wrong in the vertical — the failure that has no visible
/// symptom until something is measured against it.
fn check_declared_level(level: usize, raw: &RawGrid) -> Result<(), String> {
    let expected_m = LEVELS_KM_MSL[level] * 1000.0;
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
    /// Every level's section 1 reference time, which they agree on or the stack
    /// is refused.
    pub valid: NaiveDateTime,
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
    valid: Option<NaiveDateTime>,
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
        check_declared_level(level, &grid)?;

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
        let valid = *self.valid.get_or_insert(grid.valid);
        if valid != grid.valid {
            return Err(format!(
                "MRMS 3D: {name} is valid {} where the stack is valid {valid}; \
                 a stack of two timesteps is not a timestep",
                grid.valid,
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
            self.values = vec![f32::NAN; LEVEL_COUNT * n];
        }
        self.values[level * n..(level + 1) * n].copy_from_slice(&grid.values);
        self.filled[level] = true;
        self.compressed_bytes[level] = compressed_bytes;
        self.grib_bytes[level] = grib_bytes;
        Ok(())
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
        Ok(MrmsVolume {
            valid: self.valid.expect("a filled stack has a valid time"),
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

/// The stamps `level` published in the two UTC days up to `at`, ascending and
/// **filtered to `at`**.
///
/// Reuses [`super::fetch::listing_attempts`]' ladder whole — bounded, unbounded,
/// yesterday — so the 3D path inherits the midnight-boundary and stalled-feed
/// handling rather than restating it, and takes `at` as a parameter for the same
/// reason [`super::fetch::latest_key`] does: `listing_attempts` bounds where a
/// listing *starts* and never where it ends, so an unfiltered list asked about
/// 15:00Z still carries that evening's stamps.
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
                let mut stamps: Vec<NaiveDateTime> = keys
                    .iter()
                    .filter_map(|k| super::fetch::key_valid_time(k))
                    .filter(|valid| *valid <= at)
                    .collect();
                if !stamps.is_empty() {
                    stamps.sort_unstable();
                    stamps.dedup();
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

/// The stamps every one of the 33 levels published, given each level's own
/// sorted list.
///
/// **The intersection and not the union or the maximum**: the levels do not land
/// simultaneously, so the newest stamp of the 0.5 km directory is routinely a
/// stamp the 19 km directory has not written yet, and building 33 keys from it
/// would 404 part of the stack. A level that could not be listed at all is an
/// error rather than an empty set, or a feed outage would silently narrow the
/// intersection instead of reporting itself.
fn intersect_stamps(
    per_level: Vec<Result<Vec<NaiveDateTime>, FetchError>>,
) -> Result<Vec<NaiveDateTime>, FetchError> {
    let mut common: Option<Vec<NaiveDateTime>> = None;
    for (level, stamps) in per_level.into_iter().enumerate() {
        let stamps = stamps.map_err(|e| {
            FetchError::transient(format!(
                "MRMS 3D: {} could not be listed, so no stamp is known to be \
                 complete: {e}",
                level_prefix_name(level),
            ))
        })?;
        common = Some(match common {
            None => stamps,
            Some(so_far) => so_far
                .into_iter()
                .filter(|s| stamps.binary_search(s).is_ok())
                .collect(),
        });
    }
    Ok(common.unwrap_or_default())
}

/// **The newest stamp at or before `at` that every one of the 33 levels has
/// published.**
///
/// 33 bounded listings, ~1 KB apiece. `at` is a parameter and not a clock read,
/// matching [`super::fetch::latest_key`]: the instant the caller depicts is the
/// caller's to state, and it is what makes this testable without a wall clock.
pub async fn latest_complete_stamp(
    client: &reqwest::Client,
    sources: &DataSources,
    at: NaiveDateTime,
) -> Result<NaiveDateTime, FetchError> {
    let per_level = futures::stream::iter(0..LEVEL_COUNT)
        .map(|level| level_stamps(client, sources, level, at))
        .buffered(LISTING_CONCURRENCY)
        .collect()
        .await;
    intersect_stamps(per_level)?
        .into_iter()
        .max()
        .ok_or_else(|| {
            FetchError::absent(format!(
                "MRMS 3D: no stamp in the two UTC days up to {at} is published by \
             all {LEVEL_COUNT} levels"
            ))
        })
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
/// 33 GETs at [`STACK_FETCH_CONCURRENCY`] in flight. The keys are constructed
/// rather than listed: a stamp fixes all 33 of them, which is the whole reason
/// [`latest_complete_stamp`] exists.
pub async fn fetch_stack(
    client: &reqwest::Client,
    sources: &DataSources,
    stamp: &NaiveDateTime,
) -> Result<MrmsVolume, FetchError> {
    let mut assembler = VolumeAssembler::new();
    let mut levels = futures::stream::iter(0..LEVEL_COUNT)
        .map(|level| fetch_level(client, sources, level, stamp))
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
    let stamp = latest_complete_stamp(client, sources, at).await?;
    log::info!("MRMS 3D: stacking {LEVEL_COUNT} levels of {stamp}");
    fetch_stack(client, sources, &stamp).await
}

#[cfg(test)]
mod tests;
