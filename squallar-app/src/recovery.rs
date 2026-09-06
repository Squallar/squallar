//! **How a shed rung comes back.** The counterpart to [`crate::pressure`],
//! which only ever gives memory away.
//!
//! Every host lever this application pulls under pressure used to be
//! one-way. The session's host presumption was lowered from
//! `Pressure::page_heap_used` — a wasm `byteLength`, which only grows — and
//! latched for the process; the tile economies were zeroed by a `bool` that
//! nothing cleared; and the budget ladder's overlay-oversampling rung, the
//! one that decides how much margin a picture is rasterised with beyond the
//! viewport, was shed with them. **So a user who hit one transient spike kept
//! a degraded pan margin until they restarted the tab.** That is the shape
//! this module exists to break: the user's own report, twice, is that "even
//! the smallest of pans causes tiles to be re-rendered and nws alerts
//! redrawn".
//!
//! # The rule, in one sentence each
//!
//! * **Demotion is immediate and unchanged.** [`HostRecovery::squeeze`] takes
//!   a step the instant a page-heap event says to. Nothing here slows the
//!   response to real pressure; the whole design is about the other
//!   direction.
//! * **Promotion needs a margin AND a dwell.** One step comes back only when
//!   the spare left over *after* the step is paid for still covers the step's
//!   own byte cost again plus a refire step
//!   ([`squallar_device_profile::linear_memory::LINEAR_MEMORY_REFIRE_STEP_BYTES`]),
//!   and only when that has held across [`HostRecovery::dwell`] successive
//!   capacity readings.
//! * **A repeat squeeze lengthens the dwell**, doubling to a cap, so a
//!   session that is pressured periodically cannot thrash between rungs.
//!
//! # Why a bare "un-shed when it fits" is not enough
//!
//! It was measured. On an Android emulator leg whose host allowance sat at
//! about the scene's need, a fit with no hysteresis toggled `steps 0` and
//! `steps 1` on consecutive two-second ticks, re-rasterising every picture at
//! 150 % and then 125 % each tick — the user sees the oversampling, so the
//! oscillation is visible as sharpness pumping. **Over-firing is the worse
//! direction here**: a ladder that never promotes is no worse than the
//! session before this module existed, and one that promotes on noise is a
//! new defect at the exact moment a user is looking at the map.
//!
//! # Where the state lives, and where it does not
//!
//! Here, and on `App` as one field. **Not in `squallar_device_profile::fit`,
//! which is pure and pinned pure** (`fit/tests.rs`: the same scene against
//! the same capacity fits the same twice). `fit` is re-run on every loop walk
//! and is a few multiplications a pane; giving it memory would make the
//! budget system's central function answer differently for reasons its
//! arguments do not carry. What this module produces is a
//! `squallar_device_profile::scene::Modulation` — the capacity chain's third
//! clamp term, built for exactly this and until now the identity — so the
//! ladder comes back by `fit` being asked the same question against a larger
//! capacity, with no counter inside it.
//!
//! Nothing here is written to the store. A reopen has a fresh heap, a fresh
//! ladder and no memory of this session's pressure, which is the rule the
//! whole budget system is held to.

use squallar_device_profile::constants::ECONOMY_FRACTION;
use squallar_device_profile::linear_memory::LINEAR_MEMORY_REFIRE_STEP_BYTES;

/// **Successive qualifying capacity readings a promotion needs at the first
/// squeeze**, before the doubling below lengthens it.
///
/// Readings, not frames and not seconds. The reading is the telemetry tick's
/// — the one place the page heap, this instance's live bytes and the host
/// pool are all re-read (`App::report_frame_telemetry`), at
/// `RASTER_TELEMETRY_PERIOD`, 2 s — so four readings is the eight seconds of
/// held margin the Android oscillation could not have produced: it toggled on
/// consecutive ticks.
pub const HOST_RECOVERY_DWELL_READINGS: u32 = 4;

/// How many times the dwell may double for a session that keeps being
/// squeezed: 8x the base at the cap, so a fourth and every later squeeze asks
/// for 32 readings of held margin rather than 4.
///
/// Keyed on squeezes **ever**, not on the level standing now: a session that
/// squeezed, recovered and squeezed again has demonstrated the pattern this
/// cap is for, and reading the level instead would hand it the base dwell
/// every time round.
pub const HOST_RECOVERY_DWELL_MAX_DOUBLINGS: u32 = 3;

