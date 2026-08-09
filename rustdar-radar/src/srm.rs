//! Storm-relative mean velocity, derived from Level III **dealiased** velocity.
//!
//! Every tilt is computed here, 0.5° included:
//!
//! ```text
//! SRM_kt = V_kt + speed · cos(direction − azimuth)
//! ```
//!
//! Only the lowest SRM *product* is still published. NWS SCN 22-96 dropped
//! `N1S`/`N2S`/`N3S` from the NOAAPort broadcast in 2022, and every CORS-clean
//! source is NOAAPort-derived: `unidata-nexrad-level3` last wrote to those three
//! keys in 2020, while `N0S` runs 294 objects a day. THREDDS, GCS, IEM, COD and
//! NCEI were all checked. `N0S` is still fetched — see
//! [`STORM_MOTION_PRODUCT`] — but for its vector alone; it is no longer drawn.
//!
//! **Deriving 0.5° rather than rendering `N0S`** is what makes the four panes
//! one thing rather than two. `N0S` is 1 km at the RPG's 16 display levels
//! while the derived tilts are 0.25 km at 254, so a rendered `N0S` was visibly
//! coarser than the three tilts above it; and its gate values already have the
//! RPG's own vector baked in, so it was also the one tilt a storm motion
//! override could not reach. `N0G` is the same product 154 as `N1G` at the same
//! 0.5° cut — verified at `TLX`: product code 154, 0.5°, 1200 bins of 0.25 km
//! over 720 half-degree radials, minimum -63.5 m/s in steps of 0.5 over 254
//! levels, byte for byte the shape `N1G` has.
//!
//! **From Level III, never Level II.** L2 velocity is aliased and
//! `nexrad-decode` has no dealiasing; the errors would be 2×Nyquist — 50–70 kt
//! in exactly the mesocyclone cores the product exists to show — and would
//! render couplets inverted. The RPG dealiases before publishing `N?G`/`N?U`.
//!
//! **The vector is read, not estimated.** It is in the `N0S` Product
//! Description Block, halfwords 51 and 52; see
//! [`nexrad_level3::model::ProductDescriptionBlock::storm_motion`]. Bunkers and
//! every other estimator is refuted by that: the RPG's own SCIT average is
//! available for free and is what the RPG itself used. No velocity product can
//! supply it — halfword 51 is the BZ2 compression flag on every digital
//! product, and `N0G` carries a 1 there like the rest, which reads as "0.1 kt".
//!
//! **Native resolution is kept.** The RPG resamples to 1 km × 1° and 16 levels;
//! the source products are 0.25 km with 254 levels, so every derived tilt has
//! four times the range resolution and sixteen times the value resolution of
//! the `N?S` it replaces. [`quantize_to_rpg_levels`] exists only so the
//! validation test can compare like with like.
//!
//! # Accuracy
//!
//! **The live harness, its `validation_policy` (quarantine table, bars,
//! resampler) and the offline policy pins now live on branch
//! `campaign-harness`.** The figures below are the last measured before the
//! move; re-measuring means that branch.
//!
//! Measured by `live_validation`, which fetches exactly what production
//! fetches and pairs every velocity product with its own volume's vector.
//! Across two volumes at each of thirteen sites carrying a nonzero vector, on
//! 2026-07-26 — 23 site-volumes per tilt, then-quarantined sites excluded,
//! range over site-volumes rather than a pooled average:
//!
//! ```text
//! tilt         product   exact         within one level
//!  0.5°  N0G   154       76.1-91.0%    99.10-99.80%
//!  1.3°  N1G   154       77.3-90.1%    98.94-99.87%
//!  2.4°  N2U    99       82.2-98.8%    99.90-99.99%
//!  3.1°  N3U    99       80.7-98.6%    99.91-100.00%
//! ```
//!
//! **Two volumes a site is a small sample, and the 0.5° row above is the
//! optimistic end of one.** The forty-volume survey below puts the real 0.5°
//! spread at 93.7-99.9%. Read that row as "the sites this run reached, on the
//! volumes it caught them", not as the population.
//!
//! Only the 0.5° column and the four-tilt site total are asserted on; a single
//! upper tilt is allowed to dip, as `N1G` does at `KABR`, provided the site
//! total holds.
//!
//! **The 0.5° tilt is the strongest of the four measurements**, not the
//! weakest, however the percentages read: its oracle is `N0S`, the product
//! rustdar itself rendered until this derivation replaced it, and it is still
//! being written. The upper three are checked against `N1S`/`N2S`/`N3S`, which
//! tgftp still serves but which the NOAAPort feed dropped in 2022. So tilt 0
//! compares the new answer against the old one directly.
//!
//! ## Where the residual comes from
//!
//! Almost all of it is the comparison's resampler, not the derivation. The
//! ranking above is the tell: `N2U`/`N3U` are already 1°, so only the range
//! step of `validation_policy::resample_to_rpg_grid` runs on them and they
//! agree to 99.9%+; `N0G`/`N1G` are half-degree and need the azimuth step too,
//! and they are the two that fall short. It gets worse as the tilt gets
//! lower, where azimuthal gradients are sharpest.
//!
//! Some sites report a 0.0 kt vector, which makes the correction identically
//! zero and isolates the conversion and the resampler from the storm-motion
//! term. Those tilts are measured and printed but never asserted on. **Per
//! tilt**, so the numbers can be read against the table above rather than
//! pooled into one figure that hides which tilt did what:
//!
//! ```text
//! tilt         zero vector, still oracle     nonzero vector (table above)
//!  0.5°  N0G   88.1-88.3% / 98.52-98.80%     76.1-91.0%  /  99.10-99.80%
//!  1.3°  N1G   90.7-91.1% / 99.20-99.25%     77.3-90.1%  /  98.94-99.87%
//!  2.4°  N2U   98.3%      / 99.94%           82.2-98.8%  /  99.90-99.99%
//!  3.1°  N3U   98.5%      / 99.96-99.97%     80.7-98.6%  /  99.91-100.00%
//! ```
//!
//! Two consecutive runs, ~59k control gates at the lowest tilt and ~27k at the
//! highest; the two runs agree to within 0.3 points everywhere.
//!
//! **That is the argument.** With the correction multiplied by zero the
//! disagreement keeps exactly the shape it has with the correction applied:
//! poor at the two half-degree tilts, 99.9%+ at the two the RPG already
//! publishes at 1°. The residual tracks whether the azimuth step ran, not
//! whether the storm-motion term did. A pooled figure could not say this — it
//! averages the tilts whose recombination is hard together with the ones where
//! there is none — which is why it is no longer pooled.
//!
//! **Do not read it as absolving the correction**: it says the resampler
//! accounts for the residual, not that the correction is free.
//!
//! For that reading to hold the *oracle's* vector has to be zero as well, not
//! just the applied one — the RPG's product has its own volume's fit baked into
//! its gate values, so a zero vector applied against a moving oracle measures
//! the mismatch and nothing about the resampler. `validation_policy::classify_sample`
//! requires both; before it did, a `KMPX` tilt whose applied vector read 0.0 kt
//! against a moving volume scored 31.04% within one level and pooled this
//! control down to 74.39%.
//!
//! **This does not hold everywhere, and the exceptions are not rare.** The
//! table above is two volumes a site. At 0.5° the oracle is `N0S`, which the
//! bucket keeps for the whole UTC day, so
//! `live_validation::live_lowest_tilt_across_volumes` can measure the same
//! quantity over up to forty volumes instead of two. Two such surveys, four
//! hours apart on 2026-07-26, own-volume vector, 0.5°, pooled percentage and
//! the count of volumes under the bar:
//!
//! ```text
//! site   survey A            survey B          verdict
//! KLZK   99.66  0/38         99.63  0/40       clears
//! KMRX   99.38  0/40         99.56  0/40       clears
//! KOAX   99.52  0/40         99.49  0/40       clears
//! KMPX   99.44  0/40         99.42  0/31       clears
//! KPAH   99.47  0/40         99.34  3/40       clears, dips
//! KSGF   99.52 10/40         99.52 10/40       clears, dips often
//! KEAX   99.35  0/9          99.31  0/3        clears
//! KMOB   99.31  0/15         99.32  0/9        clears
//! KMTX   99.29  0/40         99.24  0/36       clears
//! KMVX   99.14  3/30         99.17  1/20       clears, dips
//! KFSD   99.32  2/40         99.17 10/37       QUARANTINED at 0.5°
//! KABR   98.94 23/40         98.76 36/40       QUARANTINED at 0.5°
//! KUEX   99.08  4/7          98.58 14/16       QUARANTINED at 0.5°
//! KMLB   98.71 16/18         98.57 33/33       QUARANTINED at 0.5°
//! KBIS   98.58 40/40         98.33 40/40       QUARANTINED at 0.5°
//! KSFX   96.24 40/40         97.05 32/32       QUARANTINED whole
//! KTLH   99.19  2/7          98.31  3/3        QUARANTINED at 0.5°
//! KDDC   98.82  2/3          98.82  2/3        undecided, three volumes
//! ```
//!
//! **Seven sites, not two.** `KMLB` and `KABR` were latent failures the
//! four-tilt harness had never reached — it stops after two asserted sites and
//! they sit thirteenth and sixth. `KUEX` is fifth, was left out after survey A
//! read 99.08% on seven volumes, and promptly failed a live run at 97.94%;
//! survey B settled it. `KFSD` was caught the same way, by failing. `KTLH` is
//! short on a third survey too (98.84%, 6 of 11 under, min 97.74%) — eleven of
//! twenty-one volumes across the three, and no survey has ever put it above.
//!
//! Read the columns together rather than any one alone. The pooled figures move
//! by two to five tenths between surveys and the dip counts move a lot more,
//! because what varies is how much azimuthal structure the field carries — the
//! thing the half-degree recombination is worst at. A site belongs here when it
//! is short on all of them.
//!
//! `KDDC` is the one left undecided, on three volumes each time — genuinely
//! thin, and it is twentieth in `SITES`, so a run reaches it only when most of
//! the list is quiet. If one does it will probably fail. Re-survey on a day it
//! carries a vector for longer, then quarantine or clear it; do not leave it
//! undecided indefinitely. Note that "thin evidence" was the reasoning that
//! left `KUEX` out for a round, and `KUEX` then failed a live run.
//!
//! Expect this list to grow as more sites are surveyed on more days. It is a
//! statement about the half-degree recombination, not about those radars.
//! See `validation_policy::QUARANTINED` for each site's numbers and
//! eliminations.
//!
//! Quarantining sites at 0.5° also surfaced a flaw in what the quarantine
//! *did*: the site total pooled every tilt, so a quarantined tilt's gates still
//! entered the figure the bar was applied to — excluded from its own assertion
//! and averaged into the shared one. Tilt 0 is about a quarter of a site's
//! gates, so `KBIS` failed a run at 98.87% pooled whose upper three tilts were
//! 99.52%. The total now runs over the tilts
//! `validation_policy::tilt_is_asserted` admits.
//!
//! `KSFX` remains the only whole-site exclusion, and the only one where
//! narrowing the scope would not help: it misses at its lowest *two* tilts —
//! 95.2-95.6% at 0.5°, 96.4% at 1.3°, against 99.4-99.6% at 2.4° and 3.1°. Its
//! four-tilt total is 96.93% and 96.99%, and dropping 0.5° would not rescue it
//! because 1.3° is short too — the tilt a `LowestTilt` scope keeps.
//!
//! So the claim this module supports is narrower than the table suggests: **the
//! bar is met at every site the shipped test asserts on**, and at 0.5° that is
//! now eleven of the eighteen sites that carry a vector. The upper three
//! tilts have no bucket oracle and so have never been surveyed this way at all;
//! every figure for them rests on one volume per run.
//!
//! The agreement figure is still a **reconstruction of an undocumented step**
//! rather than an independent validation, because the resampler that produces
//! it was built against this same oracle. Its *ordering* now has an argument
//! that does not appeal to the score; its averaging operator does not, and
//! cannot have one from this data — see
//! `validation_policy::resample_to_rpg_grid`. Treat exact-match as indicative
//! and within-one-level as the criterion.
//!
//! ## Volume pairing
//!
//! All four tilts of a volume share one vector, and the RPG re-fits the SCIT
//! average every volume. Only `N0S` carries one, so pairing a velocity product
//! with a vector means deciding *which* `N0S`, and taking the newest is wrong
//! most of the time it matters.
//!
//! **It is the steady state, not a boundary race.** `N0S` is published when the
//! 0.5° cut finishes and `N1G`/`N2U`/`N3U` when their own cuts do, so for most
//! of a volume the newest `N0S` belongs to a volume the upper tilts have not
//! reached yet — the vector is a volume *ahead* of them rather than behind.
//! [`MotionProvenance::PreviousVolume`] records when it happens, and
//! `live_validation::live_storm_motion_volume_pairing_rate` fetches exactly
//! what a site load fetches and measures how often. 22 sites × 9 sweeps five
//! minutes apart on 2026-07-26, 792 renders:
//!
//! ```text
//! tilt         product   renders paired with another volume's vector
//!  0.5°  N0G   154        23/198   11.6%
//!  1.3°  N1G   154        76/198   38.4%
//!  2.4°  N2U    99       100/198   50.5%
//!  3.1°  N3U    99       107/198   54.0%
//!  all                   306/792   38.6%
//! ```
//!
//! Per-sweep the total ranged 21.6% to 53.4%, so this is not one unlucky
//! minute. 0.5° is the *least* affected, because `N0S` and `N0G` come off the
//! same cut, and its mismatches run the **other** way: all 14 mismatches in the
//! sample for which no own-volume `N0S` existed anywhere in the bucket were at
//! 0.5°, and none at the upper tilts. SAILS republishes the 0.5° cut about
//! twice a volume, so `N0G` can arrive before its volume's `N0S` is written at
//! all — and no history can help with that one.
//!
//! **What it costs is bimodal, and the tail is what matters.** Measured by
//! `validation_policy::level_shift`, which derives the same velocity product
//! twice — once with the newest vector, once with its own volume's — so no
//! oracle and no resampler enters the number. The median mismatch costs
//! *nothing*: adjacent volumes usually re-fit to within 1.4 kt, and 93.6% of
//! mismatches leave every single gate inside one data level. But the tail is
//! severe, and it is worst exactly where the product matters. **Worst
//! observed**, not worst possible — every one of these is a sample, and each
//! fresh run has so far found a worse one:
//!
//! ```text
//! site  vector applied   vector that belonged   within one   own-volume
//! KFSD  66.5 kt / 298°   19.4 kt / 290°         17.1 - 19.3%   99.87 - 99.91%
//! KFSD  19.4 kt / 290°   66.5 kt / 298°         17.8 - 19.7%   99.97 - 99.98%
//! KFSD  (across volumes, all four tilts)        47.8 - 51.1%
//! KOAX  57.3 kt / 316°   37.4 kt / 307°         58.4 - 62.6%
//! KFSD  35.6 kt / 314°   24.1 kt / 292°         67.6 - 69.5%
//! KDDC   2.5 kt / 332°   11.2 kt / 216°         81.1 - 83.8%
//! KOAX  28.8 kt / 315°   40.4 kt / 309°         87.7 - 87.8%
//! KUEX  11.8 kt / 213°    0.0 kt /   0°         88.2 - 89.2%
//! ```
//!
//! The top two rows are the same site on consecutive runs, caught in both
//! directions as a 47 kt re-fit passed through: **82 points** of
//! within-one-level agreement, on gates that agree to 99.9% once the right
//! vector is applied. A field that agrees with the RPG on 17% of its gates is
//! not a degraded rendering of storm-relative velocity, it is a different
//! quantity. 1.8% of all renders came in under the 99% bar this module is held
//! to and 1.4% under 90% — and nothing about them is self-announcing: the pane
//! looks like storm-relative velocity either way.
//!
//! A median of zero is what the earlier "usually a tenth of a point" figure
//! caught, and rejecting a per-volume history on it was reading the middle of a
//! bimodal distribution as if it were the whole of it. So the history was
//! built. [`DerivedSrm::motion_volume_matches`] still records the condition, and
//! production keeps the last few volumes' vectors per site and applies the one
//! belonging to the velocity product being rendered — see
//! `render_dispatch::RenderDispatcher::storm_motion_for`. It falls back to the
//! newest only when no vector for that volume was ever seen, which is the 0.5°
//! SAILS case above: a vector one volume out beats a blank pane.
//!
//! It bites the *validation* harder still, because tgftp's `sn.last` and the
//! bucket's newest key drift independently. So the harness looks up the bucket
//! object belonging to tgftp's volume and cut rather than taking the newest —
//! without which the lowest tilt was skipped at two sites in three.

