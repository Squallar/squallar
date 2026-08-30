//! gunzip → GRIB2 → one [`MrmsGrid`].
//!
//! Both templates MRMS uses are already in the `grib` feature set this crate
//! pins: grid definition **3.0** is pure Rust, and data representation **5.41**
//! (PNG) is covered by `png-unpack-with-png-crate`, which was already on. No new
//! feature, no new dependency, no C.
//!
//! ## The grid is built from section 3, never from `latlons()`
//!
//! [`SubMessage::latlons`] would work and would be wrong twice over. It
//! materialises a `(lat, lon)` pair per point — **392 MB** at 24.5 M points —
//! and the only [`GridCoords`] arm it can be poured into is
//! [`GridCoords::Explicit`], which answers `None` from both `index_bounds` and
//! `cell_span_degrees`. That sends `rasterize`'s `projection_window` back to the
//! full grid, so every pane would project all 24.5 M points on every re-render.
//! [`regular_grid`] reads the same seven numbers off section 3 instead.
//!
//! ## Peak memory of one decode
//!
//! `grib`'s PNG stage allocates the whole image buffer inside
//! `read_image_buffer` — 24.5 M × 2 bytes ≈ **49 MB** here — and `dispatch()`
//! hands back a **lazy** iterator over it. Streaming that into a
//! `Vec::with_capacity(ni * nj)` puts the peak at 49 MB + 98 MB ≈ 147 MB, plus
//! the ~1.4 MB of GRIB2 bytes. **Never `.collect()` an intermediate** and never
//! grow the buffer from empty: either turns the peak into 49 + 98 + 98 = 245 MB.

use std::io::Read;

use grib::{
    Grib2SubmessageDecoder, GridDefinitionTemplateValues, SubMessage,
    def::grib2::template::param_set,
};
use squallar_geo::GeoBounds;

use super::{MrmsGrid, MrmsProduct};
use crate::hrrr::{GridCoords, SCAN_ALTERNATING, SCAN_J_CONSECUTIVE};
use crate::render::gridded::ResidentGrid;

/// MRMS's "no radar coverage" sentinel — the ocean, the gaps between umbrellas,
/// and everything outside the radar network.
///
/// It is also the **reference value of the packing**, so it is the value every
/// zero-coded point decodes to. Both shipped products use it, which is the only
/// thing about the sentinel set that *is* common to them; see
/// [`MrmsProduct::missing_codes`].
pub const NO_COVERAGE: f32 = -999.0;

/// How close a decoded value must sit to a reserved code to be one.
///
/// A tolerance rather than `==` because the values arrive through a 16-bit
/// scaled integer and an `f32` divide: they come back exact on today's packing,
/// but a decimal-scale change upstream would land one a ULP away, and a sentinel
/// that missed by a ULP is a third of a continent reporting a fabricated
/// reading.
///
/// **0.05 is a tenth of the packing's own quantum** (decimal scale 1, so values
/// land on 0.1 boundaries), which is what stops it eating the reading next door:
/// the composite's real −3.0 dBZ returns sit 0.1 from the rate's −3.0 code, and
/// the two are told apart by the *product*, never by this window.
const SENTINEL_EPSILON: f32 = 0.05;

