//! **The door: what the scene the user is asking for would cost, summed and
//! compared before its bytes are committed.**
//!
//! Every scene-changing door in this crate returned `()` until WO-G. The
//! application could open a seventh pane, show a fourteenth whole-picture
//! layer or arm a two-hour loop on a page heap with nothing left, and nothing
//! anywhere could refuse: `fit` runs on the loop walk, *after* the allocation,
//! and sheds a rung for the next frame. That is the right instrument for a
//! scene that is already resident and the wrong one for a scene that is about
//! to be.
//!
//! # Who prices, and who sums
//!
//! `squallar-app` prices. It is the only crate that can describe a
//! [`squallar_device_profile::scene::Scene`] — the loop aliasing, the tile
//! ledger, the planned picture sizes — so it builds the prospective scenes
//! and calls [`squallar_device_profile::admit::increment`] on them. What
//! crosses the seam is [`AdmissionCosts`]: finished byte figures per pane and
//! per layer, plus the spare.
//!
//! This module **sums**. A door names what it is about to do, adds the table's
//! figures for exactly the state transitions it will actually make, and asks
//! [`squallar_device_profile::admit::verdict`]. It prices nothing, and there is
//! no byte figure in this file.
//!
//! # Charge for the transition, not for the call
//!
//! The doors nest. `Gui::set_pane_count` calls `initialize_pane_enabled`;
//! `catalog_apply_overlay` calls `PaneState::add_layer` and then
//! `write_pane_overlay`; `write_pane_overlay`'s own `set_layer_enabled` mints
//! a slot when the layer is not held. One click reaches three doors, and a
//! door that charged per call would refuse a scene the machine can hold three
//! times over.
//!
//! So **every door charges only for a state transition it actually makes**: a
//! layer already shown on the pane is free, a pane count that does not grow is
//! free, a loop already armed on that window is free. `catalog_apply_overlay`
//! then pays once — at `add_layer`, which is the call that flips the layer on
//! — and the `write_pane_overlay` behind it finds nothing left to change and
//! asks for nothing. This is the same rule that keeps `grown_pane` from being
//! its own door.
//!
//! # A batch is one increment
//!
//! A preset is a set of layers over a set of panes, and a per-toggle refusal
//! would leave half of it applied — a state the user never asked for and
//! cannot name. So the batch doors ([`AdmissionLedger::begin_batch`]) sum the whole
//! act first, ask once, and then run with the inner doors charging nothing:
//! the transitions the batch priced are exactly the ones they would make.
//! The layer-link fan-out and the span slider are batches for the same
//! reason — the slider writes every pane at once.
//!
//! # The spare is spent as it is admitted
//!
//! The table is composed on the App's telemetry tick, not per frame: every
//! figure in it is a *level*, and a level does not need re-taking 120 times a
//! second. But a user can click six layers inside one tick, and six acts
//! priced against one unchanged spare would all be admitted. So the ledger
//! **debits what it admits** ([`AdmissionLedger::spare`]) and clears the debit
//! when a fresher table arrives. What a door is compared against is the
//! published spare less everything admitted since it was published.
//!
//! # Refusing, and being seen to
//!
//! [`AdmissionLedger::ask`] computes a verdict and records it;
//! [`AdmissionLedger::enforce`] acts on one. **A silent refusal is a worse
//! defect than the allocation it prevents**: the user asked for a layer, the
//! layer did not appear, and nothing on the glass says why or what to change.
//! So every refusal raises an [`AdmissionNotice`] that names something the
//! reader can actually move - a memory share by the label the Settings screen
//! gives it while that share is still short of its stop, and the part of the
//! scene that would close the gap once it is not - and the pane paints it.
//!
//! # A refusal that cannot be revisited is worse than the allocation it
//! prevented
//!
//! The twin of the rule above, and it is a **requirement of a door**, not a
//! nicety. Every door here refuses by returning, and nothing anywhere retains
//! the wish or asks again: `App::handle_enable_loop` returns before the pane's
//! parked wish is consumed but `looping_panes` is drained once in `App::new`
//! and only `TransportNotReady` re-queues, so one refused loop at startup is a
//! session with no loop at all — and the wish is persisted, so it is *every*
//! session. `PaneState::add_layer` and `Gui::write_pane_overlay` have the same
//! shape on a layer. The only state a refusal writes is a six-second notice.
//!
//! So a refusal here is **permanent for the session**, and that turns a
//! transient shortage into a lasting loss of function. Even a *correct*
//! refusal has to be re-asked when the thing that made the scene tight goes
//! away — a layer hidden, a pane closed, a loop stopped, a rung shed, the host
//! governor recovering. Until a door retains what it refused and asks again on
//! the telemetry tick that republishes the spare, refusing costs more than it
//! saves, which is the other half of why [`ENFORCING`] is off on the arm every
//! user is on.
//!
//! # Refusing is per-arm, measuring is not
//!
//! [`ENFORCING`] selects whether a refusal turns the act away. It is `false`
//! on wasm32 as of 2026-09-06: enforcing there cost a loaded layer, every
//! loop, and honesty in the notice, on lines that said the ladder had not
//! been asked to shed. The verdict is still taken, still logged and still
//! counted on [`Totals::would_refuse`] there — what a door is for is knowing,
//! and only the turning away is off.
//!
//! # Restore is never a refusal
//!
//! `Gui::load_ui_config` runs inside [`AdmissionLedger::begin_exempt`], and
//! that is not an oversight to be closed later. A restore the doors narrowed
//! would be **persisted by the next autosave within seconds**: the user would
//! open the app, silently lose panes they never touched, and have no way back
//! to the arrangement they saved. The scene a restore brings back is made
//! survivable by the governor shedding around it, not by refusing to bring it
//! back.
//!
//! **The killer scene arrives entirely through that exempt path** -
//! `load_ui_config` writes each pane's `loop_arm_pending`, `looping_panes`
//! collects them and `hydrate_parked_panes` drains them into
//! `App::handle_enable_loop` - so restoring into a scene that traps is
//! reachable and self-reinstating. `handle_enable_loop` is on the path
//! **after** the restore, and it is a door: a loop armed by a restore is
//! refusable there even though the pane that carries it is not. The pane's
//! own wish survives the refusal, so the config still round-trips and the loop
//! arms on a session with room for it.

