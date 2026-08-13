//! Storm-relative velocity derived locally from the Level II velocity volume.
//!
//! The Level III pipeline this replaces ([`crate::srm`]) fetched five bucket
//! objects per site — `N0S` for the storm motion vector in its PDB and
//! `N0G`/`N1G`/`N2U`/`N3U` as the four dealiased tilts — because Level II
//! velocity is aliased and, at the time, nothing local could unfold it.
//! The local dealiaser ([`crate::nrot::dealias`], a validity-marking
//! multi-pass calibrated against a reference implementation) removed that
//! constraint, so the whole product can now be computed from the volume
//! already in hand:
//!
//! ```text
//! SRV(az, r) = V_dealiased(az, r) + speed · cos(direction − az)      [m/s]
//! ```
//!
//! per gate, with the correction applied at the radial's **centre** azimuth —
//! the angle the renderer centres the strip on — and `direction` the
//! meteorological "from" direction, which is why the term adds rather than
//! subtracts. Both conventions are ported verbatim from [`crate::srm`], whose
//! sign was settled by measurement over a million live gates (branch
//! `campaign-harness`), and are pinned here offline against `srm::derive`
//! itself.
//!
//! # The dealiaser profile
//!
//! NROT censors aggressively — it differentiates the field, so a residual
//! fold wall reads as clamp-level fake shear. A displayed velocity field
//! wants the opposite posture: a censored gate is a hole in the couplet the
//! product exists to show. [`crate::nrot::DealiasProfile::Coverage`] keeps
//! every unreached data gate at raw (region size gate dropped to 1) and
//! keeps the fold-wall censor at NROT's measured 1.24·Vny. SRV also skips
//! NROT's median filter entirely: the RPG's own dealiased products are not
//! median-filtered, and the filter's ND rules cost coverage.
//!
//! Both knob choices were A/B'd live against the RPG's own dealiased
//! velocity — products 154/99, the `N0G`/`N1G`/`N2U`/`N3U` twins of the
//! same Level II volume and cut — on five climatologically spread decision
//! sites with the rest of the roster as holdout (the record lives on
//! branch `campaign-harness`):
//!
//! * kept-raw floor 16 → 1: large coverage gains everywhere (the floor-16
//!   posture would *fail* the coverage bar at some sites' upper tilts) for
//!   a negligible within-±1 cost — every decision site, same verdict;
//! * censor off: coverage gains almost nothing anywhere, and within-±1
//!   *drops* wherever real folding exists — a kept fold wall is a 2·Vny
//!   error on every gate it touches. The censor stays.
//!
//! The holdout confirmed the choice: the full-roster Protocol A survey ran
//! on the shipped knobs and passed everywhere.
//!
//! # Validation status
//!
//! **The live harness, its `validation_policy` (bars, quarantine table),
//! and the full survey record live on branch `campaign-harness`**;
//! re-measuring means that branch.
//!
//! As last measured, all roster sites: **Protocol A** — the dealiased grid
//! (Coverage profile, no median filter, pre-SRM) against the RPG's own
//! dealiased velocity, per site per tilt — passed the within-±1 and
//! coverage bars on every site-tilt, none quarantined. Unlike EET/DVL,
//! whose oracle is the DQA-edited reflectivity chain no local derivation
//! can reach, the velocity twins are reproducible from raw Level II:
//! unfolded gates carry identical 0.5 m/s codes on both sides, so the
//! residual is confined to fold regions and edited gates.
//!
//! **Protocol B** — the same `N0G` and the same `N0S` vector through this
//! module's m/s arithmetic and through [`crate::srm::derive`]'s knots
//! arithmetic — read 100% within ±1 derived level (0.5 kt) at every site
//! with a nonzero vector. The port is exact to float rounding.
//!
//! # Storm motion
//!
//! Four rungs, best first, resolved by [`storm_motion`] and carried on the
//! value itself by [`StormMotionSource`]:
//!
//! ```text
//! UserOverride       a vector the user typed in — dominant wherever set
//! RpgScitAverage     the RPG's own applied vector, read from N0S's PDB
//! BunkersRightMover  hodograph prediction from this volume's VAD profile
//! MeanWind           0–6 km mean wind, when shear cannot orient a deviation
//! ```
//!
//! ## The RPG's vector is fetched again
//!
//! This module used to record that the SCIT average "lived in `N0S`'s PDB and
//! is gone with the fetch", and made Bunkers the default on that basis. **The
//! premise is stale.** The vector the RPG actually applied is carried in the
//! `N0S` Product Description Block — halfwords 51 and 52, decoded by
//! [`nexrad_level3::model::ProductDescriptionBlock::storm_motion`] — and one
//! `N0S` is fetched per volume on the Level III round the app already makes,
//! paired to the volume by [`crate::level3::names_volume`] exactly as the
//! melting layer's `N0M` is.
//!
//! The object is small and the cost was measured rather than estimated: 21.0
//! kB mean over the corpus (15.4–24.3 kB), 0.4 % of the Level II volume it
//! accompanies, and a 144 ms median round trip against the live bucket — 72 ms
//! for an hour-narrowed list and 72 ms for the object — over eight sites.
//!
//! ## Why the RPG's vector is the default
//!
//! Bunkers and SCIT are two legitimate estimators of two different things, and
//! the gap between them is **large**. Over 21 non-degenerate volumes at eight
//! sites the direction disagreement ran from −138° to +178°, with the speed
//! difference averaging +2.5 kt — the directions diverge wildly while the
//! magnitudes broadly agree.
//!
//! **The disagreement is not a clean signed rotation.** A right-mover
//! deviation is 90° clockwise of the shear, so it is tempting to expect ours
//! to sit clockwise of the RPG's as a rule; it does not. Ours was clockwise on
//! 11 of 21 volumes, which is a coin flip, and the counterexamples are not
//! confined to the quiet cases that would excuse them — `OAX` ran ~100°
//! *counter*-clockwise on three consecutive VCP 212 volumes with both vectors
//! near 13 kt. Restricting to precipitation VCPs with both vectors over 10 kt
//! leaves 7 of 10 clockwise over a −138° to +30° spread. Whatever separates
//! the two estimators in practice, a predictable rotation is not it, and a
//! reader should not expect one.
//!
//! The divergence therefore cannot be corrected for — there is no bias to
//! subtract — so it has to be resolved by reading the RPG's vector rather than
//! by adjusting ours.
//!
//! ## The vector was the entire gap
//!
//! Scored in-band against the RPG's own `N0S` — 21 volumes, eight sites, this
//! field downsampled onto the 1 km × 1° product grid and quantized through the
//! 16-level legend, registered values-blind on the data footprint (which chose
//! offset (0, 0) on every pair, Jaccard median 0.97 with a 0.13 median margin
//! over the next-best offset):
//!
//! ```text
//!                      ours = Bunkers      ours = RPG        ceiling
//! site  grp           in-band  within1   in-band within1  in-band within1
//! AMA   decision        9.4 %   27.3 %    77.9 %  96.6 %   78.4 %  97.1 %
//! BMX   decision        5.1 %   14.7 %    85.7 %  99.4 %   85.8 %  99.4 %
//! DDC   decision       22.4 %   62.8 %    77.8 %  96.1 %   78.2 %  96.6 %
//! GRR   decision       17.1 %   57.5 %    81.5 %  99.0 %   81.4 %  99.1 %
//! OAX   decision       15.4 %   48.0 %    81.4 %  96.7 %   81.6 %  96.9 %
//! TLX   decision       17.1 %   53.1 %    80.7 %  98.9 %   80.8 %  99.0 %
//! CRP   holdout        39.7 %   58.7 %    78.1 %  96.0 %   78.5 %  96.6 %
//! MPX   holdout         7.5 %   24.3 %    83.9 %  98.7 %   84.1 %  98.9 %
//! decision (n=17)      14.96%   45.61%    80.55%  97.68%   80.76%  97.94%
//! holdout  (n=4)       23.62%   41.49%    81.00%  97.37%   81.30%  97.73%
//! all      (n=21)      16.61%   44.83%    80.64%  97.62%   80.86%  97.90%
//! ```
//!
//! The **ceiling** is the RPG's own finer dealiased velocity (`N0G`, 0.25 km,
//! 254 levels) carrying the RPG's own vector through the identical
//! downsampling and band test. It is the best score any 0.25 km derivation can
//! reach here, and it is well under 100 % because 1 km × 1° over 16 levels
//! destroys information — which is why it, and not 100 %, is the yardstick.
//!
//! The fine→coarse operator is **max-magnitude**, not the mean: the RPG keeps
//! the largest |SRV| of the eight fine gates in a coarse cell. This matters to
//! more than the score. The motion term has to be added at **fine** resolution,
//! before that pick — adding it at coarse azimuth afterwards costs 20 points —
//! which is what [`storm_relative_grid`] does, on the native 0.25 km × 0.5°
//! grid, before anything rasterizes.
//!
//! On the RPG's vector this module sits **at** that ceiling: the gap averages
//! +0.22 points, spans −0.02 to +0.57, and is within ±1 point on 21 of 21
//! volumes — with the holdout (+0.30) matching the decision sites (+0.21). The
//! vector alone moves the product from 16.6 % to 80.6 %, and it wins on every
//! volume.
//!
//! **Bunkers is not slightly off, it is a different picture.** Its within-one-
//! level agreement is 44.8 %, so on most gates it lands two or more display
//! levels away. "Keep the right-mover as the default" cannot be defended on
//! the grounds that the difference is modest; it is not modest.
//!
//! One further volume (`CRP` 2026-07-17T21:12:57) was **excluded**: it reached
//! the scorer with 600 radials instead of 720, a 60.5° missing wedge that is a
//! producer-side defect being chased separately and nothing to do with the
//! vector. It is named rather than quietly dropped.
//!
//! So the arithmetic here was never the problem. Swapping only the vector
//! changes the field by exactly the difference of the two motion projections —
//! verified to 2.8 × 10⁻⁵ m/s over the corpus, with the data footprint
//! bit-identical — and that swap is worth 64 points against the reference. The
//! input was the whole gap.
//!
//! The conclusion does not rest on the operator: under a mean operator instead
//! of max-magnitude the same comparison reads 61.89 % against a 61.93 %
//! ceiling. Lower absolutes, identical story.
//!
//! **What this corpus does not settle.** Sitting at the ceiling also says the
//! local dealiaser matched the RPG's own `N0G` almost everywhere these volumes
//! were compared — but these are largely weak-to-moderate flow days (0–6 km
//! mean winds of 4–38 kt against declared Nyquist limits of 24–33 m/s), so
//! they contain few fold walls and cannot exercise the dealiaser's known
//! shortfall in recovering the RPG's folds. A heavily-folded volume could open
//! a gap to the ceiling that nothing here would have caught. The claim proven
//! is about the **vector**, on this corpus; it is not a fresh clearance of the
//! dealiaser.
//!
//! ## What the divergence actually is, and what it implies for the fallback
//!
//! Printing the 0–6 km mean wind beside both vectors explains it. Against the
//! RPG's direction, over the same 21 volumes:
//!
//! ```text
//!                       median |Δdir|   mean |Δdir|   closer on
//! 0–6 km mean wind           9.7°          31.1°        17 / 21
//! Bunkers right-mover       73.7°          71.3°         4 / 21
//! ```
//!
//! SCIT averages **every** cell it tracked, and most cells on most days are
//! ordinary ones going where the mean wind takes them — so the RPG's vector
//! sits close to the mean wind. Bunkers deliberately steps 7.5 m/s (14.6 kt)
//! off it to predict a *supercell right-mover*. That step is a large fraction
//! of a weak-flow vector and its direction is set by the shear alone, which is
//! why the disagreement is both large and unsigned: at `OAX` the mean wind ran
//! 3.6–4.2 kt while the deviation stayed 14.6 kt, so the result pointed
//! wherever the shear did — easterly — while the RPG and the mean wind agreed
//! to within 11°.
//!
//! So the Bunkers vectors are not evidence of a broken VAD fit. They are
//! Bunkers doing exactly what Bunkers does in weak flow.
//!
//! **This is an argument that the fallback rungs are in the wrong order**, and
//! it is left recorded rather than acted on. If the fallback's job is to stand
//! in for the RPG's vector when no `N0S` arrived, the mean wind does that job
//! roughly seven times better in the median and the deviation is actively
//! harmful. If instead the fallback's job is to be the most *useful* vector to
//! a storm chaser, the right-mover is the one they want and this table is
//! beside the point. The two readings of "fallback" genuinely conflict;
//! whoever settles it should settle it deliberately, with these numbers and
//! not against them. Nothing here depends on the outcome, because the RPG's
//! own vector outranks both rungs whenever it exists.
//!
//! It is also the whole of this product's disagreement with the reference.
//! Scored in-band against the RPG's own `N0S` through a common downsampling,
//! this arithmetic on a Bunkers vector reads far under the ceiling that the
//! RPG's own finer product reaches through the identical test; forced to the
//! RPG's vector it sits at that ceiling. The subtraction is right and the
//! input was the entire gap, so the vector the reference used is the one that
//! ships. The per-site figures are in the validation section below.
//!
//! **The counter-argument is real**, and is why the fallback is labelled
//! rather than silent: for a storm chaser the right-mover may genuinely be the
//! more useful quantity, and it is what a long-time user of this pane has been
//! seeing — so switching the default silently would change what they read
//! without telling them. Every rung names itself,
//! [`StormMotionSource::caption`] puts the non-reference ones on the glass, and
//! reversing the default is a one-line change to [`storm_motion`]'s order.
//!
//! What the measurement below *does* settle is the shape of that argument. It
//! has to be made on the grounds that the right-mover is the better quantity
//! **for the reader**, because it can no longer be made on the grounds that
//! the two are close: against the reference they are 64 points apart, and
//! Bunkers agrees to within one display level on fewer than half the gates. A
//! pane defaulting to it is not showing a slightly different SRV, it is
//! showing a different field under the same name — which is precisely why the
//! rung is labelled on the glass rather than left to be inferred.
//!
//! ## Bunkers, the rung below
//!
//! Bunkers et al. 2000, *Wea. Forecasting*, 15, 61–79: "Predicting Supercell
//! Motion Using a New Hodograph Technique", the ID method, computed from the
//! volume's own VAD-fitted wind profile
//! ([`crate::velocity::volume_wind_profile`], the same fit NROT's dealiaser
//! seeds from):
//!
//! ```text
//! V_rm = V_mean + 7.5 m/s · (S × k̂) / |S|
//! V_mean = non-pressure-weighted mean wind, 0–6 km AGL
//! S      = (5.5–6 km mean wind) − (0–0.5 km mean wind)
//! S × k̂  = (S_v, −S_u)      — 90° clockwise of the shear vector
//! ```
//!
//! The bands are read at the profile's 0.3 km layer centres, so "0–0.5 km"
//! is layers centred 0.15/0.45 km and "5.5–6 km" layers centred 5.55/5.85 km
//! — a 0.1 km band skew the discretisation imposes, documented rather than
//! hidden. A user-entered vector overrides every derived rung, the RPG's
//! included; the override plumbing and its dominance are the frontend's
//! (`render_dispatch::set_storm_motion_override`), unchanged.
//!
//! ## The mean-wind rung says that it is the mean wind
//!
//! Under [`BUNKERS_MIN_SHEAR_MS`] the deviation term is dropped and the
//! estimate is pure advection. That has always been the arithmetic — it is the
//! quiet-day case, and refusing there would blank the pane. What it used to do
//! was keep reporting [`StormMotionSource::BunkersRightMover`] while doing it,
//! putting a different quantity under the label a reader uses to judge the
//! pane, with the deviation that defines a right-mover being exactly what had
//! been dropped. It now reports [`StormMotionSource::MeanWind`].
//!
//! The Bunkers arithmetic itself is untouched and stays pinned against MetPy
//! (median 1.28 kt, max 2.04 kt vector error) by
//! `rustdar-radar/tests/bunkers_metpy_oracle.rs`, whose `CALM` case pins this
//! very fallback through [`bunkers_right_mover_uv`] — which is why the split
//! is a label added beside that function rather than a change inside it.
//!
//! ## A zero vector is a reading, not a gap
//!
//! Volumes routinely publish exactly 0.0 kt from 0.0°: SCIT tracked no cells,
//! so the average over them is empty and the RPG paints an unshifted field.
//! [`SrvMotion::rpg_scit_average`] accepts that as the answer it is, so the
//! fallback never fires on it.
//!
//! It is **not** predictable from the VCP. Sampled across twelve sites, VCP 35
//! clear-air volumes published vectors up to 26 kt, VCP 212 precipitation
//! volumes published zeroes, and one site alternated zero and nonzero on
//! consecutive VCP 215 volumes. Whether SCIT found cells is the only thing
//! that decides it; reading the vector is the only way to know.
//!
//! # Units and the display seam
//!
//! Gate values are **m/s**, like every Level II product: the velocity
//! palette takes m/s, `format_value` converts m/s to the user's speed unit,
//! and the hover reads the value grid raw — so the whole display path works
//! unchanged. (The Level III pipeline stored quantized knots and converted
//! back with `l3_physical_value`'s SRV-only `× 0.514444`; that seam dies
//! with the fetch.) Nothing is quantized on the render path;
//! [`crate::srm::quantize_to_rpg_levels`] and the derived 0.5 kt levels
//! exist only so the validation below can compare like with like.

