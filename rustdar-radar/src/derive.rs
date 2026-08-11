//! Derived-product volumes: SRV, NROT and Level II KDP as sampleable scans.
//!
//! The sampler reads native moments off radials, and
//! [`crate::sampler::samplable`] deliberately refuses the derived products so
//! a raw volume can never be sampled under a derived label — storm-relative
//! velocity read from the raw velocity moment "would look right and be a
//! different field". This module is the other half of that refusal: it
//! **computes** the derived field per sweep, off the frame thread (its only
//! callers are the render-worker paths in `xsect::render_section` and
//! `voxel::build_voxels`), and writes it back as a synthetic scan whose
//! radials carry the derived values in the product's source moment slot. From
//! there the whole existing machinery — the tilt ladder, the column sampler,
//! the cross-section cut, the voxel resample — works unchanged, because the
//! derived field really is "a moment on radials" by the time anything samples
//! it.
//!
//! # The three derivations
//!
//! * **Storm-relative velocity** — per velocity sweep:
//!   [`crate::srv::compute_srv_grid`] (dealias against the volume wind fit,
//!   then subtract the storm motion). The motion vector is the user's
//!   override where set, else Bunkers from the volume's own
//!   [`crate::srv::volume_wind_profile`]; with neither, the product refuses —
//!   painting base velocity under a storm-relative label is the failure the
//!   whole arrangement exists to prevent. The derived field is dealiased by
//!   construction, which is why the sampler's velocity fold guard stays
//!   **unarmed** for SRV (`Blend::folds_at_measured_limit`).
//! * **Normalized rotation** — per velocity sweep:
//!   [`crate::nrot::compute_nrot_grid_with_profile`], the measured GR-parity
//!   pipeline (dealias, median, split-tap stencil, despeckle), wind-profile
//!   guided where the fit succeeds and unguided otherwise.
//! * **Specific differential phase** — per ΦDP sweep:
//!   [`crate::kdp::compute_kdp`], the RPG-shaped estimator over ΦDP with the
//!   Z and ρHV gates, at its recombined 1° × 0.25 km geometry. The 2D map's
//!   KDP stays the Level III product; this Level II derivation exists for the
//!   vertical views, which slice volumes the Level III feed does not carry.
//!
//! # Cadence and cost
//!
//! Derivation runs inside the section/voxel jobs, so it recomputes exactly
//! when they do: **per sealed sweep** for a live volume (the same rebuild key
//! the native moments have — a derived product is never staler than its
//! volume), per request for a section whose line moves. The cost is the
//! whole-volume derivation on the worker: NROT is the heavy one (the full
//! stencil pipeline per velocity tilt), SRV is a dealias plus a subtraction,
//! KDP a filtered range derivative per ΦDP tilt. Nothing here runs on the
//! frame thread.
//!
//! # Encodings
//!
//! Each derived field is written through its own fixed-point codec (below),
//! chosen so raw codes 2..=255 span the product's display range exactly; raw
//! 0 is "no data", raw 1 is left unused (the Level II convention reserves it
//! for range folding). `voxel::data_levels_for` declares the matching ramp
//! ranges, so the voxel index ramp and this codec agree about what the
//! extremes mean.

use nexrad_model::data::{MomentData, Radial, RadialStatus, Scan, Sweep};

use crate::kdp;
use crate::nrot;
use crate::srv;
use crate::types::{MomentSlot, RadarProduct};

/// A scan ready for sampling under `product`: the original borrow where the
/// product is a native moment, an owned synthetic scan where it is derived.
pub enum Prepared<'s> {
    /// The product is one of the six native moments — sample the scan as-is.
    Native(&'s Scan),
    /// The product was derived; the field lives in
    /// [`derived_slot`]'s moment of these synthetic sweeps.
    Derived(Box<Scan>),
}

impl Prepared<'_> {
    /// The scan to sample.
    pub fn scan(&self) -> &Scan {
        match self {
            Prepared::Native(scan) => scan,
            Prepared::Derived(scan) => scan,
        }
    }
}

/// The moment slot a derived product's field is written into (and read from),
/// or `None` for a product this module does not derive.
///
/// SRV and NROT are velocity derivations; KDP is a ΦDP derivation. The slot
/// doubles as the ladder key: an SRV ladder is the velocity ladder, which is
/// what makes `ladder_fingerprint` agree between the raw volume the frame
/// thread fingerprints and the derived volume the worker samples.
pub fn derived_slot(product: RadarProduct) -> Option<MomentSlot> {
    match product {
        RadarProduct::StormRelativeVelocity | RadarProduct::NormalizedRotation => {
            Some(MomentSlot::Velocity)
        }
        RadarProduct::SpecificDifferentialPhase => Some(MomentSlot::DifferentialPhase),
        _ => None,
    }
}