use squallar_device_profile::admit::{
    Increment, LoopFrames, Pool, Refusal, Spare, Verdict, verdict,
};
use squallar_source::id::LayerId;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// **What the App priced, for the doors to sum against.**
///
/// Composed by `App::compose_admission_costs` on the same tick as
/// [`crate::shell_api::BudgetReadout`] and off the same scene, so no walk is
/// added to the frame path. Every field is bytes, already priced; there is
/// nothing here to derive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdmissionCosts {
    /// **Which composition this is.** Bumped once per rebuild and never
    /// otherwise, so the ledger knows when to clear what it has spent — the
    /// same contract [`crate::shell_api::BudgetReadout::generation`] has. `0`
    /// is the table a fresh application carries before its first
    /// composition, and a `0` table admits everything, because nothing has
    /// priced a scene yet.
    pub generation: u64,
    /// What is left to spend, per pool, as the App last priced it.
    pub spare: Spare,
    /// One entry per visible pane, in pane order.
    pub panes: Vec<PaneAdmission>,
    /// **What one more pane costs before any layer is shown on it.** Priced
    /// from the widest pane on screen with its overlays and its loop taken
    /// off, and with its site's decoded volumes already counted — which is
    /// what a pane `Gui::set_pane_count` seeds really is, since it is born
    /// from the active pane's site and scan info. The layers
    /// `initialize_pane_enabled` then default-enables on it are that door's
    /// charge, not this one's.
    pub new_pane: Increment,
    /// **Per gridded overlay layer: what its decoded source costs the host,
    /// and whether some pane already shows it.** A layer's grid cache is one
    /// instance for the whole application, so the second pane to show it owes
    /// nothing — the entry reads `resident` and a door adds zero for it.
    pub layer_grids: Vec<LayerGrid>,
    /// How this build converts a lookback to a frame count, so a span door
    /// can count the units it is adding without re-spelling the model's
    /// arithmetic.
    pub frames: LoopFrames,
    /// **The two memory shares the user owns**, `(gpu, host)`, as percentages
    /// - `squallar_device_profile::scene::PoolPercents` verbatim.
    ///
    /// Carried so a refusal can name the setting that produced it. A wall the
    /// reader cannot act on is a defect in the notice, not a caption to word
    /// better, and "not enough memory" is exactly that wall: these two
    /// numbers are what the user can move.
    pub requested_percent: (u8, u8),
}

