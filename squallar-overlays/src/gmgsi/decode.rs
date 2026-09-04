//! A GMGSI granule into the gridded substrate.
//!
//! Reads through [`squallar_netcdf`] — the NetCDF4 and CF-convention layer,
//! which knows nothing about satellites — and produces a
//! [`ResidentGrid`] on [`GridCoords::Separable`]. Nothing here re-implements a
//! CF rule; `_FillValue` reaches the raster as a NaN because
//! [`squallar_netcdf::cf`] marked it missing, not because this file compared
//! against `-9999`.

use squallar_geo::GeoBounds;
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
/// window — but it can be blocked, which bounds peak residency at
/// `ROW_BLOCK * ni` elements rather than `nj * ni`. On the reference granule
/// that is roughly 20 MB against 240 MB.
///
/// Not coprime with anything and not required to be: this only decides how the
/// same rows are grouped, never which ones are read.
const ROW_BLOCK: usize = 256;

/// Decode a granule, **taking its bytes**.
///
/// By value on purpose: the reader needs an owned buffer, a GMGSI body is
/// 7.5 MB, and taking it here means `Granule::from_vec` can adopt the
/// allocation instead of `Granule::open` copying it.
pub fn decode(bytes: Vec<u8>, channel: GmgsiChannel) -> Result<GmgsiGrid, String> {
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

    let lat_axis = axis_from_2d(&granule, "lat", nj, ni, Axis::Row)?;
    let lon_axis = axis_from_2d(&granule, "lon", nj, ni, Axis::Column)?;

    // Read straight into the raster form. A missing point is NaN, which
    // `render::gridded::color_for` already paints as fully transparent through
    // its non-finite guard; encoding it as any in-domain number would paint it
    // as that number.
    //
    // `read_unpacked_f32` rather than `read_unpacked`: the `Option` form of
    // this variable is 240 MB against 60 MB -- see
    // `squallar_netcdf::cf::UnpackedF32`.
    //
    // Nothing in the decode may be larger than that 60,000,000 B raster, which
    // is what `tests/gmgsi_decode_blocks.rs` counts. `data` is `float` on disk
    // in both the real granule and the fixture, so the storage read is
    // 60,000,000 B too and `squallar_netcdf::cf::RawValues` keeps it there
    // rather than widening it. Measured 2026-08-31 over one decode of the real
    // `GLOBCOMPLIR_v3r0_blend` granule: 125,265,534 live bytes at the
    // high-water mark, largest single allocation 60,000,000 B.
    let data = granule
        .read_unpacked_f32("data")?
        .ok_or_else(|| "GMGSI granule has no `data` variable".to_string())?;
    if data.values.len() != ni * nj {
        return Err(format!(
            "GMGSI `data` declares {} x {} but {} values were read",
            nj,
            ni,
            data.values.len()
        ));
    }
    let values = data.values;

    let bounds = bounds_of(&lat_axis, &lon_axis);
    let valid_time = granule
        .global_str("time_coverage_start")
        .and_then(|s| parse_coverage_start(&s))
        .ok_or_else(|| "GMGSI granule has no readable `time_coverage_start`".to_string())?;

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
    let shape = granule
        .shape(name)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if shape.as_slice() != [nj as u64, ni as u64] {
        return Err(format!(
            "GMGSI `{name}` has shape {shape:?}; expected ({nj}, {ni}) to match `data`"
        ));
    }
    match axis {
        Axis::Row => row_axis(granule, name, nj, ni),
        Axis::Column => column_axis(granule, name, nj, ni),
    }
}

/// Rows `[start, start + count)` of a 2-D variable, `ni` wide, row-major.
fn rows_of(
    granule: &squallar_netcdf::Granule,
    name: &str,
    start: usize,
    count: usize,
    ni: usize,
) -> Result<Vec<Option<f64>>, String> {
    let var = granule
        .read_unpacked_rows(name, start as u64, count as u64)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if var.values.len() != count * ni {
        return Err(format!(
            "GMGSI `{name}` rows {start}..{} declare {count} x {ni} but {} values were read",
            start + count,
            var.values.len()
        ));
    }
    Ok(var.values)
}

fn present(v: Option<f64>, name: &str, j: usize, i: usize) -> Result<f64, String> {
    v.ok_or_else(|| format!("GMGSI `{name}` is missing at ({j}, {i})"))
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
/// peak residency is [`ROW_BLOCK`] rows instead of the whole variable. The
/// separability probe runs inside the block that carries its row, which visits
/// the same `(k, s)` pairs in the same order as a whole-variable walk.
fn row_axis(
    granule: &squallar_netcdf::Granule,
    name: &str,
    nj: usize,
    ni: usize,
) -> Result<Vec<f64>, String> {
    let mut out: Vec<f64> = Vec::with_capacity(nj);
    for start in (0..nj).step_by(ROW_BLOCK) {
        let count = ROW_BLOCK.min(nj - start);
        let block = rows_of(granule, name, start, count, ni)?;
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
/// This is the case the row window was worth adding for — one row of a
/// 15,000,000-element variable instead of all of it. Only the probe rows are
/// read beyond that, and there are `nj / SEPARABLE_PROBE_STRIDE` of them; they
/// are collected first so the comparison keeps its original column-outer order.
fn column_axis(
    granule: &squallar_netcdf::Granule,
    name: &str,
    nj: usize,
    ni: usize,
) -> Result<Vec<f64>, String> {
    let first = rows_of(granule, name, 0, 1, ni)?;
    let mut out: Vec<f64> = Vec::with_capacity(ni);
    for (k, v) in first.iter().enumerate() {
        out.push(present(*v, name, 0, k)?);
    }

    let probes: Vec<(usize, Vec<Option<f64>>)> = (0..nj)
        .step_by(SEPARABLE_PROBE_STRIDE)
        .map(|s| rows_of(granule, name, s, 1, ni).map(|row| (s, row)))
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