use crate::nrot::{DealiasProfile, VelocitySweep, WindProfile};
use crate::velocity::VelocityGrid;
use nexrad_model::data::Radial;

/// Metres per second per knot.
pub const KT_TO_MS: f64 = 0.514_444;

/// Bunkers et al. 2000's deviation from the 0–6 km mean wind, m/s,
/// perpendicular-right of the 0–0.5 km → 5.5–6 km shear vector.
pub const BUNKERS_DEVIATION_MS: f64 = 7.5;

/// Depth of the Bunkers mean-wind layer, km AGL.
pub const BUNKERS_MEAN_DEPTH_KM: f64 = 6.0;

/// Where a storm motion vector came from.
///
/// Four rungs, and the declaration order **is** the fallback order, so `a < b`
/// reads as "a is the rung nearer the top of the chain" — the same convention
/// [`crate::hca::MeltingLayerSource`] uses, and pinned the same way by
/// `the_storm_motion_chain_orders_itself_best_first`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StormMotionSource {
    /// A vector the user typed in. Dominant over every derived rung wherever
    /// set — including over the RPG's own.
    UserOverride,
    /// The RPG's SCIT cell-track average, read from the `N0S` Product
    /// Description Block. The vector the reference product was built with.
    RpgScitAverage,
    /// Bunkers right-mover from the volume's own VAD wind profile. A
    /// *prediction* for a supercell right-mover, not an average of what was
    /// tracked — see the module docs for how far the two have been measured
    /// apart, and in which direction (which is not a fixed one).
    BunkersRightMover,
    /// The 0–6 km mean wind: Bunkers with the deviation term dropped, because
    /// the shear was too weak to orient one. Pure advection, and it says so
    /// rather than reporting itself as a right-mover.
    MeanWind,
}