/// **How many of the promoted rung's own byte cost must remain spare once
/// the promotion is paid for**, on top of
/// [`LINEAR_MEMORY_REFIRE_STEP_BYTES`]. One: promoting a rung that costs
/// `delta` more bytes requires `delta + 32 MiB` still spare afterwards, so
/// the spare in hand beforehand has to be `2 x delta + 32 MiB`.
///
/// # The cost of this figure, stated
///
/// **A device with between `delta + 32 MiB` and `2 x delta + 32 MiB` of
/// spare never promotes at all.** On the `huge` leg's own pinned figures the
/// 150 %-to-125 % rung is a 179 MB delta, so that dead band is roughly 210 to
/// 390 MB wide, and a session sitting inside it keeps the degraded pan margin
/// for its whole life — exactly the complaint this module answers, on a
/// narrower population than the one it fixes.
///
/// It is deliberately the safe direction rather than the tight one. A device
/// in the band is no worse off than it was before this module existed; a
/// device promoted on a margin that was not really there re-rasterises every
/// picture on the next tick and squeezes again, which is a *new* visible
/// defect. Lowering this to 0 removes the hysteresis entirely and is the
/// Android oscillation; raising it widens the band. The band is pinned by
/// `a_spare_inside_the_dead_band_this_margin_creates_never_promotes` so that
/// nobody has to discover it by reading arithmetic, and
/// [`HostRecovery::churn`] is what would say from the field that the figure
/// is too lax.
pub const HOST_RECOVERY_MARGIN_DELTAS: u64 = 1;

/// **How close behind a promotion a squeeze has to fall to be counted as
/// churn** ([`HostRecovery::churn`]), in readings.
pub const HOST_RECOVERY_CHURN_READINGS: u32 = HOST_RECOVERY_DWELL_READINGS;

/// The most ceiling steps the ladder holds at once. Past it a further squeeze
/// replaces the step in force rather than stacking a new one, so a long
/// session cannot grow this without bound; the ceiling still falls, and a
/// promotion from the cap steps back to a coarser rung than it would have.
pub const HOST_RECOVERY_MAX_STEPS: usize = 16;

/// **The host capacity ceiling this session is holding, and what it would
/// take to release a step of it.**
///
/// A stack of steps: [`Self::squeeze`] pushes one, [`Self::observe`] pops one
/// when the margin has held for the dwell. Each step carries the ceiling in
/// force **after** it, so the top is what `App::capacity_modulation` carries
/// and a pop restores the one below without re-deriving anything. An empty
/// stack is no modulation at all — the capacity the device profile and the
/// session's GPU presumption alone decide.
///
/// **A step's ceiling is `Option`, and the `None` is not an absence of
/// pressure.** A bracket with no host figure — the desktop bracket, which is
/// what a headless native session resolves — has nothing to hold down, and a
/// page-heap event there still squeezes the tile economies: the level goes up
/// carrying whatever ceiling the step below had. Reading the level and the
/// ceiling as one field is what would lose that event.
///
/// **A stack rather than a re-derived function of the reading.** A ceiling
/// re-computed from live bytes on every tick would rise and fall with the
/// heap and have no hysteresis of its own, which is the oscillation; the
/// hysteresis has to live in the promotion rule, and a stack is what lets the
/// rule step back exactly the way it stepped down.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostRecovery {
    /// The steps taken, oldest first; each is the ceiling in force after it,
    /// and the last is the ceiling in force now.
    steps: Vec<Option<u64>>,
    /// Squeezes ever, this session — the dwell's multiplier and an always-on
    /// counter for the telemetry line.
    acts: u32,
    /// Successive qualifying readings since the last squeeze or promotion.
    held: u32,
    /// Promotions ever, this session.
    promotions: u32,
    /// **Promotions this session's own margin got wrong**: a squeeze that
    /// landed within [`HOST_RECOVERY_CHURN_READINGS`] of a promotion.
    churn: u32,
    /// Readings since the last promotion, while one is still recent enough
    /// for a squeeze to count as churn; `None` once the window has passed or
    /// before the first promotion.
    since_promotion: Option<u32>,
}

impl HostRecovery {
    /// A session that has never been squeezed — `Default::default()` in a
    /// `const` context, which the derive cannot give.
    pub const fn untouched() -> Self {
        Self {
            steps: Vec::new(),
            acts: 0,
            held: 0,
            promotions: 0,
            churn: 0,
            since_promotion: None,
        }
    }