/// One gridded overlay layer's admission cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerGrid {
    /// The layer.
    pub id: LayerId,
    /// The host bytes its handler's grid cache asks for.
    pub grid_bytes: u64,
    /// Whether a pane already shows it, so the grid is already paid for.
    pub resident: bool,
}

/// One pane's admission costs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneAdmission {
    /// **Showing one more whole-picture overlay layer on this pane** — the
    /// raster the dispatch holds, the same batch again in the renderer's
    /// upload queue, and whatever the arrival buffer's maximum rises by. The
    /// layer's own decoded grid is not in it: that is scene-level and counted
    /// once, on [`AdmissionCosts::layer_grids`].
    pub show_layer: Increment,
    /// **Arming this pane's loop at the span in force.** Zero where the pane
    /// would be an alias of a loop another pane already owns — the App writes
    /// the prospective scene the way `App::loop_demand` writes an alias, so
    /// ruling 8 is honoured in the price rather than by a rule here.
    pub arm_loop: Increment,
    /// **One more frame of this pane's loop.** What a span change is summed
    /// from: the frame delta times this. Zero for a pane whose loop is an
    /// alias.
    pub loop_frame: Increment,
    /// Frames its loop holds at the span in force — the baseline a span
    /// change is a delta from.
    pub loop_frames_now: usize,
    /// Its transport's cadence, for converting a span to a frame count.
    pub cadence_secs: Option<u32>,
    /// Whether it is looping now. A pane already looping pays a span delta,
    /// never [`Self::arm_loop`] again.
    pub looping: bool,
}

/// **What a door is asking for**, for the log line and the notice.
///
/// Named acts rather than a formatted string: the ledger raises these on a
/// path a user gesture runs through, and building a `String` per verdict would
/// put an allocation on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Growing the layout by `panes` panes.
    Panes { added: usize },
    /// Showing one layer on one pane.
    ShowLayer,
    /// Default-enabling the layers a new or restored pane has no slot for.
    DefaultLayers,
    /// The layer-link fan-out adopting one stack onto its group.
    AdoptLayers,
    /// A preset: pane count and layer set together.
    Preset,
    /// Arming a loop on one pane.
    ArmLoop,
    /// The lookback slider, which writes every pane.
    LoopSpan,
}

impl Act {
    /// The act, as the refusal notice names it.
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Panes { .. } => "another pane",
            Self::ShowLayer => "this layer",
            Self::DefaultLayers => "this pane's layers",
            Self::AdoptLayers => "the linked panes' layers",
            Self::Preset => "this preset",
            Self::ArmLoop => "this loop",
            Self::LoopSpan => "a longer lookback",
        }
    }
}

/// **Whether a refusal turns the act away, or is only counted** — the one
/// per-arm value in this module, and a selected value rather than a fork in
/// [`AdmissionLedger::decide`]'s body.
///
/// `true` on native, where the doors exist for a real measured failure: an
/// integrated adapter placing its pictures in memory shared with the
/// compositor, on a machine that hard-froze rather than reported a wall.
///
/// `false` on wasm32, from 2026-09-06. Enforcing there took three user-visible
/// functions with it inside hours of landing — a loaded whole-picture layer
/// leaving the glass, no loop arming at all, and a refusal notice on a phone
/// pointing at a slider already at its stop — on a `budget state:` line
/// reading `rung 0, steps 0` with spare on both pools, which is a scene the
/// ladder had not been asked to shed for and a door should never have refused.
/// **A door that eats loaded content is worse than no door.** The verdict is
/// still computed, still logged and still counted on `would refuse`, so the
/// instrument that made all three visible goes on running; only the turning
/// away is off, and it comes back when the door and the readout are shown to
/// be describing the same scene.
#[cfg(target_arch = "wasm32")]
const ENFORCING: bool = false;
#[cfg(not(target_arch = "wasm32"))]
const ENFORCING: bool = true;

