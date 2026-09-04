//! gunzip → GRIB2 → one [`MrmsGrid`].
//!
//! Both templates MRMS uses are already in the `grib` feature set this crate
//! pins: grid definition **3.0** is pure Rust, and data representation **5.41**
//! (PNG) is covered by `png-unpack-with-png-crate`, which was already on. No new
//! feature, no C, and **no new package in the lock**: `png` is named directly in
//! this crate's manifest now, at the version `grib` already resolved, because
//! [`decode_png_into`] reads section 7 itself.
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
//! **Measured 2026-08-31 with a counting `#[global_allocator]`, denominator one
//! `parse_grib2_raw_in` call over a committed granule:**
//!
//! | | cold pool | warm pool |
//! |---|---|---|
//! | peak live | 98.4 MB | **0.43 MB** |
//! | blocks ≥ 1 MiB | 1 (the values vector) | **0** |
//!
//! Neither half of the old 147 MB is left. The 98 MB values vector is not
//! *allocated* per granule at all — it comes from [`super::staging`], the one
//! retained mosaic buffer
//! [`FRAME_STAGING_BYTES`](super::FRAME_STAGING_BYTES) has always described, so
//! it is a once-per-process cost and the warm row is the steady state. And the
//! 49 MB that used to sit beside it is gone rather than pooled: `grib`'s 5.41
//! arm materialises the entire decompressed image in `read_image_buffer`'s
//! `vec![0; output_buffer_size()]` — 24.5 M × 2 bytes = **49,000,000 B exactly,
//! measured, per granule** — before it yields a value, and this module no
//! longer asks it to. [`decode_png_into`] streams section 7 a PNG row at a
//! time, 14,000 B at the mosaic's width, and `tests` pins it value for value
//! against `grib`'s own decode.
//!
//! That buffer was `vec![0; n]`: **infallible**, on a target where an
//! allocation failure traps and nothing unwinds. It is what the Tier-2
//! `firefox.huge` leg caught trapping at 910 MiB of 1024, symbolising to
//! `handle_alloc_error` under `Grib2SubmessageDecoder::dispatch`. Removing it
//! removes a trap, not just some bytes — which is why the fallback arm below
//! matters and is gated: a granule that quietly fell back to `grib` would be
//! *correct* and would allocate 49 MB again.
//!
//! **Never `.collect()` an intermediate** and never grow the buffer from
//! empty: either puts a second and third 98 MB block beside the first.
//!
//! On the arm that must still allocate — a cold pool, a contended slot, a grid
//! that is not mosaic-shaped — the reserve is **fallible**, and that is not
//! tidiness: on wasm32 the whole module lives in a memory capped at 1 GiB, an
//! allocation failure there aborts without unwinding, and an abort inside a
//! frame leaves winit's event loop permanently borrowed. [`parse_grib2_raw`]
//! carries the full reasoning at the reserve itself.

use std::io::Read;

use grib::{
    Grib2SubmessageDecoder, GridDefinitionTemplateValues, SubMessage,
    def::grib2::{DataRepresentationTemplate, template::param_set},
};
use squallar_geo::GeoBounds;

use super::{MrmsGrid, MrmsProduct};
use crate::hrrr::{GridCoords, SCAN_ALTERNATING, SCAN_J_CONSECUTIVE};
use crate::render::gridded::{GridValues, ResidentGrid, ScaledU16};

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
    /// Section 4's `(parameter category, parameter number)`, when the product
    /// definition template carries them.
    ///
    /// **What tells two MRMS products apart when everything else agrees.** The
    /// 2D column-max composite and the 3D 0.50 km level declare the *same*
    /// first fixed surface -- (102, 500 m) -- the same grid, the same packing
    /// and the same envelope, so a height check alone cannot tell one from the
    /// other. The category can: see `super::volume::PARAMETER`.
    pub parameter: Option<(u8, u8)>,
    /// In the grid's own scanning order, with `missing` already mapped to
    /// `f32::NAN` — as a reading, whichever width the store is in.
    ///
    /// [`GridValues::Scaled`] for every granule MRMS actually publishes; see
    /// [`parse_grib2_raw_in`] for when it is not.
    pub values: GridValues,
}