/// One decoded grid point of `product`, with that product's own in-band codes
/// turned into `f32::NAN`.
///
/// Section 6 of every MRMS granule carries `bitmap_indicator = 255` — **there is
/// no bitmap** — so nothing in the decode produces `NaN` on its own, and the
/// reserved codes travel as ordinary `f32` unless this function stops them.
/// Which numbers are reserved is a fact about the product and lives in
/// [`MrmsProduct::missing_codes`], measured rather than assumed: the rate's
/// no-coverage code is **−3**, not the composite's −99.
///
/// ## What that actually costs, measured rather than assumed
///
/// The obvious claim is that an unmapped −999 paints the ocean solid, the way
/// `render/rasterize/model_nan_tests.rs` documents for the model ramps. **On
/// today's code that claim is false, and it is worth writing down why.**
/// `ModelParameter::color_for_value` dispatches to descending `if` chains that
/// end in an unguarded `else`, which is where a value below every branch lands;
/// [`crate::render::gridded::color_for`] does not. It is bounded at both ends —
/// transparent below the first stop, clamped above the last — and every code
/// here is below both bars' first stops, so they drop out. Deleting the call to
/// this function changes **no pixel** of either mosaic, and a test claiming
/// otherwise would be checking a belief rather than the code. Tamper-verified:
/// pushing `raw` instead of `to_reading(product, raw)` leaves every painting
/// assertion green and fires the reading ones.
///
/// What it does change, and what the tests below assert instead:
///
/// * **the reading a hover reports.** `format_value` prints nothing for a
///   non-finite value, so a pointer over the Gulf gets no tooltip; unmapped, it
///   would read `CREF: -999.0 dBZ` or `Rate: -3.00 mm/h`;
/// * **`value_range`,** and so the blank notice built from it, which would quote
///   a reserved code back at the user as the mosaic's minimum. That is not
///   hypothetical: the live rate fetch reported a range starting at −3 mm/h
///   before this became per-product, which is how the rate's code was found;
/// * **every future bar.** The transparency above is a property of *these two
///   scales' first stops*, not of the format. A product whose bar reached below
///   its own codes — a diverging one, a velocity-shaped one — would paint a
///   sentinel for real, and it would be registered in `fields.rs` by someone who
///   never read this file.
///
/// Sentinel knowledge lives here rather than in the rasterizer deliberately: the
/// raster keeps its NaN contract and never learns what any one format spells
/// "missing" as.
#[inline]
pub fn to_reading(product: MrmsProduct, raw: f32) -> f32 {
    reading(product.missing_codes(), raw)
}

/// [`to_reading`] against an explicit code set rather than a [`MrmsProduct`].
///
/// The 3D stack (`volume`) reads a product that is **not** an [`MrmsProduct`] —
/// it has no colour bar, no persisted spelling and no `fields.rs` row, because
/// nothing draws it — so it names its codes directly. Both spellings run this
/// one body: a second sentinel test written out again is the shape that let
/// radar's reflectivity ladder drift a whole band from this crate's.
#[inline]
pub fn reading(missing: &[f32], raw: f32) -> f32 {
    if !raw.is_finite() {
        return f32::NAN;
    }
    for &code in missing {
        if (raw - code).abs() < SENTINEL_EPSILON {
            return f32::NAN;
        }
    }
    raw
}

/// Ungzip a `.grib2.gz` body.
///
/// Capped: a corrupt or hostile member could otherwise decompress without
/// bound, and a browser tab has 4 GiB of address space to lose. The cap is ~7×
/// the largest granule observed (1.4 MB), which is slack enough that a real file
/// growing is not mistaken for an attack.
const MAX_GRIB_BYTES: u64 = 10 * 1024 * 1024;

pub fn gunzip(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(body)
        .take(MAX_GRIB_BYTES)
        .read_to_end(&mut out)
        .map_err(|e| format!("MRMS gunzip failed: {e}"))?;
    if out.is_empty() {
        return Err("MRMS gunzip produced no bytes".into());
    }
    if out.len() as u64 == MAX_GRIB_BYTES {
        return Err(format!(
            "MRMS granule decompresses past the {MAX_GRIB_BYTES}-byte cap; \
             refused rather than truncated, since a truncated GRIB2 decodes to \
             a plausible partial mosaic"
        ));
    }
    Ok(out)
}