use nexrad_level3::model::{DataPacket, Level3Message, RadialPacket, RadialRun, StormMotion};

/// Knots per metre per second.
const MS_TO_KT: f64 = 1.0 / 0.514_444;

/// Product codes carrying dealiased velocity that an SRM tilt can be derived
/// from: 154 super-resolution (`N?G`, 0.5° radials) and 99 (`N?U`, 1°). Both
/// encode 0.25 km gates and 254 levels of 0.5 m/s.
pub const VELOCITY_PRODUCT_CODES: [i16; 2] = [154, 99];

/// The AWIPS ID fetched **for its storm motion vector alone**, never rendered.
///
/// Product 56 is the only thing in the bucket carrying halfwords 51/52 as a
/// vector; on a digital velocity product halfword 51 is the BZ2 compression
/// flag, so `N0G` read as a vector reports 0.1 kt — plausible enough to ship.
/// See [`nexrad_level3::model::ProductDescriptionBlock::storm_motion`].
pub const STORM_MOTION_PRODUCT: &str = "N0S";

/// The AWIPS IDs the four SRM tilts are **derived** from, lowest first: `N0G`
/// and `N1G` super-resolution (product 154, 0.5° radials), `N2U`/`N3U` at 1°
/// (product 99). All four are 0.25 km gates over 254 levels.
///
/// The bucket carries `N0G`/`N1G` but not `N2G`/`N3G`, and `N2U`/`N3U` but not
/// `N0U`/`N1U` — verified by listing a full UTC day, 294 objects each for
/// `TLX`, matching `N0S` exactly. **These are request keys, not elevations.**
/// `N1G` is *not* 1.5°: in VCP 212 it is 1.3°, and the angle always comes from
/// the fetched product's own Product Description Block.
///
/// [`STORM_MOTION_PRODUCT`] is deliberately absent. It is the RPG's own
/// already-storm-relative field at 1 km with the RPG's own vector baked in, so
/// rendering it as the 0.5° tilt made that one pane both coarser than its
/// neighbours and deaf to the storm motion override.
pub const SRM_TILT_PRODUCTS: [&str; 4] = ["N0G", "N1G", "N2U", "N3U"];

