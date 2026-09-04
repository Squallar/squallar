//! Derived-product volumes: SRV, NROT and Level II KDP as sampleable scans.
//!
//! [`crate::sampler::samplable`] refuses the derived products so a raw volume
//! can never be sampled under a derived label. This module is the other half:
//! it **computes** the derived field per sweep, off the frame thread, and
//! writes it back as a synthetic scan whose radials carry the derived values
//! in the product's source moment slot.
//!
//! # The three derivations
//!
//! * **Storm-relative velocity** — per velocity sweep:
//!   [`crate::srv::compute_srv_grid`] (dealias against the volume wind fit,
//!   then subtract the storm motion). The motion vector is the user's
//!   override where set, else the RPG's own vector for the volume, else a
//!   rung derived from [`crate::velocity::volume_wind_profile`]; with none of
//!   those the product refuses, because painting base velocity under a
//!   storm-relative label is the failure the arrangement exists to prevent.
//!   The derived field is dealiased by construction, which is why the
//!   sampler's fold guard stays **unarmed** for SRV.
//! * **Normalized rotation** — per velocity sweep:
//!   [`crate::nrot::compute_nrot_grid_with_profile`] (dealias, median,
//!   split-tap stencil, despeckle), wind-profile guided where the fit
//!   succeeds and unguided otherwise.
//! * **Specific differential phase** — per ΦDP sweep:
//!   [`crate::kdp::compute_kdp`] over ΦDP with the Z and ρHV gates, at its
//!   recombined 1° × 0.25 km geometry. The 2D map's KDP stays the Level III
//!   product; this exists for the vertical views.
//!
//! # Who derives, and what is shared
//!
//! **Two consumers come through here and share one memo.** The section job
//! ([`crate::xsect`]) and the 3D volume job ([`crate::voxel`]) both call
//! [`prepare`], which memoizes the derived scans in a process-local LRU of
//! [`DERIVE_MEMO_CAPACITY`] entries keyed by [`DeriveKey`] — volume start
//! clock, sealed sweep count, radar position, product, the resolved motion
//! bits and fallback rung, and a digest of the declared Nyquist. Native
//! invalidation is owned by the App's volume-eviction pass through
//! [`retain_volumes`]; on wasm the worker is bounded by the LRU capacity plus
//! the `sealed_sweeps` re-keying.
//!
//! **It is a cache, never eager.** Nothing is derived until a consumer asks
//! for it; the memo only stops the *second* ask for the same identity paying
//! again. A derivation for a volume nobody samples is never run.
//!
//! **The 2D plan-view render does NOT come through here**, and this doc used
//! to be silent about that. `render_nrot_to_image`, the SRV image path and the
//! Level II KDP image path in [`crate::render`] call
//! [`crate::nrot::compute_nrot_grid_with_profile`],
//! [`crate::srv::compute_srv_grid`] and [`crate::kdp::compute_kdp`] directly,
//! per sweep, and share none of this memo. They want an RGBA image, not a
//! sampleable synthetic scan, so they never build one — which is why they are
//! outside [`prepare`] rather than a duplication of it. NROT is the heavy one
//! on both paths.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex};

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
    /// The product was derived; the field lives in [`derived_slot`]'s moment
    /// of these synthetic sweeps. An `Arc` because the scan may be shared with
    /// the derivation memo (see [`prepare`]).
    Derived(Arc<Scan>),
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
pub fn volume_slot(product: RadarProduct) -> Option<MomentSlot> {
    crate::sampler::samplable(product).or_else(|| derived_slot(product))
}

/// The fixed-point codec `(scale, offset)` a derived product's synthetic
/// moment is written through: `value = (raw − offset) / scale`.
///
/// * SRV reuses velocity's own `(2, 129)` — raw 2..=255 spans −63.5..+63.0 m/s.
/// * NROT: raw 2..=255 spans exactly −5..+5 (unitless) at 0.0395 resolution,
///   matching the field's own `nrot::NROT_LIMIT` clamp.
/// * KDP: raw 2..=255 spans exactly
///   [`kdp::KDP_MIN_DISPLAY`]..[`kdp::KDP_MAX_DISPLAY`] (−2.05..10 °/km).
///
/// **NROT's ±5 is the reference's own lattice.** 254 codes across ±5 decode
/// onto `(raw − 128.5)·10/253` — spacing 10/253 = 0.0395257, offset half a
/// step, zero deliberately not a lattice point. Pooled over 14 780 hovered
/// NROT readouts every one falls within 0.005 (the 2-dp readout bound) of a
/// point on it, while the denominators 252 and 254 are both infeasible.
///
/// The span was ±4 until measured: over 158 volumes and 32 201 946 finite
/// bins, 15 reach |NROT| ≥ 4.0 and 4 sit exactly on the ±5 clamp, all at KCRP
/// 2017-08-26, and the old span wrote those four as exactly 4.0.
fn codec(product: RadarProduct) -> (f32, f32) {
    match product {
        RadarProduct::StormRelativeVelocity => (2.0, 129.0),
        RadarProduct::NormalizedRotation => (253.0 / 10.0, 2.0 + 5.0 * (253.0 / 10.0)),
        RadarProduct::SpecificDifferentialPhase => {
            let scale = 253.0 / (kdp::KDP_MAX_DISPLAY - kdp::KDP_MIN_DISPLAY);
            (scale, 2.0 - kdp::KDP_MIN_DISPLAY * scale)
        }
        _ => unreachable!("codec is only defined for the derived products"),
    }
}