/// **Which arm this build selected, asserted at compile time.**
///
/// No test in this workspace runs a wasm build, so the value of [`ENFORCING`]
/// on the arm that matters most is otherwise carried by reading two `cfg`
/// attributes and trusting them. This is checked by the same
/// `cargo check --target wasm32-unknown-unknown` row CI already runs, and it
/// is one expression rather than a per-arm pair, so it cannot itself be
/// selected wrong.
const _: () = assert!(ENFORCING != cfg!(target_arch = "wasm32"));

/// **The always-on admission counters**, in the shape
/// [`crate::frame_need`] and [`crate::overlay_cache::ledger`] already use:
/// product telemetry, no feature gate, one relaxed `fetch_add` per verdict.
///
/// The App reads them through [`totals`] and prints them on `budget state:`.
/// They live in a `static` rather than on the `Gui` because the App may not
/// grow another reach into the UI layer to read them — that coupling's ceiling
/// is permanent and sits on its measured value.
static ASKED: AtomicU32 = AtomicU32::new(0);
static ADMITTED: AtomicU32 = AtomicU32::new(0);
static WOULD_REFUSE: AtomicU32 = AtomicU32::new(0);
static REFUSED: AtomicU32 = AtomicU32::new(0);

/// What the doors have decided since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Verdicts taken — every door that priced a transition it was about to
    /// make. A door that charged zero is not counted: it asked nothing.
    pub asked: u32,
    /// Verdicts that admitted.
    pub admitted: u32,
    /// **Verdicts that came back a refusal**, whether or not the arm acted on
    /// one. The measurement, and it runs on every arm.
    pub would_refuse: u32,
    /// **Acts actually turned away** — the subset of [`Self::would_refuse`]
    /// that [`ENFORCING`] let through to the caller as a `false`.
    ///
    /// The two are the same number on native and the pair is `n / 0` on
    /// wasm32, where the doors are advisory. **Keep them apart**: the gap
    /// between them is what a refusal costs the user, and collapsing them
    /// into one field is how an advisory arm stops being visible.
    pub refused: u32,
}

/// The counters, as running totals from boot.
pub fn totals() -> Totals {
    Totals {
        asked: ASKED.load(Relaxed),
        admitted: ADMITTED.load(Relaxed),
        would_refuse: WOULD_REFUSE.load(Relaxed),
        refused: REFUSED.load(Relaxed),
    }
}

impl Totals {
    /// Two readings' difference, so a caller can window a running total.
    #[must_use]
    pub const fn since(self, earlier: Self) -> Self {
        Self {
            asked: self.asked.saturating_sub(earlier.asked),
            admitted: self.admitted.saturating_sub(earlier.admitted),
            would_refuse: self.would_refuse.saturating_sub(earlier.would_refuse),
            refused: self.refused.saturating_sub(earlier.refused),
        }
    }
}

/// **What a refusal has to say**, held until the pane that can paint it runs.
///
/// A silent refusal is a worse defect than the allocation it prevents: the
/// user asked for a layer, the layer did not appear, and nothing on the glass
/// says why or what to change. This carries the act, the pool and the setting
/// whose percentage produced the wall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionNotice {
    /// The sentence the pane paints.
    pub text: String,
    /// When it was raised, so a stale one can be aged out.
    pub raised_at: web_time::Instant,
}

/// How long a refusal notice stays on the glass.
pub const NOTICE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(6);

/// **The per-application ledger**: the table the App published, what has been
/// admitted against it since, and the notice a refusal left.
#[derive(Clone, Debug, Default)]
pub struct AdmissionLedger {
    costs: AdmissionCosts,
    /// **Increments admitted since [`AdmissionCosts::generation`] last moved.**
    /// Debited from the published spare, so six acts inside one telemetry tick
    /// are compared against six shrinking spares rather than one standing
    /// figure.
    spent: Increment,
    /// Depth of an open batch. While non-zero the inner doors charge nothing:
    /// the batch asked for the whole and the transitions it priced are the
    /// ones they will make.
    batch: u32,
    /// The last refusal, for the glass.
    notice: Option<AdmissionNotice>,
    /// **This ledger's own verdict counts.** The `static`s above are the
    /// process-wide figures the App prints; these are per-application, which
    /// is what a test can assert on without racing every other test in the
    /// binary. Both move on the same verdict, so they cannot disagree about
    /// what happened — only about the denominator.
    counts: Totals,
}

