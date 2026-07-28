//! Volume-derived products computed from the Level II volume.
//!
//! The heart is [`VolumeCube`]: the whole volume collapsed once per scan onto
//! a 360° × 230 km polar grid per tilt, for whatever moments a product needs,
//! with beam geometry and sweep provenance alongside. Products
//! ([`compute_echo_tops`], and the EET/DVL/KDP/HCA family to come) are then
//! column scans over the cube rather than owners of their own gridding.
//!
//! The RPG's EET/DVL products use coarser grids and beam-top conventions; the
//! interpolated echo tops here interpolate between tilt centers, calibrated
//! against a reference implementation's readouts.

use crate::types::RadarProduct;
use nexrad_model::data::{DataMoment, MomentValue, Radial, Scan};

/// Effective earth radius (4/3 model), km.
const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;

/// Half-power beamwidth of the WSR-88D antenna, degrees. Beam bottom and top
/// heights sit half of this below and above the tilt centre.
pub const HALF_POWER_BEAMWIDTH_DEG: f64 = 0.95;

/// Reflectivity threshold for echo tops, dBZ.
const ET_THRESHOLD_DBZ: f32 = 18.3;

/// Range cells of the cube and of every volumetric product: 1 km each, 230 km
/// total — the domain the RPG specifies its derived products over.
pub const RANGE_BINS: usize = 230;

/// Polar grid of a volume-derived product: 360 azimuth degrees × 1-km range
/// bins, value `NaN` where undefined.
pub struct VolumetricGrid {
    pub values: Vec<Vec<f32>>, // [az_deg][range_km]
    pub range_bins: usize,
}

/// Beam-center height above the radar, km, for a slant range and elevation.
fn beam_height_km(range_km: f64, elev_deg: f64) -> f64 {
    let el = elev_deg.to_radians();
    range_km * el.sin() + range_km * range_km / (2.0 * RE_EFF_KM)
}

/// A sweep's elevation angle: the **median** of its radials' instantaneous
/// angles. `None` for an empty sweep.
///
/// Not the first radial's: the antenna can still be settling onto the cut
/// when the sweep starts, and the error is not small — a live KMRX volume's
/// 0.5° cut opened at 0.283° and its 19.5° cut at 19.297°. Keying tilts on
/// the first radial split SAILS revisits into phantom tilts (and collided
/// them with neighbouring cuts), and any height ladder built from it sat a
/// fifth of a degree low.
pub fn sweep_elevation_deg(radials: &[Radial]) -> Option<f64> {
    if radials.is_empty() {
        return None;
    }
    let mut els: Vec<f32> = radials
        .iter()
        .map(|r| r.elevation_angle_degrees())
        .collect();
    els.sort_by(f32::total_cmp);
    Some(f64::from(els[els.len() / 2]))
}

/// The statistic collapsing a radial's gates into a 1-km cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellStat {
    /// Mean in linear Z (`10^(dBZ/10)`), read back in dBZ. Averaging
    /// reflectivity in dB space would understate every mixed cell.
    LinearZMean,
    /// Arithmetic mean of the physical values.
    Mean,
    /// Largest value in the cell.
    Max,
}

impl CellStat {
    /// The statistic a moment's physics wants: linear-Z mean for reflectivity
    /// (and the products that read it), arithmetic mean for everything else.
    pub fn for_moment(moment: RadarProduct) -> Self {
        match moment {
            RadarProduct::Reflectivity | RadarProduct::EchoTopsInterpolated => Self::LinearZMean,
            _ => Self::Mean,
        }
    }
}

/// How a repeated elevation (a SAILS/MRLE revisit of the lowest cuts) is
/// resolved to one sweep per tilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupPolicy {
    /// The latest sweep at an elevation wins — the freshest look, what the
    /// shipped interpolated echo tops have always done.
    NewestWins,
    /// The first sweep of the volume wins — the coherent snapshot the RPG's
    /// own volume products are computed from, which the validation harnesses
    /// need when comparing against an EET/DVL twin.
    FirstOfVolume,
}