/// The slot a product samples through in the vertical views: its native slot,
/// or its derivation's source slot.
///
/// **This is the vertical views' product gate.** `samplable` alone answers
/// "can a raw scan be sampled under this product"; this answers "can the
/// vertical pipeline render it at all", which additionally admits the three
/// derived products because [`prepare`] can manufacture the scan they sample.
/// `None` remains an honest refusal: the hybrid classification, the column
/// integrals and the precipitation rate have no per-tilt field to derive.
pub fn volume_slot(product: RadarProduct) -> Option<MomentSlot> {
    crate::sampler::samplable(product).or_else(|| derived_slot(product))
}

/// The fixed-point codec `(scale, offset)` a derived product's synthetic
/// moment is written through: `value = (raw − offset) / scale`.
///
/// * SRV reuses velocity's own `(2, 129)` — same units, same resolution, and
///   raw 2..=255 spans −63.5..+63.0 m/s.
/// * NROT: raw 2..=255 spans exactly −4..+4 (unitless); GR pins the meso
///   class near |1|, so ±4 headroom keeps extreme couplets on scale at
///   0.0316 resolution.
/// * KDP: raw 2..=255 spans exactly
///   [`kdp::KDP_MIN_DISPLAY`]..[`kdp::KDP_MAX_DISPLAY`] (−2.05..10 °/km),
///   the estimator's own display clamp.
fn codec(product: RadarProduct) -> (f32, f32) {
    match product {
        RadarProduct::StormRelativeVelocity => (2.0, 129.0),
        RadarProduct::NormalizedRotation => (253.0 / 8.0, 2.0 + 4.0 * (253.0 / 8.0)),
        RadarProduct::SpecificDifferentialPhase => {
            let scale = 253.0 / (kdp::KDP_MAX_DISPLAY - kdp::KDP_MIN_DISPLAY);
            (scale, 2.0 - kdp::KDP_MIN_DISPLAY * scale)
        }
        _ => unreachable!("codec is only defined for the derived products"),
    }
}

/// Prepare a volume for sampling under `product`: pass a native moment through,
/// derive a derived one, refuse (`None`) what cannot be derived — a product
/// with no per-tilt field, a volume without the source moment, or SRV with no
/// storm motion vector from either the user or the volume's own wind fit.
///
/// A [`crate::nyquist::Volume`] and not a bare `Scan`, for the reason that type
/// exists: SRV and NROT both **dealias**, and the limit they fold around is the
/// one each cut declared, which the model type drops. Both callers
/// ([`crate::xsect::render_section`] and [`crate::voxel::build_voxels`]) already
/// hold the pairing — they pass it straight on to the sampler — so this asks
/// for what they have rather than for the half of it that would silently make
/// the vertical views dealias on different limits from the guard that samples
/// them.
///
/// `storm_motion_override` is the user's `(speed_kt, direction_from_deg)`
/// vector, read only by SRV — the same pair the plan-view SRV path carries.
pub fn prepare<'s>(
    volume: crate::nyquist::Volume<'s>,
    product: RadarProduct,
    storm_motion_override: Option<(f32, f32)>,
) -> Option<Prepared<'s>> {
    if crate::sampler::samplable(product).is_some() {
        return Some(Prepared::Native(volume.scan()));
    }
    let derived = match product {
        RadarProduct::StormRelativeVelocity => derive_srv(volume, storm_motion_override)?,
        RadarProduct::NormalizedRotation => derive_nrot(volume)?,
        RadarProduct::SpecificDifferentialPhase => derive_kdp(volume.scan())?,
        _ => return None,
    };
    Some(Prepared::Derived(Box::new(derived)))
}

