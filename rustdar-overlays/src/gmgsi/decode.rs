//! A GMGSI granule into the gridded substrate.
//!
//! Reads through [`crate::glm::h5`] and [`crate::glm::cf`] — the NetCDF4 and
//! CF-convention layers, which are not GLM's own — and produces a
//! [`ResidentGrid`] on [`GridCoords::Separable`]. Nothing here re-implements a
//! CF rule; `_FillValue` reaches the raster as a NaN because
//! [`crate::glm::cf::unpack`] marked it missing, not because this file compared
//! against `-9999`.

use rustdar_geo::GeoBounds;
use rustdar_source::product::FieldId;

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

pub fn decode(bytes: &[u8], channel: GmgsiChannel) -> Result<GmgsiGrid, String> {
    let granule = crate::glm::h5::Granule::open(bytes)?;

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

    let data = granule
        .read_unpacked("data")?
        .ok_or_else(|| "GMGSI granule has no `data` variable".to_string())?;
    if data.values.len() != ni * nj {
        return Err(format!(
            "GMGSI `data` declares {} x {} but {} values were read",
            nj,
            ni,
            data.values.len()
        ));
    }
    // A missing point becomes NaN, which `render::gridded::color_for` already
    // paints as fully transparent through its non-finite guard. Encoding it as
    // any in-domain number would paint it as that number.
    let values: Vec<f32> = data
        .values
        .iter()
        .map(|v| v.map_or(f32::NAN, |v| v as f32))
        .collect();

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
            values,
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
    granule: &crate::glm::h5::Granule,
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
    let var = granule
        .read_unpacked(name)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if var.values.len() != ni * nj {
        return Err(format!(
            "GMGSI `{name}` declares {nj} x {ni} but {} values were read",
            var.values.len()
        ));
    }
    let at = |j: usize, i: usize| -> Result<f64, String> {
        var.values[j * ni + i].ok_or_else(|| format!("GMGSI `{name}` is missing at ({j}, {i})"))
    };

    let (len, stride_len) = match axis {
        Axis::Row => (nj, ni),
        Axis::Column => (ni, nj),
    };
    let mut out = Vec::with_capacity(len);
    for k in 0..len {
        out.push(match axis {
            Axis::Row => at(k, 0)?,
            Axis::Column => at(0, k)?,
        });
    }
    // Probe the other dimension: every entry must repeat the axis value.
    for k in (0..len).step_by(SEPARABLE_PROBE_STRIDE) {
        for s in (0..stride_len).step_by(SEPARABLE_PROBE_STRIDE) {
            let v = match axis {
                Axis::Row => at(k, s)?,
                Axis::Column => at(s, k)?,
            };
            if (v - out[k]).abs() > SEPARABLE_EPS {
                return Err(format!(
                    "GMGSI `{name}` is not separable: entry {k} reads {v} off-axis \
                     against {} on it",
                    out[k]
                ));
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
