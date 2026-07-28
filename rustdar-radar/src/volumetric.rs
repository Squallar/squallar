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

#[cfg(test)]
mod tests {
    use super::*;
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Sweep, VolumeCoveragePattern,
    };

    const SCALE: f32 = 2.0;
    const OFFSET: f32 = 66.0;
    /// 0.25 km gates out to 250 km — past the 230 km grid, so the tail is
    /// exercised as "outside the domain" rather than never generated.
    const GATES: usize = 1000;
    const GATE_INTERVAL_M: u16 = 250;

    fn vcp() -> VolumeCoveragePattern {
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        )
    }

    /// The synthetic volume's reflectivity, dBZ, at a polar position and beam
    /// height. `None` is "no return" (encoded as below-threshold, gate byte 0).
    ///
    /// Three storm cores of different intensities decay with height at
    /// 3.5 dBZ/km, so their columns cross the 18.3 dBZ echo-top threshold at
    /// different tilts:
    ///
    /// * core A (60 dBZ, az ~45°, r ~40 km) tops out above the highest tilt;
    /// * core B (32 dBZ, az ~150°, r ~80 km) crosses between mid tilts;
    /// * core C (27 dBZ, az ~300°, r ~120 km) crosses above the lowest tilt
    ///   only, so its top interpolates between the two lowest tilt centres.
    ///
    /// The sector 200°–240° carries no data at all — a hole the grid must
    /// leave NaN — and everything else is a 15 dBZ background that sits below
    /// the threshold without being absent.
    fn dbz_at(az_deg: f64, r_km: f64, height_km: f64, sails_shift: bool) -> Option<f64> {
        if (200.0..240.0).contains(&az_deg) {
            return None;
        }
        let shift = if sails_shift { 8.0 } else { 0.0 };
        let core = |c_az: f64, c_r: f64, w_az: f64, w_r: f64, amp: f64| {
            let mut daz = (az_deg - (c_az + shift)).abs();
            if daz > 180.0 {
                daz = 360.0 - daz;
            }
            let dr = r_km - c_r;
            amp * (-(daz * daz) / (2.0 * w_az * w_az) - (dr * dr) / (2.0 * w_r * w_r)).exp()
        };
        let surface = 15.0
            + core(45.0, 40.0, 12.0, 15.0, 45.0)
            + core(150.0, 80.0, 15.0, 20.0, 17.0)
            + core(300.0, 120.0, 10.0, 12.0, 12.0);
        Some(surface - 3.5 * height_km)
    }

    /// One reflectivity sweep: `n_radials` evenly spaced, first azimuth at
    /// `az_offset`°, gate bytes encoding [`dbz_at`] through scale 2 offset 66.
    fn refl_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        n_radials: usize,
        az_offset: f32,
        sails_shift: bool,
    ) -> Sweep {
        let spacing = 360.0 / n_radials as f32;
        let radials = (0..n_radials)
            .map(|i| {
                let az = az_offset + i as f32 * spacing;
                let bytes: Vec<u8> = (0..GATES)
                    .map(|j| {
                        let r_km = j as f64 * 0.25;
                        let h_km = beam_height_km(r_km, elevation_deg as f64);
                        match dbz_at(az as f64, r_km, h_km, sails_shift) {
                            None => 0, // below threshold: skipped by the grid
                            Some(dbz) => ((dbz * SCALE as f64 + OFFSET as f64).round() as i64)
                                .clamp(2, 255) as u8,
                        }
                    })
                    .collect();
                Radial::new(
                    0,
                    i as u16,
                    az,
                    spacing,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    Some(MomentData::from_fixed_point(
                        GATES as u16,
                        0,
                        GATE_INTERVAL_M,
                        8,
                        SCALE,
                        OFFSET,
                        bytes,
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    }

    /// A velocity-only sweep — the Doppler half of a split cut. It carries no
    /// reflectivity, so the reflectivity tilt selection must skip it entirely.
    fn velocity_only_sweep(elevation_number: u8, elevation_deg: f32) -> Sweep {
        let radials = (0..360)
            .map(|i| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32 + 0.5,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    None,
                    Some(MomentData::from_fixed_point(
                        400,
                        0,
                        250,
                        8,
                        2.0,
                        129.0,
                        vec![129; 400],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    }

    /// A five-tilt volume with the shapes a real SAILS volume throws at the
    /// echo-top scan:
    ///
    /// * 0.5° at half-degree super-resolution, radials **off** the whole-degree
    ///   cell centres (0.1° and 0.6°), so the nearest-radial choice is a real
    ///   choice;
    /// * a velocity-only split cut at the same elevation, to be skipped;
    /// * three upper tilts at 1° spacing;
    /// * a SAILS repeat of 0.5° **late in the scan** whose cores are shifted 8°
    ///   in azimuth — under newest-wins it must displace the first 0.5° sweep,
    ///   which the pinned digest can tell because the two sweeps disagree.
    fn golden_scan() -> Scan {
        Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, 720, 0.1, false),
                velocity_only_sweep(2, 0.5),
                refl_sweep(3, 1.5, 360, 0.5, false),
                refl_sweep(4, 2.4, 360, 0.5, false),
                refl_sweep(5, 3.4, 360, 0.5, false),
                refl_sweep(6, 0.5, 720, 0.1, true), // SAILS repeat: newest wins
                refl_sweep(7, 4.3, 360, 0.5, false),
            ],
        )
    }

    /// FNV-1a over every cell's bit pattern, azimuth-major. Implemented here
    /// rather than through `DefaultHasher` so the pinned literal does not
    /// depend on the standard library's unspecified hash algorithm.
    fn fnv1a64(grid: &VolumetricGrid) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for row in &grid.values {
            for v in row {
                for b in v.to_bits().to_le_bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
            }
        }
        h
    }

    /// The golden pin for `compute_echo_tops`: the full grid digest, the
    /// defined-cell count, and spot values, captured from the shipped
    /// implementation before the volume cube refactor. Any change to the
    /// gridding, dedup, beam-height or interpolation arithmetic moves at least
    /// the digest; the refactor must reproduce all of it bit for bit.
    #[test]
    fn golden_echo_tops_grid_is_pinned() {
        let grid = compute_echo_tops(&golden_scan());
        assert_eq!(grid.range_bins, 230);
        assert_eq!(grid.values.len(), 360);

        let defined: usize = grid.values.iter().flatten().filter(|v| !v.is_nan()).count();
        assert_eq!(defined, 4680, "defined-cell count moved");
        assert_eq!(fnv1a64(&grid), 0x4559ce366731e030, "grid digest moved");

        // Spot pins, exact to the bit, each hand-checked against the beam
        // height formula. Chosen to cover every code path:
        //
        // * (45, 40): core A crosses at the top tilt, so its top is *clamped*
        //   to that tilt's centre height — 40.5·sin 4.3° + 40.5²/(2·8494.7)
        //   = 3.134 km = 10.28 kft.
        // * (45, 41): one cell further out, the same clamp moves with range.
        // * (150, 80): core B is topmost at 2.4° (18.9 dBZ at 3.75 km) and
        //   interpolates toward 3.4° (14.0 dBZ at 5.16 km) — ~3.95 km.
        // * (308, 120): core C crosses only at 0.5°, interpolating toward
        //   1.5° — ~2.3 km — and sits at 308° only because the SAILS repeat
        //   (cores shifted +8°) displaced the first 0.5° sweep.
        // * (300, 120): the first sweep's core C centre, NaN for the same
        //   reason. A first-of-volume dedup defines this cell and empties the
        //   one above, so these two pin newest-wins, not merely "some sweep".
        let spots = [
            (45usize, 40usize, 0x412478bcu32), // core A: 10.279476 kft
            (45, 41, 0x4128a92f),              // core A, next cell: 10.541305
            (150, 80, 0x414f4a1d),             // core B interpolated: 12.955594
            (308, 120, 0x40f447cc),            // core C via SAILS repeat: 7.6337643
        ];
        for (az, r, bits) in spots {
            let got = grid.values[az][r];
            assert_eq!(
                got.to_bits(),
                bits,
                "cell az {az}° r {r} km: got {got} ({:#010x})",
                got.to_bits(),
            );
        }
        assert!(
            grid.values[300][120].is_nan(),
            "the SAILS repeat no longer displaces the first 0.5° sweep",
        );

        // The hole must stay a hole, and the sub-threshold background must not
        // produce tops.
        assert!(
            grid.values[220][100].is_nan(),
            "the no-data sector filled in"
        );
        assert!(grid.values[10][200].is_nan(), "15 dBZ background topped");
    }
}