/// Every velocity-carrying sweep of the volume with its decoded grid and the
/// fold limit its cut declared, in scan order — the shared walk SRV and NROT
/// derive over.
///
/// The declaration is looked up here, by `elevation_number`, because that is
/// the key [`crate::nyquist::DeclaredNyquist`] is written under and the one
/// thing about a sweep that survives every hop the table takes. `None` for a
/// cut the volume did not name, which both derivations pass on to the
/// dealiaser as "estimate it".
fn velocity_sweeps(
    volume: crate::nyquist::Volume<'_>,
) -> Vec<(&Sweep, f64, srv::VelocityGrid, Option<f64>)> {
    volume
        .scan()
        .sweeps()
        .iter()
        .filter_map(|sweep| {
            let radials = sweep.radials();
            let first = radials.first()?;
            if first.velocity().is_none() || radials.len() < 3 {
                return None;
            }
            let grid = srv::velocity_grid(radials)?;
            Some((
                sweep,
                f64::from(first.elevation_angle_degrees()),
                grid,
                volume.declared_nyquist().get(sweep.elevation_number()),
            ))
        })
        .collect()
}

fn derive_srv(
    volume: crate::nyquist::Volume<'_>,
    storm_motion_override: Option<(f32, f32)>,
) -> Option<Scan> {
    let scan = volume.scan();
    let profile = srv::volume_wind_profile(scan);
    let user = storm_motion_override.and_then(|(speed_kt, direction_deg)| {
        srv::SrvMotion::user_override(speed_kt, direction_deg)
    });
    // No vector, no SRV: base velocity under a storm-relative label is the
    // failure this refusal exists to prevent.
    let Some(motion) = srv::storm_motion(profile.as_ref(), user) else {
        log::warn!(
            "SRV derivation refused: no storm motion vector — no user override \
             and no Bunkers fit from the volume's own winds"
        );
        return None;
    };

    let sweeps: Vec<Sweep> = velocity_sweeps(volume)
        .into_iter()
        .filter_map(|(sweep, elevation_deg, _, declared_nyquist_ms)| {
            let grid = srv::compute_srv_grid(
                sweep.radials(),
                elevation_deg,
                profile.as_ref(),
                &motion,
                declared_nyquist_ms,
            )?;
            Some(synth_sweep(
                sweep,
                &grid.values,
                &grid.azimuths_deg,
                grid.first_gate_range_km,
                grid.gate_interval_km,
                RadarProduct::StormRelativeVelocity,
            ))
        })
        .collect();
    non_empty_scan(scan, sweeps)
}

fn derive_nrot(volume: crate::nyquist::Volume<'_>) -> Option<Scan> {
    let scan = volume.scan();
    let profile = srv::volume_wind_profile(scan);
    let sweeps: Vec<Sweep> = velocity_sweeps(volume)
        .into_iter()
        .map(|(sweep, elevation_deg, grid, declared_nyquist_ms)| {
            let values = nrot::compute_nrot_grid_with_profile(
                &nrot::VelocitySweep {
                    vel_grid: &grid.values,
                    azimuths_deg: &grid.azimuths_deg,
                    gate_count: grid.gate_count,
                    first_gate_range_km: grid.first_gate_range_km,
                    gate_interval_km: grid.gate_interval_km,
                    declared_nyquist_ms,
                },
                elevation_deg,
                profile.as_ref(),
            );
            synth_sweep(
                sweep,
                &values,
                &grid.azimuths_deg,
                grid.first_gate_range_km,
                grid.gate_interval_km,
                RadarProduct::NormalizedRotation,
            )
        })
        .collect();
    non_empty_scan(scan, sweeps)
}

fn derive_kdp(scan: &Scan) -> Option<Scan> {
    let params = kdp::KdpParams {
        isdp_est_deg: kdp::estimate_volume_isdp(scan),
        ..kdp::KdpParams::render_fallback()
    };
    let sweeps: Vec<Sweep> = scan
        .sweeps()
        .iter()
        .filter_map(|sweep| {
            let radials = sweep.radials();
            radials.first()?.differential_phase()?;
            let derived = kdp::compute_kdp(radials, &params);
            if derived.is_none() {
                log::warn!(
                    "KDP derivation: the estimator refused a \u{3a6}DP-carrying sweep \
                     (elevation {}, {} radials)",
                    sweep.elevation_number(),
                    radials.len(),
                );
            }
            let derived = derived?;
            // The estimator's f32 rows, widened once for the shared encoder.
            let values: Vec<Vec<f64>> = derived
                .values
                .iter()
                .map(|row| row.iter().map(|&v| f64::from(v)).collect())
                .collect();
            Some(synth_sweep(
                sweep,
                &values,
                &derived.azimuths_deg,
                derived.first_gate_km,
                derived.gate_interval_km,
                RadarProduct::SpecificDifferentialPhase,
            ))
        })
        .collect();
    non_empty_scan(scan, sweeps)
}