/// Everything rustdar fetches for storm-relative velocity: the vector source
/// followed by the four tilts it is applied to.
///
/// One more object per site than rendering `N0S` directly cost, and by far the
/// largest of the five: the 0.5° cut is super-resolution and sees the most
/// echo, so `N0G` alone outweighs the other four together. Measured on
/// 2026-07-26 over every Level III object a site load fetches:
///
/// ```text
/// site   N0S     N0G      without N0G   with N0G
/// TLX    30 KiB  258 KiB  359 KiB       616 KiB
/// MPX    27 KiB  237 KiB  412 KiB       649 KiB
/// ```
///
/// It scales with echo coverage, so a site in widespread precipitation costs
/// more than these and a clear one much less.
pub const SRM_FETCH_PRODUCTS: [&str; 5] = ["N0S", "N0G", "N1G", "N2U", "N3U"];

/// Physical value per gate step in the derived packet, in knots. Finer than the
/// 0.5 m/s (0.97 kt) the source products carry, so the requantisation adds no
/// error of its own.
const DERIVED_SCALE: f32 = 2.0;

/// Gate value standing for 0 kt, so the representable range is
/// `(2 - offset)/scale` upward: **-499 kt to +32,267 kt**.
///
/// Sized against the worst case that can actually reach here, not against
/// meteorology: the source products floor at -63.5 m/s (-123.4 kt) and the
/// settings dialog admits up to [`MAX_OVERRIDE_SPEED_KT`] of storm motion, for
/// -323.4 kt. Below the floor the gate value would clamp, and a clamped gate
/// is still ≥ 2, so it paints as data rather than dropping out — which is why
/// the range has to cover the input rather than merely be "generous".
const DERIVED_OFFSET: f32 = 1000.0;