impl AdmissionLedger {
    /// Take a freshly published table. Clears what has been spent when the
    /// generation moved, since the new spare already accounts for it.
    pub fn adopt(&mut self, costs: &AdmissionCosts) {
        if costs.generation == self.costs.generation {
            return;
        }
        self.costs = costs.clone();
        self.spent = Increment::ZERO;
    }

    /// The table in force.
    pub fn costs(&self) -> &AdmissionCosts {
        &self.costs
    }

    /// One pane's costs, or the zero row for an index no pane occupies — a
    /// door addressing a pane the table has not seen asks for nothing rather
    /// than refusing on a figure that does not exist.
    pub fn pane(&self, idx: usize) -> PaneAdmission {
        self.costs.panes.get(idx).copied().unwrap_or_default()
    }

    /// What showing `id` costs on top of the pane's own picture: the layer's
    /// decoded grid, where no pane already holds it.
    pub fn layer_grid(&self, id: &LayerId) -> Increment {
        self.costs
            .layer_grids
            .iter()
            .find(|layer| layer.id == *id)
            .filter(|layer| !layer.resident)
            .map_or(Increment::ZERO, |layer| Increment::host(layer.grid_bytes))
    }

    /// **The spare a door is compared against**: what the App published, less
    /// everything admitted since it published it.
    pub fn spare(&self) -> Spare {
        let debit =
            |published: Option<u64>, spent: u64| published.map(|bytes| bytes.saturating_sub(spent));
        Spare {
            gpu_bytes: debit(self.costs.spare.gpu_bytes, self.spent.gpu_bytes),
            host_bytes: debit(self.costs.spare.host_bytes, self.spent.host_bytes),
            joint_bytes: debit(self.costs.spare.joint_bytes, self.spent.joint_bytes()),
        }
    }

    /// Whether a batch is open, so an inner door knows to charge nothing.
    pub fn in_batch(&self) -> bool {
        self.batch > 0
    }

    /// Open a batch. Every door reached until [`Self::end_batch`] charges
    /// nothing.
    pub fn begin_batch(&mut self) {
        self.batch = self.batch.saturating_add(1);
    }

    /// Close a batch.
    pub fn end_batch(&mut self) {
        self.batch = self.batch.saturating_sub(1);
    }

    /// **Ask, and record.** Prices nothing: `want` is already summed from the
    /// table.
    ///
    /// Returns [`Verdict::Admit`] for a free act, for an act inside an open
    /// batch, and for anything asked before the App has published a table.
    /// On an admission the increment is debited, so the next door in the same
    /// tick sees the smaller spare.
    ///
    /// **The measurement, on every arm.** A refusing verdict counted here is
    /// counted whether or not [`ENFORCING`] then turns the act away, so the
    /// advisory arm reports exactly what the enforcing one would have done.
    pub fn ask(&mut self, act: Act, want: Increment) -> Verdict {
        if want.is_zero() || self.in_batch() || self.costs.generation == 0 {
            return Verdict::Admit;
        }
        ASKED.fetch_add(1, Relaxed);
        self.counts.asked = self.counts.asked.saturating_add(1);
        let v = verdict(self.spare(), want);
        match v {
            Verdict::Admit => {
                ADMITTED.fetch_add(1, Relaxed);
                self.counts.admitted = self.counts.admitted.saturating_add(1);
                self.spent = self.spent.plus(want);
            }
            Verdict::Refuse(refusal) => {
                WOULD_REFUSE.fetch_add(1, Relaxed);
                self.counts.would_refuse = self.counts.would_refuse.saturating_add(1);
                log::warn!(
                    "admission: would refuse {} - {} asks {} MiB of the {:?} pool, \
                     {} MiB spare, short {} MiB",
                    act.noun(),
                    act_word(act),
                    refusal.wanted_bytes / (1024 * 1024),
                    refusal.pool,
                    refusal.spare_bytes / (1024 * 1024),
                    refusal.short_bytes().div_ceil(1024 * 1024),
                );
            }
        }
        v
    }

    /// **This ledger's own verdict counts**, from its first ask. The figure
    /// a test asserts on: the process-wide [`totals`] are shared by every
    /// test in the binary and by every application in the process.
    pub fn counts(&self) -> Totals {
        self.counts
    }

