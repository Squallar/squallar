//! GMGSI — NOAA's Global Mosaic of Geostationary Satellite Imagery.
//!
//! Four channels, blended hourly from every geostationary imager in orbit
//! (`Meteosat9,Meteosat10,G19,H-9,G18` in the reference granule), onto one
//! 3000 x 5000 global grid. NetCDF4, i.e. HDF5, read through
//! [`squallar_netcdf::h5`] and unpacked through [`squallar_netcdf::cf`] — those two
//! modules are the NetCDF4/CF standard and not GLM's own, and this source is
//! their second consumer.
//!
//! # What the granule actually says, measured
//!
//! Every figure below was read off the granules named under "Provenance", not
//! taken from the product guide, and each one overturned a plausible reading.
//!
//! **The values are 0-255 counts, not Kelvin.** `data:units` says `"K"` and
//! `data:long_name` says `"0-255 Brightness Temperature"`; the `long_name` is
//! the honest one. Over all 15,000,000 samples of the reference LW granule
//! every value is an integer in `0..=255` — none is fractional and none is
//! outside the interval. A Kelvin-scaled ramp starting at 180 K renders the
//! layer **entirely blank**, which is what [`fields`] pins. Higher count is
//! colder, so the greyscale ascends; it is not negated.
//!
//! **The grid is separable in Mercator y, not uniform in latitude.** See
//! [`crate::hrrr::GridCoords::Separable`], whose doc carries the measurement.
//!
//! **The declared attributes cannot rebuild either axis.**
//! `geospatial_lat_resolution` and `geospatial_lon_resolution` both say
//! `0.0722`, where the longitude array steps `0.0720089` — 0.955° of drift
//! accumulated across the 4998 steps from column 1 to column 4999. In latitude
//! the error is far worse, because the axis is not uniform in latitude at all:
//! the declared corners `72.7154` and `-72.7368` interpolated linearly put row
//! 500 at `48.4653` where the array says `58.19307`, off by **9.73°**. Both
//! axes are read from the arrays.
//!
//! **`lat` and `lon` are two-dimensional `(yc, xc)` variables**, one value per
//! grid *point* — 15,000,000 floats each, 60 MB apiece — not the 1-D axes the
//! grid's separability would allow. [`decode`] verifies the separability it
//! then exploits rather than assuming it: over a strided sweep of the reference
//! granule, `lat(j, i) - lat(j, 0)` and `lon(j, i) - lon(0, i)` are both
//! **exactly** 0.
//!
//! **`_FillValue` is `-9999` and never occurs in a healthy granule.** All four
//! reference channels report 0 fill cells out of 15,000,000, and
//! `quality_information:total_number_retrievals` is the full `15000000` with
//! `percentage_optimal_retrievals = 100`. That is precisely why the fixture has
//! to plant one.
//!
//! # Two product generations share the bucket
//!
//! Up to mid-2025 each hour also carried a legacy McIDAS-derived granule named
//! `GLOBCOMP{LIR,SIR,VIS,WV}_nc.YYYYMMDDHH`, 4999 columns wide, `units = "none"`,
//! carrying none of the `geospatial_*` or `quality_information` attributes. It
//! is **no longer produced** — a listing of `GMGSI_LW/2026/08/20/12/` returns
//! the `v3r0_blend` object alone. This module reads the `v3r0_blend` product.
//!
//! **`GMGSI_SSR` is deliberately absent.** That fifth prefix still exists in the
//! bucket but the product was discontinued 2025-06-03. Do not add it.
//!
//! # Provenance
//!
//! Bucket `noaa-gmgsi-pds` (`https://noaa-gmgsi-pds.s3.amazonaws.com/`), the
//! four granules for **2025-06-01 12:00 UTC**, fetched 2026-08-22:
//!
//! | channel | key |
//! |---|---|
//! | LW  | `GMGSI_LW/2025/06/01/12/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc` |
//! | SW  | `GMGSI_SW/2025/06/01/12/GLOBCOMPSIR_v3r0_blend_s202506011200000_e202506011209599_c202506011237208.nc` |
//! | VIS | `GMGSI_VIS/2025/06/01/12/GLOBCOMPVIS_v3r0_blend_s202506011200000_e202506011242310.nc` |
//! | WV  | `GMGSI_WV/2025/06/01/12/GLOBCOMPWV_v3r0_blend_s202506011200000_e202506011209599_c202506011239397.nc` |
//!
//! ## The committed fixture, and what it costs
//!
//! `testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc`
//! is the LW granule above with **only the `data` and `dqf` values replaced**:
//!
//! - `lat` and `lon` carry the real granule's values, **byte-exact** — verified
//!   by comparing the re-read arrays element-for-element against the source.
//!   They are the whole of what [`crate::hrrr::GridCoords::Separable`] is
//!   tested against, and they cost only 446 KB of the file because a separable
//!   coordinate array shuffles-and-deflates 813:1 (`lat`) and 161:1 (`lon`).
//! - every global and per-variable attribute is the granule's own.
//! - `data` is a synthetic band ramp — constant along each row, stepping one
//!   count every 12 rows so the whole `0..=255` domain appears — with the real
//!   LW equator reading planted at `(row 1499, column 2500)` and one
//!   `_FillValue` planted at `(1000, 1000)`.
//!
//! ### Reproducing it
//!
//! The generator was a throwaway (campaign instruments do not land on main), so
//! the recipe is here instead. Against the LW granule named above:
//!
//! 1. Open it with `hdf5_pure::File::from_bytes` and read `lat` and `lon` with
//!    `read_f32()` — 15,000,000 values each — plus every attribute of the root
//!    group and of `data`, `lat`, `lon`, `dqf`, `time` and
//!    `quality_information`.
//! 2. Build `values: Vec<f32>` of `3000 * 5000` where
//!    `values[j * 5000 + i] = ((j / 12) % 256) as f32`, then set
//!    `values[1499 * 5000 + 2500] = 82.0` and
//!    `values[1000 * 5000 + 1000] = -9999.0`.
//! 3. Write a fresh `hdf5_pure::FileBuilder`: `data` at shape `[1, 3000, 5000]`
//!    with `with_chunks(&[1, 793, 1322])`, `with_shuffle()`, `with_deflate(5)`;
//!    `lat`/`lon` at `[3000, 5000]` with the values from step 1 verbatim,
//!    `with_chunks(&[3000, 5000])`, `with_shuffle()`, `with_deflate(9)`; `dqf`
//!    all-zero; `time` and `quality_information` carried across. Copy every
//!    attribute except the netCDF-internal `_NC*`, `_Netcdf4*`,
//!    `DIMENSION_LIST`, `CLASS` and `NAME`.
//!
//! Two things that do **not** work and cost an hour each if retried:
//! `File::open_rw` + `Dataset::write` refuses the NOAA file
//! (`EditUnsupported`: it tracks message creation order), and
//! `hdf5_pure::repack` refuses it too (`RepackUnsupported`: `data`'s
//! `DIMENSION_LIST` attribute is a VLEN of object references). Rebuilding
//! through `FileBuilder` is the path that works.
//!
//! **The cost**: the real 15-megapixel imagery is not committed (it is 6.96 MB
//! of the 7.5 MB granule, five times the largest fixture in this tree), so no
//! test here reads real satellite radiance — only the real geometry, the real
//! attributes and one real sample per channel. The file is also written by
//! `hdf5_pure::FileBuilder` rather than by NOAA's netCDF-4.7.0 writer, so the
//! netCDF dimension-scale linkage (`DIMENSION_LIST`, `_Netcdf4Coordinates`) is
//! absent; nothing here reads it, because [`squallar_netcdf::h5`] addresses datasets
//! by name. That the reader handles NOAA's *actual* bytes — chunked, shuffled,
//! deflate-5 — was verified by hand against all eight granules above and is
//! **not** re-run by any test in this tree.