impl StormMotionSource {
    /// Whether this is the vector the RPG itself applied. The accuracy signal:
    /// scored against the RPG's own product, only this rung reaches the
    /// achievable ceiling.
    pub fn is_rpg(self) -> bool {
        self == Self::RpgScitAverage
    }

    /// A short human-readable name, for logs and for the hover.
    pub fn label(self) -> &'static str {
        match self {
            Self::UserOverride => "user storm motion",
            Self::RpgScitAverage => "RPG storm motion",
            Self::BunkersRightMover => "Bunkers right-mover",
            Self::MeanWind => "0-6 km mean wind",
        }
    }

    /// The quiet on-pane notice this rung earns, or `None` when it earns none.
    ///
    /// `None` for the RPG's own vector because that is the expected case and a
    /// notice on every SRV pane is a notice nobody reads, and `None` for an
    /// override because the user set it deliberately and the override widget
    /// already shows it — a second notice would only ever stack.
    pub fn caption(self) -> Option<&'static str> {
        match self {
            Self::UserOverride | Self::RpgScitAverage => None,
            Self::BunkersRightMover => Some(
                "no RPG storm motion for this volume - showing the Bunkers \
                 right-mover, which can differ from the RPG's cell average by \
                 a large and unpredictable angle",
            ),
            Self::MeanWind => Some(
                "no RPG storm motion for this volume, and too little shear for \
                 a right-mover - showing the 0-6 km mean wind",
            ),
        }
    }
}