/// [`GridCoords::Regular`] from a plate-carrée section 3, in **closed form**.
///
/// The steps are taken end-to-end (`(last - first) / (n - 1)`) rather than from
/// the `i_direction_inc` / `j_direction_inc` octets, which is what `grib`'s own
/// `evenly_spaced_degrees` does: those octets are `0xffffffff` — "not given" —
/// on plenty of real products, and the corner pair is always present.
///
/// Longitudes are wrapped into `[-180, 180)` the way `grib::grid::normalize_latlon`
/// does, so MRMS's section-3 `230.005..299.995` becomes `-129.995..-60.005`.
/// **The wrap is applied to `lon0` only**, never per point: `dlon` is a
/// difference and adding 360 to both ends leaves it alone, and re-wrapping every
/// point is what would put a seam in the middle of a grid that has none.
fn regular_grid(grid: &param_set::LatLonGrid) -> Result<GridCoords, String> {
    let (ni, nj) = (grid.grid.ni as usize, grid.grid.nj as usize);
    if ni < 2 || nj < 2 {
        return Err(format!(
            "MRMS grid is {ni}×{nj}; a regular grid needs at least two points \
             on each axis for its steps to be defined"
        ));
    }

    // The same unit rule `grib::grid::AngleUnit for Grid` applies: 1e-6 degrees
    // unless section 3 states a basic angle and its subdivisions.
    let basic = grid.grid.initial_production_domain_basic_angle;
    let subdivisions = grid.grid.basic_angle_subdivisions;
    let unit = if basic == 0 {
        1e-6
    } else {
        if subdivisions == 0 || subdivisions == u32::MAX {
            return Err(format!(
                "MRMS section 3 states basic angle {basic} with unusable \
                 subdivisions {subdivisions}"
            ));
        }
        f64::from(basic) / f64::from(subdivisions)
    };

    let lat0 = f64::from(grid.grid.first_point_lat) * unit;
    let lat1 = f64::from(grid.grid.last_point_lat) * unit;
    let dlat = (lat1 - lat0) / (nj - 1) as f64;

    // Longitude is unsigned in the octets, so a westward domain arrives as
    // `230e6..300e6` and a domain that crosses the prime meridian arrives with
    // `last < first`. Reconcile against the scanning direction exactly as
    // `evenly_spaced_longitudes` does, before taking the step.
    let scan = grid.scanning_mode.0;
    let scans_positively_for_i = scan & 0b1000_0000 == 0;
    let (raw_first, raw_last) = (
        f64::from(grid.grid.first_point_lon),
        f64::from(grid.grid.last_point_lon),
    );
    let consistent = (raw_last > raw_first) == scans_positively_for_i;
    let (start, end) = if consistent {
        (raw_first, raw_last)
    } else if raw_first > raw_last {
        (raw_first, raw_last + 360_000_000.0)
    } else {
        (raw_first + 360_000_000.0, raw_last)
    };
    let lon0 = wrap_longitude(start * unit);
    let dlon = ((end - start) * unit) / (ni - 1) as f64;

    if !(lat0.is_finite() && lon0.is_finite() && dlat.is_finite() && dlon.is_finite())
        || dlat == 0.0
        || dlon == 0.0
    {
        return Err(format!(
            "MRMS section 3 yields a degenerate grid: lat0={lat0} lon0={lon0} \
             dlat={dlat} dlon={dlon}"
        ));
    }

    Ok(GridCoords::Regular {
        lat0,
        lon0,
        dlat,
        dlon,
        ni,
        nj,
        scan_mode: scan & (SCAN_J_CONSECUTIVE | SCAN_ALTERNATING),
    })
}

/// Into `[-180, 180)`, matching `grib::grid::helpers::normalize_latlon`.
fn wrap_longitude(lon: f64) -> f64 {
    (lon + 540.0) % 360.0 - 180.0
}