/// Largest storm motion the settings dialog admits, in knots. Lives here
/// because [`DERIVED_OFFSET`] is sized from it; the widget reads it.
///
/// Well past anything meteorological — the fastest observed storm motions are
/// around 70 kt — but the encoding must survive whatever the widget permits.
pub const MAX_OVERRIDE_SPEED_KT: f32 = 200.0;

/// Gate values 0 and 1 are "below threshold" and "range folded" in every
/// product involved, and the renderer skips both.
const NO_DATA: u16 = 0;
const FIRST_DATA_GATE: u16 = 2;

/// A storm-relative velocity field computed from a dealiased velocity product.
#[derive(Debug, Clone)]
pub struct DerivedSrm {
    /// Gate values are storm-relative knots through
    /// [`scale`](Self::scale)/[`offset`](Self::offset), in the same geometry as
    /// the source product.
    pub packet: RadialPacket,
    /// `knots = (gate - offset) / scale`.
    pub scale: f32,
    /// See [`scale`](Self::scale).
    pub offset: f32,
    /// From the source product's PDB, never from its AWIPS mnemonic.
    pub elevation_angle: f32,
    /// From the source product's PDB. Identifies the cut within the volume;
    /// split cuts and SAILS/MRLE repeats share an angle but not a number.
    pub elevation_number: u16,
    /// The vector applied.
    pub motion: StormMotion,
    /// Which volume the vector belongs to, relative to this velocity product.
    pub motion_provenance: MotionProvenance,
}

/// Where the vector a derived field used stands relative to the velocity
/// product it was applied to.
///
/// Three states, not a bool: "not this volume" and "no volume at all" are
/// different claims, and a bool made the second read as the first — the
/// override path used to report a stale RPG vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionProvenance {
    /// The RPG's vector, fitted for this very volume scan.
    SameVolume,
    /// The RPG's vector, but fitted for an earlier volume.
    ///
    /// The RPG re-fits the SCIT average every volume. This usually costs
    /// nothing within one data level — adjacent fits typically agree to about
    /// 1.4 kt — but the distribution is bimodal and the tail has been measured
    /// at 82 points, so it is not a figure to average. See the volume-pairing
    /// section of this module's docs; production keeps a per-volume history
    /// precisely so this state stays rare.
    PreviousVolume,
    /// A vector the user typed in. It belongs to no volume, so the velocity
    /// product's volume says nothing about it either way.
    UserOverride,
}

impl DerivedSrm {
    /// Whether the vector was fitted for this very volume — the accuracy
    /// signal, and `false` for an override, which has no volume to agree with.
    pub fn motion_volume_matches(&self) -> bool {
        self.motion_provenance == MotionProvenance::SameVolume
    }
}

/// Where a storm motion vector came from, and which volume it describes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StormMotionSample {
    pub motion: StormMotion,
    /// [`ProductDescriptionBlock::volume_key`] of the `N0S` it was read from,
    /// or `None` for a vector the user typed in.
    ///
    /// [`ProductDescriptionBlock::volume_key`]: nexrad_level3::model::ProductDescriptionBlock::volume_key
    pub volume: Option<(u16, u32)>,
}

impl StormMotionSample {
    /// The vector an `N0S` product carries, or `None` for anything else.
    pub fn from_message(msg: &Level3Message) -> Option<Self> {
        Some(Self {
            motion: msg.pdb.storm_motion()?,
            volume: Some(msg.pdb.volume_key()),
        })
    }

    /// A vector the user typed in. It belongs to no volume, so a derived field
    /// built from it never claims the RPG's provenance — and never claims to
    /// be *stale* either, which a sentinel volume key made it do.
    ///
    /// `None` for a non-finite speed or direction. The guard is here rather
    /// than only at the widget because a NaN is not merely a bad render: it
    /// makes every equality test on the sample false, so a change detector
    /// comparing two identical overrides sees a change on every frame. A
    /// constructor that cannot produce one closes that off for every caller.
    pub fn user_override(speed_kt: f32, direction_deg: f32) -> Option<Self> {
        if !speed_kt.is_finite() || !direction_deg.is_finite() {
            return None;
        }
        Some(Self {
            motion: StormMotion {
                speed_kt,
                direction_deg,
                is_scit_average: false,
            },
            volume: None,
        })
    }
}

/// Whether a validation run's nonzero-vector sample is worth drawing a
/// conclusion from.
///
/// A zero vector multiplies the correction by zero, so those gates exercise the
/// m/s→kt conversion and the resampler and say nothing whatever about the sign,
/// magnitude or azimuth convention of the storm-motion term. A run made
/// entirely of them can report a high number while testing none of that.
///
/// Lives out here, as a pure function, so both halves can be exercised without
/// the network — inside the live test the site count is never zero when the
/// gate count is large, which makes that conjunct unfalsifiable in place.
pub fn sample_is_conclusive(sites_asserted: usize, nonzero_gates: usize) -> bool {
    sites_asserted > 0 && nonzero_gates > MIN_NONZERO_GATES
}

/// Floor for [`sample_is_conclusive`]. Roughly one tilt's worth of echo.
pub const MIN_NONZERO_GATES: usize = 10_000;

/// The first digital radial packet in a message's symbology.
pub fn radial_packet(msg: &Level3Message) -> Option<&RadialPacket> {
    msg.symbology.as_ref()?.layers.iter().find_map(|layer| {
        layer.packets.iter().find_map(|pkt| match pkt {
            DataPacket::DigitalRadial(rp) => Some(rp),
            _ => None,
        })
    })
}

/// Whether `msg` is a dealiased velocity product an SRM tilt can be built from.
pub fn is_velocity_source(msg: &Level3Message) -> bool {
    VELOCITY_PRODUCT_CODES.contains(&msg.pdb.product_code)
}