/// A storm motion vector in the product's conventions: knots, and the
/// meteorological direction the storm comes **from**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SrvMotion {
    pub speed_kt: f32,
    /// Direction the storm comes *from*, degrees. See the module docs for
    /// why the radial correction then adds rather than subtracts.
    pub direction_deg: f32,
    pub source: StormMotionSource,
}

impl SrvMotion {
    /// A vector the user typed in. `None` for a non-finite speed or
    /// direction — the same refusal [`crate::srm::StormMotionSample::user_override`]
    /// makes, for the same reason: a NaN defeats every equality test
    /// downstream change detectors rely on.
    pub fn user_override(speed_kt: f32, direction_deg: f32) -> Option<Self> {
        if !speed_kt.is_finite() || !direction_deg.is_finite() {
            return None;
        }
        Some(Self {
            speed_kt,
            direction_deg,
            source: StormMotionSource::UserOverride,
        })
    }

    /// The RPG's own applied vector, as carried in an `N0S` Product
    /// Description Block.
    ///
    /// `None` for a non-finite speed or direction, refused here for the same
    /// reason [`user_override`](Self::user_override) refuses one.
    ///
    /// **A zero vector is a reading, not a gap.** Volumes routinely publish
    /// exactly 0.0 kt from 0.0°: SCIT tracked no cells, so the average over
    /// them is empty, and the RPG then paints a plain unshifted velocity
    /// field. That is its real answer. Treating it as "absent" would drop the
    /// pane onto Bunkers exactly where the reference applied nothing at all,
    /// which is the one place the two are guaranteed to disagree — so the zero
    /// is accepted and the fallback never sees it. Pinned by
    /// `a_zero_rpg_vector_is_a_reading_and_not_a_gap`.
    ///
    /// The zero is **not** predictable from the VCP, which is worth stating
    /// because it is tempting to shortcut on: sampled across sites, clear-air
    /// VCP 35 volumes published vectors up to 26 kt while precipitation VCP
    /// 212 volumes published zeroes, and one site alternated zero and nonzero
    /// on consecutive VCP 215 volumes. Whether SCIT found cells is the only
    /// thing that decides it, and reading the vector is the only way to know.
    pub fn rpg_scit_average(speed_kt: f32, direction_deg: f32) -> Option<Self> {
        if !speed_kt.is_finite() || !direction_deg.is_finite() {
            return None;
        }
        Some(Self {
            speed_kt,
            direction_deg,
            source: StormMotionSource::RpgScitAverage,
        })
    }
}

