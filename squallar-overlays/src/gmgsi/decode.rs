//! A GMGSI granule into the gridded substrate.
//!
//! Reads through [`squallar_netcdf`] — the NetCDF4 and CF-convention layer,
//! which knows nothing about satellites — and produces a
//! [`ResidentGrid`] on [`GridCoords::Separable`]. Nothing here re-implements a
//! CF rule; `_FillValue` reaches the raster as a NaN because
//! [`squallar_netcdf::cf`] marked it missing, not because this file compared
//! against `-9999`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use squallar_geo::GeoBounds;
use squallar_netcdf::StoredFingerprint;
use squallar_source::product::FieldId;

use super::GmgsiChannel;
use crate::hrrr::GridCoords;
use crate::render::gridded::ResidentGrid;

/// A decoded granule: the raster, plus the instant it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct GmgsiGrid {
    pub channel: GmgsiChannel,
    pub grid: ResidentGrid,
    pub bounds: GeoBounds,
    /// `time_coverage_start`, the granule's own stamp.
    pub valid_time: chrono::NaiveDateTime,
}

/// How far apart two coordinates may sit before the grid is refused as
/// non-separable, in degrees.
///
/// The reference granule's deviation is **exactly** zero, so this is not a
/// tolerance the real product needs — it is the width of the claim being
/// checked. A grid that fails it is one whose latitude genuinely varies along a
/// row, which a per-axis representation cannot describe at all.
const SEPARABLE_EPS: f64 = 1e-6;

/// Rows and columns sampled per axis when checking separability.
///
/// A full check is 15,000,000 comparisons per coordinate; this is 10,000. The
/// strides are coprime with the grid's dimensions so the probes do not all land
/// on the same few columns.
const SEPARABLE_PROBE_STRIDE: usize = 97;

/// Rows held resident at once while streaming a coordinate variable.
///
/// The latitude axis is column 0 of *every* row, so it cannot be narrowed to a
/// window — but it can be blocked, which bounds the window a read copies out
/// at `ROW_BLOCK * ni` elements rather than `nj * ni`: 5 MB against 60 MB on
/// the reference granule.
///
/// Not coprime with anything and not required to be: this only decides how the
/// same rows are grouped, never which ones are read.
const ROW_BLOCK: usize = 256;

/// Decode a granule, **taking its bytes**.
///
/// By value on purpose: the reader needs an owned buffer, a GMGSI body is
/// 7.5 MB, and taking it here means `Granule::from_vec` can adopt the
/// allocation instead of `Granule::open` copying it.
///
/// The raster is decoded into [`super::staging`]'s retained slot; see
/// [`decode_in`] for the shape of the read and what it costs.
pub fn decode(bytes: Vec<u8>, channel: GmgsiChannel) -> Result<GmgsiGrid, String> {
    decode_in(bytes, channel, super::staging::global(), axis_cache())
}