/// The grid coordinates for a submessage, refusing anything that is not a plain
/// lat/lon grid.
///
/// **Refused rather than fallen back to [`GridCoords::Explicit`]**: that arm
/// would be 392 MB of coordinates *and* would disable windowing, so it is not a
/// degraded answer, it is a different product. A template this build does not
/// read is a change in what NOAA publishes, and the layer says so.
fn grid_coords<R>(submessage: &SubMessage<'_, R>) -> Result<GridCoords, String> {
    let grid_def = submessage.grid_def();
    let template = GridDefinitionTemplateValues::try_from(grid_def)
        .map_err(|e| format!("MRMS: cannot read grid definition: {e}"))?;
    let GridDefinitionTemplateValues::Template0(ref plain) = template else {
        return Err(format!(
            "MRMS: grid definition template {} is not the plain lat/lon \
             template 3.0 this decoder reads",
            grid_def.grid_tmpl_num(),
        ));
    };
    let coords = regular_grid(&plain.lat_lon)?;
    let declared = grid_def.num_points() as usize;
    if coords.len() != declared {
        return Err(format!(
            "MRMS grid point count mismatch: {declared} declared in section 3 \
             vs {} from its own shape",
            coords.len(),
        ));
    }
    Ok(coords)
}

/// `!= 1`, not `< 1` — the same reasoning as `hrrr::fetch`: **two** submessages
/// means the bytes span a record boundary, which decodes fine as a sequence and
/// produces a plausible mosaic of the wrong field.
fn exactly_one_submessage(count: usize) -> Result<(), String> {
    if count != 1 {
        return Err(format!(
            "MRMS: expected exactly one GRIB2 submessage, found {count}"
        ));
    }
    Ok(())
}

/// One MRMS granule as GRIB2 states it, before any product framing.
///
/// The half of a decode that is the same for every object in the bucket: the
/// grid, its envelope, its valid time and its values. [`parse_grib2`] wraps this
/// into an [`MrmsGrid`] -- a paint, a legend and a `fields.rs` row; the 3D stack
/// takes it plain, because it has none of those and draws nothing.
pub struct RawGrid {
    pub ni: usize,
    pub nj: usize,
    pub coords: GridCoords,
    pub bounds: GeoBounds,
    /// Section 1's reference time, which for MRMS **is** the valid time.
    pub valid: chrono::NaiveDateTime,
    /// Section 4's **first fixed surface** as `(type, value)`, when the product
    /// definition template carries one: the code-table 4.5 surface type and the
    /// value in that surface's own unit (metres for types 102 and 103).
    ///
    /// `None` for a template `grib` does not read a surface out of, which the
    /// two shipped 2D products do not need and the 3D stack refuses --
    /// `super::volume` reads the height back off the granule so the level
    /// table is checked against the data rather than against a directory name.
    pub first_fixed_surface: Option<(u8, f64)>,
    /// In the grid's own scanning order, with `missing` already mapped to
    /// `f32::NAN`.
    pub values: Vec<f32>,
}

/// Decode one MRMS granule's GRIB2 bytes.
pub fn parse_grib2(bytes: &[u8], product: MrmsProduct) -> Result<MrmsGrid, String> {
    let raw = parse_grib2_raw(bytes, product.missing_codes())?;

    let paint = crate::render::gridded::field_paint(&super::fields::spec(product).id)
        .ok_or_else(|| format!("MRMS product {} registers no paint", product.as_str()))?;
    let (visible_points, value_range) =
        crate::hrrr::summarize_values(&raw.values, |v| paint.paints(v));

    Ok(MrmsGrid {
        product,
        grid: std::sync::Arc::new(ResidentGrid {
            field: super::fields::spec(product).id.clone(),
            ni: raw.ni,
            nj: raw.nj,
            coords: raw.coords,
            values: raw.values,
        }),
        bounds: raw.bounds,
        valid: raw.valid,
        visible_points,
        value_range,
    })
}