    /// **Ask, and act on the answer** - the enforcing door.
    ///
    /// [`Self::ask`]'s verdict, with two things added: a refusal is counted as
    /// a refusal rather than as a would-refuse, and it raises the notice the
    /// pane paints. Returns whether the caller may proceed, so a door reads
    /// `if !admission.enforce(..) { return; }`.
    ///
    /// Everything [`Self::ask`] admits, this admits: a free act, an act inside
    /// an open batch or exemption, and anything asked before the App has
    /// priced a scene. **An application that has priced nothing refuses
    /// nothing** - refusing on an absent figure is how an admission system
    /// turns into a wall at startup, which is the moment the config restore is
    /// putting the user's panes back.
    /// **On an arm where [`ENFORCING`] is false the refusal is counted and
    /// the act proceeds** - see that constant for which arm and why.
    pub fn enforce(&mut self, act: Act, want: Increment) -> bool {
        self.decide(act, want, ENFORCING)
    }

    /// [`Self::enforce`]'s body, with the arm's policy passed in rather than
    /// read from the `const`.
    ///
    /// Both arms are then reachable from one test binary. A `cfg` in the body
    /// would leave whichever arm this build did not compile with no test at
    /// all, and the advisory arm is the wasm one — the arm no test in this
    /// workspace executes.
    fn decide(&mut self, act: Act, want: Increment, enforcing: bool) -> bool {
        match self.ask(act, want) {
            Verdict::Admit => true,
            Verdict::Refuse(refusal) => {
                if !enforcing {
                    return true;
                }
                REFUSED.fetch_add(1, Relaxed);
                self.counts.refused = self.counts.refused.saturating_add(1);
                let text = refusal_text(act, refusal, self.costs.requested_percent);
                self.raise_notice(text, web_time::Instant::now());
                false
            }
        }
    }

    /// **Open an exemption**: every door reached until [`Self::end_exempt`]
    /// admits whatever it is asked.
    ///
    /// Spelled apart from [`Self::begin_batch`] though it moves the same
    /// counter, because the **reason** is different and the reason is what a
    /// reader at the call site needs. A batch is "this act was already priced
    /// whole". An exemption is "**restore is never a refusal**": a restore the
    /// doors narrowed is persisted by the next autosave, and the user loses
    /// panes without having acted.
    pub fn begin_exempt(&mut self) {
        self.batch = self.batch.saturating_add(1);
    }

    /// Close an exemption. See [`Self::begin_exempt`].
    pub fn end_exempt(&mut self) {
        self.batch = self.batch.saturating_sub(1);
    }

    /// **Take a refusal raised on the App's side of the seam.**
    ///
    /// The loop door lives in `squallar-app` and keeps its own ledger, so its
    /// refusals are raised there and cross on the frame's inputs. Raised here
    /// only when the text **changed**, so a notice already up is not
    /// re-stamped every frame and does age out.
    pub fn adopt_remote_notice(&mut self, text: Option<&str>, now: web_time::Instant) {
        let Some(text) = text else {
            return;
        };
        if self.notice.as_ref().is_some_and(|held| held.text == text) {
            return;
        }
        self.raise_notice(text.to_string(), now);
    }

    /// The last refusal's notice, once it is fresh enough to paint.
    pub fn notice(&self, now: web_time::Instant) -> Option<&AdmissionNotice> {
        self.notice
            .as_ref()
            .filter(|notice| now.duration_since(notice.raised_at) < NOTICE_LIFETIME)
    }

    /// Put a notice up. Test support and the enforcing land's own use.
    pub fn raise_notice(&mut self, text: String, now: web_time::Instant) {
        self.notice = Some(AdmissionNotice {
            text,
            raised_at: now,
        });
    }
}

/// **The top of the memory-share sliders**, which is where a share stops
/// being a lever — `ui_settings`'s `Slider::new(percent, FLOOR..=100)`.
///
/// A share sitting on this number is a control the reader has already run out
/// of, and a refusal that points at it is a warning about something they
/// cannot fix.
const SHARE_MAX_PERCENT: u8 = 100;