/// One already-decoded grid, dealiased in place for display: the Coverage
/// profile, **no median filter** — see the module docs for both choices.
///
/// `declared_nyquist_ms` is what this cut declared its velocity folds at, from
/// [`crate::nyquist::DeclaredNyquist`]; `None` leaves the dealiaser to estimate
/// the limit off the sweep, which is what it did for every caller before the
/// declaration crossed the model boundary.
///
/// Takes the grid rather than the radials so a caller that already has one —
/// [`crate::derive`], which decoded the whole velocity volume once for the
/// wind fit — does not decode the same sweep a second time.
pub fn dealias_grid(
    grid: &mut VelocityGrid,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    declared_nyquist_ms: Option<f64>,
) {
    // The dealiaser writes the values it is also reading the sweep's geometry
    // from, so the borrow it is handed has to be a copy. Only the geometry and
    // the declaration are read off this view — the values it unfolds are the
    // `&mut` — but the field is there and a stale slice is a worse answer than
    // a clone.
    let reported = grid.values.clone();
    let sweep_view = VelocitySweep {
        vel_grid: &reported,
        azimuths_deg: &grid.azimuths_deg,
        gate_count: grid.gate_count,
        first_gate_range_km: grid.first_gate_range_km,
        gate_interval_km: grid.gate_interval_km,
        declared_nyquist_ms,
        // The plane is the decoder's and the dealiaser does not write it, so
        // it needs no copy: it still describes `reported` cell for cell.
        status: Some(&grid.status),
    };
    // The mask is dropped rather than ignored: this profile sets
    // `refuse_incoherent` off, so the value is `None` and there is nothing here
    // to drop. A profile that refused would have to answer for it.
    let _ = crate::nrot::dealias(
        &mut grid.values,
        &sweep_view,
        elevation_deg,
        profile,
        DealiasProfile::Coverage,
    );
}