/// Decode one MRMS granule's GRIB2 bytes.
pub fn parse_grib2(bytes: &[u8], product: MrmsProduct) -> Result<MrmsGrid, String> {
    let raw = parse_grib2_raw(bytes, product.missing_codes())?;

    let paint = crate::render::gridded::field_paint(&super::fields::spec(product).id)
        .ok_or_else(|| format!("MRMS product {} registers no paint", product.as_str()))?;
    // `summarize` and not `summarize_values_iter(values.iter(), ..)`: this walks
    // the whole mosaic — 24.5 M points at CONUS — and the former matches the
    // storage arm once, outside the loop.
    let (visible_points, value_range) = raw.values.summarize(|v| paint.paints(v));

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
    parse_grib2_raw_in(bytes, missing, super::staging::global())
}

/// [`parse_grib2_raw`] against an explicit staging pool rather than the
/// process-wide one.
///
/// **Public so a suite can drive the real decoder over a slot it owns.** The
/// counters this module's fix turns on are process-global on the shipped path,
/// and a filtered run in this workspace is explicitly not self-contained: a
/// test reading the global slot's totals cannot tell its own reuse from the
/// reuse another test in the same binary left behind. Every shipped caller goes
/// through [`parse_grib2_raw`]; nothing chooses a pool at runtime.
pub fn parse_grib2_raw_in(
    bytes: &[u8],
    missing: &[f32],
    staging: &super::staging::StagingPool,
) -> Result<RawGrid, String> {
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
    let parameter = submessage
        .prod_def()
        .parameter_category()
        .zip(submessage.prod_def().parameter_number());

    // **Pre-sized, streamed, and — since the staging pool — usually not
    // allocated at all.** `collect()` here would hold a fresh 98 MB result and
    // the growth copies at once. See this module's header.
    //
    // `super::staging` carries the full reasoning for the retained buffer, and
    // the short version is that a *fresh* 98 MB block per granule is what
    // killed the page. wasm32 linear memory only grows; ~147 MB of large-block
    // churn per granule — this vector, plus the 49 MB PNG image buffer the
    // decode below no longer takes — fragmented a 1 GiB heap until a 98 MB
    // request could not be served contiguously out of a free pool twice its
    // size. Measured 2026-08-31: a pane with the layer set
    // enabled and a loop playing, no input at all, hit that at ~122 s on
    // Firefox 154 and Chromium 151 alike — 0.3 s apart, because dlmalloc is
    // compiled into the module and both engines run one allocator over one
    // request sequence.
    //
    // The pool's own `take` keeps `try_reserve_exact`, not `with_capacity`, on
    // the arm that must still allocate: this is the largest single allocation
    // the app makes anywhere, and on wasm32 it is made against a memory with a
    // **hard 1 GiB ceiling** (`--max-memory=1073741824`, set in
    // `.github/scripts/wasm-threads.sh` because a shared memory has to declare
    // one at link time). An infallible allocation the engine cannot serve calls
    // `handle_alloc_error`, which aborts — and wasm32-unknown-unknown is
    // `panic-strategy = "abort"`, so **nothing unwinds**: every `RefCell` guard
    // live on the stack at that instant is never dropped. When the abort lands
    // inside a frame, winit's web event-loop runner keeps its own `RefCell`
    // borrowed for the life of the page (`web/event_loop/runner.rs:599` then
    // panics `already borrowed` on the next timer wake), the frame loop stops
    // for good, and the canvas keeps its last painted frame while
    // `requestAnimationFrame`, the network and the workers all carry on — a
    // silent freeze that every screenshot and rAF check reports as healthy.
    // That reserve is the net that keeps the page alive; the pool is the cure,
    // and the net stays because a net that is never needed costs nothing.
    //
    // `checked_mul` for the same reason `pmtiles` grew one: `usize` is 32 bits
    // on this target, so a malformed section 3 could wrap `ni * nj` to a small
    // capacity in release and hand the loop below a `Vec` that grows from
    // nothing — the exact peak this module's header forbids.
    let points = ni
        .checked_mul(nj)
        .ok_or_else(|| format!("MRMS: a {ni}×{nj} grid overflows this target's index width"))?;

    // **Which width this granule is stored in, decided from what section 5
    // declares — never from which source is asking.**
    //
    // `png_stream_plan` admits any non-zero multiple of eight, so 8, 16, 24 and
    // 32 all reach here. Only `num_bits <= 16` may take the narrow arm, and it
    // takes it through a `Vec<u16>`: a 24-bit granule *cannot* be built into
    // that arm, so it is stored whole as `f32` — **correct and unhalved, never
    // truncated**. The grib fallback below is `f32` for a different reason
    // again: `dispatch()` yields values with no code behind them at all.
    //
    // The reserved-code set is settled here too, before a mosaic is decoded
    // into a buffer that might have to be widened again: it is a function of
    // the packing alone, and a packing whose tolerance swallows more codes than
    // `MAX_NAN_CODES` declines the narrow arm rather than putting a long list
    // on the sampling path.
    let plan = png_stream_plan(bytes, &submessage, points);
    let narrow = plan
        .as_ref()
        .filter(|plan| plan.simple.num_bits <= 16)
        .and_then(|plan| {
            let two_pow = 2_f32.powi(i32::from(plan.simple.exp));
            let dig_factor = 10_f32.powi(-i32::from(plan.simple.dec));
            ScaledU16::nan_codes_for(plan.simple.ref_val, two_pow, dig_factor, |v| {
                !reading(missing, v).is_finite()
            })
            .map(|nan_codes| (two_pow, dig_factor, nan_codes))
        });

    let too_big = |width: usize| {
        format!(
            "MRMS: cannot hold a {ni}×{nj} grid ({} MB of values) in this \
             build's memory",
            points.saturating_mul(width) / (1024 * 1024),
        )
    };

    let values = match (plan, narrow) {
        (Some(plan), Some((two_pow, dig_factor, nan_codes))) => {
            // The pooled buffer, in the store's own width. See `super::staging`.
            let mut codes = staging
                .take(points)
                .map_err(|_| too_big(size_of::<u16>()))?;
            decode_png_codes_into(&plan, points, &mut codes)?;
            GridValues::Scaled(ScaledU16 {
                codes,
                ref_val: plan.simple.ref_val,
                two_pow,
                dig_factor,
                nan_codes,
            })
        }
        (Some(plan), None) => {
            // Streamable, but wider than the narrow arm holds. Fallible for the
            // reason the pool's own reserve is; see this module's header.
            let mut floats: Vec<f32> = Vec::new();
            floats
                .try_reserve_exact(points)
                .map_err(|_| too_big(size_of::<f32>()))?;
            decode_png_into(&plan, points, missing, &mut floats)?;
            GridValues::F32(floats)
        }
        (None, _) => {
            // Consumes the submessage, so it stays inside the arm that needs it.
            let decoder = Grib2SubmessageDecoder::from(submessage)
                .map_err(|e| format!("MRMS decode init error: {e}"))?;
            let mut floats: Vec<f32> = Vec::new();
            floats
                .try_reserve_exact(points)
                .map_err(|_| too_big(size_of::<f32>()))?;
            for raw in decoder
                .dispatch()
                .map_err(|e| format!("MRMS decode error: {e}"))?
            {
                floats.push(reading(missing, raw));
            }
            GridValues::F32(floats)
        }
    };

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
        parameter,
        values,
    })
}