/// [`decode`] against an explicit staging pool and axis cache rather than
/// the process-wide ones.
///
/// **Public so a suite can drive the real decoder over state it owns.** The
/// counters both turn on are process-global on the shipped path, and a
/// filtered run in this workspace is explicitly not self-contained. Every
/// shipped caller goes through [`decode`]; nothing chooses either at runtime.
///
/// # What one decode costs, and in what order
///
/// The two coordinate variables come first and the slot is taken **after**
/// them, deliberately. `lat` and `lon` are each stored as one 3000 x 5000
/// chunk, and reading any window of one costs `hdf5_pure` two 60 MB blocks
/// while it inflates and unshuffles the chunk — so a 60 MB slot already in
/// hand at that moment would put a cold decode's peak a whole mosaic higher
/// than it has to be (measured: 240 MB against 130 MB, one cold decode of
/// the committed granule).
///
/// In the steady state the coordinate variables are not read at all: every
/// granule of the product stores the same two arrays, and [`AxisCache`]
/// proves it granule by granule from their stored bytes before handing back
/// the axes it derived last time. When they do have to be read — the first
/// granule, or a granule whose stored geometry differs — each is opened
/// **once**, as a [`Variable`](squallar_netcdf::Variable) whose chunk cache
/// holds its one chunk, so the walk's row windows share a single inflation.
/// Before that, every window re-inflated the chunk: 44 windows, 88 blocks of
/// 60,000,000 B per decode, measured.
///
/// Then the raster: `data` lands straight in the slot's buffer through
/// [`read_unpacked_f32_into`](squallar_netcdf::Granule::read_unpacked_f32_into),
/// so no fresh raster-sized block is taken past the first granule. What
/// remains, per steady-state granule, is **one** transient 60,000,000 B
/// block — the stored bytes `hdf5_pure` assembles for `data` before decoding
/// them — because its public API in 0.44 reads a dataset whole or by
/// first-dimension row window and `data` is `(time=1, yc, xc)`.
/// `tests/gmgsi_staging_blocks.rs` counts exactly that.
pub fn decode_in(
    bytes: Vec<u8>,
    channel: GmgsiChannel,
    pool: &super::staging::StagingPool,
    axes: &AxisCache,
) -> Result<GmgsiGrid, String> {
    let granule = squallar_netcdf::Granule::from_vec(bytes)?;

    let shape = granule
        .shape("data")?
        .ok_or_else(|| "GMGSI granule has no `data` variable".to_string())?;
    // `(time, yc, xc)` with a single time step. A granule that ever carried
    // more than one would need a frame axis, not a silently-dropped dimension.
    let (nj, ni) = match shape.as_slice() {
        [1, nj, ni] => (*nj as usize, *ni as usize),
        [nj, ni] => (*nj as usize, *ni as usize),
        other => {
            return Err(format!(
                "GMGSI `data` has shape {other:?}; expected (time=1, yc, xc)"
            ));
        }
    };
    if ni == 0 || nj == 0 {
        return Err(format!("GMGSI `data` is empty at {nj} x {ni}"));
    }
    let points = ni
        .checked_mul(nj)
        .ok_or_else(|| format!("GMGSI `data` at {nj} x {ni} overflows this platform"))?;

    let lat_axis = axes.axis(&granule, "lat", nj, ni, Axis::Row)?;
    let lon_axis = axes.axis(&granule, "lon", nj, ni, Axis::Column)?;
    let bounds = bounds_of(&lat_axis, &lon_axis);
    let valid_time = granule
        .global_str("time_coverage_start")
        .and_then(|s| parse_coverage_start(&s))
        .ok_or_else(|| "GMGSI granule has no readable `time_coverage_start`".to_string())?;

    // Read straight into the raster form. A missing point is NaN, which
    // `render::gridded::color_for` already paints as fully transparent through
    // its non-finite guard; encoding it as any in-domain number would paint it
    // as that number.
    let mut values = pool.take(points)?;
    if let Err(e) = read_whole(&granule, "data", nj, ni, &mut values) {
        // A refused granule must not cost the slot its buffer.
        pool.give(values);
        return Err(e);
    }

    Ok(GmgsiGrid {
        channel,
        grid: ResidentGrid {
            field: FieldId::from_static(channel.as_str()),
            ni,
            nj,
            coords: GridCoords::Separable { lat_axis, lon_axis },
            // **`F32`, and that is a fact about the source, not a default.**
            // GMGSI's brightness values are genuinely `float` on disk, so the
            // narrow arm MRMS takes would be a real quantisation here rather
            // than the repacking it is there. Narrowing this needs its own
            // losslessness proof or an explicit quality ruling.
            values: crate::render::gridded::GridValues::F32(values),
        },
        bounds,
        valid_time,
    })
}

/// Read a whole `nj x ni` variable into `into`, which is emptied first and
/// holds exactly `nj * ni` values afterwards or the read is refused.
///
/// The count is checked against the shape the caller already established
/// rather than trusted: a variable that declares one shape and delivers
/// another would otherwise be a raster indexed off the end, silently.
fn read_whole(
    granule: &squallar_netcdf::Granule,
    name: &str,
    nj: usize,
    ni: usize,
    into: &mut Vec<f32>,
) -> Result<(), String> {
    into.clear();
    let read = granule
        .read_unpacked_f32_into(name, into)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if read != nj * ni || into.len() != nj * ni {
        return Err(format!(
            "GMGSI `{name}` declares {nj} x {ni} but {read} values were read"
        ));
    }
    Ok(())
}