/// [`dealias_grid`] straight off a sweep's radials, decoding it first.
/// `None` when the sweep carries no velocity.
pub fn dealiased_grid(
    radials: &[Radial],
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    declared_nyquist_ms: Option<f64>,
) -> Option<VelocityGrid> {
    let mut grid = crate::velocity::grid(radials)?;
    dealias_grid(&mut grid, elevation_deg, profile, declared_nyquist_ms);
    Some(grid)
}

/// Add the storm-motion term to every defined gate, in place:
/// `v += speed · cos(direction − azimuth)`, at the radial's centre azimuth.
///
/// The correction is constant along range, so it is computed once per
/// radial. NaN gates stay NaN — mapping them through the arithmetic would
/// paint the storm-motion field itself across every gate the radar saw
/// nothing in, the same failure [`crate::srm::derive`] guards against.
pub fn apply_storm_motion(grid: &mut VelocityGrid, motion: &SrvMotion) {
    let speed_ms = motion.speed_kt as f64 * KT_TO_MS;
    for (row, &az) in grid.values.iter_mut().zip(&grid.azimuths_deg) {
        let component = speed_ms * (motion.direction_deg as f64 - az).to_radians().cos();
        for v in row.iter_mut() {
            if !v.is_nan() {
                *v += component;
            }
        }
    }
}

/// The full per-tilt derivation on an already-decoded grid: dealias
/// (Coverage, no median filter), then the storm-motion correction.
pub fn storm_relative_grid(
    mut grid: VelocityGrid,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    motion: &SrvMotion,
    declared_nyquist_ms: Option<f64>,
) -> VelocityGrid {
    dealias_grid(&mut grid, elevation_deg, profile, declared_nyquist_ms);
    apply_storm_motion(&mut grid, motion);
    grid
}