/// **What the streaming PNG path needs, when this granule is one it can read.**
///
/// `payload` is a slice of the caller's own GRIB2 bytes -- section 7 minus its
/// five-octet header -- and `simple` is the packing section 5 declares.
struct PngPlan<'b> {
    payload: &'b [u8],
    simple: param_set::SimplePacking,
    /// `num_bits / 8`, and the reason the flat-buffer walk is a byte walk.
    sample_bytes: usize,
}

/// A whole GRIB2 section, header and all, out of the caller's own buffer.
///
/// `SectionInfo`'s `offset` and `size` are public and are indices into exactly
/// the bytes [`parse_grib2_raw_in`] was handed, so a section can be borrowed
/// rather than read back out through `grib`'s reader -- which is `pub(crate)`
/// anyway, and which for section 7 copies the whole ~1.3 MB compressed
/// payload.
fn section_bytes(bytes: &[u8], offset: usize, size: usize) -> Option<&[u8]> {
    let end = offset.checked_add(size)?;
    bytes.get(offset..end)
}

/// The same section without its five-octet header.
///
/// **Sections 5 and 7 want different halves of this and mixing them up decodes
/// nothing.** `Section5::try_from_slice` parses the header as part of the
/// section -- it reports `SectionHeader { len, sect_num }` -- so section 5 goes
/// in whole; `Grib2SubmessageDecoder::sect7_payload` is `&sect7_bytes[5..]`, so
/// section 7's data starts after it. Getting this backwards is silent: the
/// template number reads as garbage, [`png_stream_plan`] answers `None`, and
/// the decode falls back to `grib` and is *correct* — just still allocating the
/// 49 MB buffer the fallback exists to avoid. That is what
/// `tests::every_shipped_granule_is_streamable` is for.
fn section_payload(bytes: &[u8], offset: usize, size: usize) -> Option<&[u8]> {
    section_bytes(bytes, offset, size)?.get(5..)
}