/// **The two axes the last granule's coordinate arrays collapsed to, keyed
/// by the stored bytes that produced them.**
///
/// GMGSI stores `lat` and `lon` as full 2-D arrays on every granule, and the
/// arrays are the same on every granule of the product — the mosaic's grid
/// does not move hour to hour. Reading them is the expensive half of a
/// decode: two 60 MB chunks inflated and unshuffled, 120 MB of transient
/// beside the slot, for two axes that total 64 KB.
///
/// This is not a decision to trust the geometry by shape. The key is a
/// [`StoredFingerprint`] — the variable's stored bytes, chunk by chunk, with
/// its type, chunking, filters and CF attributes — and decoding is a pure
/// function of exactly those, so an equal fingerprint *is* an equal array.
/// A granule whose stored arrays differ in one byte misses and is read and
/// verified as the first one was. ~446 KB of stored bytes are compared per
/// variable per decode; nothing is inflated.
///
/// One entry per variable, `try_lock` only, for the reasons the staging pool
/// gives: the contenders are a live fetch and a frame fetch, contention is
/// rare, and a contended cache simply reads, which is what every decode did
/// before this existed. Injectable for the reason the pool is.
pub struct AxisCache {
    lat: Mutex<Option<(StoredFingerprint, Vec<f64>)>>,
    lon: Mutex<Option<(StoredFingerprint, Vec<f64>)>>,
    /// Axes handed back without a read. Always on, like the pool's totals.
    hits: AtomicUsize,
    /// Axes read and verified off the granule — a first granule, a changed
    /// geometry, a variable that could not be fingerprinted, or a contended
    /// lock.
    misses: AtomicUsize,
}

/// Running totals off [`AxisCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisCacheTotals {
    pub hits: usize,
    pub misses: usize,
}

