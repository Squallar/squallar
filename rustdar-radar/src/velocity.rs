//! The volume's velocity, decoded once: one grid per tilt, the walk that
//! yields them in scan order, and the VAD wind profile fitted from them.
//!
//! Three products read the whole velocity volume rather than one cut of it —
//! NROT and SRV seed their dealiasers from a wind profile fitted across every
//! tilt ([`crate::nrot::WindProfileBuilder`]), and SRV's default storm motion
//! is the Bunkers right-mover off that same profile
//! ([`crate::srv::bunkers_right_mover`]). Each of them reached the volume
//! through its own transcription of the same twenty lines: walk the sweeps,
//! admit the ones carrying velocity, decode each into a dense grid, hand the
//! grid to the builder. There were three transcriptions —
//! `render::build_wind_profile`, `srv::volume_wind_profile` and
//! `derive::velocity_sweeps` — and they agreed, which is the good outcome of
//! a bad arrangement rather than a reason to keep it: what separates two
//! copies of a fit is not the arithmetic they share but *which tilts each
//! admits and what geometry it hands the builder*, and nothing was checking
//! that they still answered those the same way.
//!
//! # What the walk admits
//!
//! A sweep contributes when any of its radials carries the velocity moment
//! and it has at least three radials. The velocity test reads the *whole*
//! sweep, not its first radial: [`grid`] finds its geometry with a `find_map`
//! over every radial, so a first-radial test would have refused a tilt the
//! decoder was perfectly willing to decode. The two guards used to disagree
//! about that in all three copies — see
//! `a_tilt_whose_first_radial_lost_velocity_still_fits`.
//!
//! Nothing is excluded for being a repeat. A SAILS volume revisits 0.5°
//! several times and a split cut names the same angle twice; both are extra
//! samples of the same air, which is what a VAD fit wants, and
//! [`crate::nrot::WindProfileBuilder`] thins each layer to a uniform sample of
//! everything it was offered rather than keeping a prefix.
//!
//! # The elevation each tilt is fitted at
//!
//! The **first radial's** angle, not the sweep's median and not the VCP's
//! nominal cut. That is the same number the dealiaser and NROT's shear
//! normalization use for the sweep, so the fit, the prediction that seeds the
//! unfold, and the value that is finally painted all refer to one geometry —
//! and a profile queried at the angle it was fitted at is the case
//! [`crate::nrot::WindProfileBuilder`]'s own docs turn on.
//!
//! It is worth naming what that costs, because the cost is not zero and it is
//! not measured here. A cut's first radial is the one the antenna is still
//! settling on: `render.rs` bounds the spread at 0.23° (a KMRX 0.5° cut
//! opening at 0.283°, worst of 203 volumes), and 0.23° of elevation is 0.8 km
//! of beam height at 200 km — nearly three of the profile's 0.3 km layers, so
//! the far gates of an unsettled cut are binned into layers they do not
//! belong in. Whether a per-sweep median elevation fits a better profile is a
//! question for a measurement against real volumes, not for a rewrite here:
//! it would move every NROT and SRV pixel, and this module changed nobody's
//! output.

use crate::nrot::{VelocitySweep, WindProfile, WindProfileBuilder};
use nexrad_model::data::{DataMoment, Radial, Scan, Sweep};
use std::borrow::Borrow;