    /// The host ceiling in force, or `None` where nothing is shed and where
    /// what was shed had no host figure to hold down.
    pub fn ceiling(&self) -> Option<u64> {
        self.steps.last().copied().flatten()
    }

    /// The ceiling one promotion would leave in force — the step under the
    /// top, or `None` where the top is the only one.
    pub fn ceiling_after_promotion(&self) -> Option<u64> {
        let under = self.steps.len().checked_sub(2)?;
        self.steps.get(under).copied().flatten()
    }

    /// Whether the host levers are pulled: the tile economies are at nothing.
    /// A LEVEL that steps back down, not the one-way `bool` it replaces.
    pub fn is_squeezed(&self) -> bool {
        !self.steps.is_empty()
    }

    /// How many steps are held.
    pub fn level(&self) -> u32 {
        self.steps.len() as u32
    }

    /// Squeezes ever, this session.
    pub fn acts(&self) -> u32 {
        self.acts
    }

    /// Promotions ever, this session.
    pub fn promotions(&self) -> u32 {
        self.promotions
    }

    /// **Promotions this margin got wrong** — squeezes that landed within
    /// [`HOST_RECOVERY_CHURN_READINGS`] readings of a promotion.
    ///
    /// An always-on counter and the only thing that can ever say from the
    /// field that [`HOST_RECOVERY_MARGIN_DELTAS`] is too lax. Every other
    /// trace of a promotion is a `log::info!` in a console ring that turns
    /// over in seconds; this rides the `budget state:` line, which is re-said
    /// every telemetry period, so the answer is in the last tick of any leg
    /// however short the capture window.
    pub fn churn(&self) -> u32 {
        self.churn
    }

    /// Successive qualifying readings banked toward the next promotion.
    pub fn held(&self) -> u32 {
        self.held
    }

    /// **Qualifying readings this session's next promotion needs**: the base
    /// dwell, doubled once per squeeze after the first and capped at
    /// [`HOST_RECOVERY_DWELL_MAX_DOUBLINGS`] doublings. Zero squeezes is the
    /// base too — there is nothing to promote then, and answering the base is
    /// what makes the figure printable before an event.
    pub fn dwell(&self) -> u32 {
        let doublings = self
            .acts
            .saturating_sub(1)
            .min(HOST_RECOVERY_DWELL_MAX_DOUBLINGS);
        HOST_RECOVERY_DWELL_READINGS << doublings
    }

    /// **Take one step down, now.** `host` is the host figure in force — or
    /// `None` on a bracket that carries none — and `observed` what the page
    /// heap was holding when the event fired; the new ceiling is one economy
    /// fraction under the lower of the two, which is the arithmetic this
    /// replaces, unchanged.
    ///
    /// **A `None` host still takes a step.** There is no ceiling to write,
    /// but the event happened and the tile economies go with it; the step
    /// carries whatever ceiling stood below it, so a promotion later gives
    /// the economies back the same way it would give a rung back.
    ///
    /// The dwell is reset: a session under pressure has banked nothing toward
    /// getting the rung back. A squeeze inside
    /// [`HOST_RECOVERY_CHURN_READINGS`] of a promotion is counted as
    /// [`Self::churn`].
    ///
    /// Returns the ceiling now in force.
    pub fn squeeze(&mut self, host: Option<u64>, observed: u64) -> Option<u64> {
        let stepped = host.map(|host| host.min(observed) / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0);
        // Never above the step already in force: a later event may read a
        // higher mark than an earlier one, and a ceiling that rose on an
        // event would be pressure buying capacity.
        let ceiling = match (self.ceiling(), stepped) {
            (Some(held), Some(stepped)) => Some(held.min(stepped)),
            (held, None) => held,
            (None, stepped) => stepped,
        };
        if self.steps.len() >= HOST_RECOVERY_MAX_STEPS {
            // The cap: replace the step in force rather than stack another.
            if let Some(top) = self.steps.last_mut() {
                *top = ceiling;
            }
        } else {
            self.steps.push(ceiling);
        }
        if self.since_promotion.is_some() {
            self.churn = self.churn.saturating_add(1);
        }
        self.since_promotion = None;
        self.held = 0;
        self.acts = self.acts.saturating_add(1);
        ceiling
    }