/// **Whether this granule's values can be streamed off section 7 a PNG row at
/// a time, and the three things that takes.**
///
/// `None` for anything at all unusual, and the caller then runs `grib`'s own
/// `dispatch()`. This is a *narrowing*, never a reimplementation of GRIB2: the
/// conditions below are exactly the ones under which `grib`'s 5.41 arm reduces
/// to "walk the decompressed image as a flat array of big-endian samples", and
/// `decode::tests` pins the two paths equal value for value on every committed
/// fixture.
///
/// * **data representation template 5.41** with `orig_field_type == 0`, the
///   only value of code table 5.1 `grib`'s own unpack arms accept either;
/// * **`num_bits` a non-zero multiple of eight.** PNG only ever carries 1, 2,
///   4, 8, 16, 24 or 32 bits a sample, and at a multiple of eight a sample is a
///   whole number of bytes, so a row is `width` big-endian integers and no
///   sample straddles a row boundary. `grib`'s `NBitwiseIterator` walks the
///   image as one uninterrupted bit stream; the two agree **only** because of
///   that alignment, which is why anything else falls back rather than being
///   handled;
/// * **no bitmap** -- section 6's indicator is 255 -- so every grid point is
///   encoded, in order, and `num_encoded_points` is the whole grid. Every MRMS
///   granule is bitmap-free (this module's header says so and
///   `to_reading`'s doc explains what that costs), but a granule that grew one
///   must go to `grib`, which knows how to expand it.
fn png_stream_plan<'b, R>(
    bytes: &'b [u8],
    submessage: &SubMessage<'_, R>,
    points: usize,
) -> Option<PngPlan<'b>> {
    let bitmap_indicator = *bytes.get(submessage.6.body.offset.checked_add(5)?)?;
    if bitmap_indicator != 255 {
        return None;
    }

    let sect5 = section_bytes(bytes, submessage.5.body.offset, submessage.5.body.size)?;
    let sect7 = section_payload(bytes, submessage.7.body.offset, submessage.7.body.size)?;

    // **`grib` parses section 5, not this module.** A `Grib2SubmessageDecoder`
    // is the only public door to `Section5`, so one is built purely to read it
    // and never dispatched. `num_points_total` is **0 on purpose**: it is used
    // for exactly one thing inside `new` -- sizing the all-ones dummy bitmap a
    // bitmap-free granule gets -- and at zero points that vector is empty. The
    // real figure is `points`, which would allocate 3 MB here and 3 MB again in
    // the `append` that grows section 6 onto it, for a bitmap this path has
    // just established says nothing. Passing the true count and throwing the
    // bitmap away would cost 6.1 MB a granule (measured) to learn four numbers.
    let params =
        Grib2SubmessageDecoder::new(0, sect5.to_vec(), vec![0, 0, 0, 0, 0, 255], Vec::new())
            .ok()?;
    let section5 = params.section5();
    let DataRepresentationTemplate::_5_41(ref template) = section5.payload.template else {
        return None;
    };
    if template.orig_field_type != 0 {
        return None;
    }
    let simple = template.simple.clone();
    if simple.num_bits == 0 || !simple.num_bits.is_multiple_of(8) {
        return None;
    }
    // A bitmap-free granule encodes every point, so anything else means the
    // sections disagree and `grib`'s own consistency check should be the one
    // to say so.
    if section5.payload.num_encoded_points as usize != points {
        return None;
    }

    let sample_bytes = usize::from(simple.num_bits / 8);
    Some(PngPlan {
        payload: sect7,
        simple,
        sample_bytes,
    })
}