/// One moment's 360×230 grid on one tilt, with the sweep it came from.
pub struct MomentGrid {
    /// `[az_deg][range_km]`, `NaN` where no gate carried data.
    pub values: Vec<Vec<f32>>,
    /// Index into [`Scan::sweeps`] of the sweep this grid was computed from.
    pub sweep_index: usize,
    /// Whether this sweep displaced an earlier sweep at the same elevation — a
    /// SAILS/MRLE repeat resolved by [`DedupPolicy::NewestWins`]. Always
    /// `false` under [`DedupPolicy::FirstOfVolume`], which keeps the sweep a
    /// repeat would have displaced.
    pub displaced_repeat: bool,
}

/// Beam bottom/centre/top heights above the radar, km, at every range cell
/// centre (`r + 0.5` km) of one tilt.
pub struct BeamHeights {
    pub bottom_km: Vec<f64>,
    pub centre_km: Vec<f64>,
    pub top_km: Vec<f64>,
}

impl BeamHeights {
    /// Heights for a tilt centred on `elev_deg`, the bottom and top at half
    /// the half-power beamwidth below and above it.
    fn at_elevation(elev_deg: f64) -> Self {
        let half = HALF_POWER_BEAMWIDTH_DEG / 2.0;
        let at = |e: f64| -> Vec<f64> {
            (0..RANGE_BINS)
                .map(|r| beam_height_km(r as f64 + 0.5, e))
                .collect()
        };
        Self {
            bottom_km: at(elev_deg - half),
            centre_km: at(elev_deg),
            top_km: at(elev_deg + half),
        }
    }
}

/// One distinct elevation of the volume.
pub struct Tilt {
    /// The elevation key, degrees, rounded to 0.1° — the resolution sweeps are
    /// deduplicated at.
    pub elevation_deg: f64,
    /// Beam geometry at every range cell centre.
    pub heights: BeamHeights,
    /// One entry per requested moment, in the cube's moment order. `None` when
    /// no sweep at this elevation carries the moment.
    grids: Vec<Option<MomentGrid>>,
}

/// The volume as a stack of polar grids: one 360° × 230 km grid per tilt per
/// requested moment, computed **once** per scan and shared by every product
/// derived from it.
///
/// Sweeps are chosen **per moment**: a split cut publishes reflectivity and
/// velocity at the same elevation on different sweeps, so a tilt's
/// reflectivity grid and its velocity grid may legitimately come from
/// different sweep indices. The tilt list is the union of every requested
/// moment's elevations, ascending.
pub struct VolumeCube {
    moments: Vec<RadarProduct>,
    pub tilts: Vec<Tilt>,
}

impl VolumeCube {
    /// Build the cube with each moment's default statistic
    /// ([`CellStat::for_moment`]).
    pub fn build(scan: &Scan, moments: &[RadarProduct], policy: DedupPolicy) -> Self {
        let with_stats: Vec<(RadarProduct, CellStat)> = moments
            .iter()
            .map(|&m| (m, CellStat::for_moment(m)))
            .collect();
        Self::build_with_stats(scan, &with_stats, policy)
    }

