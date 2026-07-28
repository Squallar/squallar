//! Volume-derived products computed from Level II reflectivity: interpolated echo tops. The RPG's EET/DVL products use coarser
//! grids and beam-top conventions; these implementations interpolate between
//! tilt centers, calibrated against a reference implementation's readouts.

use crate::types::RadarProduct;
use nexrad_model::data::{DataMoment, MomentValue, Scan};

/// Effective earth radius (4/3 model), km.
const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;

/// Reflectivity threshold for echo tops, dBZ.
const ET_THRESHOLD_DBZ: f32 = 18.3;

/// Polar grid of a volume-derived product: 360 azimuth degrees × 1-km range
/// bins, value `NaN` where undefined.
pub struct VolumetricGrid {
    pub values: Vec<Vec<f32>>, // [az_deg][range_km]
    pub range_bins: usize,
}

const RANGE_BINS: usize = 230;

/// Beam-center height above the radar, km, for a slant range and elevation.
fn beam_height_km(range_km: f64, elev_deg: f64) -> f64 {
    let el = elev_deg.to_radians();
    range_km * el.sin() + range_km * range_km / (2.0 * RE_EFF_KM)
}

/// Reflectivity per (az°, range-km) cell for one sweep: linear-Z mean of the
/// gates falling in the cell on the radial nearest the cell centre.
fn sweep_to_grid(radials: &[nexrad_model::data::Radial]) -> Vec<Vec<f32>> {
    let mut grid = vec![vec![f32::NAN; RANGE_BINS]; 360];
    // nearest radial per whole-degree centre
    let mut nearest: Vec<Option<usize>> = vec![None; 360];
    for (ri, radial) in radials.iter().enumerate() {
        let az = (radial.azimuth_angle_degrees() as f64).rem_euclid(360.0);
        let cell = az as usize % 360;
        let centre = cell as f64 + 0.5;
        let d = (az - centre).abs();
        let better = match nearest[cell] {
            None => true,
            Some(prev) => {
                let paz = (radials[prev].azimuth_angle_degrees() as f64).rem_euclid(360.0);
                d < (paz - centre).abs()
            }
        };
        if better {
            nearest[cell] = Some(ri);
        }
    }
    for (cell, slot) in nearest.iter().enumerate() {
        let Some(ri) = slot else { continue };
        let radial = &radials[*ri];
        if let Some(moment) = RadarProduct::Reflectivity.get_moment(radial) {
            let fg = moment.first_gate_range_km();
            let gi = moment.gate_interval_km();
            // linear-Z mean of the gates falling in each 1-km cell
            let mut acc = vec![(0.0f64, 0u32); RANGE_BINS];
            for (j, v) in moment.values().iter().enumerate() {
                let MomentValue::Value(z) = v else { continue };
                if *z >= 999.0 || z.is_nan() {
                    continue;
                }
                let r = (fg + j as f64 * gi) as usize;
                if r < RANGE_BINS {
                    acc[r].0 += 10f64.powf(*z as f64 / 10.0);
                    acc[r].1 += 1;
                }
            }
            for (r, (zsum, n)) in acc.into_iter().enumerate() {
                if n > 0 {
                    grid[cell][r] = (10.0 * (zsum / n as f64).log10()) as f32;
                }
            }
        }
    }
    grid
}

/// Distinct reflectivity tilts of a volume, ascending elevation, newest sweep
/// per 0.1°-rounded elevation. Returns (elevation°, per-cell max dBZ grid).
fn tilt_grids(scan: &Scan) -> Vec<(f64, Vec<Vec<f32>>)> {
    let mut by_elev: Vec<(f64, &nexrad_model::data::Sweep)> = Vec::new();
    for sweep in scan.sweeps() {
        let Some(first) = sweep.radials().first() else {
            continue;
        };
        if RadarProduct::Reflectivity.get_moment(first).is_none() {
            continue;
        }
        let elev = first.elevation_angle_degrees() as f64;
        let key = (elev * 10.0).round() / 10.0;
        match by_elev.iter_mut().find(|(k, _)| (*k - key).abs() < 0.05) {
            Some(entry) => entry.1 = sweep, // newest wins (sweeps in scan order)
            None => by_elev.push((key, sweep)),
        }
    }
    by_elev.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    by_elev
        .into_iter()
        .map(|(e, s)| (e, sweep_to_grid(s.radials())))
        .collect()
}
/// Echo tops: height (kft above radar) of the interpolated crossing of
/// [`ET_THRESHOLD_DBZ`], scanning tilts top-down per column.
pub fn compute_echo_tops(scan: &Scan) -> VolumetricGrid {
    let tilts = tilt_grids(scan);
    let mut out = vec![vec![f32::NAN; RANGE_BINS]; 360];
    for (az, row) in out.iter_mut().enumerate() {
        for (r, cell) in row.iter_mut().enumerate() {
            let rr = r as f64 + 0.5;
            // topmost tilt meeting the threshold
            let mut top: Option<f32> = None;
            for ti in (0..tilts.len()).rev() {
                let z = tilts[ti].1[az][r];
                if !z.is_nan() && z >= ET_THRESHOLD_DBZ {
                    let h = beam_height_km(rr, tilts[ti].0);
                    let ht = if ti + 1 < tilts.len() {
                        let z_up = tilts[ti + 1].1[az][r];
                        let h_up = beam_height_km(rr, tilts[ti + 1].0);
                        if z_up.is_nan() {
                            // echo absent above: the tilt centre itself
                            h
                        } else {
                            // z_up < threshold (else ti wouldn't be topmost)
                            h + (h_up - h) * ((z - ET_THRESHOLD_DBZ) / (z - z_up)) as f64
                        }
                    } else {
                        h
                    };
                    top = Some((ht * 3.28084) as f32); // km -> kft
                    break;
                }
            }
            if let Some(t) = top {
                *cell = t;
            }
        }
    }
    VolumetricGrid {
        values: out,
        range_bins: RANGE_BINS,
    }
}