use squallar_source::origins::DataSources;

pub mod decode;
pub mod fetch;
pub mod fields;
pub mod staging;

/// **The mosaic shape this build's byte budgets are sized for**, in points:
/// 3000 x 5000. A nominal figure, deliberately kept, and **not a claim about
/// what the product publishes today**.
///
/// It was written as "the one definition" of the product's shape, and it is
/// not one: every GMGSI granule dated 2026-09-03 is `[1, 3000, 4999]` on all
/// four channels (LW, SW, VIS, WV), against `[1, 3000, 5000]` on the 2025-06
/// and 2026-07 granules. The product's grid width moved and no build failed,
/// because nothing here is a shape the decoder enforces — [`decode::decode_in`]
/// reads `nj` and `ni` off the granule's own `data` variable.
///
/// **What it still means.** `GLOBAL_GRID_BYTES` (and through it
/// `GRID_CACHE_BYTES` and `FRAME_STAGING_BYTES`) is `GRID_POINTS` times four,
/// so this is the size one resident channel is priced at. A 4999-wide mosaic is
/// 59,988,000 B against the 60,000,000 B priced here: **12,000 B per grid, over
/// a budget of four of them**, so it over-provisions by 0.02 % and nothing is
/// under-counted. Held at the round shape on purpose — a budget that follows a
/// product's width from release to release buys nothing and moves four
/// `const _` pins every time it does.
///
/// **What it no longer means.** It is not the staging pool's capacity. That is
/// discovered at runtime from the granule being decoded; [`staging`] and
/// [`crate::staging`] carry the account. A reader who needs the shape a granule
/// actually has must read it off the granule.
pub const GRID_POINTS: usize = 3000 * 5000;