/// **Stream `plan`'s PNG a row at a time into `values`, unscaled and
/// sentinel-mapped.**
///
/// The whole point of this function, and the reason it exists beside a `grib`
/// call that already works: `grib`'s 5.41 arm materialises the **entire**
/// decompressed image before it yields a single value --
/// `read_image_buffer`'s `vec![0; output_buffer_size()]`, which at MRMS's
/// 7000x3500 16-bit mosaic is **49,000,000 B, measured, per granule** -- and
/// then hands back a lazy iterator over it. That buffer is `vec![0; n]`:
/// infallible, and on wasm32 an infallible allocation the engine cannot serve
/// calls `handle_alloc_error` against a memory with a hard 1 GiB ceiling on a
/// target where **nothing unwinds**. `staging`'s account of what that does to
/// the page applies here unchanged; the difference is that the staging buffer
/// could be retained and this one cannot, because `grib` allocates it inside a
/// private function and moves it into the iterator.
///
/// A row is `width` samples and nothing else is live, so the same decode's
/// image-side cost is **14,000 B** at the mosaic's width. The `png` reader is
/// asked to fill a buffer this function owns (`read_row`) rather than its own
/// (`next_row`), so there is one row buffer in the process and not two.
///
/// This is the decision `regular_grid` already makes one section earlier, for
/// the same reason: `grib` will answer with a materialised whole, section 3's
/// coordinates or section 7's values, and at 24.5 M points neither fits a
/// budget the browser gives us. Reading the seven numbers, or the one row, is
/// what fits.
fn decode_png_into(
    plan: &PngPlan<'_>,
    points: usize,
    missing: &[f32],
    values: &mut Vec<f32>,
) -> Result<(), String> {
    // Hoisted out of the sample loop, and this is not a rearrangement of the
    // arithmetic: `grib`'s `NonZeroSimplePackingDecoder` computes exactly
    // `(ref_val + encoded * 2^exp) * 10^-dec` in this order, and recomputes
    // both powers per value. Same operands, same order, same rounding.
    //
    // The same three operands are what `ScaledU16` stores when the narrow arm
    // is taken, so the two arms are the same expression over the same numbers.
    let two_pow = 2_f32.powi(i32::from(plan.simple.exp));
    let dig_factor = 10_f32.powi(-i32::from(plan.simple.dec));
    let ref_val = plan.simple.ref_val;

    png_rows(plan, points, |sample| {
        // Big-endian, most significant bit first -- the order
        // `NBitwiseIterator` reads the same bytes in.
        let mut encoded: u32 = 0;
        for &byte in sample {
            encoded = (encoded << 8) | u32::from(byte);
        }
        let raw = (ref_val + encoded as f32 * two_pow) * dig_factor;
        values.push(reading(missing, raw));
    })
}