/// The derived scan, under the source volume's own coverage pattern — the
/// ladder is resolved against the same cut table either way — or `None` when
/// nothing derived. Logged, so a `None` swallowed by a `?` upstream still
/// leaves its reason somewhere — the same rule `render_section` states.
fn non_empty_scan(source: &Scan, sweeps: Vec<Sweep>) -> Option<Scan> {
    if sweeps.is_empty() {
        log::warn!(
            "derivation refused: no sweep of the volume derived (of {} sweeps present)",
            source.sweeps().len(),
        );
        return None;
    }
    Some(Scan::new(source.coverage_pattern().clone(), sweeps))
}

/// One synthetic sweep: `values` written through the product's codec into the
/// product's [`derived_slot`], geometry from the derivation's own grid,
/// identity (elevation number and angle) from the source sweep.
fn synth_sweep(
    source: &Sweep,
    values: &[Vec<f64>],
    azimuths_deg: &[f64],
    first_gate_km: f64,
    gate_interval_km: f64,
    product: RadarProduct,
) -> Sweep {
    let (scale, offset) = codec(product);
    let slot = derived_slot(product).expect("synth_sweep is only called for derived products");
    let elevation_number = source.elevation_number();
    let elevation_deg = source
        .radials()
        .first()
        .map_or(0.0, Radial::elevation_angle_degrees);
    // The source sweep's clock, carried onto every synthetic radial for the
    // same reason `elevation_deg` above is: a derivation computes new *values*
    // for a tilt the radar already flew, and when it was flown is a property of
    // that tilt rather than of this computation. Left at the `0` it started
    // from, the derived products would be the one family whose sections could
    // not say how old the rung under the pointer is — silently, and only in the
    // product, since a rung age is read off these radials. See
    // [`crate::sampler::Rung`]'s `collected_ms`.
    let collected_ms = crate::render_input::sweep_collected_ms(source.radials());

    // How much sky one row of *this* grid stands for, which on a sector is not
    // `360 / rows`: a 36° NROT sector of 72 rows would declare 5°, ten times
    // the arc each row was computed over. [`crate::azimuth::Rows`] is the one
    // place that is decided, so a synthetic radial declares the step the
    // stencils differentiated the grid over and the plan view paints it at
    // (`render::derived_grid_wedge_deg`). A complete rotation declares exactly
    // what it always has: the closed branch *is* `360 / rows`, and that
    // division agrees to the bit in f32 and f64 for every row count up to
    // 100 000.
    //
    // Nothing reads *this field* off a derived radial today — the vertical
    // views sample these sweeps by moment and geometry, and `render_input`
    // extracts from the source volume, upstream of any derivation. (The
    // timestamp above is the one field that is read back, which is why it is
    // carried rather than left at zero.) It is still a claim the radial makes
    // about itself, and the derived grids are the one place in the tree that
    // manufactures radials rather than decoding them.
    let spacing = crate::azimuth::Rows::of(azimuths_deg, values.len()).step_deg as f32;
    let first_gate_m = (first_gate_km * 1000.0).round().clamp(0.0, 65535.0) as u16;
    let gate_m = (gate_interval_km * 1000.0).round().clamp(1.0, 65535.0) as u16;

    let radials = values
        .iter()
        .zip(azimuths_deg)
        .enumerate()
        .map(|(i, (row, &az))| {
            let bytes: Vec<u8> = row
                .iter()
                .map(|&v| {
                    if v.is_nan() {
                        0
                    } else {
                        ((v * f64::from(scale) + f64::from(offset)).round() as i64).clamp(2, 255)
                            as u8
                    }
                })
                .collect();
            let moment = MomentData::from_fixed_point(
                bytes.len() as u16,
                first_gate_m,
                gate_m,
                8,
                scale,
                offset,
                bytes,
            );
            let (mut refl, mut vel, mut sw, mut zdr, mut phi, mut rho) =
                (None, None, None, None, None, None);
            match slot {
                MomentSlot::Reflectivity => refl = Some(moment),
                MomentSlot::Velocity => vel = Some(moment),
                MomentSlot::SpectrumWidth => sw = Some(moment),
                MomentSlot::DifferentialReflectivity => zdr = Some(moment),
                MomentSlot::DifferentialPhase => phi = Some(moment),
                MomentSlot::CorrelationCoefficient => rho = Some(moment),
            }
            Radial::new(
                collected_ms,
                i as u16,
                az as f32,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                refl,
                vel,
                sw,
                zdr,
                phi,
                rho,
                None,
            )
        })
        .collect();
    Sweep::new(elevation_number, radials)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