/// A velocity field as a dense azimuth × range grid in m/s, NaN where
/// undefined — the shape [`crate::nrot::VelocitySweep`] borrows, with the
/// geometry the renderer needs carried alongside.
///
/// **Three of the decoder's four answers arrive here as the same NaN.**
/// [`grid`] initialises every gate to `f64::NAN` and writes only
/// `MomentValue::Value`, so `BelowThreshold`, `RangeFolded` and a gate the
/// radar never reported are indistinguishable to everything downstream — the
/// dealiaser, the median filter's occupancy rules, and the stencils' demand
/// that every tap cell be intact. `sampler.rs` keeps the distinction
/// (`SampleStatus`), `xsect.rs` consumes it and `palette.rs` paints
/// range-folded deliberately; this path alone flattens it.
///
/// Measured 2026-08-12 on real archive volumes, lowest Doppler cut, over the
/// 7.05–20 nm annulus the reference comparison uses:
///
/// | volume | below threshold | range folded |
/// |---|---|---|
/// | KDMX 2022-03-05 | 19.7% | 2.9% |
/// | KTLX 2024-12-16 | 21.0% | 1.0% |
/// | KHNX 2024-12-16 | 9.9% | **0.0%** |
/// | KFTG 2023-06-22 | 3.1% | **4.5%** |
/// | KMSX 2022-06-04 | 7.3% | 1.3% |
/// | KCRP 2017-08-26 | 0.6% | 0.0% |
///
/// So neither is a rounding error. Below-threshold means *the radar looked and
/// found nothing*, which an occupancy rule ought to weigh differently from *no
/// gate was reported*; range-folded means *there is signal and its velocity is
/// ambiguous*, which is the opposite of absence, and it peaks on the
/// mesocyclone volume. The census stands as measured — the numbers above were
/// re-measured against it and reproduced (see [`status`](Self::status)) — and
/// the synthetic corpus still cannot show it, because its patcher repaints
/// every gate and so reads zero folded everywhere.
///
/// **The flattening is now recoverable**: [`status`](Self::status) carries the
/// three apart. What has *not* changed is any number this grid reports, or any
/// consumer's behaviour — see that field for what still reads only `values`.
#[derive(Debug, Clone)]
pub struct VelocityGrid {
    /// m/s per (radial, gate); NaN is no data.
    pub values: Vec<Vec<f64>>,
    /// Why each gate of [`values`](Self::values) is `NaN`, at the same
    /// `(radial, gate)` indices — the distinction the census above measures
    /// the size of. [`GateReport::Value`] exactly where `values` is finite.
    ///
    /// One byte per gate against the value's eight: about 12% on top of a
    /// super-res cut, which is why [`tilts`] stays lazy for the callers that
    /// only want a wind fit. The alternatives, and why a parallel plane beat
    /// them, are argued at [`crate::volumetric`]'s `sweep_to_grid`; the same
    /// reasoning applies here and the pattern is already shipped in
    /// `xsect.rs`.
    ///
    /// **Nothing reads this yet.** That is deliberate and is the scope line:
    /// the channel is what this delivers, and every consumer that could use it
    /// — the dealiaser's coverage rules, the median filter's two occupancy
    /// cliffs (`crate::nrot`'s `MEDIAN_MIN_RAW_OCC` and
    /// `MEDIAN_MIN_DEALIASED_OCC`), `tap_stencil`'s demand that every tap be
    /// intact, and NROT's data-margin rule — is a separate change with its own
    /// measurement, because each of them would move painted pixels.
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
    ///
    /// `declared_nyquist_ms` is what this cut said about where its velocity
    /// folds ([`crate::nyquist::DeclaredNyquist`]); `None` leaves the reader
    /// to estimate the limit off the data. The wind fit passes `None` and
    /// that is not an omission: it never unfolds anything — it trims folded
    /// samples out of each layer statistically — so it has no use for a fold
    /// limit, and handing it one would suggest it did.
    ///
    /// The view carries [`status`](Self::status) as well as `values`, so a
    /// consumer in `nrot` that asks "did the radar look here" gets the
    /// decoder's answer rather than the flattened `NaN`.
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
///
/// The grid's geometry — gate count, first gate range, gate interval — is the
/// first velocity-carrying radial's, and a radial that carries none is a row
/// of NaN rather than a missing row, so the row count and `azimuths_deg`
/// always match the sweep the caller handed in.
///
/// Range-folded and below-threshold gates both become NaN in `values` — see
/// [`VelocityGrid`] for how much of a real sweep that flattens, and
/// [`VelocityGrid::status`] for the plane that now says which is which.
///
/// A radial carrying no velocity moment at all is a row of
/// [`GateReport::NotReported`], which is the honest answer and the one the
/// row-of-NaN convention could not give.
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
        // One gate is one cell here — no aggregation — so this is the
        // decoder's answer verbatim, not a `max` over several.
        let mut reports = vec![GateReport::NotReported; gate_count];
        if let Some(moment) = radial.velocity() {
            // `iter`, not `values`: sequential, and `take` then stops decoding
            // at `gate_count` instead of collecting every gate first.
            for (j, val) in moment.iter().enumerate().take(gate_count) {
                reports[j] = GateReport::of(&val);
                if let nexrad_model::data::MomentValue::Value(v) = val {
                    if !v.is_nan() && v < 999.0 {
                        gates[j] = v as f64;
                    } else {
                        // A reported gate whose number is the decoder's
                        // out-of-range sentinel. It says nothing this plane
                        // can carry, so it does not claim to be a value.
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
/// everything downstream computes it at (see the module docs), and its
/// decoded grid.
pub struct VelocityTilt<'s> {
    /// The source sweep, kept so a caller can reach its `elevation_number` —
    /// the key [`crate::nyquist::DeclaredNyquist`] is written under — and its
    /// radials.
    pub sweep: &'s Sweep,
    pub elevation_deg: f64,
    pub grid: VelocityGrid,
}

/// Every velocity-carrying tilt of the volume, decoded, in scan order.
///
/// Lazy on purpose: a super-res volume's fifteen velocity cuts are tens of
/// megabytes of `f64` between them, and a caller that only wants the wind
/// profile ([`volume_wind_profile`]) drops each grid before decoding the
/// next. A caller that wants to keep them — a derivation that renders every
/// tilt — collects.
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
            // The velocity guard: no grid, no tilt. See the module docs for
            // why this is the whole-sweep test rather than a first-radial one.
            grid: grid(radials)?,
        })
    })
}