/// [`storm_relative_grid`] straight off a sweep's radials, decoding it first.
/// `None` when the sweep carries no velocity.
pub fn compute_srv_grid(
    radials: &[Radial],
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    motion: &SrvMotion,
    declared_nyquist_ms: Option<f64>,
) -> Option<VelocityGrid> {
    Some(storm_relative_grid(
        crate::velocity::grid(radials)?,
        elevation_deg,
        profile,
        motion,
        declared_nyquist_ms,
    ))
}

/// The Bunkers et al. 2000 right-mover from a fitted wind profile, as a
/// motion vector `(u, v)` in m/s (the direction the storm moves *toward*),
/// or `None` when the profile cannot support it.
///
/// The paper's ID method, exactly (their Eq. 1, right member):
/// `V_rm = V_mean + D · (S × k̂)/|S|` with `V_mean` the non-pressure-weighted
/// 0–6 km AGL mean wind, `S` the shear vector from the 0–500 m mean wind to
/// the 5500–6000 m mean wind, `D` = 7.5 m/s, and `S × k̂ = (S_v, −S_u)` the
/// 90°-clockwise rotation that puts the deviation perpendicular-**right** of
/// the shear through the mean wind.
///
/// Bands are read at the profile's 0.3 km layer centres (see the module
/// docs). Refused — `None` — when fewer than [`BUNKERS_MIN_MEAN_LAYERS`] of
/// the twenty 0–6 km layers carry a fit, or when either shear band is empty.
///
/// When the shear magnitude is under [`BUNKERS_MIN_SHEAR_MS`] this does **not**
/// refuse: the deviation term is dropped and the mean wind comes back, because
/// a near-zero shear vector has no meaningful "right of" and the deviation
/// direction would be noise. That is the quiet-day case and it must keep
/// painting. What comes back then is the mean wind and not a right-mover, so
/// callers that need to say which they got want [`bunkers_right_mover`], whose
/// [`SrvMotion::source`] distinguishes the two.
pub fn bunkers_right_mover_uv(profile: &WindProfile) -> Option<(f64, f64)> {
    bunkers_estimate(profile).map(|(uv, _)| uv)
}

/// [`bunkers_right_mover_uv`]'s arithmetic, plus which quantity it produced.
///
/// The split exists so the label can tell the truth without the arithmetic
/// moving: the `(u, v)` returned here is bit-for-bit what
/// `bunkers_right_mover_uv` has always returned, and that function is pinned
/// against MetPy by `rustdar-radar/tests/bunkers_metpy_oracle.rs` — including
/// its `CALM` case, which pins exactly this mean-wind fallback. The defect
/// being fixed is a mislabel, not a miscalculation.
fn bunkers_estimate(profile: &WindProfile) -> Option<((f64, f64), StormMotionSource)> {
    let layer_km = WindProfile::LAYER_KM;
    let layers = (BUNKERS_MEAN_DEPTH_KM / layer_km).round() as usize;

    let mut mean = (0.0f64, 0.0f64, 0usize);
    let mut head = (0.0f64, 0.0f64, 0usize); // 0–0.5 km
    let mut tail = (0.0f64, 0.0f64, 0usize); // 5.5–6 km
    for l in 0..layers {
        let centre = (l as f64 + 0.5) * layer_km;
        let Some((u, v)) = profile.wind_at_km(centre) else {
            continue;
        };
        mean = (mean.0 + u, mean.1 + v, mean.2 + 1);
        if centre < 0.5 {
            head = (head.0 + u, head.1 + v, head.2 + 1);
        }
        if (5.5..BUNKERS_MEAN_DEPTH_KM).contains(&centre) {
            tail = (tail.0 + u, tail.1 + v, tail.2 + 1);
        }
    }
    if mean.2 < BUNKERS_MIN_MEAN_LAYERS || head.2 == 0 || tail.2 == 0 {
        return None;
    }
    let mean = (mean.0 / mean.2 as f64, mean.1 / mean.2 as f64);
    let head = (head.0 / head.2 as f64, head.1 / head.2 as f64);
    let tail = (tail.0 / tail.2 as f64, tail.1 / tail.2 as f64);
    let shear = (tail.0 - head.0, tail.1 - head.1);
    let magnitude = (shear.0 * shear.0 + shear.1 * shear.1).sqrt();
    if magnitude < BUNKERS_MIN_SHEAR_MS {
        // No shear direction worth deviating from: the propagation term is
        // dropped and the estimate is pure advection — the storm moves with
        // the mean wind. This is the quiet-day case, where the RPG's own
        // SCIT average reads ~0 kt and the Level III product still painted;
        // refusing here would blank the pane instead.
        //
        // It reports itself as the mean wind, because that is what it is. It
        // used to come back stamped `BunkersRightMover`, which put a different
        // quantity under the label a reader uses to decide how much to trust
        // the pane — the deviation that makes a right-mover a right-mover is
        // precisely what was dropped to get here.
        return Some((mean, StormMotionSource::MeanWind));
    }
    // (S × k̂)/|S| = (S_v, −S_u)/|S|: perpendicular, 90° clockwise of S.
    Some((
        (
            mean.0 + BUNKERS_DEVIATION_MS * shear.1 / magnitude,
            mean.1 - BUNKERS_DEVIATION_MS * shear.0 / magnitude,
        ),
        StormMotionSource::BunkersRightMover,
    ))
}