/// **The same walk, stopping at the code.**
///
/// What [`decode_png_into`] does minus the arithmetic and minus the sentinel
/// test: the code IS the stored value on the narrow arm, and
/// [`ScaledU16::value`] applies the identical expression at the far end of
/// whatever the code travelled through. Splitting them is what makes the
/// mosaic 49,000,000 B instead of 98,000,000 B without a second decoder
/// existing to disagree with the first — both walk one [`png_rows`].
///
/// `sample` is one or two bytes here and never more: the caller has already
/// established `num_bits <= 16`, which is what makes the accumulator a `u16`
/// rather than a truncation of a wider one.
fn decode_png_codes_into(
    plan: &PngPlan<'_>,
    points: usize,
    codes: &mut Vec<u16>,
) -> Result<(), String> {
    png_rows(plan, points, |sample| {
        let mut encoded: u16 = 0;
        for &byte in sample {
            encoded = (encoded << 8) | u16::from(byte);
        }
        codes.push(encoded);
    })
}

/// **Stream `plan`'s PNG a row at a time, handing each sample's bytes to `f`.**
///
/// The row walk itself, and the three checks the whole streaming path rests on,
/// written once so the two decoders above cannot come to disagree about which
/// granules they will read.
///
/// The reason it exists at all: `grib`'s 5.41 arm materialises the **entire**
/// decompressed image before it yields a single value --
/// `read_image_buffer`'s `vec![0; output_buffer_size()]`, which at MRMS's
/// 7000x3500 16-bit mosaic is **49,000,000 B, measured, per granule** -- and
/// then hands back a lazy iterator over it. That buffer is `vec![0; n]`:
/// infallible, and on wasm32 an infallible allocation the engine cannot serve
/// calls `handle_alloc_error` against a memory with a hard 1 GiB ceiling on a
/// target where **nothing unwinds**. `staging`'s account of what that does to
/// the page applies here unchanged; the difference is that the staging buffer
/// could be retained and this one cannot, because `grib` allocates it inside a
/// private function and moves it into the iterator.
///
/// A row is `width` samples and nothing else is live, so the same decode's
/// image-side cost is **14,000 B** at the mosaic's width. The `png` reader is
/// asked to fill a buffer this function owns (`read_row`) rather than its own
/// (`next_row`), so there is one row buffer in the process and not two.
///
/// This is the decision `regular_grid` already makes one section earlier, for
/// the same reason: `grib` will answer with a materialised whole, section 3's
/// coordinates or section 7's values, and at 24.5 M points neither fits a
/// budget the browser gives us. Reading the seven numbers, or the one row, is
/// what fits.
fn png_rows(plan: &PngPlan<'_>, points: usize, mut f: impl FnMut(&[u8])) -> Result<(), String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(plan.payload));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("MRMS PNG decode error: {e}"))?;

    let width = reader.info().width as usize;
    let line = reader
        .output_line_size(reader.info().width)
        .ok_or_else(|| "MRMS PNG: the image declares no line size".to_string())?;
    // **The alignment the whole path rests on.** `grib` walks the flat image as
    // one bit stream; this walks it row by row. They coincide exactly when a
    // row is a whole number of whole samples and the image is the whole grid,
    // so both are checked rather than assumed -- a mismatch here is a granule
    // this path must not read, not one to read approximately.
    if line != width * plan.sample_bytes {
        return Err(format!(
            "MRMS PNG: a {line}-byte row is not {width} samples of \
             {} bytes",
            plan.sample_bytes,
        ));
    }
    let height = reader.info().height as usize;
    if width.checked_mul(height) != Some(points) {
        return Err(format!(
            "MRMS PNG: a {width}x{height} image is not the {points} points \
             section 3 declares",
        ));
    }

    let mut row = vec![0u8; line];
    while reader
        .read_row(&mut row)
        .map_err(|e| format!("MRMS PNG row decode error: {e}"))?
        .is_some()
    {
        for sample in row.chunks_exact(plan.sample_bytes) {
            f(sample);
        }
    }
    Ok(())
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
