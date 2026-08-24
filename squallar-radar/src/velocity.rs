//! The volume's velocity, decoded once: one grid per tilt, the walk that
//! yields them in scan order, and the VAD wind profile fitted from them.
//!
//! A sweep contributes when any of its radials carries the velocity moment
//! and it has at least three radials. The velocity test reads the *whole*
//! sweep, not its first radial.
//!
//! Nothing is excluded for being a repeat. A SAILS volume revisits 0.5°
//! several times and a split cut names the same angle twice; both are extra
//! samples of the same air, which is what a VAD fit wants, and
//! [`crate::nrot::WindProfileBuilder`] thins each layer to a uniform sample of
//! everything it was offered rather than keeping a prefix.
//!
//! The tilt elevation is the **first radial's** angle, not the sweep's median
//! and not the VCP's nominal cut — the same number the dealiaser and NROT's
//! shear normalization use for the sweep.

use crate::nrot::{VelocitySweep, WindProfile, WindProfileBuilder};
use nexrad_model::data::{DataMoment, Radial, Scan, Sweep};
use std::borrow::Borrow;

/// A velocity field as a dense azimuth × range grid in m/s, NaN where
/// undefined — the shape [`crate::nrot::VelocitySweep`] borrows, with the
/// geometry the renderer needs carried alongside.
#[derive(Debug, Clone)]
pub struct VelocityGrid {
    /// m/s per (radial, gate); NaN is no data.
    pub values: Vec<Vec<f64>>,
    /// Why each gate of [`values`](Self::values) is `NaN`, at the same
    /// `(radial, gate)` indices — the distinction the census above measures
    /// the size of. [`GateReport::Value`] exactly where `values` is finite.
    pub status: Vec<Vec<crate::types::GateReport>>,
    /// Radial **centre** azimuths, degrees, in sweep order.
    pub azimuths_deg: Vec<f64>,
    pub gate_count: usize,
    /// Range to the **centre** of the first gate, km — the Level II moment
    /// header's convention, and what the renderer centres gate strips on.
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
}

impl VelocityGrid {
    /// The borrowed view every consumer in [`crate::nrot`] reads — the
    /// dealiaser, the NROT stencil, the VAD fit.
    pub fn sweep(&self, declared_nyquist_ms: Option<f64>) -> VelocitySweep<'_> {
        VelocitySweep {
            vel_grid: &self.values,
            azimuths_deg: &self.azimuths_deg,
            gate_count: self.gate_count,
            first_gate_range_km: self.first_gate_range_km,
            gate_interval_km: self.gate_interval_km,
            declared_nyquist_ms,
            status: Some(&self.status),
        }
    }
}

/// The raw velocity of one sweep as a grid, or `None` when no radial carries
/// the moment.
pub fn grid(radials: &[Radial]) -> Option<VelocityGrid> {
    use crate::types::GateReport;
    let first_vel = radials.iter().find_map(|r| r.velocity())?;
    let gate_count = first_vel.gate_count() as usize;
    let first_gate_range_km = first_vel.first_gate_range_km();
    let gate_interval_km = first_vel.gate_interval_km();

    let mut values: Vec<Vec<f64>> = Vec::with_capacity(radials.len());
    let mut status: Vec<Vec<GateReport>> = Vec::with_capacity(radials.len());
    let mut azimuths_deg: Vec<f64> = Vec::with_capacity(radials.len());
    for radial in radials {
        azimuths_deg.push(radial.azimuth_angle_degrees() as f64);
        let mut gates = vec![f64::NAN; gate_count];
        let mut reports = vec![GateReport::NotReported; gate_count];
        if let Some(moment) = radial.velocity() {
            for (j, val) in moment.iter().enumerate().take(gate_count) {
                reports[j] = GateReport::of(&val);
                if let nexrad_model::data::MomentValue::Value(v) = val {
                    if !v.is_nan() && v < 999.0 {
                        gates[j] = v as f64;
                    } else {
                        reports[j] = GateReport::NotReported;
                    }
                }
            }
        }
        values.push(gates);
        status.push(reports);
    }
    Some(VelocityGrid {
        values,
        status,
        azimuths_deg,
        gate_count,
        first_gate_range_km,
        gate_interval_km,
    })
}

/// One velocity tilt of a volume: the sweep it came from, the elevation
pub struct VelocityTilt<'s> {
    pub sweep: &'s Sweep,
    pub elevation_deg: f64,
    pub grid: VelocityGrid,
}

/// Every velocity-carrying tilt of the volume, decoded, in scan order.
pub fn tilts(scan: &Scan) -> impl Iterator<Item = VelocityTilt<'_>> {
    scan.sweeps().iter().filter_map(|sweep| {
        let radials = sweep.radials();
        let first = radials.first()?;
        if radials.len() < 3 {
            return None;
        }
        Some(VelocityTilt {
            sweep,
            elevation_deg: f64::from(first.elevation_angle_degrees()),
            grid: grid(radials)?,
        })
    })
}

/// Fit the volume wind profile from a run of velocity tilts.
pub fn wind_profile_of<'s, T: Borrow<VelocityTilt<'s>>>(
    tilts: impl IntoIterator<Item = T>,
) -> Option<WindProfile> {
    let tilts: Vec<T> = tilts.into_iter().collect();
    let mut builder = WindProfileBuilder::new();
    for tilt in &tilts {
        let tilt = tilt.borrow();
        builder.add_sweep(&tilt.grid.sweep(None), tilt.elevation_deg);
    }
    let first = builder.finish()?;
    let mut builder = WindProfileBuilder::new();
    for tilt in &tilts {
        offer_dealiased(&mut builder, tilt.borrow(), &first);
    }
    Some(builder.finish().unwrap_or(first))
}

/// Offer one tilt to `builder` after unfolding it under `seed` — the second
/// pass's single step.
fn offer_dealiased(builder: &mut WindProfileBuilder, tilt: &VelocityTilt<'_>, seed: &WindProfile) {
    let mut unfolded = tilt.grid.values.clone();
    let _ = crate::nrot::dealias_for_refit(
        &mut unfolded,
        &tilt.grid.sweep(None),
        tilt.elevation_deg,
        Some(seed),
    );
    builder.add_sweep(
        &VelocitySweep {
            vel_grid: &unfolded,
            azimuths_deg: &tilt.grid.azimuths_deg,
            gate_count: tilt.grid.gate_count,
            first_gate_range_km: tilt.grid.first_gate_range_km,
            gate_interval_km: tilt.grid.gate_interval_km,
            declared_nyquist_ms: None,
            status: Some(&tilt.grid.status),
        },
        tilt.elevation_deg,
    );
}

/// Fit the volume wind profile from every velocity tilt in the scan.
///
/// The environmental wind NROT's and SRV's dealiasers seed from, and the
/// hodograph SRV's default storm motion is read off. `None` when no layer
/// gathered enough samples to solve — see [`crate::nrot::WindProfileBuilder`]
/// for what "enough" is and what it does with the layers that had none.
pub fn volume_wind_profile(scan: &Scan) -> Option<WindProfile> {
    let mut builder = WindProfileBuilder::new();
    for tilt in tilts(scan) {
        builder.add_sweep(&tilt.grid.sweep(None), tilt.elevation_deg);
    }
    let first = builder.finish()?;
    let mut builder = WindProfileBuilder::new();
    for tilt in tilts(scan) {
        offer_dealiased(&mut builder, &tilt, &first);
    }
    Some(builder.finish().unwrap_or(first))
}

#[cfg(test)]
#[path = "velocity/tests.rs"]
mod tests;