    /// Build the cube with an explicit statistic per moment.
    pub fn build_with_stats(
        scan: &Scan,
        moments: &[(RadarProduct, CellStat)],
        policy: DedupPolicy,
    ) -> Self {
        // Per moment: (elevation key, sweep index, displaced an earlier
        // same-elevation sweep), in encounter order.
        let mut chosen: Vec<Vec<(f64, usize, bool)>> = vec![Vec::new(); moments.len()];
        for (si, sweep) in scan.sweeps().iter().enumerate() {
            let Some(first) = sweep.radials().first() else {
                continue;
            };
            // Keyed on the sweep's median elevation, not the first radial's —
            // see [`sweep_elevation_deg`] for what settling does to the first.
            let key =
                (sweep_elevation_deg(sweep.radials()).unwrap_or_default() * 10.0).round() / 10.0;
            for (mi, (moment, _)) in moments.iter().enumerate() {
                if moment.get_moment(first).is_none() {
                    continue;
                }
                match chosen[mi]
                    .iter_mut()
                    .find(|(k, ..)| (*k - key).abs() < 0.05)
                {
                    Some(entry) => {
                        if policy == DedupPolicy::NewestWins {
                            *entry = (entry.0, si, true);
                        }
                    }
                    None => chosen[mi].push((key, si, false)),
                }
            }
        }

        // The union of every moment's elevations, ascending.
        let mut keys: Vec<f64> = Vec::new();
        for per_moment in &chosen {
            for &(k, ..) in per_moment {
                if !keys.iter().any(|e| (e - k).abs() < 0.05) {
                    keys.push(k);
                }
            }
        }
        keys.sort_by(f64::total_cmp);

        let tilts = keys
            .into_iter()
            .map(|key| {
                let grids = moments
                    .iter()
                    .enumerate()
                    .map(|(mi, &(moment, stat))| {
                        chosen[mi]
                            .iter()
                            .find(|(k, ..)| (k - key).abs() < 0.05)
                            .map(|&(_, si, displaced)| MomentGrid {
                                values: sweep_to_grid(scan.sweeps()[si].radials(), moment, stat),
                                sweep_index: si,
                                displaced_repeat: displaced,
                            })
                    })
                    .collect();
                Tilt {
                    elevation_deg: key,
                    heights: BeamHeights::at_elevation(key),
                    grids,
                }
            })
            .collect();

        Self {
            moments: moments.iter().map(|&(m, _)| m).collect(),
            tilts,
        }
    }

    /// The moments this cube was built for, in grid order.
    pub fn moments(&self) -> &[RadarProduct] {
        &self.moments
    }

    /// The grid for one moment on one tilt. `None` when the tilt index is out
    /// of range, the moment was not requested, or no sweep at that elevation
    /// carries the moment.
    pub fn grid(&self, tilt: usize, moment: RadarProduct) -> Option<&MomentGrid> {
        let mi = self.moments.iter().position(|m| *m == moment)?;
        self.tilts.get(tilt)?.grids[mi].as_ref()
    }
}

/// One sweep collapsed onto the cube's grid for one moment: per whole-degree
/// azimuth cell the radial nearest the cell centre, per 1-km range cell `stat`
/// over the gates falling in it. `NaN` where no gate carried data; gate values
/// ≥ 999 are the decoder's sentinels and are dropped.
fn sweep_to_grid(radials: &[Radial], moment: RadarProduct, stat: CellStat) -> Vec<Vec<f32>> {
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
        let Some(md) = moment.get_moment(radial) else {
            continue;
        };
        let fg = md.first_gate_range_km();
        let gi = md.gate_interval_km();
        // (accumulator, gate count) per cell; what the accumulator holds
        // depends on `stat`.
        let mut acc = vec![(0.0f64, 0u32); RANGE_BINS];
        for (j, v) in md.values().iter().enumerate() {
            let MomentValue::Value(z) = v else { continue };
            if *z >= 999.0 || z.is_nan() {
                continue;
            }
            let r = (fg + j as f64 * gi) as usize;
            if r >= RANGE_BINS {
                continue;
            }
            match stat {
                CellStat::LinearZMean => acc[r].0 += 10f64.powf(*z as f64 / 10.0),
                CellStat::Mean => acc[r].0 += *z as f64,
                CellStat::Max => {
                    acc[r].0 = if acc[r].1 == 0 {
                        *z as f64
                    } else {
                        acc[r].0.max(*z as f64)
                    }
                }
            }
            acc[r].1 += 1;
        }
        for (r, (sum, n)) in acc.into_iter().enumerate() {
            if n > 0 {
                grid[cell][r] = match stat {
                    CellStat::LinearZMean => (10.0 * (sum / n as f64).log10()) as f32,
                    CellStat::Mean => (sum / n as f64) as f32,
                    CellStat::Max => sum as f32,
                };
            }
        }
    }
    grid
}