/// **What a refusal says, and it names something the reader can actually
/// move.**
///
/// Three things, in the order a reader needs them: which memory ran out, how
/// much short the act was, and **what to do**. The last is the point - a
/// notice that stopped at "not enough memory" would be a warning about
/// something the reader cannot fix.
///
/// Where a share is still a lever it is named by the label the Settings
/// screen actually shows (`ui_settings`'s `"GPU memory"` and `"System
/// memory"`, under the `Memory` heading), so the sentence and the screen
/// cannot drift into two names for one control.
///
/// **A share already at [`SHARE_MAX_PERCENT`] is not named at all.** On a
/// phone reporting one figure for everything both shares read `100 % asked
/// for, 100 % in force`, and the sentence this used to build told the reader
/// to raise a slider that was already at its stop - an instruction with
/// nothing behind it. When neither share is left to move, the notice says the
/// device has no more to give and names the part of the *scene* that would
/// close the gap instead ([`scene_lever`]); ruling 13 and ruling 15 keep the
/// governor from shortening those itself, so they are the user's to spend.
///
/// A unified pool names both shares where both are movable, because on one
/// memory either share moves the same wall.
fn refusal_text(act: Act, refusal: Refusal, percents: (u8, u8)) -> String {
    let short = refusal.short_bytes().div_ceil(1000 * 1000);
    let (gpu, host) = percents;
    let movable = |percent: u8| percent < SHARE_MAX_PERCENT;
    let (gpu_movable, host_movable) = match refusal.pool {
        Pool::Gpu => (movable(gpu), false),
        Pool::Host => (false, movable(host)),
        Pool::Joint => (movable(gpu), movable(host)),
    };
    if !gpu_movable && !host_movable {
        return format!(
            "Not enough memory for {} - {short} MB short. This device has no \
             more to give it: {}.",
            act.noun(),
            scene_lever(act),
        );
    }
    match refusal.pool {
        Pool::Gpu => format!(
            "Not enough GPU memory for {} - {short} MB short. Raise \"GPU \
             memory\" in Settings > Memory (now {gpu} %).",
            act.noun(),
        ),
        Pool::Host => format!(
            "Not enough system memory for {} - {short} MB short. Raise \
             \"System memory\" in Settings > Memory (now {host} %).",
            act.noun(),
        ),
        // One memory, and whichever share is still short of its stop is the
        // one that moves the wall. Naming a share already at 100 % beside a
        // movable one would send the reader to the dead control half the time.
        Pool::Joint if gpu_movable && host_movable => format!(
            "Not enough memory for {} - {short} MB short. This machine shares \
             one pool between the display and the system: raise \"GPU \
             memory\" (now {gpu} %) or \"System memory\" (now {host} %) in \
             Settings > Memory.",
            act.noun(),
        ),
        Pool::Joint if gpu_movable => format!(
            "Not enough memory for {} - {short} MB short. This machine shares \
             one pool between the display and the system: raise \"GPU \
             memory\" in Settings > Memory (now {gpu} %).",
            act.noun(),
        ),
        Pool::Joint => format!(
            "Not enough memory for {} - {short} MB short. This machine shares \
             one pool between the display and the system: raise \"System \
             memory\" in Settings > Memory (now {host} %).",
            act.noun(),
        ),
    }
}

/// **What of the scene the reader can spend, when no share is left to move.**
///
/// The governor may not shorten a lookback or thin a loop by itself (rulings
/// 13 and 15), so on a device that has already given everything it has these
/// are the only figures left that can close the gap — and they are the user's
/// to spend, not the model's. Named per act because the reader is standing in
/// front of one act, not in front of the whole scene.
const fn scene_lever(act: Act) -> &'static str {
    match act {
        Act::ArmLoop | Act::LoopSpan => "shorten the lookback, or turn off a layer",
        Act::Panes { .. } | Act::Preset => "close a pane, or turn off a layer",
        Act::ShowLayer | Act::DefaultLayers | Act::AdoptLayers => {
            "turn off another layer, shorten a lookback, or close a pane"
        }
    }
}

/// The verb the log line uses for an act.
fn act_word(act: Act) -> &'static str {
    match act {
        Act::Panes { .. } => "opening a pane",
        Act::ShowLayer => "showing a layer",
        Act::DefaultLayers => "seeding a pane's layers",
        Act::AdoptLayers => "syncing linked layers",
        Act::Preset => "applying a preset",
        Act::ArmLoop => "arming a loop",
        Act::LoopSpan => "widening the lookback",
    }
}

#[path = "admission/tests.rs"]
#[cfg(test)]
mod tests;