/// The identity of one derivation — everything [`prepare`]'s output bytes are
/// a function of, besides the volume's own gate values.
#[derive(Clone, PartialEq, Debug)]
struct DeriveKey {
    /// When the volume's first radial was collected, ms since the Unix epoch —
    /// the minimum positive per-sweep clock, which reads the same off an
    /// original decoded volume and off a payload-reconstructed one. Never `0`:
    /// an unclocked volume has no identity and is never memoized.
    volume_start: i64,
    /// How many sweeps the scan carried when it was derived — the live
    /// volume's re-key.
    sealed_sweeps: usize,
    radar_lat_bits: u64,
    radar_lon_bits: u64,
    product: RadarProduct,
    /// `(speed_kt, direction_from_deg)` bits of the vector that outranks the
    /// wind fit, or `None` when the derivation would fall to the fitted rung.
    motion_bits: Option<(u32, u32)>,
    /// Which fitted rung it would fall to ([`srv::SrvFallback`] discriminant).
    motion_rung: u8,
    /// FNV-1a over [`crate::nyquist::DeclaredNyquist::to_bytes`].
    declared_digest: u64,
}

/// The derivation memo: most-recently-used last, at most
/// [`DERIVE_MEMO_CAPACITY`] entries.
struct DeriveMemo {
    /// Most-recently-used last. The `usize` is the entry's price by
    /// [`crate::scan_size::scan_bytes`], carried with it so an eviction
    /// subtracts a figure instead of re-walking a volume it is about to drop.
    entries: Vec<(DeriveKey, Arc<Scan>, usize)>,
}

/// **Capacity 3 is deliberate**: the live volume's section + 3D pair plus one
/// loop volume. Entries are whole synthetic scans, tens of MB apiece — raise
/// this only with a measured reason recorded in the raising commit.
const DERIVE_MEMO_CAPACITY: usize = 3;

static DERIVE_MEMO: LazyLock<Mutex<DeriveMemo>> = LazyLock::new(|| {
    Mutex::new(DeriveMemo {
        entries: Vec::new(),
    })
});

/// **What the memo is holding, in host bytes**, mirrored out of the lock.
///
/// An atomic beside the mutex rather than a sum under it, because the reader
/// is the frame thread's telemetry tick and the writer is the derive path:
/// a lock-free read cannot stall a frame behind a derivation, and cannot
/// deadlock a caller that is already inside the memo. Written only where
/// `entries` changes, always while the lock is held, so the two never
/// disagree by more than the instant between the store and the unlock.
static DERIVE_MEMO_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Bytes the derivation memo is holding, by [`crate::scan_size::scan_bytes`]
/// — a floor, and an upper bound on what emptying it would free: a derived
/// volume the still inventory or a loop cache also names is counted by each
/// of them. At most [`DERIVE_MEMO_CAPACITY`] whole synthetic scans.
pub fn memo_bytes() -> usize {
    DERIVE_MEMO_BYTES.load(Relaxed)
}

/// Re-total the memo and publish it. Called with the lock held, from every
/// place `entries` changes — one pass over at most three prices.
fn republish_memo_bytes(memo: &DeriveMemo) {
    DERIVE_MEMO_BYTES.store(
        memo.entries
            .iter()
            .fold(0usize, |sum, (_, _, bytes)| sum.saturating_add(*bytes)),
        Relaxed,
    );
}

/// FNV-1a, 64-bit — the digest [`DeriveKey::declared_digest`] carries.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// When the volume's first radial was collected: the minimum positive
/// per-sweep clock, or `0` when no radial anywhere carries one.
fn volume_start_ms(scan: &Scan) -> i64 {
    scan.sweeps()
        .iter()
        .map(|sweep| crate::render_input::sweep_collected_ms(sweep.radials()))
        .filter(|&ms| ms > 0)
        .min()
        .unwrap_or(0)
}

