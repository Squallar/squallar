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

/// Half-power beamwidth of the WSR-88D antenna, degrees. Beam bottom and top
/// heights sit half of this below and above the tilt centre.
///
/// Re-exported from [`crate::beam`], which owns the crate's beam geometry;
/// [`crate::hail`] imports it from here alongside the rest of the cube's API.
pub const HALF_POWER_BEAMWIDTH_DEG: f64 = crate::beam::HALF_POWER_BEAMWIDTH_DEG;

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
/// `pub(crate)` for [`crate::hail`], whose column geometry has to sit in the
/// same 4/3-model vertical coordinate the cube's [`BeamHeights`] use.
///
/// The arithmetic moved to [`crate::beam::height_km`] — the crate's one home
/// for beam geometry — bit for bit; this name stays so the cube's own call
/// sites read in the cube's vocabulary. Every pinned echo-tops digest is a
/// test of that identity, and `beam::tests::
/// the_lifted_beam_height_is_bit_identical_to_the_one_volumetric_shipped`
/// is the local one.
pub(crate) fn beam_height_km(range_km: f64, elev_deg: f64) -> f64 {
    crate::beam::height_km(range_km, elev_deg)
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
        // `iter`, not `values`: this walk is sequential, so the `Vec` `values`
        // collects into would be eight bytes per gate allocated and dropped
        // for every azimuth cell of every sweep of the volume.
        for (j, v) in md.iter().enumerate() {
            let MomentValue::Value(z) = v else { continue };
            if z >= 999.0 || z.is_nan() {
                continue;
            }
            let r = (fg + j as f64 * gi) as usize;
            if r >= RANGE_BINS {
                continue;
            }
            match stat {
                CellStat::LinearZMean => acc[r].0 += 10f64.powf(z as f64 / 10.0),
                CellStat::Mean => acc[r].0 += z as f64,
                CellStat::Max => {
                    acc[r].0 = if acc[r].1 == 0 {
                        z as f64
                    } else {
                        acc[r].0.max(z as f64)
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
pub(crate) mod tests;
