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

/// Prepare `scan` for sampling under `product`: pass a native moment through,
/// derive a derived one, refuse (`None`) what cannot be derived — a product
/// with no per-tilt field, a volume without the source moment, or SRV with no
/// storm motion vector from either the user or the volume's own wind fit.
///
/// `storm_motion_override` is the user's `(speed_kt, direction_from_deg)`
/// vector, read only by SRV — the same pair the plan-view SRV path carries.
pub fn prepare<'s>(
    scan: &'s Scan,
    product: RadarProduct,
    storm_motion_override: Option<(f32, f32)>,
) -> Option<Prepared<'s>> {
    if crate::sampler::samplable(product).is_some() {
        return Some(Prepared::Native(scan));
    }
    let derived = match product {
        RadarProduct::StormRelativeVelocity => derive_srv(scan, storm_motion_override)?,
        RadarProduct::NormalizedRotation => derive_nrot(scan)?,
        RadarProduct::SpecificDifferentialPhase => derive_kdp(scan)?,
        _ => return None,
    };
    Some(Prepared::Derived(Box::new(derived)))
}

/// Every velocity-carrying sweep of `scan` with its decoded grid, in scan
/// order — the shared walk SRV and NROT derive over.
fn velocity_sweeps(scan: &Scan) -> Vec<(&Sweep, f64, srv::VelocityGrid)> {
    scan.sweeps()
        .iter()
        .filter_map(|sweep| {
            let radials = sweep.radials();
            let first = radials.first()?;
            if first.velocity().is_none() || radials.len() < 3 {
                return None;
            }
            let grid = srv::velocity_grid(radials)?;
            Some((sweep, f64::from(first.elevation_angle_degrees()), grid))
        })
        .collect()
}