/// Echo tops: height (kft above radar) of the interpolated crossing of
/// [`ET_THRESHOLD_DBZ`], scanning tilts top-down per column of a
/// newest-wins reflectivity [`VolumeCube`].
pub fn compute_echo_tops(scan: &Scan) -> VolumetricGrid {
    let cube = VolumeCube::build(scan, &[RadarProduct::Reflectivity], DedupPolicy::NewestWins);
    // The tilts actually carrying reflectivity, bottom-up.
    let tilts: Vec<(&BeamHeights, &Vec<Vec<f32>>)> = cube
        .tilts
        .iter()
        .enumerate()
        .filter_map(|(ti, t)| {
            cube.grid(ti, RadarProduct::Reflectivity)
                .map(|g| (&t.heights, &g.values))
        })
        .collect();

    let mut out = vec![vec![f32::NAN; RANGE_BINS]; 360];
    for (az, row) in out.iter_mut().enumerate() {
        for (r, cell) in row.iter_mut().enumerate() {
            // topmost tilt meeting the threshold
            for ti in (0..tilts.len()).rev() {
                let z = tilts[ti].1[az][r];
                if !z.is_nan() && z >= ET_THRESHOLD_DBZ {
                    let h = tilts[ti].0.centre_km[r];
                    let ht = if ti + 1 < tilts.len() {
                        let z_up = tilts[ti + 1].1[az][r];
                        let h_up = tilts[ti + 1].0.centre_km[r];
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
                    *cell = (ht * 3.28084) as f32; // km -> kft
                    break;
                }
            }
        }
    }
    VolumetricGrid {
        values: out,
        range_bins: RANGE_BINS,
    }
}

#[cfg(test)]
pub(crate) mod tests {
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

    pub(crate) fn vcp() -> VolumeCoveragePattern {
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
    pub(crate) fn golden_scan() -> Scan {
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
    pub(crate) fn fnv1a64(grid: &VolumetricGrid) -> u64 {
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

    // ── VolumeCube ──────────────────────────────────────────────────────────

    /// A one-radial sweep whose moment is handed in directly, for tests that
    /// need full control of the encoding.
    fn one_radial_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        azimuth: f32,
        refl: Option<MomentData>,
        vel: Option<MomentData>,
        zdr: Option<MomentData>,
    ) -> Sweep {
        let radial = Radial::new(
            0,
            0,
            azimuth,
            1.0,
            RadialStatus::IntermediateRadialData,
            elevation_number,
            elevation_deg,
            refl,
            vel,
            None,
            zdr,
            None,
            None,
            None,
        );
        Sweep::new(elevation_number, radials_vec(radial))
    }

    fn radials_vec(r: Radial) -> Vec<Radial> {
        vec![r]
    }

    /// 1-km gates encoding the given bytes at scale/offset.
    fn moment(bytes: &[u8], scale: f32, offset: f32) -> MomentData {
        MomentData::from_fixed_point(
            bytes.len() as u16,
            0,
            1000,
            8,
            scale,
            offset,
            bytes.to_vec(),
        )
    }

    #[test]
    fn beam_heights_match_the_hand_computed_four_thirds_model() {
        // Range cell 100 (centre 100.5 km) on a 0.5° tilt, half-power
        // beamwidth 0.95°, effective radius 6371·4/3 km:
        //   centre = 100.5·sin 0.500° + 100.5²/(2·8494.667) = 1.4715221935 km
        //   bottom = 100.5·sin 0.025° + …                   = 0.6383567720 km
        //   top    = 100.5·sin 0.975° + …                   = 2.3046273386 km
        let h = BeamHeights::at_elevation(0.5);
        assert!((h.centre_km[100] - 1.4715221935087277).abs() < 1e-9);
        assert!((h.bottom_km[100] - 0.638356771987057).abs() < 1e-9);
        assert!((h.top_km[100] - 2.3046273386189857).abs() < 1e-9);
        // Cell 0 (centre 0.5 km) on a 19.5° tilt: 0.5·sin 19.5° + 0.5²/(2·Re′).
        let steep = BeamHeights::at_elevation(19.5);
        assert!((steep.centre_km[0] - 0.16691814473225194).abs() < 1e-9);
        assert_eq!(h.centre_km.len(), RANGE_BINS);
        assert_eq!(h.bottom_km.len(), RANGE_BINS);
        assert_eq!(h.top_km.len(), RANGE_BINS);
    }

    /// Both dedup policies, on the same volume, disagree exactly where they
    /// must: sweep identity, the displaced flag, and the values themselves —
    /// the SAILS repeat's cores are shifted, so cell (308°, 120 km) is hotter
    /// on the repeat than on the first look.
    #[test]
    fn dedup_policies_pick_opposite_ends_of_a_sails_pair() {
        let scan = Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, 360, 0.5, false),
                refl_sweep(2, 1.5, 360, 0.5, false),
                refl_sweep(3, 0.5, 360, 0.5, true), // SAILS repeat, shifted
            ],
        );
        let newest = VolumeCube::build(
            &scan,
            &[RadarProduct::Reflectivity],
            DedupPolicy::NewestWins,
        );
        let first = VolumeCube::build(
            &scan,
            &[RadarProduct::Reflectivity],
            DedupPolicy::FirstOfVolume,
        );
        assert_eq!(newest.tilts.len(), 2);
        assert_eq!(first.tilts.len(), 2);
        assert!((newest.tilts[0].elevation_deg - 0.5).abs() < 1e-12);
        assert!((newest.tilts[1].elevation_deg - 1.5).abs() < 1e-12);

        let n = newest.grid(0, RadarProduct::Reflectivity).unwrap();
        assert_eq!(n.sweep_index, 2);
        assert!(n.displaced_repeat, "the repeat displaced the first look");

        let f = first.grid(0, RadarProduct::Reflectivity).unwrap();
        assert_eq!(f.sweep_index, 0);
        assert!(!f.displaced_repeat);

        // The two policies must yield *different fields*, not just different
        // indices: the repeat's core C sits at 308°, the first look's at 300°.
        assert!(n.values[308][120] > f.values[308][120]);
        assert!(n.values[300][120] < f.values[300][120]);

        // The unrepeated tilt is identical under both policies.
        let nu = newest.grid(1, RadarProduct::Reflectivity).unwrap();
        let fu = first.grid(1, RadarProduct::Reflectivity).unwrap();
        assert_eq!(nu.sweep_index, 1);
        assert_eq!(fu.sweep_index, 1);
        assert!(!nu.displaced_repeat);
    }

    /// Two radials contend for one azimuth cell; the one nearer the cell
    /// centre must supply it.
    #[test]
    fn the_radial_nearest_the_cell_centre_wins() {
        // Cell 10's centre is 10.5°. 10.2° is 0.3 away, 10.4° is 0.1 away.
        let far = Radial::new(
            0,
            0,
            10.2,
            0.5,
            RadialStatus::IntermediateRadialData,
            1,
            0.5,
            Some(moment(&[126; 5], SCALE, OFFSET)), // 30 dBZ
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let near = Radial::new(
            0,
            1,
            10.4,
            0.5,
            RadialStatus::IntermediateRadialData,
            1,
            0.5,
            Some(moment(&[166; 5], SCALE, OFFSET)), // 50 dBZ
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let scan = Scan::new(vcp(), vec![Sweep::new(1, vec![far, near])]);
        let cube = VolumeCube::build(
            &scan,
            &[RadarProduct::Reflectivity],
            DedupPolicy::NewestWins,
        );
        let g = cube.grid(0, RadarProduct::Reflectivity).unwrap();
        assert!(
            (g.values[10][2] - 50.0).abs() < 1e-4,
            "cell 10 read {} — the farther radial won",
            g.values[10][2],
        );
        assert!(g.values[11][2].is_nan(), "no radial points at cell 11");
    }

    /// Below-threshold gates, ≥999 sentinels and empty cells all come out NaN;
    /// a legitimate value in the same radial survives.
    #[test]
    fn nan_propagation_keeps_holes_and_drops_sentinels() {
        // ZDR at scale 0.1, offset 0: byte 0 below threshold, byte 100 →
        // 1000 (a ≥999 sentinel, dropped), byte 50 → 500 (kept).
        let mut bytes = vec![0u8; 3];
        bytes.extend_from_slice(&[100, 100, 100]); // cell 3..6 → sentinel
        bytes.extend_from_slice(&[50, 50]); // cells 6, 7 → 500
        let zdr = MomentData::from_fixed_point(8, 0, 1000, 8, 0.1, 0.0, bytes);
        let scan = Scan::new(
            vcp(),
            vec![one_radial_sweep(1, 0.5, 42.5, None, None, Some(zdr))],
        );
        let cube = VolumeCube::build(
            &scan,
            &[RadarProduct::DifferentialReflectivity],
            DedupPolicy::NewestWins,
        );
        let g = cube
            .grid(0, RadarProduct::DifferentialReflectivity)
            .unwrap();
        assert!(g.values[42][0].is_nan(), "below-threshold gates filled in");
        assert!(g.values[42][3].is_nan(), "a ≥999 sentinel was kept");
        assert!((g.values[42][6] - 500.0).abs() < 1e-3);
        assert!(g.values[42][7 + 1..].iter().all(|v| v.is_nan()));
        assert!(g.values[43][0].is_nan(), "another azimuth cell filled in");
    }

    /// The statistic really is per moment: identical gate bytes read through
    /// reflectivity average in linear Z, through ZDR arithmetically, and a
    /// [`CellStat::Max`] override keeps the peak.
    #[test]
    fn cell_statistics_dispatch_per_moment() {
        // Two 0.5-km gates per 1-km cell: 20 dBZ and 40 dBZ.
        let bytes = vec![106u8, 146]; // (b-66)/2 → 20, 40
        let make = || MomentData::from_fixed_point(2, 0, 500, 8, SCALE, OFFSET, bytes.clone());
        let scan = Scan::new(
            vcp(),
            vec![one_radial_sweep(
                1,
                0.5,
                7.5,
                Some(make()),
                None,
                Some(make()),
            )],
        );
        let cube = VolumeCube::build(
            &scan,
            &[
                RadarProduct::Reflectivity,
                RadarProduct::DifferentialReflectivity,
            ],
            DedupPolicy::NewestWins,
        );
        let z = cube.grid(0, RadarProduct::Reflectivity).unwrap().values[7][0];
        let zdr = cube
            .grid(0, RadarProduct::DifferentialReflectivity)
            .unwrap()
            .values[7][0];
        // 10·log₁₀((10² + 10⁴)/2) = 37.0329…, not the 30.0 a dB-space mean
        // would give.
        assert!((z - 37.032_913).abs() < 1e-4, "got {z}");
        assert_eq!(zdr, 30.0, "ZDR must average arithmetically");

        let peaked = VolumeCube::build_with_stats(
            &scan,
            &[(RadarProduct::Reflectivity, CellStat::Max)],
            DedupPolicy::NewestWins,
        );
        let m = peaked.grid(0, RadarProduct::Reflectivity).unwrap().values[7][0];
        assert_eq!(m, 40.0, "Max must keep the peak");
    }

    /// A split cut: reflectivity and velocity at the same elevation on
    /// different sweeps. Each moment must come from its own sweep, on one
    /// shared tilt.
    #[test]
    fn a_split_cut_supplies_each_moment_from_its_own_sweep() {
        let scan = Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, 360, 0.5, false),
                velocity_only_sweep(2, 0.5),
                refl_sweep(3, 1.5, 360, 0.5, false),
            ],
        );
        let cube = VolumeCube::build(
            &scan,
            &[RadarProduct::Reflectivity, RadarProduct::Velocity],
            DedupPolicy::NewestWins,
        );
        assert_eq!(cube.tilts.len(), 2);

        let z = cube.grid(0, RadarProduct::Reflectivity).unwrap();
        let v = cube.grid(0, RadarProduct::Velocity).unwrap();
        assert_eq!(z.sweep_index, 0, "reflectivity from the surveillance cut");
        assert_eq!(v.sweep_index, 1, "velocity from the Doppler cut");
        assert!(
            !z.displaced_repeat && !v.displaced_repeat,
            "a split cut is not a SAILS repeat: neither moment displaced anything",
        );
        assert_eq!(v.values[42][50], 0.0, "byte 129 at scale 2/offset 129");

        // The upper tilt has reflectivity but no velocity.
        assert!(cube.grid(1, RadarProduct::Reflectivity).is_some());
        assert!(cube.grid(1, RadarProduct::Velocity).is_none());

        // A moment the cube was not built for is None everywhere.
        assert!(cube.grid(0, RadarProduct::SpectrumWidth).is_none());
        assert_eq!(
            cube.moments(),
            &[RadarProduct::Reflectivity, RadarProduct::Velocity],
        );
    }
}