/// Fit the volume wind profile from a run of velocity tilts.
///
/// Takes tilts by value or by reference — [`volume_wind_profile`] streams
/// them in and drops each after offering it, [`crate::derive`] holds the
/// whole volume's and lends them — so neither caller has to decode a tilt
/// twice to have both the grid and the fit.
pub fn wind_profile_of<'s, T: Borrow<VelocityTilt<'s>>>(
    tilts: impl IntoIterator<Item = T>,
) -> Option<WindProfile> {
    // Collecting is what lets the second pass see the same tilts the first
    // did. It is cheap where it happens: `crate::derive` already holds the
    // volume's tilts and lends them, so `T` is a reference there and this is a
    // vector of pointers. The streaming caller does not come through here —
    // see [`volume_wind_profile`], which walks the scan twice instead so it
    // can keep dropping each grid as it goes.
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
///
/// # Breaking the circle
///
/// The profile is fitted on aliased velocity and then used to unfold that same
/// velocity. Every folded gate in a layer's samples pulls that layer's wind
/// toward zero, the dealiaser is seeded with the wind that folding produced,
/// and it proposes branches from it. The fit is not short — its median top
/// fitted layer is 6.6 km, *higher* than the RPG's own NVW — it is simply
/// wrong at the heights it does reach, and wrong in one direction.
///
/// So: fit, unfold under that fit, refit on what came back. A seed-swap over
/// 754 315 RPG-folded gates measured the arms with the dealiaser held
/// byte-identical, and a VAD fitted on a well-dealiased field was the best
/// seed of any tried — 98.72% precision against the shipped fit's 82.14%,
/// ahead even of substituting the RPG's own published profile. The same
/// experiment is why this is not sold as a recall fix: the seed saturates near
/// 34% recall however good it gets, because 79.4% of the dealiaser's remaining
/// gap is structural and is not a seed question at all.
///
/// # What this pass is not
///
/// It is **not** the experiment's winning arm, and the two must not be
/// confused. That arm fitted over a field dealiased by something better than
/// us; this one fits over a field dealiased by us, seeded by the very profile
/// it is trying to improve on. It is a bootstrap toward that ceiling, not the
/// ceiling, and the only honest way to know where between the two it lands is
/// to measure it.
///
/// # The fold limit this pass runs under
///
/// `sweep(None)`, so [`crate::nrot`] estimates the fold limit off the data
/// rather than reading a declaration. That is not an oversight and it is not
/// free. [`VelocityTilt`] carries the sweep, so a caller holding a
/// [`crate::nyquist::DeclaredNyquist`] could look the declaration up by
/// elevation number — but the two callers here reach this through signatures
/// whose other users are outside this module, and the estimate is exact on
/// precisely the volumes this pass exists for: a sweep that folded reaches its
/// own limit by construction, so the estimate is only an *under*estimate on
/// sweeps with nothing to unfold. Under one, `nrot::dealias` abandons the pass
/// outright rather than inventing folds. Passing a limit here would also
/// suggest the fit unfolds, and it does not — it fits what the dealiaser
/// already unfolded.
fn offer_dealiased(builder: &mut WindProfileBuilder, tilt: &VelocityTilt<'_>, seed: &WindProfile) {
    let mut unfolded = tilt.grid.values.clone();
    // `Coverage`, not `NoFalseShear`: a wind fit wants as many correctly
    // unfolded gates as it can get and does its own trimming, where the
    // stricter profile censors residual fold walls that a fit would simply
    // have discarded. The clone is one tilt's grid at a time and is dropped
    // with `unfolded` at the end of the call.
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
    // Two lazy walks rather than one collected one: [`tilts`] drops each grid
    // before decoding the next, and a super-res volume's fifteen velocity cuts
    // are tens of megabytes of `f64` between them. Walking twice pays for the
    // second pass in decode time, which is the cheaper of the two currencies
    // here and the one this function was written to spend.
    let mut builder = WindProfileBuilder::new();
    for tilt in tilts(scan) {
        builder.add_sweep(&tilt.grid.sweep(None), tilt.elevation_deg);
    }
    let first = builder.finish()?;
    let mut builder = WindProfileBuilder::new();
    for tilt in tilts(scan) {
        offer_dealiased(&mut builder, &tilt, &first);
    }
    // A refit that came back with nothing is not an improvement on something.
    // `dealias` writes NaN over every gate it could not resolve, so the second
    // pass is fitted on strictly fewer samples than the first and can fall
    // under the 200-sample floor in layers the first pass cleared.
    Some(builder.finish().unwrap_or(first))
}

#[cfg(test)]
#[path = "velocity/tests.rs"]
mod tests;
