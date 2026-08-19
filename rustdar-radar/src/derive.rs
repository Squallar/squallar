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
//!   override where set, else the RPG's own vector for the volume, else a
//!   rung derived from the volume's own
//!   [`crate::velocity::volume_wind_profile`] — the 0–6 km mean wind unless the
//!   reader asked for the Bunkers right-mover; with none of those, the product
//!   refuses — painting base velocity under a storm-relative label is the
//!   failure the whole arrangement exists to prevent. The derived field is
//!   dealiased by construction, which is why the sampler's fold guard stays
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
//! Derivation runs inside the section/voxel jobs, so it is *asked for*
//! exactly when they run: **per sealed sweep** for a live volume (the same
//! rebuild key the native moments have — a derived product is never staler
//! than its volume), per request for a section whose line moves. The cost is
//! the whole-volume derivation on the worker: NROT is the heavy one (the full
//! stencil pipeline per velocity tilt), SRV is a dealias plus a subtraction,
//! KDP a filtered range derivative per ΦDP tilt. Nothing here runs on the
//! frame thread.
//!
//! Asked for, not necessarily recomputed (WO-E4.8): [`prepare`] memoizes the
//! derived scans in a process-local three-entry LRU keyed by [`DeriveKey`],
//! so a section and a 3D pane of one volume — or a section whose line moved —
//! share one derivation instead of running the stencil pipeline once each.
//! The memo is a cache, never an eager step: nothing derives until the first
//! consumer asks, and callers are unchanged (AF1). It serves both targets by
//! construction — native pool threads share the process, and the wasm
//! worker's own instance of this static serves its jobs; nothing crosses the
//! wire. Native invalidation is owned by the App's volume-eviction pass
//! through [`retain_volumes`]; on wasm the worker is bounded by the LRU
//! capacity plus the `sealed_sweeps` re-keying (a growing live volume keys
//! away from its own stale entries).
//!
//! # Encodings
//!
//! Each derived field is written through its own fixed-point codec (below),
//! chosen so raw codes 2..=255 span the product's display range exactly; raw
//! 0 is "no data", raw 1 is left unused (the Level II convention reserves it
//! for range folding). `voxel::data_levels_for` declares the matching ramp
//! ranges, so the voxel index ramp and this codec agree about what the
//! extremes mean.

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
    /// The product was derived; the field lives in
    /// [`derived_slot`]'s moment of these synthetic sweeps.
    ///
    /// An `Arc` because the scan may be shared with the derivation memo (see
    /// [`prepare`]): the second consumer of one volume's derivation holds the
    /// same allocation the first one computed. A native moment is never
    /// memoized — it is a borrow, not a computation.
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
/// * NROT: raw 2..=255 spans exactly −5..+5 (unitless) at 0.0395 resolution —
///   one number with the field's own `nrot::NROT_LIMIT` clamp, so no value
///   the algorithm can produce is outside what this can write.
/// * KDP: raw 2..=255 spans exactly
///   [`kdp::KDP_MIN_DISPLAY`]..[`kdp::KDP_MAX_DISPLAY`] (−2.05..10 °/km),
///   the estimator's own display clamp.
///
/// # NROT's span is the reference's own lattice, and was measured onto it
///
/// This row spanned ±4 until it was measured. That was narrower than the
/// field's ±5 clamp, and the encoder below saturates at raw 255 with nothing
/// marking a saturated bin — so a bin over 4 was silently flattened. Both the
/// incidence of that and the cost of fixing it were measured before it moved.
///
/// **Incidence.** Over all 158 volumes of the Nyquist corpus, every velocity
/// tilt, 32 201 946 finite bins: 117 reach |NROT| ≥ 3.0 in 8 volumes, **15
/// reach ≥ 4.0 in 2**, 6 reach ≥ 4.5, and **4 sit exactly on the ±5 clamp**.
/// All fifteen are KCRP 2017-08-26 — Harvey's landfall — at 00:30:35 and
/// 00:52:37. On the old span those four were written as exactly 4.0, an error
/// of 1.0 against an eighth of full scale. The nine-cut record in
/// [`crate::nrot`] cannot see any of this: nothing on those nine volumes
/// exceeds 3.07, so the count had to come from the corpus rather than from the
/// record. (A reader who remembers a KCRP **4.776** in that record is
/// remembering a bin `MEDIAN_MIN_DEALIASED_OCC` removed — the loudest
/// magnitude on the nine cuts has been 2.1445 since it landed.)
///
/// **Cost.** The step coarsens 0.0316 → 0.0395. Scored against the nine
/// decoded GR2Analyst captures on `campaign-harness`, ±4 → ±5 moves aggregate
/// precision 0.7225 → 0.7152 and recall 0.2885 → 0.3355, with mean |Δ| over a
/// fixed comparable bin set flat at 0.0775 → 0.0768; the KDDC holdout moves
/// the same way. The recall is bins the coarser lattice lifts across
/// [`nrot::SIGNIFICANT`], which is also `crate::palette`'s first visible
/// class. They are the reference's bins and not manufactured: the lifted band
/// is reference-painted at **0.642** against **0.456** in the equal-width band
/// below it, and the lift adds 626 agreeing bins against 304 spurious.
///
/// **Why ±5 exactly.** 254 codes across ±5 decode onto `(raw − 128.5)·10/253`
/// — spacing 10/253 = 0.0395257, offset half a step, zero deliberately not a
/// lattice point. That is GR2Analyst's own lattice, and not approximately so:
/// pooled over 14 780 hovered NROT readouts in the `campaign-harness` record,
/// every one falls within 0.005 — the 2-dp readout bound — of a point on it,
/// while the denominators 252 and 254 are both infeasible against those same
/// readings. So this span adopts the reference's quantisation rather than
/// authoring one, and the pin below asserts it against the reference's
/// numbers rather than against these. (The record's own prose quotes the
/// lattice as `n·0.03950 + 0.0210`; that offset is a stray fit which breaks
/// 6.6% of the readings it was drawn from, and the half-step here is what
/// survives them all.)
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
/// a function of, besides the volume's own gate values (WO-E4.8).
///
/// The gate values themselves are proxied by `(volume_start, sealed_sweeps)`:
/// a real volume is named by when its first radial was collected, and a live
/// volume that seals another sweep grows `sealed_sweeps` and keys away from
/// its own earlier entries. `radar_lat_bits`/`radar_lon_bits` pin the site,
/// because two radars' clocks are the one thing `volume_start` alone cannot
/// separate. The declared-Nyquist table is hashed whole (`declared_digest`)
/// — SRV and NROT unfold around the limits it declares, so a re-declared cut
/// is a different derivation.
///
/// # The motion component is the *resolution*, not the raw inputs
///
/// `motion_bits` carries the vector that would win [`srv::MotionInputs::resolve`]'s
/// precedence — the finite user override, else the finite RPG vector — and
/// `motion_rung` the fallback the resolution would drop to with neither.
/// That is exactly the set of facts the derived bytes depend on:
/// [`srv::apply_storm_motion`] reads only the winning vector's speed and
/// direction (the provenance label never touches a gate), and the fallback
/// rung matters only when no explicit vector arrived, in which case the
/// vector it derives is a function of the volume — which the key already
/// names. NROT and KDP read no motion at all; keying them on it costs a miss
/// when the vector moves, never a stale hit.
#[derive(Clone, PartialEq, Debug)]
struct DeriveKey {
    /// When the volume's first radial was collected, ms since the Unix epoch
    /// — the minimum positive per-sweep clock, which is the same number
    /// whether read off an original decoded volume or off a payload-
    /// reconstructed one (reconstruction stamps each sweep's own minimum
    /// back onto its radials). Never `0`: an unclocked volume has no
    /// identity and is never memoized — see [`prepare`].
    volume_start: i64,
    /// How many sweeps the scan carried when it was derived — the live
    /// volume's re-key, exactly the "per sealed sweep" cadence the module
    /// docs state.
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
    entries: Vec<(DeriveKey, Arc<Scan>)>,
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
///
/// Per-sweep minima rather than a flat walk on purpose: a job's payload
/// carries only the sweeps that hold the product's moment, and its
/// reconstruction stamps each sweep's own minimum onto every rebuilt radial —
/// so "the minimum over per-sweep minima" is the one spelling that reads the
/// same off an original volume (what [`retain_volumes`] is handed) and off a
/// reconstructed one (what [`prepare`] sees inside a job).
fn volume_start_ms(scan: &Scan) -> i64 {
    scan.sweeps()
        .iter()
        .map(|sweep| crate::render_input::sweep_collected_ms(sweep.radials()))
        .filter(|&ms| ms > 0)
        .min()
        .unwrap_or(0)
}

/// The memo key for this prepare call, or `None` for a volume the memo must
/// not touch: an unclocked one (`volume_start == 0` is the decoder's own "no
/// clock" sentinel, and a volume that cannot say when it was flown has no
/// identity to cache under — two unclocked volumes of one shape would
/// otherwise collide and serve each other's fields).
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
    // Mirrors `MotionInputs::resolve` exactly, non-finite refusals included:
    // a non-finite override falls through to the RPG vector there, so it
    // must fall through here too or the key would split what the resolution
    // joins.
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

/// Drop every memo entry whose volume is not among `live` (WO-E4.8's native
/// invalidation, called from the App's once-a-frame volume-eviction pass —
/// the same pass that frees the volumes themselves).
///
/// Matching is by per-sweep clock: an entry's `volume_start` was computed
/// over a payload's *subset* of the volume's sweeps, so it equals one of the
/// live volume's per-sweep minima rather than necessarily the volume-wide
/// one. The candidate set is therefore every live sweep's clock, which the
/// entry's start must be exactly one of.
///
/// On wasm this is a harmless no-op where the App calls it: the memo that
/// holds entries lives in the *worker's* instance of this static (jobs run
/// there), and the worker is bounded by the LRU capacity plus the
/// `sealed_sweeps` re-keying instead.
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
        .retain(|(key, _)| live_sweep_clocks.contains(&key.volume_start));
}