/// Compute storm-relative velocity from a dealiased velocity product.
///
/// Returns `None` for anything that is not one of
/// [`VELOCITY_PRODUCT_CODES`], or that carries no radial data. An `N0S` is
/// refused: it is already storm-relative, so the correction would be applied
/// twice. Nothing renders it — it is fetched for its vector alone.
pub fn derive(velocity: &Level3Message, sample: &StormMotionSample) -> Option<DerivedSrm> {
    if !is_velocity_source(velocity) {
        return None;
    }
    let source = radial_packet(velocity)?;
    if source.radials.is_empty() {
        return None;
    }

    let pdb = &velocity.pdb;
    let scale = pdb.data_scale();
    let offset = pdb.data_offset();
    let motion = sample.motion;

    let radials = source
        .radials
        .iter()
        .map(|run| {
            // The packet records the leading edge of the radial; the correction
            // belongs at its centre, which is also where the renderer places it.
            let azimuth = run.start_angle as f64 + run.angle_delta as f64 / 2.0;
            let component = motion.radial_component_kt(azimuth);
            let gate_values = run
                .gate_values
                .iter()
                .map(|&gate| {
                    if gate < FIRST_DATA_GATE {
                        return NO_DATA;
                    }
                    let v_kt = (gate as f32 - offset) as f64 / scale as f64 * MS_TO_KT;
                    let derived = (v_kt + component) * DERIVED_SCALE as f64 + DERIVED_OFFSET as f64;
                    derived
                        .round()
                        .clamp(FIRST_DATA_GATE as f64, u16::MAX as f64) as u16
                })
                .collect();
            RadialRun {
                start_angle: run.start_angle,
                angle_delta: run.angle_delta,
                gate_values,
            }
        })
        .collect();

    // The packet's own scale factor halfword reads 999 for the 1 km product 56
    // and the 0.25 km velocity products alike, so it is replaced rather than
    // carried over — see `ProductDescriptionBlock::range_gate_km`.
    let scale_factor = match pdb.range_gate_km() {
        Some(km) if km > 0.0 => (1.0 / km) as f32,
        _ => source.scale_factor,
    };

    Some(DerivedSrm {
        packet: RadialPacket {
            first_range_bin: source.first_range_bin,
            num_range_bins: source.num_range_bins,
            i_center: source.i_center,
            j_center: source.j_center,
            scale_factor,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials,
        },
        scale: DERIVED_SCALE,
        offset: DERIVED_OFFSET,
        elevation_angle: pdb.elevation_angle(),
        elevation_number: pdb.elevation_number,
        motion,
        motion_provenance: match sample.volume {
            None => MotionProvenance::UserOverride,
            Some(volume) if volume == pdb.volume_key() => MotionProvenance::SameVolume,
            Some(_) => MotionProvenance::PreviousVolume,
        },
    })
}

/// The 14 displayable levels of the RPG's legacy velocity products, in knots.
/// Level `i` covers `[RPG_LEVEL_EDGES[i-2], RPG_LEVEL_EDGES[i-1])`; levels 0
/// and 15 are "no data" and "range folded".
///
/// Transcribed from the data level thresholds of a real `N0S` — halfwords
/// 31–46 decode to `-64, -50, -36, -26, -20, -10, -1, 0, 10, 20, 26, 36, 50,
/// 64` — with the `-1`/`0` pair read as the single boundary at zero the AWIPS
/// colour bar draws.
pub const RPG_LEVEL_EDGES: [f32; 13] = [
    -64.0, -50.0, -36.0, -26.0, -20.0, -10.0, 0.0, 10.0, 20.0, 26.0, 36.0, 50.0, 64.0,
];