fn derive_srv(scan: &Scan, storm_motion_override: Option<(f32, f32)>) -> Option<Scan> {
    let profile = srv::volume_wind_profile(scan);
    let user = storm_motion_override.and_then(|(speed_kt, direction_deg)| {
        srv::SrvMotion::user_override(speed_kt, direction_deg)
    });
    // No vector, no SRV: base velocity under a storm-relative label is the
    // failure this refusal exists to prevent.
    let motion = srv::storm_motion(profile.as_ref(), user)?;

    let sweeps: Vec<Sweep> = velocity_sweeps(scan)
        .into_iter()
        .filter_map(|(sweep, elevation_deg, _)| {
            let grid =
                srv::compute_srv_grid(sweep.radials(), elevation_deg, profile.as_ref(), &motion)?;
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

fn derive_nrot(scan: &Scan) -> Option<Scan> {
    let profile = srv::volume_wind_profile(scan);
    let sweeps: Vec<Sweep> = velocity_sweeps(scan)
        .into_iter()
        .map(|(sweep, elevation_deg, grid)| {
            let values = nrot::compute_nrot_grid_with_profile(
                &nrot::VelocitySweep {
                    vel_grid: &grid.values,
                    azimuths_deg: &grid.azimuths_deg,
                    gate_count: grid.gate_count,
                    first_gate_range_km: grid.first_gate_range_km,
                    gate_interval_km: grid.gate_interval_km,
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
            let derived = kdp::compute_kdp(radials, &params)?;
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
/// nothing derived.
fn non_empty_scan(source: &Scan, sweeps: Vec<Sweep>) -> Option<Scan> {
    if sweeps.is_empty() {
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
    let spacing = 360.0 / values.len().max(1) as f32;
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
                0,
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
mod tests {
    use super::*;
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, PulseWidth, VolumeCoveragePattern, WaveformType,
    };

    const FIRST_GATE_M: u16 = 2125;
    const GATE_M: u16 = 250;

    fn cut(angle_deg: f64) -> ElevationCut {
        ElevationCut::new(
            angle_deg,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    }

    fn vcp(cut_angles: &[f64]) -> VolumeCoveragePattern {
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
            cut_angles.iter().copied().map(cut).collect(),
        )
    }

    fn gate_slant_km(j: usize) -> f64 {
        f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0
    }

    /// Per-gate fields, m/s / dBZ / degrees / unitless, `None` = no data.
    type Fields<'f> = &'f dyn Fn(f64, f64) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>);

    /// One sweep carrying reflectivity, velocity, ΦDP and ρHV through their
    /// real codecs, over `fields(az, slant_km) -> (refl, vel, phi, rho)`.
    fn sweep_with(
        elevation_number: u8,
        elevation_deg: f32,
        n_radials: usize,
        n_gates: usize,
        fields: Fields<'_>,
    ) -> Sweep {
        let encode = |v: Option<f64>, scale: f64, offset: f64| -> u8 {
            match v {
                None => 0,
                Some(v) => ((v * scale + offset).round() as i64).clamp(2, 255) as u8,
            }
        };
        let spacing = 360.0 / n_radials as f32;
        let radials = (0..n_radials)
            .map(|i| {
                let az = i as f64 * f64::from(spacing);
                let mut refl = Vec::with_capacity(n_gates);
                let mut vel = Vec::with_capacity(n_gates);
                let mut phi = Vec::with_capacity(n_gates);
                let mut rho = Vec::with_capacity(n_gates);
                for j in 0..n_gates {
                    let (r, v, p, c) = fields(az, gate_slant_km(j));
                    refl.push(encode(r, 2.0, 66.0));
                    vel.push(encode(v, 2.0, 129.0));
                    phi.push(encode(p, 2.8361, 2.0));
                    rho.push(encode(c, 300.0, -60.5));
                }
                let moment = |bytes: Vec<u8>, scale: f32, offset: f32| {
                    Some(MomentData::from_fixed_point(
                        bytes.len() as u16,
                        FIRST_GATE_M,
                        GATE_M,
                        8,
                        scale,
                        offset,
                        bytes,
                    ))
                };
                Radial::new(
                    0,
                    i as u16,
                    az as f32,
                    spacing,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    moment(refl, 2.0, 66.0),
                    moment(vel, 2.0, 129.0),
                    None,
                    None,
                    moment(phi, 2.8361, 2.0),
                    moment(rho, 300.0, -60.5),
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    }

    /// Two tilts over `fields`, under a matching two-cut pattern.
    fn scan_with(fields: Fields<'_>) -> Scan {
        Scan::new(
            vcp(&[0.5, 1.5]),
            vec![
                sweep_with(1, 0.53, 360, 240, fields),
                sweep_with(2, 1.47, 360, 240, fields),
            ],
        )
    }

    /// Decode one gate of the given slot from a scan's sweep, m/s (or the
    /// slot's units), `None` for no data.
    fn decoded(
        scan: &Scan,
        sweep: usize,
        radial: usize,
        gate: usize,
        slot: MomentSlot,
    ) -> Option<f64> {
        use nexrad_model::data::MomentValue;
        let radial = &scan.sweeps()[sweep].radials()[radial];
        let moment = slot.read(radial)?;
        match moment.iter().nth(gate)? {
            MomentValue::Value(v) => Some(f64::from(v)),
            _ => None,
        }
    }

    /// The volume-slot table: natives sample, the three derivations sample
    /// through their source slot, everything else refuses.
    #[test]
    fn the_volume_slot_admits_the_natives_and_the_three_derivations() {
        use RadarProduct::*;
        let mut admitted = Vec::new();
        let mut refused = Vec::new();
        for product in RadarProduct::all() {
            match volume_slot(*product) {
                Some(slot) => admitted.push((*product, slot)),
                None => refused.push(*product),
            }
        }
        assert_eq!(
            admitted,
            vec![
                (Reflectivity, MomentSlot::Reflectivity),
                (Velocity, MomentSlot::Velocity),
                (SpectrumWidth, MomentSlot::SpectrumWidth),
                (DifferentialPhase, MomentSlot::DifferentialPhase),
                (CorrelationCoefficient, MomentSlot::CorrelationCoefficient),
                (
                    DifferentialReflectivity,
                    MomentSlot::DifferentialReflectivity
                ),
                (StormRelativeVelocity, MomentSlot::Velocity),
                (SpecificDifferentialPhase, MomentSlot::DifferentialPhase),
                (NormalizedRotation, MomentSlot::Velocity),
            ],
            "the vertical views' product set changed",
        );
        assert_eq!(
            refused,
            vec![
                EchoTops,
                EchoTopsInterpolated,
                VerticallyIntegratedLiquid,
                VilDensity,
                ProbabilityOfSevereHail,
                MaxExpectedHailSize,
                HydrometeorClassification,
                PrecipitationRate,
            ],
            "a product with no per-tilt field must stay refused",
        );
        // And none of the derived three may pass the raw-scan gate: that is
        // the door `for_derived` exists to keep shut.
        for product in [
            StormRelativeVelocity,
            SpecificDifferentialPhase,
            NormalizedRotation,
        ] {
            assert!(crate::sampler::samplable(product).is_none());
        }
    }

    /// SRV honours the user's storm motion vector, radial by radial: the
    /// derived field minus the raw field is exactly the subtracted motion
    /// component, which is `apply_storm_motion`'s contract carried through
    /// the whole prepare → synthesize → decode round trip.
    ///
    /// The fixture wind is a smooth 25 m/s southerly flow (no folds, so the
    /// dealias pass is the identity) and the override is deliberately not the
    /// flow — a fixture whose override matched its wind would hide a sign
    /// error in the correction.
    #[test]
    fn srv_subtracts_the_override_motion_radial_by_radial() {
        // Radial velocity of a uniform wind FROM 180° at 25 m/s.
        let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
        let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));

        let speed_kt: f32 = 20.0;
        let speed_ms = f64::from(speed_kt) * 0.514444;
        let direction: f32 = 240.0;
        let prepared = prepare(
            &scan,
            RadarProduct::StormRelativeVelocity,
            Some((speed_kt, direction)),
        )
        .expect("a velocity volume with an override derives");
        let Prepared::Derived(derived) = prepared else {
            panic!("SRV must be derived, never served from the raw scan");
        };

        let mut checked = 0;
        for radial in (0..360usize).step_by(45) {
            let az = radial as f64;
            let raw = decoded(&scan, 0, radial, 40, MomentSlot::Velocity).unwrap();
            let srv = decoded(&derived, 0, radial, 40, MomentSlot::Velocity)
                .expect("the derived field covers the raw field");
            let component = speed_ms * (f64::from(direction) - az).to_radians().cos();
            assert!(
                (srv - (raw + component)).abs() < 0.6,
                "az {az}: srv {srv} != raw {raw} + component {component:.2} \
                 (within one codec step + dealias identity)",
            );
            if component.abs() > 1.0 {
                checked += 1;
                assert!(
                    (srv - raw).abs() > 0.5,
                    "az {az}: the derived field equals the raw field — the \
                     motion was never subtracted",
                );
            }
        }
        assert!(
            checked >= 4,
            "precondition: the sweep sampled moving azimuths"
        );
    }

    /// SRV with neither an override nor a usable wind fit refuses: base
    /// velocity under a storm-relative label is the failure the refusal
    /// exists to prevent.
    #[test]
    fn srv_with_no_motion_vector_refuses() {
        // No velocity anywhere: no wind fit, and (no override) no vector.
        let scan = scan_with(&|_, _| (Some(40.0), None, None, Some(0.99)));
        assert!(prepare(&scan, RadarProduct::StormRelativeVelocity, None).is_none());
    }

    /// NROT is the rotation pipeline's output, not relabelled velocity: on a
    /// uniform 15 m/s field — every gate moving, zero shear — the derived
    /// field must read no-data or near-zero everywhere, never 15.
    #[test]
    fn nrot_is_rotation_not_relabelled_velocity() {
        let scan = scan_with(&|_, _| (Some(40.0), Some(15.0), None, Some(0.99)));
        let prepared = prepare(&scan, RadarProduct::NormalizedRotation, None)
            .expect("a velocity volume derives");
        let Prepared::Derived(derived) = prepared else {
            panic!("NROT must be derived, never served from the raw scan");
        };
        let mut seen = 0;
        for radial in (0..360).step_by(20) {
            for gate in (20..200).step_by(30) {
                if let Some(v) = decoded(&derived, 0, radial, gate, MomentSlot::Velocity) {
                    seen += 1;
                    assert!(
                        v.abs() < 1.0,
                        "({radial},{gate}): a shear-free field read NROT {v} — \
                         the raw velocity leaked through",
                    );
                }
            }
        }
        // `seen` may honestly be 0: the pipeline censors a zero-information
        // field to no-data, which is also not-velocity. The assertion above
        // is the pin; this is just the record of which way it went.
        let _ = seen;
    }

    /// A cross-section of a derived product slices the derived field — the
    /// integrated pin over `xsect::render_section`'s derivation seam. An SRV
    /// section and a BV section of the same volume must differ wherever the
    /// motion component is non-zero; a seam that forgot to derive (sampled
    /// the raw scan under the SRV label) makes them equal, which is the
    /// "looks right, different field" failure the sampler's refusal exists
    /// to prevent.
    #[test]
    fn a_derived_section_slices_the_derived_field() {
        let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
        let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));
        // KTLX; a west–east line through the site so azimuths span the flow.
        let site = (35.33306, -97.2775);
        let req = |product| crate::xsect::SectionRequest {
            start: (site.0, site.1 - 0.4),
            end: (site.0, site.1 + 0.4),
            top_km_msl: None,
            product,
        };
        let bv =
            crate::xsect::render_section(&scan, &req(RadarProduct::Velocity), site.0, site.1, None)
                .expect("the velocity section renders");
        let srv = crate::xsect::render_section(
            &scan,
            &req(RadarProduct::StormRelativeVelocity),
            site.0,
            site.1,
            Some((20.0, 240.0)),
        )
        .expect("the SRV section renders");

        let differing = bv
            .values()
            .iter()
            .zip(srv.values())
            .filter(|(b, s)| b.is_finite() && s.is_finite() && (**b - **s).abs() > 1.0)
            .count();
        assert!(
            differing > 100,
            "only {differing} samples differ between the BV and SRV sections \
             — the section sampled the raw field under the SRV label",
        );
    }

    /// A voxel grid of a derived product resamples the derived field — the
    /// same pin as the section one, over `voxel::build_voxels_with_motion`'s
    /// seam — and the derived grids carry their own value ranges, not their
    /// source slot's.
    #[test]
    fn a_derived_voxel_grid_resamples_the_derived_field() {
        use crate::voxel::{VoxelRequest, VoxelShape, build_voxels_with_motion};
        let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
        let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));
        let site = (35.33306, -97.2775);
        let req = |product| VoxelRequest {
            centre: site,
            half_width_km: 30.0,
            base_km_msl: 0.0,
            top_km_msl: 4.0,
            product,
            shape: VoxelShape {
                nx: 32,
                ny: 32,
                nz: 8,
            },
            values_wanted: true,
        };
        let bv =
            build_voxels_with_motion(&scan, &req(RadarProduct::Velocity), site.0, site.1, None)
                .expect("the velocity grid builds");
        let srv = build_voxels_with_motion(
            &scan,
            &req(RadarProduct::StormRelativeVelocity),
            site.0,
            site.1,
            Some((20.0, 240.0)),
        )
        .expect("the SRV grid builds");

        let mut differing = 0;
        for z in 0..8 {
            for y in 0..32 {
                for x in 0..32 {
                    let (Some(b), Some(s)) = (bv.value_at(x, y, z), srv.value_at(x, y, z)) else {
                        continue;
                    };
                    if b.is_finite() && s.is_finite() && (b - s).abs() > 1.0 {
                        differing += 1;
                    }
                }
            }
        }
        assert!(
            differing > 100,
            "only {differing} cells differ between the BV and SRV grids — the \
             grid resampled the raw field under the SRV label",
        );

        // The derived ranges: SRV shares velocity's, NROT and KDP carry
        // their own — pinned here as the literals `derive`'s codecs match.
        let nrot = build_voxels_with_motion(
            &scan,
            &req(RadarProduct::NormalizedRotation),
            site.0,
            site.1,
            None,
        )
        .expect("the NROT grid builds");
        assert_eq!(srv.value_range().1, 63.5, "SRV rides velocity's ramp");
        assert_eq!(
            nrot.value_range().1,
            4.0,
            "NROT carries its own ±4 unitless ramp",
        );
        assert!(
            (f64::from(nrot.value_range().0) - (-4.0 - 8.0 / 254.0)).abs() < 1e-3,
            "NROT's index 0 sits one step under −4",
        );

        // And a derived grid survives its own wire form: the far end
        // re-derives the range and the table from the product and must agree.
        let restored = crate::voxel::VoxelGrid::from_bytes(&srv.to_bytes())
            .expect("a derived grid round-trips the wire");
        assert_eq!(restored.value_range(), srv.value_range());
        assert_eq!(restored.lut(), srv.lut());
    }

    /// KDP is the phase derivative, not relabelled ΦDP: a linear 1 °/km ramp
    /// must derive to ~0.5 °/km (KDP = dΦ/2dr), never to the ~30–70° the raw
    /// phase reads.
    #[test]
    fn kdp_is_the_phase_derivative_not_relabelled_phase() {
        let scan = scan_with(&|_, slant| (Some(45.0), None, Some(10.0 + 1.0 * slant), Some(0.99)));
        let prepared = prepare(&scan, RadarProduct::SpecificDifferentialPhase, None)
            .expect("a ΦDP volume derives");
        let Prepared::Derived(derived) = prepared else {
            panic!("KDP must be derived, never served from the raw scan");
        };
        let mut values = Vec::new();
        let radial_count = derived.sweeps()[0].radials().len();
        for radial in (0..radial_count).step_by(radial_count / 12) {
            for gate in [60, 100, 140] {
                if let Some(v) = decoded(&derived, 0, radial, gate, MomentSlot::DifferentialPhase) {
                    values.push(v);
                }
            }
        }
        assert!(
            !values.is_empty(),
            "precondition: the estimator produced values on a clean ramp",
        );
        for &v in &values {
            assert!(
                (0.1..1.2).contains(&v),
                "a 1 °/km ΦDP ramp must derive to ≈0.5 °/km, not {v} — raw \
                 phase would read tens of degrees",
            );
        }
    }
}