/// Fewest fitted 0–6 km layers (of twenty) for a mean wind worth calling
/// "the 0–6 km mean": under this the estimate is a few tilts' sidelobes.
pub const BUNKERS_MIN_MEAN_LAYERS: usize = 12;

/// Smallest 0–0.5 → 5.5–6 km shear magnitude, m/s, whose direction is worth
/// deviating from. Well under any supercell environment (Bunkers' own
/// dataset median is ~2.6× this) — under the floor the deviation is dropped
/// and the estimate is the mean wind alone, not refused.
pub const BUNKERS_MIN_SHEAR_MS: f64 = 2.0;

/// [`bunkers_right_mover_uv`] in the product's conventions: knots, and the
/// meteorological direction the motion comes **from**.
///
/// The returned [`SrvMotion::source`] is [`StormMotionSource::MeanWind`], not
/// [`StormMotionSource::BunkersRightMover`], whenever the shear fell under
/// [`BUNKERS_MIN_SHEAR_MS`] and the deviation was dropped — the two are
/// different quantities and only one of them is a right-mover. Pinned by
/// `a_mean_wind_fallback_does_not_claim_to_be_a_right_mover`.
pub fn bunkers_right_mover(profile: &WindProfile) -> Option<SrvMotion> {
    let ((u, v), source) = bunkers_estimate(profile)?;
    let speed_kt = (u * u + v * v).sqrt() / KT_TO_MS;
    // Toward-direction is atan2(u, v) in compass degrees; "from" is its
    // reciprocal. rem_euclid keeps it in [0, 360).
    let direction_deg = (u.atan2(v).to_degrees() + 180.0).rem_euclid(360.0);
    Some(SrvMotion {
        speed_kt: speed_kt as f32,
        direction_deg: direction_deg as f32,
        source,
    })
}

/// The storm motion a render should apply, resolved down the chain: the user's
/// override, else the RPG's own vector for this volume, else Bunkers from the
/// volume's profile (which degrades to the mean wind under its shear floor,
/// and says so).
///
/// `None` means **no SRV render**, because painting base velocity under a
/// storm-relative label is the failure the Level III path refused too. It can
/// only happen when there is no override, no `N0S` for the volume, and no
/// usable wind profile.
///
/// `rpg` is the vector read from this volume's own `N0S`. It is threaded in
/// rather than fetched here because this crate's derivation path is
/// synchronous and offline by design — the fetch and the volume pairing are
/// the frontend's, exactly as the melting layer's are. Whatever arrives here
/// must already belong to the volume being rendered; a vector from a
/// neighbouring volume is a measurable error and pairing is where it is
/// refused.
///
/// Pinned by `the_rpg_vector_outranks_bunkers_and_yields_to_an_override`.
pub fn storm_motion(
    profile: Option<&WindProfile>,
    user_override: Option<SrvMotion>,
    rpg: Option<SrvMotion>,
) -> Option<SrvMotion> {
    if let Some(motion) = user_override {
        // Only an override may claim to be one; a mislabelled sample would
        // let a Bunkers vector survive an override change detector.
        if motion.source == StormMotionSource::UserOverride {
            return Some(motion);
        }
    }
    if let Some(motion) = rpg {
        // Same guard, same reason: only a vector actually read from an `N0S`
        // may claim the RPG's provenance, or a derived vector could ride the
        // reference's label onto the glass.
        if motion.source == StormMotionSource::RpgScitAverage {
            return Some(motion);
        }
    }
    profile.and_then(bunkers_right_mover)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