/// Decode one MRMS granule's GRIB2 bytes into its [`RawGrid`], mapping every
/// value within `SENTINEL_EPSILON` of a code in `missing` to `f32::NAN`.
///
/// **`missing` is a parameter and not a lookup**, for the reason
/// [`MrmsProduct::missing_codes`] gives at length: which numbers a product
/// reserves is a fact about that product, and taking one product's set for
/// another is what left a third of the rate mosaic reporting -3 mm/h as a
/// measurement.
pub fn parse_grib2_raw(bytes: &[u8], missing: &[f32]) -> Result<RawGrid, String> {
    let grib2 = grib::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| format!("MRMS GRIB2 parse error: {e}"))?;

    // Its own pass, before any `SubMessage` is held: grib's iterator borrows the
    // reader through a `RefCell`, so advancing it while a submessage is alive
    // panics with "RefCell already borrowed".
    exactly_one_submessage(grib2.iter().count())?;

    let (_index, submessage) = grib2
        .iter()
        .next()
        .ok_or_else(|| "MRMS: no submessages in GRIB2 data".to_string())?;

    let coords = grid_coords(&submessage)?;
    let (ni, nj) = submessage
        .grid_shape()
        .map_err(|e| format!("MRMS: cannot determine grid shape: {e}"))?;

    let valid = reference_time(&submessage)?;
    let first_fixed_surface = submessage
        .prod_def()
        .fixed_surfaces()
        .map(|(first, _second)| (first.surface_type, first.value()));

    // Read before the submessage is consumed by the decoder.
    let decoder = Grib2SubmessageDecoder::from(submessage)
        .map_err(|e| format!("MRMS decode init error: {e}"))?;

    // **Pre-sized, and streamed.** `dispatch()` is lazy over grib's PNG image
    // buffer; `collect()` here would hold that buffer, a fresh 98 MB result and
    // the growth copies at once. See this module's header.
    let mut values: Vec<f32> = Vec::with_capacity(ni * nj);
    for raw in decoder
        .dispatch()
        .map_err(|e| format!("MRMS decode error: {e}"))?
    {
        values.push(reading(missing, raw));
    }

    if values.is_empty() {
        return Err("MRMS: no grid points decoded".into());
    }
    if values.len() != ni * nj {
        return Err(format!(
            "MRMS decoded {} values for a {ni}×{nj} grid ({} points)",
            values.len(),
            ni * nj,
        ));
    }

    // One streaming pass; the coordinates are seven scalars, so nothing is
    // materialised to walk them.
    let Some(bounds) = GeoBounds::from_points((0..coords.len()).map_while(|i| coords.at(i))) else {
        return Err("MRMS grid decoded no coordinates".into());
    };
    crate::hrrr::fetch::check_domain_longitude(&bounds, &super::MRMS_DOMAIN_LON, "MRMS")?;

    Ok(RawGrid {
        ni,
        nj,
        coords,
        bounds,
        valid,
        first_fixed_surface,
        values,
    })
}

/// Section 1's reference time, which for MRMS **is** the valid time: every
/// product in this bucket is an analysis, published under the minute it depicts.
///
/// A malformed time is a hard error rather than `unwrap_or_default()`: 1970 is
/// rendered by the pane control as an oddly-timed mosaic rather than as corrupt
/// data, and this layer's whole clock story is "the latest granule".
fn reference_time<R>(submessage: &SubMessage<'_, R>) -> Result<chrono::NaiveDateTime, String> {
    let raw = submessage.temporal_raw_info();
    let t = &raw.ref_time_unchecked;
    let date = chrono::NaiveDate::from_ymd_opt(t.year as i32, t.month as u32, t.day as u32)
        .ok_or_else(|| {
            format!(
                "MRMS reference date is not a real date: {}-{:02}-{:02}",
                t.year, t.month, t.day
            )
        })?;
    let clock = chrono::NaiveTime::from_hms_opt(t.hour as u32, t.minute as u32, t.second as u32)
        .ok_or_else(|| {
            format!(
                "MRMS reference time is not a real time: {:02}:{:02}:{:02}",
                t.hour, t.minute, t.second
            )
        })?;
    Ok(chrono::NaiveDateTime::new(date, clock))
}

#[cfg(test)]
mod tests;