/// The memo key for this prepare call, or `None` for a volume the memo must
/// not touch: an unclocked one, since two unclocked volumes of one shape would
/// otherwise collide and serve each other's fields.
fn derive_key(
    volume: &crate::nyquist::Volume<'_>,
    product: RadarProduct,
    motion: srv::MotionInputs,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<DeriveKey> {
    let volume_start = volume_start_ms(volume.scan());
    if volume_start == 0 {
        return None;
    }
    // Mirrors `MotionInputs::resolve` exactly, non-finite refusals included,
    // or the key would split what the resolution joins.
    let finite = |&(speed, direction): &(f32, f32)| speed.is_finite() && direction.is_finite();
    let motion_bits = motion
        .user_override
        .filter(finite)
        .or(motion.rpg.filter(finite))
        .map(|(speed, direction)| (speed.to_bits(), direction.to_bits()));
    Some(DeriveKey {
        volume_start,
        sealed_sweeps: volume.scan().sweeps().len(),
        radar_lat_bits: radar_lat.to_bits(),
        radar_lon_bits: radar_lon.to_bits(),
        product,
        motion_bits,
        motion_rung: motion.fallback as u8,
        declared_digest: fnv1a(&volume.declared_nyquist().to_bytes()),
    })
}

/// Drop every memo entry whose volume is not among `live`, called from the
/// App's once-a-frame volume-eviction pass.
pub fn retain_volumes<'a>(live: impl IntoIterator<Item = &'a Scan>) {
    let mut memo = DERIVE_MEMO.lock().expect("the derive memo mutex");
    if memo.entries.is_empty() {
        return;
    }
    let live_sweep_clocks: Vec<i64> = live
        .into_iter()
        .flat_map(|scan| {
            scan.sweeps()
                .iter()
                .map(|sweep| crate::render_input::sweep_collected_ms(sweep.radials()))
        })
        .filter(|&ms| ms > 0)
        .collect();
    memo.entries
        .retain(|(key, _, _)| live_sweep_clocks.contains(&key.volume_start));
    republish_memo_bytes(&memo);
}

/// The memoized scan for `key`, marked most-recently-used.
fn memo_get(key: &DeriveKey) -> Option<Arc<Scan>> {
    let mut memo = DERIVE_MEMO.lock().expect("the derive memo mutex");
    let position = memo.entries.iter().position(|(held, _, _)| held == key)?;
    let entry = memo.entries.remove(position);
    let scan = Arc::clone(&entry.1);
    memo.entries.push(entry);
    Some(scan)
}

/// Insert a freshly derived scan, evicting the least-recently-used past
/// capacity. A racing duplicate insert keeps the newer allocation — the two
/// are byte-identical.
fn memo_insert(key: DeriveKey, scan: Arc<Scan>) {
    let bytes = crate::scan_size::scan_bytes(&scan);
    let mut memo = DERIVE_MEMO.lock().expect("the derive memo mutex");
    memo.entries.retain(|(held, _, _)| held != &key);
    memo.entries.push((key, scan, bytes));
    while memo.entries.len() > DERIVE_MEMO_CAPACITY {
        memo.entries.remove(0);
    }
    republish_memo_bytes(&memo);
}

/// Empty the memo — the determinism gate's "fresh compute" arm. Gated as the
/// tests module is: under a wasm test build an ungated helper would be this
/// crate's one new dead-code warning.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn memo_clear() {
    DERIVE_MEMO
        .lock()
        .expect("the derive memo mutex")
        .entries
        .clear();
    DERIVE_MEMO_BYTES.store(0, Relaxed);
}

/// Prepare a volume for sampling under `product`: pass a native moment through,
/// derive a derived one, refuse (`None`) what cannot be derived — a product
/// with no per-tilt field, a volume without the source moment, or SRV with no
/// storm motion vector from either the user or the volume's own wind fit.
pub fn prepare<'s>(
    volume: crate::nyquist::Volume<'s>,
    product: RadarProduct,
    motion: crate::srv::MotionInputs,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<Prepared<'s>> {
    if crate::sampler::samplable(product).is_some() {
        return Some(Prepared::Native(volume.scan()));
    }
    derived_slot(product)?;
    let key = derive_key(&volume, product, motion, radar_lat, radar_lon);
    if let Some(key) = &key
        && let Some(hit) = memo_get(key)
    {
        return Some(Prepared::Derived(hit));
    }
    let derived = match product {
        RadarProduct::StormRelativeVelocity => derive_srv(volume, motion)?,
        RadarProduct::NormalizedRotation => derive_nrot(volume)?,
        RadarProduct::SpecificDifferentialPhase => derive_kdp(volume.scan())?,
        _ => return None,
    };
    let derived = Arc::new(derived);
    if let Some(key) = key {
        memo_insert(key, Arc::clone(&derived));
    }
    Some(Prepared::Derived(derived))
}