/// The four channels of the mosaic.
///
/// `SSR` is **not** here and must not be added: NOAA discontinued the
/// shortwave-solar-reflectance product on 2025-06-03. The `GMGSI_SSR/` prefix
/// survives in the bucket and will list objects for older dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GmgsiChannel {
    /// Longwave infrared window, ~10.8 um. Cloud-top temperature, day and night.
    LongwaveIr,
    /// Shortwave infrared, ~3.9 um. Fog and low cloud at night.
    ShortwaveIr,
    /// Visible, ~0.6 um. Daylight only.
    Visible,
    /// Water vapour, ~6.2 um. Mid-tropospheric moisture.
    WaterVapor,
}

// **The key space's tripwire**, for the reason `crate::mrms`'s copy of this
// block states in full: the GMGSI grid cache is a `HashMap` keyed by this enum
// and its byte budget is priced at one grid per entry of `all()`, so an `all()`
// that under-counts the enum is a budget under the real key space with every
// assert green. `index` is the exhaustive `match`; this block checks `all()` is
// dense and ordered under it. It does not prove completeness — that check is
// circular without a derive — so it is a tripwire at the point of change, and
// the byte ceiling remains what bounds the heap if one is walked past.
const _: () = {
    let all = GmgsiChannel::all();
    let mut i = 0;
    while i < all.len() {
        assert!(all[i].index() == i);
        i += 1;
    }
};