/// The memoized scan for `key`, marked most-recently-used.
fn memo_get(key: &DeriveKey) -> Option<Arc<Scan>> {
    let mut memo = DERIVE_MEMO.lock().expect("the derive memo mutex");
    let position = memo.entries.iter().position(|(held, _)| held == key)?;
    let entry = memo.entries.remove(position);
    let scan = Arc::clone(&entry.1);
    memo.entries.push(entry);
    Some(scan)
}

/// Insert a freshly derived scan, evicting the least-recently-used past
/// capacity. A racing duplicate insert (two threads that both missed) keeps
/// the newer allocation — the two are byte-identical, so which one survives
/// is unobservable.
fn memo_insert(key: DeriveKey, scan: Arc<Scan>) {
    let mut memo = DERIVE_MEMO.lock().expect("the derive memo mutex");
    memo.entries.retain(|(held, _)| held != &key);
    memo.entries.push((key, scan));
    while memo.entries.len() > DERIVE_MEMO_CAPACITY {
        memo.entries.remove(0);
    }
}

/// Empty the memo — the determinism gate's "fresh compute" arm. Gated as the
/// tests module is: under a wasm test build the module is compiled out, and
/// an ungated helper would be this crate's one new dead-code warning there.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn memo_clear() {
    DERIVE_MEMO
        .lock()
        .expect("the derive memo mutex")
        .entries
        .clear();
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
/// vector and `rpg_storm_motion` is the RPG's own for this volume, read only
/// by SRV — the same pair the plan-view SRV path carries. Both are threaded
/// through rather than resolved here so a section and the plan view of one
/// volume shift by the *same* vector; resolving them separately is how the two
/// panes come to disagree with no error and no visible difference.
///
/// `radar_lat`/`radar_lon` name the site, for the [`DeriveKey`] alone — the
/// derivations themselves never read them. Both callers already hold the
/// pair; this asks for it rather than leaving two radars' identically-clocked
/// volumes to collide in the memo.
///
/// # Memoized (WO-E4.8)
///
/// A derived result is served from the process-local memo when the same
/// [`DeriveKey`] was derived before, and computed **outside the memo's lock**
/// otherwise — the mutex covers only the map operations, so a pool thread
/// deriving NROT never blocks a sibling deriving KDP. Two threads that miss
/// the same key at once both compute (byte-identical by determinism, so the
/// double-compute race is acceptable) and the later insert wins. A refusal
/// (`None`) is never cached: it is already cheap, and SRV's refusal depends
/// on inputs the key deliberately reduces.
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