impl AxisCache {
    pub const fn new() -> Self {
        Self {
            lat: Mutex::new(None),
            lon: Mutex::new(None),
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }

    pub fn totals(&self) -> AxisCacheTotals {
        AxisCacheTotals {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    fn slot(&self, axis: &Axis) -> &Mutex<Option<(StoredFingerprint, Vec<f64>)>> {
        match axis {
            Axis::Row => &self.lat,
            Axis::Column => &self.lon,
        }
    }

    /// The axis `name` collapses to: remembered if this granule stores the
    /// variable byte for byte as the remembered one did, read and verified
    /// otherwise.
    fn axis(
        &self,
        granule: &squallar_netcdf::Granule,
        name: &str,
        nj: usize,
        ni: usize,
        axis: Axis,
    ) -> Result<Vec<f64>, String> {
        let fingerprint = granule.stored_fingerprint(name)?;
        let slot = self.slot(&axis);
        if let Some(fingerprint) = &fingerprint
            && let Ok(remembered) = slot.try_lock()
            && let Some((known, out)) = remembered.as_ref()
            && known == fingerprint
        {
            // The shape is part of the fingerprint, so an equal one is an
            // axis of the length this granule declares.
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(out.clone());
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let out = axis_from_2d(granule, name, nj, ni, axis)?;
        if let Some(fingerprint) = fingerprint
            && let Ok(mut remembered) = slot.try_lock()
        {
            *remembered = Some((fingerprint, out.clone()));
        }
        Ok(out)
    }
}

impl Default for AxisCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide axis cache — what every shipped decode uses. One for the
/// application, because every pane's granules share one grid.
static AXES: AxisCache = AxisCache::new();

/// See [`AXES`].
pub fn axis_cache() -> &'static AxisCache {
    &AXES
}

enum Axis {
    /// Varies down the rows, constant along each one.
    Row,
    /// Varies along the columns, constant down each one.
    Column,
}

/// Collapse a 2-D `(yc, xc)` coordinate variable to the axis it repeats,
/// refusing the collapse if the variable does not in fact repeat.
///
/// The refusal is the point. A separable representation of a non-separable
/// grid does not misplace one point — it misplaces the whole raster along one
/// dimension, and it does so silently, because every method still answers.
fn axis_from_2d(
    granule: &squallar_netcdf::Granule,
    name: &str,
    nj: usize,
    ni: usize,
    axis: Axis,
) -> Result<Vec<f64>, String> {
    let var = granule
        .variable(name)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if var.shape() != [nj as u64, ni as u64] {
        return Err(format!(
            "GMGSI `{name}` has shape {:?}; expected ({nj}, {ni}) to match `data`",
            var.shape()
        ));
    }
    match axis {
        Axis::Row => row_axis(&var, name, nj, ni),
        Axis::Column => column_axis(&var, name, nj, ni),
    }
}

/// Rows `[start, start + count)` of a 2-D variable, `ni` wide, row-major.
///
/// The raster form rather than the `Option<f64>` one: a 256-row window is
/// 5 MB this way and 20 MB that way, and a coordinate the file marked missing
/// is a `NaN` either way — see [`present`].
fn rows_of(
    var: &squallar_netcdf::Variable,
    name: &str,
    start: usize,
    count: usize,
    ni: usize,
) -> Result<Vec<f32>, String> {
    let rows = var.read_unpacked_rows_f32(start as u64, count as u64)?;
    if rows.values.len() != count * ni {
        return Err(format!(
            "GMGSI `{name}` rows {start}..{} declare {count} x {ni} but {} values were read",
            start + count,
            rows.values.len()
        ));
    }
    Ok(rows.values)
}

/// A coordinate the file marked missing is `NaN` in the raster form, and an
/// axis cannot carry one. `f64::from` is exact, so the axis holds the stored
/// value bit for bit.
fn present(v: f32, name: &str, j: usize, i: usize) -> Result<f64, String> {
    if v.is_nan() {
        return Err(format!("GMGSI `{name}` is missing at ({j}, {i})"));
    }
    Ok(f64::from(v))
}

fn not_separable(name: &str, k: usize, v: f64, on_axis: f64) -> String {
    format!(
        "GMGSI `{name}` is not separable: entry {k} reads {v} off-axis \
         against {on_axis} on it"
    )
}

/// The axis that varies **down** the rows: column 0 of every row.
///
/// Every row is needed, so this streams in blocks rather than windowing —
/// what is resident is [`ROW_BLOCK`] rows of the variable plus the one chunk
/// the variable's handle keeps. The separability probe runs inside the block
/// that carries its row, which visits the same `(k, s)` pairs in the same
/// order as a whole-variable walk.
fn row_axis(
    var: &squallar_netcdf::Variable,
    name: &str,
    nj: usize,
    ni: usize,
) -> Result<Vec<f64>, String> {
    let mut out: Vec<f64> = Vec::with_capacity(nj);
    for start in (0..nj).step_by(ROW_BLOCK) {
        let count = ROW_BLOCK.min(nj - start);
        let block = rows_of(var, name, start, count, ni)?;
        for r in 0..count {
            out.push(present(block[r * ni], name, start + r, 0)?);
        }
        // Probe the columns of every row in this block that the stride lands
        // on. `(0..nj).step_by(S)` is exactly `k % S == 0`.
        for r in 0..count {
            let k = start + r;
            if !k.is_multiple_of(SEPARABLE_PROBE_STRIDE) {
                continue;
            }
            for s in (0..ni).step_by(SEPARABLE_PROBE_STRIDE) {
                let v = present(block[r * ni + s], name, k, s)?;
                if (v - out[k]).abs() > SEPARABLE_EPS {
                    return Err(not_separable(name, k, v, out[k]));
                }
            }
        }
    }
    Ok(out)
}

/// The axis that varies **along** the columns: row 0, and nothing else.
///
/// Only the probe rows are read beyond that, and there are
/// `nj / SEPARABLE_PROBE_STRIDE` of them; they are collected first so the
/// comparison keeps its original column-outer order. Each is a one-row
/// window off the handle's cached chunk.
fn column_axis(
    var: &squallar_netcdf::Variable,
    name: &str,
    nj: usize,
    ni: usize,
) -> Result<Vec<f64>, String> {
    let first = rows_of(var, name, 0, 1, ni)?;
    let mut out: Vec<f64> = Vec::with_capacity(ni);
    for (k, v) in first.iter().enumerate() {
        out.push(present(*v, name, 0, k)?);
    }

    let probes: Vec<(usize, Vec<f32>)> = (0..nj)
        .step_by(SEPARABLE_PROBE_STRIDE)
        .map(|s| rows_of(var, name, s, 1, ni).map(|row| (s, row)))
        .collect::<Result<_, _>>()?;

    for k in (0..ni).step_by(SEPARABLE_PROBE_STRIDE) {
        for (s, row) in &probes {
            let v = present(row[k], name, *s, k)?;
            if (v - out[k]).abs() > SEPARABLE_EPS {
                return Err(not_separable(name, k, v, out[k]));
            }
        }
    }
    Ok(out)
}

/// The envelope the two axes span.
fn bounds_of(lat_axis: &[f64], lon_axis: &[f64]) -> GeoBounds {
    let fold = |axis: &[f64]| {
        axis.iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, &b| {
                (a.0.min(b), a.1.max(b))
            })
    };
    let (min_lat, max_lat) = fold(lat_axis);
    let (min_lon, max_lon) = fold(lon_axis);
    GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    }
}

/// `time_coverage_start` is ISO 8601 with a `Z`, e.g. `2025-06-01T12:00:00Z`.
/// The retired legacy granule wrote the same field without the `Z`, so both are
/// accepted rather than one being the parse and the other a failure.
fn parse_coverage_start(s: &str) -> Option<chrono::NaiveDateTime> {
    let trimmed = s.trim().trim_end_matches('Z');
    chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S").ok()
}

#[cfg(test)]
mod tests;