/// Quantise storm-relative knots to the RPG's 16-level scale.
///
/// **Only for validating against `N1S`/`N2S`/`N3S`.** The shipped product keeps
/// its 254 levels; chasing the RPG's legacy quantisation would throw away
/// fifteen sixteenths of the value resolution to gain nothing.
pub fn quantize_to_rpg_levels(knots: f32) -> u8 {
    for (level, edge) in (1u8..).zip(RPG_LEVEL_EDGES) {
        if knots < edge {
            return level;
        }
    }
    14
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use nexrad_level3::model::{DataLayer, MessageHeader, ProductDescriptionBlock, SymbologyBlock};

    fn header(code: i16) -> MessageHeader {
        MessageHeader {
            message_code: code,
            date_of_message: 20661,
            time_of_message: 7108,
            message_length: 0,
            source_id: 0,
            destination_id: 0,
            number_of_blocks: 3,
        }
    }

    /// Halfwords 31–33 of a real `MPX_N1G`: -63.5 m/s minimum, 0.5 m/s
    /// increment, 254 levels.
    fn velocity_pdb(
        product_code: i16,
        elevation_tenths: i16,
        elevation_number: u16,
        volume: u32,
    ) -> ProductDescriptionBlock {
        let mut thresholds = [0u16; 16];
        thresholds[0] = -635i16 as u16;
        thresholds[1] = 5;
        thresholds[2] = 254;
        ProductDescriptionBlock {
            block_divider: -1,
            latitude: 44.849,
            longitude: -93.565,
            height: 1000,
            product_code,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 39,
            volume_scan_date: 20661,
            volume_scan_time: volume,
            generation_date: 20661,
            generation_time: volume,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number,
            product_specific_3: elevation_tenths,
            thresholds,
            // Halfword 51 is the BZ2 compression flag on a digital product.
            product_specific_47_53: [-93, 74, 0, 8097, 1, 13, 16382],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        }
    }

    /// Gate 129 is 0 m/s; each step is 0.5 m/s.
    fn gate_for_ms(ms: f32) -> u16 {
        (129.0 + ms / 0.5).round() as u16
    }

    fn message(pdb: ProductDescriptionBlock, radials: Vec<RadialRun>) -> Level3Message {
        let code = pdb.product_code;
        let num_range_bins = radials
            .iter()
            .map(|r| r.gate_values.len())
            .max()
            .unwrap_or(0) as u16;
        Level3Message {
            header: header(code),
            pdb,
            symbology: Some(SymbologyBlock {
                block_id: 1,
                block_length: 0,
                num_layers: 1,
                layers: vec![DataLayer {
                    layer_length: 0,
                    packets: vec![DataPacket::DigitalRadial(RadialPacket {
                        first_range_bin: 0,
                        num_range_bins,
                        i_center: 0,
                        j_center: 0,
                        // What the RPG really writes: 999/1000, for a product
                        // whose gates are 0.25 km.
                        scale_factor: 0.999,
                        is_legacy: false,
                        xdr_data_scale: None,
                        xdr_data_offset: None,
                        radials,
                    })],
                }],
            }),
        }
    }

    /// One radial per listed azimuth, every gate at the same velocity, on the
    /// 1.3° cut.
    fn uniform(product_code: i16, azimuths: &[f32], width: f32, ms: f32) -> Level3Message {
        uniform_at(product_code, 13, 9, azimuths, width, ms)
    }

    /// [`uniform`] at a named cut, for the tests that care which tilt it is.
    fn uniform_at(
        product_code: i16,
        elevation_tenths: i16,
        elevation_number: u16,
        azimuths: &[f32],
        width: f32,
        ms: f32,
    ) -> Level3Message {
        let radials = azimuths
            .iter()
            .map(|&a| RadialRun {
                start_angle: a,
                angle_delta: width,
                gate_values: vec![gate_for_ms(ms); 4],
            })
            .collect();
        message(
            velocity_pdb(product_code, elevation_tenths, elevation_number, 7108),
            radials,
        )
    }

    fn sample(speed_kt: f32, direction_deg: f32, volume: u32) -> StormMotionSample {
        StormMotionSample {
            motion: StormMotion {
                speed_kt,
                direction_deg,
                is_scit_average: true,
            },
            volume: Some((20661, volume)),
        }
    }

    fn knots_at(d: &DerivedSrm, radial: usize, gate: usize) -> f32 {
        (d.packet.radials[radial].gate_values[gate] as f32 - d.offset) / d.scale
    }

    /// The correction is `+speed·cos(direction − azimuth)`, in knots, on top of
    /// a velocity the source stores in metres per second.
    ///
    /// The fixture is a *uniform* 10 m/s field, so every number below is the
    /// storm-motion term plus a constant — a dropped conversion, a dropped
    /// cosine or a flipped sign each move a different one.
    #[test]
    fn the_storm_motion_term_is_added_along_the_radial() {
        // Radials at 0/90/180/270, each 1° wide, so their centres are at 0.5°,
        // 90.5°, … — near enough to read the cardinal cosines off.
        let msg = uniform(154, &[89.5, 179.5, 269.5, 359.5], 1.0, 10.0);
        let d = derive(&msg, &sample(30.0, 90.0, 7108)).expect("154 is a velocity source");
        let base: f32 = 10.0 * (1.0 / 0.514_444);
        assert!((base - 19.438).abs() < 0.01, "10 m/s is 19.4 kt");

        // Azimuth 90 points at the direction the storm comes from: full +30 kt.
        assert!((knots_at(&d, 0, 0) - (base + 30.0)).abs() < 0.5, "az 090");
        // Azimuth 270 is the reciprocal: full -30 kt.
        assert!((knots_at(&d, 2, 0) - (base - 30.0)).abs() < 0.5, "az 270");
        // Orthogonal radials keep the base velocity.
        assert!((knots_at(&d, 1, 0) - base).abs() < 0.5, "az 180");
        assert!((knots_at(&d, 3, 0) - base).abs() < 0.5, "az 000");
    }

    /// The base field must arrive in knots. A missing conversion leaves 10
    /// where 19.4 belongs — a 48% error that no sign or index test sees,
    /// because the storm-motion term is unaffected.
    #[test]
    fn the_source_velocity_is_converted_from_metres_per_second() {
        let msg = uniform(99, &[0.0], 1.0, 25.0);
        let d = derive(&msg, &sample(0.0, 0.0, 7108)).unwrap();
        assert!(
            (knots_at(&d, 0, 0) - 48.60).abs() < 0.3,
            "25 m/s is 48.6 kt"
        );
        assert!(knots_at(&d, 0, 0) > 30.0, "not left in metres per second");
    }

    /// A zero vector must leave the field alone: with no storm motion,
    /// storm-relative velocity *is* base velocity. This is the control that
    /// separates the conversion from the correction.
    #[test]
    fn a_zero_vector_reproduces_the_base_velocity() {
        for ms in [-40.0f32, -12.5, 0.0, 7.5, 33.0] {
            let msg = uniform(154, &[0.0, 137.0, 300.0], 0.5, ms);
            let d = derive(&msg, &sample(0.0, 285.7, 7108)).unwrap();
            for r in 0..3 {
                let want = ms as f64 * MS_TO_KT;
                assert!(
                    (knots_at(&d, r, 0) as f64 - want).abs() < 0.3,
                    "{ms} m/s radial {r}: got {}",
                    knots_at(&d, r, 0),
                );
            }
        }
    }

    /// The correction uses the radial's **centre**, matching where
    /// `render_level3_radial_to_image` places the gate.
    ///
    /// Deliberately exaggerated geometry: at the 0.5° and 1° widths real
    /// products carry, centre and leading edge differ by under 0.02 kt, so no
    /// realistic fixture can tell them apart and one that tried would be
    /// asserting on rounding. A 60°-wide radial makes the *convention*
    /// observable, which is the thing that has to match the renderer.
    #[test]
    fn the_correction_uses_the_centre_of_the_radial_not_its_leading_edge() {
        // Leading edge 60°, width 60° → centre 90°, which is the peak.
        let msg = uniform(154, &[60.0], 60.0, 0.0);
        let d = derive(&msg, &sample(40.0, 90.0, 7108)).unwrap();
        assert!(
            (knots_at(&d, 0, 0) - 40.0).abs() < 0.3,
            "the centre is 090, the peak: got {}",
            knots_at(&d, 0, 0),
        );
        // The leading edge would give cos(30°) = 0.866 → 34.6 kt.
        assert!(
            (knots_at(&d, 0, 0) - 34.64).abs() > 1.0,
            "the correction was taken at the leading edge",
        );

        // And the reverse pairing: a radial whose centre is the zero crossing
        // but whose leading edge is not, so neither case passes by symmetry.
        let msg = uniform(154, &[150.0], 60.0, 0.0);
        let d2 = derive(&msg, &sample(40.0, 90.0, 7108)).unwrap();
        assert!(
            knots_at(&d2, 0, 0).abs() < 0.3,
            "the centre is 180, the zero crossing: got {}",
            knots_at(&d2, 0, 0),
        );
    }

    /// Below-threshold and range-folded gates stay below-threshold. Mapping
    /// them through the arithmetic would paint the storm-motion field itself
    /// across every gate the radar saw nothing in.
    #[test]
    fn gates_with_no_data_stay_empty() {
        let radials = vec![RadialRun {
            start_angle: 90.0,
            angle_delta: 1.0,
            gate_values: vec![0, 1, gate_for_ms(5.0), 0],
        }];
        let msg = message(velocity_pdb(99, 24, 5, 7108), radials);
        let d = derive(&msg, &sample(35.0, 90.0, 7108)).unwrap();
        let g = &d.packet.radials[0].gate_values;
        assert_eq!(g[0], 0, "below threshold");
        assert_eq!(g[1], 0, "range folded");
        assert_eq!(g[3], 0);
        assert!(g[2] > 1, "the gate that had data still does");
    }

    /// The gate spacing must come from the product code. The packet says 999,
    /// which reads as ~1 km — four times too coarse for a 0.25 km product, and
    /// the field would be drawn out to 1200 km.
    #[test]
    fn the_derived_packet_carries_quarter_kilometre_gates() {
        for code in VELOCITY_PRODUCT_CODES {
            let msg = uniform(code, &[0.0], 1.0, 0.0);
            assert!(
                (radial_packet(&msg).unwrap().gate_interval_km() - 1.001).abs() < 0.01,
                "the fixture really does carry the RPG's misleading 999",
            );
            let d = derive(&msg, &sample(0.0, 0.0, 7108)).unwrap();
            assert!(
                (d.packet.gate_interval_km() - 0.25).abs() < 1e-9,
                "product {code} gates are 0.25 km",
            );
        }
    }

    /// Elevation comes from the Product Description Block. `N1G` is 1.3° in
    /// VCP 212, not the 1.5° its mnemonic suggests, and the two adjacent cuts
    /// at one angle are told apart only by elevation number.
    #[test]
    fn elevation_comes_from_the_product_description_block() {
        let msg = message(
            velocity_pdb(154, 13, 9, 7108),
            vec![RadialRun {
                start_angle: 0.0,
                angle_delta: 0.5,
                gate_values: vec![gate_for_ms(0.0)],
            }],
        );
        let d = derive(&msg, &sample(0.0, 0.0, 7108)).unwrap();
        assert_eq!(d.elevation_angle, 1.3, "not the mnemonic's nominal 1.5");
        assert_eq!(d.elevation_number, 9, "the MRLE repeat, not cut 3");
    }

    /// Only dealiased velocity may be derived from. Handed the RPG's own
    /// product 56 — which is already storm-relative — this must decline rather
    /// than apply the correction a second time.
    #[test]
    fn an_already_storm_relative_product_is_not_a_source() {
        for code in [56i16, 55, 94, 134, 135, 163, 176, 177] {
            let msg = uniform(code, &[0.0], 1.0, 10.0);
            assert!(
                derive(&msg, &sample(30.0, 90.0, 7108)).is_none(),
                "product {code}"
            );
        }
        for code in VELOCITY_PRODUCT_CODES {
            assert!(derive(&uniform(code, &[0.0], 1.0, 10.0), &sample(30.0, 90.0, 7108)).is_some());
        }
    }

    /// A vector from another volume still produces a field — the alternative is
    /// no storm-relative velocity at all — but says so.
    #[test]
    fn a_vector_from_another_volume_is_used_and_flagged() {
        let msg = uniform(99, &[0.0], 1.0, 10.0);
        let matched = derive(&msg, &sample(20.0, 270.0, 7108)).unwrap();
        let stale = derive(&msg, &sample(20.0, 270.0, 6952)).unwrap();
        assert_eq!(matched.motion_provenance, MotionProvenance::SameVolume);
        assert_eq!(stale.motion_provenance, MotionProvenance::PreviousVolume);
        // The accuracy signal itself, not just the provenance it reads. Its
        // only other assertion is negative, so a body of `false` — or one
        // inverted to `PreviousVolume`, which flips the validation harness's
        // "vector one volume stale" annotation — would otherwise go unnoticed;
        // that harness is `#[ignore]`d and cannot catch it.
        assert!(matched.motion_volume_matches());
        assert!(!stale.motion_volume_matches());
        // Same arithmetic either way: the flag is provenance, not a switch.
        assert_eq!(
            matched.packet.radials[0].gate_values,
            stale.packet.radials[0].gate_values,
        );
    }

    /// Both halves of the conclusiveness predicate, which cannot be falsified
    /// where it is used: inside the live test the site count is never zero when
    /// the gate count is large, so a mutant on that conjunct would survive by
    /// construction.
    #[test]
    fn a_sample_is_conclusive_only_with_both_sites_and_gates() {
        assert!(sample_is_conclusive(1, MIN_NONZERO_GATES + 1));
        assert!(sample_is_conclusive(9, 500_000));
        // No site asserted on, however many gates were seen elsewhere — the
        // case where every site was quiet or quarantined.
        assert!(!sample_is_conclusive(0, 500_000));
        // Too few gates for a percentage to mean anything.
        assert!(!sample_is_conclusive(3, MIN_NONZERO_GATES));
        assert!(!sample_is_conclusive(3, 0));
        // Absolute, not relative to the constant: a floor expressed only in
        // terms of `MIN_NONZERO_GATES` moves with it, so lowering the constant
        // to 1 would leave every assertion above still passing.
        assert!(
            !sample_is_conclusive(3, 5_000),
            "5,000 gates is not a sample"
        );
        assert!(!sample_is_conclusive(3, 9_999));
        assert!(sample_is_conclusive(3, 200_000));
    }

    /// A non-finite vector must not become a sample at all. NaN makes every
    /// equality test on the sample false, so a change detector comparing two
    /// identical overrides fires on every frame.
    #[test]
    fn a_non_finite_override_is_not_constructible() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                StormMotionSample::user_override(bad, 240.0).is_none(),
                "speed {bad}"
            );
            assert!(
                StormMotionSample::user_override(30.0, bad).is_none(),
                "direction {bad}"
            );
        }
        assert!(
            StormMotionSample::user_override(0.0, 0.0).is_some(),
            "zero is legitimate"
        );
    }

    /// A hand-entered vector belongs to no volume and is never a SCIT average.
    ///
    /// Not the same claim as "a different volume": a sentinel key made the two
    /// indistinguishable, so every override rendered under a `(previous
    /// volume)` annotation that named provenance it never had. It must also
    /// not read as *this* volume — that would claim the RPG had fitted it.
    #[test]
    fn a_user_override_claims_no_provenance() {
        let s = StormMotionSample::user_override(45.0, 210.0).expect("finite");
        assert!(!s.motion.is_scit_average);
        assert_eq!(
            s.volume, None,
            "an override must carry no volume key at all"
        );
        let d = derive(&uniform(154, &[0.0], 0.5, 0.0), &s).unwrap();
        assert_eq!(d.motion_provenance, MotionProvenance::UserOverride);
        assert!(
            !d.motion_volume_matches(),
            "an override agrees with no volume, so it is not this one either"
        );
        assert_eq!(d.motion.speed_kt, 45.0);
        assert_eq!(d.motion.direction_deg, 210.0);
    }

    /// The four request keys, and the reason they are not `N0S`..`N3S`.
    #[test]
    fn every_tilt_product_is_a_dealiased_velocity_key() {
        assert_eq!(SRM_TILT_PRODUCTS, ["N0G", "N1G", "N2U", "N3U"]);
        for dead in ["N1S", "N2S", "N3S"] {
            assert!(
                !SRM_TILT_PRODUCTS.contains(&dead),
                "{dead} has had no data written since 2020 (NWS SCN 22-96)",
            );
        }
        // `N2G`/`N3G` and `N0U`/`N1U` are not in the bucket; asserted by name
        // because swapping one in is the obvious thing to try.
        for absent in ["N2G", "N3G", "N0U", "N1U"] {
            assert!(
                !SRM_TILT_PRODUCTS.contains(&absent),
                "{absent} is not published"
            );
        }
    }

    /// `N0S` is fetched but is not a tilt. Rendering it was the 0.5° pane's
    /// old behaviour and is the thing this module exists to have stopped
    /// doing: 1 km against 0.25 km, 16 display levels against 254, and the
    /// RPG's vector baked in where the user's override belongs.
    #[test]
    fn the_vector_source_is_fetched_but_never_rendered() {
        assert_eq!(STORM_MOTION_PRODUCT, "N0S");
        assert!(
            !SRM_TILT_PRODUCTS.contains(&STORM_MOTION_PRODUCT),
            "{STORM_MOTION_PRODUCT} is back as a tilt: the 0.5° pane would be \
             1 km where the other three are 0.25 km, and would ignore the \
             storm motion override",
        );
        // The fetch list is exactly the vector source followed by the tilts,
        // in order — a tilt dropped from the fetch list never arrives, and a
        // key fetched but absent from the tilt list is never drawn.
        assert_eq!(SRM_FETCH_PRODUCTS[0], STORM_MOTION_PRODUCT);
        assert_eq!(SRM_FETCH_PRODUCTS[1..], SRM_TILT_PRODUCTS);
    }

    /// The lowest tilt derives from the same product 154 as `N1G`, at the same
    /// 0.25 km, and honours a vector the same way. Built from the real `N0G`
    /// PDB halfwords, so a 0.5° special case anywhere in `derive` shows up as
    /// a disagreement with 1.3° rather than as a silently coarser pane.
    #[test]
    fn the_lowest_tilt_derives_exactly_as_the_ones_above_it() {
        // 0.5° cut 1 and 1.3° cut 3, the elevation numbers `TLX` really
        // publishes, over the identical field and vector.
        let low = uniform_at(154, 5, 1, &[89.5], 1.0, 10.0);
        let high = uniform_at(154, 13, 3, &[89.5], 1.0, 10.0);
        let s = sample(30.0, 90.0, 7108);
        let d0 = derive(&low, &s).expect("N0G is product 154");
        let d1 = derive(&high, &s).expect("N1G is product 154");

        assert_eq!(d0.elevation_angle, 0.5);
        assert_eq!(d1.elevation_angle, 1.3);
        assert_eq!(
            d0.packet.radials[0].gate_values,
            d1.packet.radials[0].gate_values
        );
        assert!(
            (d0.packet.gate_interval_km() - 0.25).abs() < 1e-9,
            "0.5° is 0.25 km"
        );
        assert_eq!(d0.scale, d1.scale);
        assert_eq!(d0.offset, d1.offset);
        // 10 m/s is 19.4 kt, and azimuth 090 takes the full +30 kt.
        assert!(
            (knots_at(&d0, 0, 0) - (19.438 + 30.0)).abs() < 0.5,
            "got {}",
            knots_at(&d0, 0, 0)
        );
    }

    /// The vector cannot come off `N0G`: halfword 51 is the BZ2 compression
    /// flag there, exactly as on `N1G`.
    #[test]
    fn the_lowest_tilts_source_carries_no_vector_of_its_own() {
        let low = uniform_at(154, 5, 1, &[0.0], 0.5, 0.0);
        assert!(
            StormMotionSample::from_message(&low).is_none(),
            "N0G reported a vector — halfword 51 is its compression flag, and \
             reading it yields 0.1 kt from 1.3°",
        );
    }

    /// The quantiser's bins, checked against the boundaries a real `N0S`
    /// declares. Each edge is exercised from both sides — a `<=` for a `<`
    /// moves every boundary gate by one level.
    #[test]
    fn the_rpg_level_bins_run_from_below_minus_64_to_above_64() {
        assert_eq!(quantize_to_rpg_levels(-100.0), 1);
        assert_eq!(quantize_to_rpg_levels(-64.1), 1);
        assert_eq!(
            quantize_to_rpg_levels(-64.0),
            2,
            "the edge belongs to the bin above"
        );
        assert_eq!(quantize_to_rpg_levels(-50.1), 2);
        assert_eq!(quantize_to_rpg_levels(-50.0), 3);
        assert_eq!(quantize_to_rpg_levels(-0.1), 7, "just negative");
        assert_eq!(quantize_to_rpg_levels(0.0), 8, "zero reads positive");
        assert_eq!(quantize_to_rpg_levels(9.9), 8);
        assert_eq!(quantize_to_rpg_levels(10.0), 9);
        assert_eq!(quantize_to_rpg_levels(63.9), 13);
        assert_eq!(quantize_to_rpg_levels(64.0), 14);
        assert_eq!(quantize_to_rpg_levels(200.0), 14);
        // Monotone, and every one of the 14 levels reachable.
        let mut seen = std::collections::BTreeSet::new();
        let mut last = 0;
        for i in -2000..2000 {
            let l = quantize_to_rpg_levels(i as f32 / 10.0);
            assert!(l >= last, "not monotone at {}", i as f32 / 10.0);
            last = l;
            seen.insert(l);
        }
        assert_eq!(seen.len(), 14, "reached {seen:?}");
    }

    /// The worst case the settings dialog admits must survive the encoding.
    ///
    /// A clamped gate is still ≥ 2, so saturation does not drop out — it paints
    /// at the clamp, which reads as a real -199 kt inbound rather than as
    /// missing data. The encoding therefore has to cover the input range, and
    /// the input range is set by the widget, not by meteorology.
    #[test]
    fn the_largest_vector_the_ui_permits_cannot_saturate_the_encoding() {
        // The radial centre is 90.0°, so a vector from 270° subtracts its full
        // speed and one from 090° adds it. Gate 2 is the source's floor
        // (-63.5 m/s = -123.4 kt), gate 255 its ceiling (+63.0 m/s = +122.4 kt).
        for (gate, direction, want) in [
            (2u16, 270.0f32, -123.4 - MAX_OVERRIDE_SPEED_KT as f64),
            (255, 90.0, 122.4 + MAX_OVERRIDE_SPEED_KT as f64),
        ] {
            let radials = vec![RadialRun {
                start_angle: 89.5,
                angle_delta: 1.0,
                gate_values: vec![gate],
            }];
            let msg = message(velocity_pdb(154, 13, 9, 7108), radials);
            let s = StormMotionSample::user_override(MAX_OVERRIDE_SPEED_KT, direction)
                .expect("the UI maximum is finite");
            let d = derive(&msg, &s).expect("154 is a velocity source");
            let raw = d.packet.radials[0].gate_values[0];
            assert!(raw > FIRST_DATA_GATE, "gate {gate} clamped to the floor");
            assert!(raw < u16::MAX, "gate {gate} clamped to the ceiling");
            // The value must come back intact, not at the clamp.
            let got = knots_at(&d, 0, 0) as f64;
            assert!(
                (got - want).abs() < 1.0,
                "gate {gate} from {direction}°: got {got:.1} kt, want {want:.1} kt",
            );
        }
    }

    /// The derived scale must not be coarser than the source's, or the
    /// requantisation adds error of its own. 0.5 kt per step against the
    /// source's 0.5 m/s (0.97 kt).
    #[test]
    fn the_derived_scale_is_finer_than_the_source_step() {
        let source_step_kt = 0.5 * MS_TO_KT;
        assert!(1.0 / DERIVED_SCALE as f64 <= source_step_kt);
        // Round-tripping every source level must be exact to well under a step.
        let msg = uniform(154, &[0.0], 0.5, 0.0);
        for gate in 2u16..=255 {
            let want = (gate as f64 - 129.0) * 0.5 * MS_TO_KT;
            let radials = vec![RadialRun {
                start_angle: 0.0,
                angle_delta: 0.5,
                gate_values: vec![gate],
            }];
            let m = message(msg.pdb.clone(), radials);
            let d = derive(&m, &sample(0.0, 0.0, 7108)).unwrap();
            assert!(
                (knots_at(&d, 0, 0) as f64 - want).abs() <= 0.25,
                "gate {gate}: {} vs {want}",
                knots_at(&d, 0, 0),
            );
        }
    }
}
