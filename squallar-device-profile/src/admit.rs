//! **What one more pane, layer or loop would cost, and whether there is room
//! for it** — the arithmetic behind admission.
//!
//! [`crate::fit`] answers *does the scene on screen fit*, on the loop walk,
//! after the bytes are already committed. This module answers the question
//! that comes first and that nothing in this tree asked until now: *would the
//! scene the user is about to ask for fit*, priced **before** the allocation.
//!
//! # One pricing primitive
//!
//! Every increment here is [`increment`]: `need_terms(after) - need_terms
//! (before)` over two [`Scene`]s. Nothing in this module invents a byte
//! figure or re-spells a cost function — a door's price is a difference of
//! the one model, so a term that moves in `fit` moves in admission on the
//! same land and the two can never come to describe different scenes.
//!
//! That is also what makes **aliasing** free (ruling 8). A second pane on a
//! loop another pane already owns is written into the prospective scene the
//! way `App::loop_demand` writes it — `looping: false`, `volume_grids: 0`,
//! `loop_scans_shared: true` — so the model prices it at its own cost and no
//! more, without this module knowing what an alias is. A door that
//! double-counted a shared loop would refuse panes the machine can easily
//! afford, which is the failure mode that makes an admission system get
//! switched off.
//!
//! # Who prices, and who compares
//!
//! The split is fixed. `squallar-app` is the only crate that can describe a
//! [`Scene`], so it builds the prospective ones and calls [`increment`]; what
//! crosses the Gui seam is a table of finished byte figures plus [`Spare`].
//! The UI **sums** those figures for the act it is about to perform and calls
//! [`verdict`]. It prices nothing.
//!
//! # Spare has one definition
//!
//! [`Spare`] is a carrier, not a second model: the figures in it are the ones
//! `App::compose_budget_readout` already publishes on
//! `squallar_egui::shell_api::PoolReadout::spare_bytes` — the model's spare
//! (`allowance - need`) bounded by what the heap itself says, which on a
//! walled instance is `min(act_line - live, max - byteLength)` and on a
//! native one `allowance - live`. See `squallar_app::app_render::
//! host_spare_bytes`, the one producer.
//!
//! # One memory asks one question
//!
//! On a [`crate::scene::Pools::Unified`] capacity the two axes are one
//! memory, and [`crate::fit::over`] collapses its two tests into a test of
//! the whole need against [`crate::scene::Capacity::joint_allowance`].
//! [`verdict`] collapses the same way, through [`Spare::joint_bytes`]: the
//! two increments are summed and tested once. On the Framework 13 that froze,
//! the picture terms are charged to the host axis alone while the driver
//! places them in memory shared with the compositor — so an admission
//! decision taken per-axis there could not see the bytes at all.

use crate::budget::Budgets;
use crate::constants::MIN_LOOP_FRAMES_PER_PANE;
use crate::fit::{GridBytes, need_terms};
use crate::scene::Scene;

/// **Bytes an act would add**, on each pool.
///
/// Saturating throughout: an increment that overflows a `u64` is one no
/// machine admits, and the saturated value refuses exactly as the true one
/// would.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Increment {
    /// Bytes it adds to the GPU pool.
    pub gpu_bytes: u64,
    /// Bytes it adds to the host pool.
    pub host_bytes: u64,
}

impl Increment {
    /// An act that costs nothing — what every door charges for a state
    /// transition it does not actually make.
    pub const ZERO: Self = Self {
        gpu_bytes: 0,
        host_bytes: 0,
    };

    /// A host-only cost, for the terms that have no GPU axis (a decoded
    /// overlay grid, a tile working set).
    pub const fn host(bytes: u64) -> Self {
        Self {
            gpu_bytes: 0,
            host_bytes: bytes,
        }
    }

    /// Two increments summed — the whole of what "sum the increments" means
    /// on the UI side.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            gpu_bytes: self.gpu_bytes.saturating_add(other.gpu_bytes),
            host_bytes: self.host_bytes.saturating_add(other.host_bytes),
        }
    }

    /// One increment `n` times — a batch of `n` identical units (`n` more
    /// panes, `n` more loop frames).
    #[must_use]
    pub const fn times(self, n: u64) -> Self {
        Self {
            gpu_bytes: self.gpu_bytes.saturating_mul(n),
            host_bytes: self.host_bytes.saturating_mul(n),
        }
    }

    /// Whether this act costs nothing on either pool.
    pub const fn is_zero(self) -> bool {
        self.gpu_bytes == 0 && self.host_bytes == 0
    }

    /// The two axes summed — what a unified capacity's one memory is asked
    /// for.
    pub const fn joint_bytes(self) -> u64 {
        self.gpu_bytes.saturating_add(self.host_bytes)
    }
}

/// **What `after` costs beyond `before`** at `budgets` — the one pricing
/// primitive.
///
/// A difference of [`need_terms`], so every rule the model holds holds here:
/// the max-folded terms (the arrival buffer, the render peak, the loop
/// picture burst) contribute only what they actually raise the maximum by,
/// and a pane written down as an alias contributes nothing for the loop it
/// shares.
///
/// **Saturating at zero, and that is a statement not a guard.** An `after`
/// that costs *less* than `before` is an act that frees bytes; admission has
/// nothing to refuse and returns [`Increment::ZERO`]. What it must never do
/// is hand back a negative that a later sum could use to pay for something
/// else.
pub fn increment(
    before: &Scene,
    after: &Scene,
    budgets: &Budgets,
    grid_bytes: GridBytes,
) -> Increment {
    let before = need_terms(before, budgets, grid_bytes).total();
    let after = need_terms(after, budgets, grid_bytes).total();
    Increment {
        gpu_bytes: after.gpu_bytes.saturating_sub(before.gpu_bytes),
        host_bytes: after.host_bytes.saturating_sub(before.host_bytes),
    }
}