impl GmgsiChannel {
    pub const fn all() -> &'static [GmgsiChannel] {
        &[
            GmgsiChannel::LongwaveIr,
            GmgsiChannel::ShortwaveIr,
            GmgsiChannel::Visible,
            GmgsiChannel::WaterVapor,
        ]
    }

    /// A dense index over the variants, by a `match` with **no wildcard arm**.
    ///
    /// Its only caller is the `const _` above, and that is the point: a channel
    /// added to [`GmgsiChannel`] fails to compile *here*, one line from
    /// [`Self::all`], which is the list the grid cache's key space is counted
    /// from.
    const fn index(self) -> usize {
        match self {
            GmgsiChannel::LongwaveIr => 0,
            GmgsiChannel::ShortwaveIr => 1,
            GmgsiChannel::Visible => 2,
            GmgsiChannel::WaterVapor => 3,
        }
    }

    /// The persisted spelling. This lands in a user's config file and on the
    /// worker wire, so it may never be re-spelled.
    pub const fn as_str(self) -> &'static str {
        match self {
            GmgsiChannel::LongwaveIr => "GmgsiLongwaveIr",
            GmgsiChannel::ShortwaveIr => "GmgsiShortwaveIr",
            GmgsiChannel::Visible => "GmgsiVisible",
            GmgsiChannel::WaterVapor => "GmgsiWaterVapor",
        }
    }

    /// ASCII only, and that is enforced: `squallar-egui`'s
    /// `ui_glyphs::ui_string_literals_use_only_registered_glyphs` scans this
    /// crate's string literals and fails the build on an unregistered glyph.
    pub const fn display_name(self) -> &'static str {
        match self {
            GmgsiChannel::LongwaveIr => "Longwave IR",
            GmgsiChannel::ShortwaveIr => "Shortwave IR",
            GmgsiChannel::Visible => "Visible",
            GmgsiChannel::WaterVapor => "Water Vapor",
        }
    }

    /// The `GMGSI_*` key prefix this channel's objects live under.
    pub const fn prefix(self) -> &'static str {
        match self {
            GmgsiChannel::LongwaveIr => "GMGSI_LW",
            GmgsiChannel::ShortwaveIr => "GMGSI_SW",
            GmgsiChannel::Visible => "GMGSI_VIS",
            GmgsiChannel::WaterVapor => "GMGSI_WV",
        }
    }

    /// The leading token of every object name under [`Self::prefix`]. Note that
    /// shortwave is `SIR` and longwave is `LIR`, which is not the spelling
    /// either bucket prefix uses.
    pub const fn object_stem(self) -> &'static str {
        match self {
            GmgsiChannel::LongwaveIr => "GLOBCOMPLIR",
            GmgsiChannel::ShortwaveIr => "GLOBCOMPSIR",
            GmgsiChannel::Visible => "GLOBCOMPVIS",
            GmgsiChannel::WaterVapor => "GLOBCOMPWV",
        }
    }

    /// Resolved from the origin table rather than hardcoded: the Android
    /// network-security and web service-worker validations read [`DataSources`],
    /// so a bucket named anywhere else is invisible to both.
    pub fn bucket(self, sources: &DataSources) -> &str {
        &sources.gmgsi_bucket
    }
}

/// One channel's round: one listing, one granule, one request that either
/// answered or did not.
///
/// **[`Whole`](crate::fetch_policy::Whole), not assembled**: there is nothing
/// partial a GMGSI round can deliver.
pub struct GmgsiFetchResult(
    pub Result<decode::GmgsiGrid, squallar_source::fetch_policy::FetchError>,
);

impl crate::fetch_policy::FetchRound for GmgsiFetchResult {
    type Shape = crate::fetch_policy::Whole;
}

/// **What one frame listing found**, carried back to `apply_frame_listing` as
/// its scope.
///
/// The channel is captured at dispatch, not read back off the arriving pane:
/// the `PaneRef` a listing lands with is the union across panes and its config
/// is null by construction, so a listing taken for Longwave IR would otherwise
/// be filed under whatever channel the pane holds by then.
///
/// `keys` is the whole of what the listing bought — an hour and the object
/// name under it — because the name ends in an unpredictable creation stamp
/// and cannot be rebuilt from the hour.
///
/// **Public so a test can drive the real handler.** Every other frame payload
/// in this tree is private, which is why `loop_overlay_render_tests` had to
/// stand a double in the layer's place; a double cannot catch a layer that
/// files its frames wrongly.
pub struct GmgsiListing {
    pub channel: GmgsiChannel,
    pub range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    pub keys: Vec<(chrono::NaiveDateTime, String)>,
    /// Whether the hours listed were every hour in `range`.
    pub complete: bool,
}

/// **One loop frame's granule**, as its fetch hands it back.
///
/// `channel` and `valid` come off the dispatch rather than off the decoded
/// granule for the reason [`GmgsiListing`] states, and `grid: None` is a fetch
/// that failed: the frame is left without a picture rather than being given
/// another hour's.
pub struct GmgsiFrameFetch {
    pub channel: GmgsiChannel,
    pub valid: chrono::NaiveDateTime,
    pub grid: Option<decode::GmgsiGrid>,
}

/// Spelled as the trait and not as an inherent `from_str`, which is both this
/// tree's convention (see `MrmsProduct`) and what stops clippy's
/// `should_implement_trait` from firing.
impl std::str::FromStr for GmgsiChannel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        GmgsiChannel::all()
            .iter()
            .copied()
            .find(|c| c.as_str() == s)
            .ok_or(())
    }
}
