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
//! thin, and it is twentieth in the harness branch's `SITES`, so a run
//! reaches it only when most of the list is quiet. If one does it will
//! probably fail. Re-survey on a day it carries a vector for longer, then
//! quarantine or clear it; do not leave it undecided indefinitely. Note that
//! "thin evidence" was the reasoning that left `KUEX` out for a round, and
//! `KUEX` then failed a live run.
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

    // `first_range_bin` is an index *denominated in gates*
    // (`RadialPacket::gate_range_km`), so re-spacing the packet above changes
    // what the very same number means: carried over unchanged across a
    // 1 km -> 0.25 km rewrite it would pull the field's start four times
    // closer to the radar. Re-index onto the new spacing so the first gate
    // stays where the source put it. Every live product declares 0 here, so
    // this is inert on the wire today and the test below is what holds it.
    let old_gate_km = source.gate_interval_km();
    let new_gate_km = pdb.range_gate_km().unwrap_or(old_gate_km);
    let first_range_bin = if new_gate_km > 0.0 {
        ((source.first_range_bin as f64 * old_gate_km) / new_gate_km).round() as i16
    } else {
        source.first_range_bin
    };

    Some(DerivedSrm {
        packet: RadialPacket {
            first_range_bin,
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
mod tests;