/// **What is left to spend**, per pool, as the App last priced it.
///
/// `None` on a pool is "this session has no figure for it", and nothing is
/// ever over a pool it cannot see — the same reading
/// [`crate::fit::over`] gives a capacity with no host figure. It is not
/// zero, and the difference matters: a native arm no reader has answered for
/// would otherwise refuse everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spare {
    /// The GPU pool's spare.
    pub gpu_bytes: Option<u64>,
    /// The host pool's spare.
    pub host_bytes: Option<u64>,
    /// **One memory under two names.** Where the capacity is
    /// [`crate::scene::Pools::Unified`] the two needs face one allowance, so
    /// the increments are summed and tested against this single figure
    /// instead of against the two above. `None` on a split capacity, which is
    /// every discrete card.
    ///
    /// Carried rather than derived as `gpu + host`: both of those are
    /// saturating differences floored at zero, so summing them over-states
    /// the joint room whenever one axis is already over its share of the
    /// partition. The App computes this one from `joint_allowance` and the
    /// whole need.
    pub joint_bytes: Option<u64>,
}

/// Which pool had no room — what a refusal names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pool {
    /// The card, or the GPU half of a partitioned pool.
    Gpu,
    /// The page heap, or system RAM.
    Host,
    /// One memory serving both, on a unified adapter.
    Joint,
}

/// A refusal, with the arithmetic that produced it — never a bare "no".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// Which pool ran out.
    pub pool: Pool,
    /// What the act asked that pool for.
    pub wanted_bytes: u64,
    /// What the pool had.
    pub spare_bytes: u64,
}

impl Refusal {
    /// Bytes short — what the user would have to free, or the budget raise.
    pub const fn short_bytes(&self) -> u64 {
        self.wanted_bytes.saturating_sub(self.spare_bytes)
    }
}

/// Whether an act fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It fits. The caller proceeds and debits `want` from the spare it holds.
    Admit,
    /// It does not.
    Refuse(Refusal),
}

impl Verdict {
    /// Whether this is an admission.
    pub const fn is_admit(&self) -> bool {
        matches!(self, Self::Admit)
    }

    /// The refusal, where there is one.
    pub const fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Admit => None,
            Self::Refuse(refusal) => Some(*refusal),
        }
    }
}

/// **Does `want` fit `spare`.**
///
/// Two arms, by how many memories the capacity names — the same cut
/// [`crate::fit::over`] makes and for the same reason:
///
/// * a **unified** capacity ([`Spare::joint_bytes`] present) faces one test
///   of the summed increment against the one figure. Charging a picture to
///   both axes would refuse acts that fit; testing each axis against half a
///   partition fences bytes the hardware does not fence.
/// * a **split** capacity faces one test per axis. A pool with no figure is
///   skipped, never treated as empty.
///
/// An act costing nothing is admitted whatever the spare, including on a pool
/// already over: refusing a free act would make a scene that is already too
/// large impossible to *reduce*, since the doors that shed go through the
/// same seam.
pub fn verdict(spare: Spare, want: Increment) -> Verdict {
    if want.is_zero() {
        return Verdict::Admit;
    }
    if let Some(joint) = spare.joint_bytes {
        let wanted = want.joint_bytes();
        return if wanted > joint {
            Verdict::Refuse(Refusal {
                pool: Pool::Joint,
                wanted_bytes: wanted,
                spare_bytes: joint,
            })
        } else {
            Verdict::Admit
        };
    }
    if let Some(gpu) = spare.gpu_bytes
        && want.gpu_bytes > gpu
    {
        return Verdict::Refuse(Refusal {
            pool: Pool::Gpu,
            wanted_bytes: want.gpu_bytes,
            spare_bytes: gpu,
        });
    }
    if let Some(host) = spare.host_bytes
        && want.host_bytes > host
    {
        return Verdict::Refuse(Refusal {
            pool: Pool::Host,
            wanted_bytes: want.host_bytes,
            spare_bytes: host,
        });
    }
    Verdict::Admit
}

/// **How this build converts a lookback to a frame count.**
///
/// The two figures [`Budgets::frames_for_span_of`] reads, carried alone so a
/// door can ask the question for a span the user is *dragging toward* without
/// the whole [`Budgets`] crossing the seam. [`Self::frames`] is that
/// function's body, moved here: `Budgets::frames_for_span_of` delegates to
/// it, so there is one spelling and a UI that counts units is counting them
/// the way the model does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopFrames {
    /// The budget's own span ceiling, in seconds — a pane's lookback is held
    /// to it.
    pub budget_span_secs: usize,
    /// The render budget: frames a pane may hold at once.
    pub render_budget: usize,
}

impl LoopFrames {
    /// The two figures a [`Budgets`] carries.
    pub const fn of(budgets: &Budgets) -> Self {
        Self {
            budget_span_secs: budgets.loop_span_secs,
            render_budget: budgets.loop_render_budget,
        }
    }

    /// Frames of `cadence_secs` apiece it takes to cover `span_secs`, held to
    /// the budget's own span and to the render budget. A loop with no cadence
    /// yet buys the whole render budget, as it always has.
    pub fn frames(&self, span_secs: usize, cadence_secs: Option<u32>) -> usize {
        let Some(cadence) = cadence_secs.filter(|secs| *secs > 0) else {
            return self.render_budget;
        };
        (1 + span_secs.min(self.budget_span_secs) / cadence as usize).clamp(
            MIN_LOOP_FRAMES_PER_PANE,
            self.render_budget.max(MIN_LOOP_FRAMES_PER_PANE),
        )
    }
}

#[path = "admit/tests.rs"]
#[cfg(test)]
mod tests;