/// Every velocity-carrying tilt of the volume, decoded as it is reached, each
/// paired with the fold limit its cut declared — the shared walk SRV and NROT
/// derive over.
///
/// A stream, not a collection. A tilt decodes to an `f64` grid — a super-res
/// cut is 720 × 1192 × 8 B, 6.55 MiB, plus a status byte per gate — and this
/// walk used to collect every tilt of the volume before the first was used,
/// so a derivation held the volume's grids for its whole length on top of the
/// one tilt's dealias transients. The wind fit the walk seeds from needs every
/// tilt twice, and takes them from [`crate::velocity::volume_wind_profile`],
/// which decodes each tilt as it offers it; the walk here decodes each a third
/// time. Three decodes of a tilt for one grid resident is the trade, and
/// `tests/derive_tilt_residency.rs` is the pin on the residency.
fn velocity_tilts<'v>(
    volume: crate::nyquist::Volume<'v>,
) -> impl Iterator<Item = (crate::velocity::VelocityTilt<'v>, Option<f64>)> {
    let declared = volume.declared_nyquist();
    crate::velocity::tilts(volume.scan()).map(move |tilt| {
        let declared = declared.get(tilt.sweep.elevation_number());
        (tilt, declared)
    })
}

fn derive_srv(volume: crate::nyquist::Volume<'_>, motion: srv::MotionInputs) -> Option<Scan> {
    let scan = volume.scan();
    let profile = crate::velocity::volume_wind_profile(scan);
    // No vector, no SRV: base velocity under a storm-relative label is the
    // failure this refusal exists to prevent.
    let Some(motion) = motion.resolve(profile.as_ref()) else {
        log::warn!(
            "SRV derivation refused: no storm motion vector — no user override, \
             no RPG vector for this volume, and no wind fit from the volume's \
             own winds"
        );
        return None;
    };

    let sweeps: Vec<Sweep> = velocity_tilts(volume)
        .map(|(tilt, declared_nyquist_ms)| {
            let grid = srv::storm_relative_grid(
                tilt.grid,
                tilt.elevation_deg,
                profile.as_ref(),
                &motion,
                declared_nyquist_ms,
            );
            synth_sweep(
                tilt.sweep,
                &grid.values,
                &grid.azimuths_deg,
                grid.first_gate_range_km,
                grid.gate_interval_km,
                RadarProduct::StormRelativeVelocity,
            )
        })
        .collect();
    non_empty_scan(scan, sweeps)
}

fn derive_nrot(volume: crate::nyquist::Volume<'_>) -> Option<Scan> {
    let scan = volume.scan();
    let profile = crate::velocity::volume_wind_profile(scan);
    let sweeps: Vec<Sweep> = velocity_tilts(volume)
        .map(|(tilt, declared_nyquist_ms)| {
            let grid = &tilt.grid;
            let values = nrot::compute_nrot_grid_with_profile(
                &grid.sweep(declared_nyquist_ms),
                tilt.elevation_deg,
                profile.as_ref(),
            );
            synth_sweep(
                tilt.sweep,
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
            // Every radial, not the first — a sweep carries ΦDP when any of it
            // does, which is the same test `kdp::compute_kdp` below makes. Asked
            // of the leading radial alone, one blank radial refused a cut the
            // estimator was willing to derive.
            if !radials.iter().any(|r| r.differential_phase().is_some()) {
                return None;
            }
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

/// The derived scan, under the source volume's own coverage pattern, or `None`
/// when nothing derived. Logged, so a `None` swallowed by a `?` upstream still
/// leaves its reason somewhere.
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
    // The source sweep's clock, carried onto every synthetic radial: when a
    // tilt was flown is a property of that tilt rather than of this
    // computation, and a rung age is read off these radials.
    let collected_ms = crate::render_input::sweep_collected_ms(source.radials());

    // How much sky one row of *this* grid stands for, which on a sector is not
    // `360 / rows`: a 36° NROT sector of 72 rows would declare 5°, ten times
    // the arc each row was computed over. [`crate::azimuth::Rows`] is the one
    // place that is decided. A complete rotation declares exactly what it
    // always has, and that division agrees to the bit in f32 and f64 for every
    // row count up to 100 000.
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