/// Every velocity-carrying tilt of the volume, decoded once, each paired with
/// the fold limit its cut declared — the shared walk SRV and NROT derive over.
///
/// The walk itself is [`crate::velocity::tilts`]; what this adds is the
/// declaration, looked up by `elevation_number` because that is the key
/// [`crate::nyquist::DeclaredNyquist`] is written under and the one thing
/// about a sweep that survives every hop the table takes. `None` for a cut the
/// volume did not name, which both derivations pass on to the dealiaser as
/// "estimate it".
///
/// Collected rather than streamed: both derivations fit the volume's wind
/// profile from these tilts *and then* render every one of them, so the
/// alternative is decoding the whole velocity volume twice.
fn velocity_tilts(
    volume: crate::nyquist::Volume<'_>,
) -> Vec<(crate::velocity::VelocityTilt<'_>, Option<f64>)> {
    crate::velocity::tilts(volume.scan())
        .map(|tilt| {
            let declared = volume.declared_nyquist().get(tilt.sweep.elevation_number());
            (tilt, declared)
        })
        .collect()
}

fn derive_srv(volume: crate::nyquist::Volume<'_>, motion: srv::MotionInputs) -> Option<Scan> {
    let scan = volume.scan();
    let tilts = velocity_tilts(volume);
    let profile = crate::velocity::wind_profile_of(tilts.iter().map(|(tilt, _)| tilt));
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

    let sweeps: Vec<Sweep> = tilts
        .into_iter()
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
    let tilts = velocity_tilts(volume);
    let profile = crate::velocity::wind_profile_of(tilts.iter().map(|(tilt, _)| tilt));
    let sweeps: Vec<Sweep> = tilts
        .iter()
        .map(|(tilt, declared_nyquist_ms)| {
            let grid = &tilt.grid;
            let values = nrot::compute_nrot_grid_with_profile(
                &grid.sweep(*declared_nyquist_ms),
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
            // does, which is the same test `kdp::compute_kdp` below then makes
            // for itself. Asked of the leading radial alone, the two disagreed:
            // one blank radial refused a cut the estimator was willing to
            // derive, exactly as it did for the wind fit (`crate::velocity`).
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