    /// **One capacity reading.** `qualified` is whether the margin
    /// ([`promotion_qualifies`]) held on it.
    ///
    /// A qualifying reading banks one toward the dwell; a reading that does
    /// not qualify spends the whole bank, so the margin has to hold
    /// *successively* and a brief dip below the act line that recovers for
    /// one tick promotes nothing. At the dwell exactly ONE step comes back
    /// and the bank resets, so a session ten steps down climbs back one dwell
    /// at a time rather than in a jump.
    ///
    /// Returns whether a step was released. Called only with readings the
    /// caller has confirmed are fresh observations — see
    /// `crate::platform::LinearMemory::page_live_bytes` for what makes one.
    pub fn observe(&mut self, qualified: bool) -> bool {
        self.since_promotion = self
            .since_promotion
            .map(|n| n.saturating_add(1))
            .filter(|n| *n < HOST_RECOVERY_CHURN_READINGS);
        if !self.is_squeezed() {
            self.held = 0;
            return false;
        }
        if !qualified {
            self.held = 0;
            return false;
        }
        self.held = self.held.saturating_add(1);
        if self.held < self.dwell() {
            return false;
        }
        self.steps.pop();
        self.held = 0;
        self.promotions = self.promotions.saturating_add(1);
        self.since_promotion = Some(0);
        true
    }
}

/// **Whether one reading carries the margin a promotion needs.**
///
/// Every figure is the promoted rung's, not the rung in force: `need_after`
/// is what the scene would cost at the budgets a released ceiling fits,
/// `allowance_after` what that released capacity allows on the host, and
/// `room_after` the heap's own room priced against the batch the promoted
/// rung would rasterise. The question is therefore literally "would this
/// still be comfortable once the step is taken", not "is it comfortable
/// now".
///
/// `room_after` is `None` where nothing can observe the heap coming back —
/// no counting allocator, or no reading — and then the answer is `false`
/// whatever the model says. A model figure alone is the arithmetic that
/// admits one more layer into a trap (`crate::app_render::host_spare_bytes`
/// carries the same argument for the readout); recovery has to be *observed*
/// or it has not happened.
pub fn promotion_qualifies(
    need_now: u64,
    need_after: u64,
    allowance_after: u64,
    room_after: Option<u64>,
) -> bool {
    let Some(room_after) = room_after else {
        return false;
    };
    let delta = need_after.saturating_sub(need_now);
    let model_spare = allowance_after.saturating_sub(need_after);
    let spare_after = model_spare.min(room_after);
    let wanted = delta
        .saturating_mul(HOST_RECOVERY_MARGIN_DELTAS)
        .saturating_add(LINEAR_MEMORY_REFIRE_STEP_BYTES);
    spare_after >= wanted
}

/// **The falling half of the room the margin is judged against**, for a page
/// with a declared wall: where the watermark's action line falls once the
/// promoted rung's batch is in front of it, less what this instance's
/// allocator is holding right now.
///
/// # Two decisions in this one expression, both of them load-bearing
///
/// **The wall term is not here, and its absence is the fix.**
/// `host_spare_bytes` bounds the readout by `max - byteLength` as well, which
/// is right for a readout and fatal for a governor: `byteLength` only grows,
/// so a page that once touched 1011 of 1024 MiB has thirteen MiB of wall room
/// for the rest of its life and could never promote however much it freed.
/// That is the ratchet this module exists to break, in miniature. The term
/// that is here — the act line less **live bytes** — is the one that can
/// fall.
///
/// **`act_line` is priced at the PROMOTED batch**, so the test is exactly
/// "would the watermark be in `Act` the instant after promoting?". A bigger
/// rung rasterises a bigger batch, which lowers the action line, which
/// shrinks this room: the same arithmetic that decides demotion decides
/// whether the promotion would immediately undo itself, and the two cannot
/// disagree about a scene.
///
/// # An unverified premise, labelled
///
/// This consumes live bytes on the assumption that **live bytes fall while
/// `byteLength` holds** — that a wasm page which frees pictures returns them
/// to the allocator's free lists rather than merely reserving more. The
/// measurement that would show that on a real browser (campaign item V1) has
/// **never been executed, on either engine**. It is safe by construction
/// natively, where there is no `byteLength` at all and the allocator figure
/// is the only one there is; on web it is an assumption, and the honest
/// consequence if it is wrong is that this room never opens and the ladder
/// simply never promotes — the behaviour of the session before this module,
/// not a worse one. The direction of the risk is what makes shipping it
/// before the measurement defensible; the claim is not that it was measured.
pub fn walled_room_after(max: u64, headroom_after: u64, live_bytes: u64) -> u64 {
    squallar_device_profile::linear_memory::act_line(max, headroom_after).saturating_sub(live_bytes)
}

#[cfg(test)]
mod tests;
