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
//! **Opening a pane is one too**, and it did not used to be. `set_pane_count`
//! charged for the pane bare and let the `initialize_pane_enabled` two lines
//! below it ask separately for the layers that pane ships with — two asks,
//! either of which can be refused alone. Admitted then refused leaves a pane
//! that opened without its own default layers, which is neither the split the
//! user clicked nor the layout they had, and the notice on the glass names
//! layers rather than the pane. It is now summed and asked as one act; see
//! `Gui::set_pane_count`.
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
//! nicety. A door that refuses by returning and writes nothing but a
//! six-second notice turns a transient shortage into a lasting loss of
//! function: the scene that was tight when the user clicked is not the scene a
//! minute later, and even a *correct* refusal has to be re-asked when the
//! thing that made the scene tight goes away — a layer hidden, a pane closed,
//! a loop stopped, a rung shed, the host governor recovering.
//!
//! So a refused act is retained as a [`PendingWish`], which **outlives the
//! generation it was refused in**, and every wish is re-asked against the
//! table [`AdmissionLedger::adopt`] takes — the same telemetry tick that
//! republishes the spare. A wish that fits is resolved and dropped; a wish
//! that does not stays pending and is asked again on the next table.
//!
//! **What resolving means is per act, not one global answer**, because the
//! acts differ in what re-asking costs and in what the user would attribute
//! the result to. [`Recovery`] is the answer and [`Act::recovery`] is the
//! table: a layer's eye-click is re-*offered* on the glass, because painting a
//! layer minutes after the gesture is a scene change with no gesture behind
//! it; a pane's default layer set is re-*driven*, because "a pane holds the
//! layers it ships with" is a standing invariant the app owes the user rather
//! than a gesture they made. **The re-driven arm has no live customer today**
//! and that is stated rather than implied: every caller of that one door is
//! batched or exempt, so it is a policy waiting for a caller, and
//! [`Act::recovery`] names the three callers and what covers each.
//!
//! Two things are deliberately kept out of this, and both read like it:
//!
//! - [`AdmissionLedger::refused`] is a **within-generation memo**, not
//!   retention. It stops one question being answered forty times against one
//!   table, and it is cleared on every new table because a fresh table is a
//!   fresh answer. The wish set is the opposite of that clear: it *survives*
//!   it, and it is what asks the question again.
//! - **Two acts need no wish at all**, because a standing loop already
//!   re-asks them the moment that memo clears. `App::hydrate_parked_panes`
//!   re-drives a refused loop arm off its own parked queue on every redraw,
//!   and `Gui::propagate_pane_sync` re-drives the layer-link fan-out at the
//!   end of every shell frame. Retaining those here would be a second copy of
//!   a wish that already exists, and a second copy is a thing that drifts.
//!   They are [`Recovery::SelfDriven`], and the constant says so where a
//!   reader looking for them will be.
//!
//! **Nothing is retained on an arm that did not turn the act away.** Where
//! [`ENFORCING`] is `false` the act proceeded, so there is no wish: retaining
//! one would re-drive something that already happened. The whole of this
//! section is therefore inert on wasm32 today, and that is the point of it —
//! it removes the *reason* [`ENFORCING`] is off there. It does **not** flip
//! it. That needs field evidence off [`Totals::would_refuse`] on the arm every
//! user is on, and it is the user's call.
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
    /// `initialize_pane_enabled` then default-enables on it are **not in this
    /// figure** — it is one pane, bare. They are summed beside it by the same
    /// door, from [`Self::panes`] and [`Self::layer_grids`], so the pane and
    /// its layers are asked for as one act.
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
    /// **The most frames this pane's loop may hold**, once everything in the
    /// scene that is *not this loop's frames* is paid for.
    ///
    /// The figure the **listing door** compares a frame list against
    /// ([`AdmissionLedger::admit_loop_frames`]), and the one admission
    /// question about a loop that can be asked honestly: not "does one more
    /// increment fit" but "how many frames may this hold".
    ///
    /// **Derived over the scene with this pane's own loop excluded**, and
    /// that is what makes it usable where a byte increment is not. Between an
    /// arm and its listing a pane is `is_active()` and therefore prices as
    /// `looping` with no cadence, so the model charges it the whole render
    /// budget — 1120 MiB on the web bracket — and
    /// [`AdmissionCosts::spare`] is that scene's allowance less that need,
    /// floored at zero by `saturating_sub`. A door comparing against that
    /// spare refuses every frame count including the two-frame floor, on
    /// every arm. Excluding the pane's own loop makes this immune to that by
    /// construction rather than by correction.
    ///
    /// **Host axis only, and deliberately.** It is the axis
    /// `NeedTerms::loop_scans_host` prices and the axis the wasm trap lives
    /// on. A GPU-bound loop is still the ladder's to answer through
    /// `fit`, exactly as before; this door cannot refuse on GPU grounds and
    /// so cannot over-fire on them.
    ///
    /// **Not floored at `MIN_LOOP_FRAMES_PER_PANE`.** `fit::
    /// reachable_loop_frames` floors there because a count is all it can
    /// return, and its own comment says the sub-two case "is a refusal to
    /// make at admission, not a count to round down to one". This is that
    /// admission, so it takes the raw figure and refuses.
    pub loop_frames_allowed: usize,
    /// **What one of this pane's not-yet-arrived loop frames is reserved
    /// at** — `fit`'s own `scan_reserve` for this pane, the bootstrap or the
    /// site's calibrated floor above it.
    ///
    /// Carried so a refusal can state itself in bytes. The door decides in
    /// frames, but "three frames short" means nothing on the glass and
    /// "240 MB short" is the same fact in the units the memory settings are
    /// in.
    pub loop_frame_reserve_bytes: u64,
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
    /// **Committing to the frames a loop's listing named** — the loop's one
    /// whole-loop verdict, taken when its cadence lands and before any of it
    /// is downloaded.
    ///
    /// Spelled apart from [`Self::ArmLoop`] because the two are different
    /// questions with different answers *and different consequences*. An arm
    /// refusal costs nothing to re-ask and is re-asked for the user on the
    /// next table; a listing refusal has already paid for a frame listing
    /// over the network and is **not**, so its notice has to tell the reader
    /// to turn the loop back on and an arm's must not. Keeping them apart
    /// also keeps their refusals apart in the ledger's memo, where they are
    /// genuinely two answers.
    LoopFrames,
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
            Self::ArmLoop | Self::LoopFrames => "this loop",
            Self::LoopSpan => "a longer lookback",
        }
    }

    /// **What becomes of this act's wish when the spare that refused it
    /// grows.** See [`Recovery`], which is where the reasoning per act is.
    ///
    /// Wildcard-free on purpose: an act added to this enum does not get an
    /// answer by default, it gets a build failure until somebody decides
    /// which of the three it is.
    pub const fn recovery(self) -> Recovery {
        match self {
            // A standing loop already re-asks it. `App::hydrate_parked_panes`
            // takes `App::loop_arm_pending` on every redraw and a refused arm
            // re-parks itself onto that queue, so what holds the loop is the
            // within-generation memo and what releases it is that memo being
            // cleared by a fresher table. This act's own doc has said so since
            // the queue landed.
            Self::ArmLoop => Recovery::SelfDriven,
            // The same shape one level up: `Gui::propagate_pane_sync` runs at
            // the end of every shell frame, so the fan-out asks for itself
            // again the moment the memo clears. A linked group's arrangement
            // is a standing invariant rather than a gesture, which is also why
            // re-driving it needs no permission.
            Self::AdoptLayers => Recovery::SelfDriven,
            // **The one silently re-driven act.** "A pane holds the layers it
            // ships with" is an invariant the application owes the user, not a
            // gesture the user made: nothing was clicked to produce these
            // layers and nothing will be clicked to ask for them again, so a
            // refusal here leaves a pane in a state the user never curated and
            // has no name for. `Gui::initialize_pane_enabled` charges only the
            // transitions it will actually make, so a replay onto a pane that
            // has since gained its slots asks for nothing.
            //
            // **And it cannot be refused on main today**, which is worth
            // knowing before reading this as live behaviour: all three of that
            // door's callers are covered. `Gui::set_pane_count` holds a batch
            // over it (the pane and its layers are one act), `load_ui_config`
            // holds an exemption over it (restore is never a refusal), and
            // `Gui::new` reaches it before any table exists. So this arm is
            // the answer to "what should happen if it ever is", exercised by
            // `crate::admission::tests` and by nothing on the live path. It
            // becomes reachable the moment the door gains a caller that is
            // neither batched nor exempt.
            Self::DefaultLayers => Recovery::Silent,
            // A gesture on one pane's eye. Painting a layer minutes after the
            // click is a scene change with no gesture behind it, and a pane's
            // stack is user-curated state - the one thing a door may not write
            // on the user's behalf. Re-offered.
            Self::ShowLayer => Recovery::Reoffer,
            // The layout is the most visible thing on the glass and the
            // hardest change to attribute: a split that appears on its own
            // reads as a bug, not as a wish granted. Re-offered. The preset is
            // the same act with more of the scene in it.
            Self::Panes { .. } | Self::Preset => Recovery::Reoffer,
            // The slider shows the number in force. Widening the window
            // silently would move a control the reader may be looking at, and
            // the lookback is the user's to spend by rulings 13 and 15.
            Self::LoopSpan => Recovery::Reoffer,
            // **It has already paid for a frame listing over the network**, so
            // re-driving it costs a listing per table rather than nothing -
            // which is the distinction this variant was spelled apart from
            // `ArmLoop` to keep. Its refusal notice already tells the reader
            // to turn the loop back on ([`follow_up`]); this tells them when
            // that will work.
            Self::LoopFrames => Recovery::Reoffer,
        }
    }

    /// **The key a wish is retained under**: the variant, with any payload
    /// excluded.
    ///
    /// Deliberately coarser than the `PartialEq` [`AdmissionLedger::refused`]
    /// keys on. That memo answers "what did this table say to *this exact*
    /// question", so `Panes { added: 2 }` and `Panes { added: 3 }` are two
    /// questions there and must be. A wish is "what did the user last want",
    /// and a user who asked for three panes and then for two wants two - so
    /// the later ask replaces the earlier one rather than sitting beside it.
    /// It is also what makes the retained set's ceiling a fixed number instead
    /// of one that scales with a payload's range.
    ///
    /// Wildcard-free for the same reason [`Self::recovery`] is.
    const fn retention_key(self) -> u8 {
        match self {
            Self::Panes { .. } => 0,
            Self::ShowLayer => 1,
            Self::DefaultLayers => 2,
            Self::AdoptLayers => 3,
            Self::Preset => 4,
            Self::ArmLoop => 5,
            Self::LoopFrames => 6,
            Self::LoopSpan => 7,
        }
    }
}

/// **Every kind of act, for the retained set's ceiling to be derived over
/// rather than typed.**
///
/// **The premise the compiler does not check**: that this lists every variant.
/// A variant added to [`Act`] is forced to get a [`Act::retention_key`] arm -
/// that match is wildcard-free and will not compile without one - but nothing
/// forces it into this roster, so a new act would leave [`PENDING_CAP`] one
/// short. `every_act_kind_has_its_own_retention_key` is the check, and it is a
/// test rather than a `const _` because the distinctness it asserts needs a
/// walk. The consequence of being one short is a dropped wish, never an
/// unbounded set: what bounds the set is the key replacement in [`place`], and
/// this ceiling is the backstop behind it.
const ACT_KINDS: &[Act] = &[
    Act::Panes { added: 1 },
    Act::ShowLayer,
    Act::DefaultLayers,
    Act::AdoptLayers,
    Act::Preset,
    Act::ArmLoop,
    Act::LoopFrames,
    Act::LoopSpan,
];

/// **What happens to a refused act when the spare that refused it grows.**
///
/// The three answers exist because the acts are genuinely three different
/// things, and picking one answer for all of them gets two of the three wrong:
/// re-driving everything paints layers and opens panes nobody just asked for,
/// and re-offering everything puts a notice on the glass for work the
/// application should simply have done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// **Re-drive the act for the user.** Nothing external was paid for it and
    /// the application would have done it unasked, so there is no gesture to
    /// attribute the result to and none is needed.
    Silent,
    /// **Say there is room now, and let the reader ask again.** The act was a
    /// gesture, and performing a gesture the user made a minute ago is a scene
    /// change they cannot attribute; or it has already paid for something
    /// external and re-driving it would pay again on every table.
    Reoffer,
    /// **Retain nothing**: a standing loop in the application already re-asks
    /// this act, and the within-generation memo clearing on a fresh table is
    /// what releases it. A wish here would be a second copy of one that
    /// already exists.
    SelfDriven,
}

/// **What a wish asked for, in the unit its own door decides in.**
///
/// The byte doors compare an [`Increment`] against [`AdmissionLedger::spare`];
/// the listing door compares a frame count against
/// [`PaneAdmission::loop_frames_allowed`], for the reasons written on
/// [`AdmissionLedger::admit_loop_frames`]. Re-asking has to use the same unit
/// the refusal used or it is answering a different question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    /// A byte door's increment.
    Bytes(Increment),
    /// The listing door's frame count.
    Frames(usize),
}

/// **A refused act, retained so the door can ask again when the scene
/// changes.**
///
/// Held past the generation it was refused in - which is the whole difference
/// between this and [`AdmissionLedger::refused`] - and re-asked on every table
/// [`AdmissionLedger::adopt`] takes until one of four things happens: it fits,
/// the pane it named is gone, it ages out ([`WISH_LIFETIME`]), or the same
/// question is asked again and replaces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingWish {
    /// What was asked for.
    pub act: Act,
    /// The pane it was asked on, where the act names one.
    pub pane: Option<usize>,
    /// The price the table in force at the refusal put on it. An estimate by
    /// the time it is re-asked - see [`AdmissionLedger::revisit`].
    want: Want,
    /// When the verdict that refused it was taken. Wall clock, because the
    /// quantity being bounded is how long ago the user asked; a frame count
    /// measures how busy the machine has been instead.
    wished_at: web_time::Instant,
}

/// **How long a refused act stays a wish.**
///
/// A wish is the user's intent, and intent goes stale: nobody wants the layer
/// they gave up on two minutes ago to appear while they are doing something
/// else. So the retained set is bounded in time as well as in size, and this
/// is the time.
///
/// **Sixty seconds, and the reason is the errand the refusal notice sends the
/// reader on.** That notice names a control - `Raise "System memory" in
/// Settings > Memory` - and lives [`NOTICE_LIFETIME`], six seconds. Opening
/// the settings screen, finding the Memory heading, moving a share and coming
/// back is a ten-second job for someone who knows the screen and most of a
/// minute for someone meeting it for the first time. A wish that expired
/// before the second reader finished would make the instruction a lie, which
/// is the defect the notice exists to avoid. Sixty seconds covers them and is
/// still comfortably inside the span over which a person attributes an
/// outcome to their own gesture.
///
/// It is also about thirty tables at the composition cadence
/// (`App::compose_admission_costs`, ~2 s), so a wish gets on the order of
/// thirty re-asks - far more than a governor recovery or a closed pane needs
/// to show up.
///
/// **Measured from the last verdict that refused this question, not the
/// first.** A repeat inside one generation is answered from the memo without
/// a verdict, so a door re-driven sixty times a second refreshes nothing; only
/// a fresh refusal - a new gesture, or a replay that was refused again - moves
/// the clock, and a new gesture *should* start the clock again.
pub const WISH_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// **The most wishes either set may hold**, derived rather than typed.
///
/// The real bound is [`place`]: one entry per `(act kind, pane)`, the later
/// ask replacing the earlier. That gives a ceiling of every act kind against
/// every pane a layout can show, plus the `None` key the acts that name no
/// pane share - so it is derived from [`ACT_KINDS`] and the layout's own
/// maximum rather than written as a number that would go stale beside them.
///
/// This is the **backstop** behind that bound, not the bound (see the plan's
/// rule: an unreachable budget is a backstop, not a redundancy). Its
/// unreachability rests on three premises and the compiler checks none of
/// them: that [`place`] really is keyed, that [`ACT_KINDS`] is exhaustive, and
/// that no pane index reaches this ledger above the layout's maximum. The
/// first two have tests; the third is a runtime property of `Gui::panes`.
const PENDING_CAP: usize =
    ACT_KINDS.len() * (squallar_device_profile::budget::MAX_PANES_DESKTOP + 1);

/// The ceiling prices one wish per act kind per addressable pane, plus the
/// one key every pane-less act shares. Named here so a change to either term
/// fails with the arithmetic in front of the reader.
const _: () = assert!(PENDING_CAP == 8 * 7);

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
///
/// **One of the two reasons is now gone, and this is deliberately still
/// `false`.** The other half of why enforcing here was untenable was that a
/// refusal was permanent for the session: a *correct* refusal cost the user a
/// function until they restarted the application. Refusals are retained and
/// re-asked on every table now ([`PendingWish`], [`Recovery`]), so being wrong
/// costs a table rather than a session. That removes a reason. It is not
/// evidence, and removing a reason is not a flip: what a flip needs is
/// [`Totals::would_refuse`] read on the arm every user is on, beside a
/// `budget state:` line that agrees with it about the same scene — and it is
/// the user's call, not this module's.
///
/// Note what that means for everything in this file about wishes: on the arm
/// where this is `false` **no wish is ever retained**, because nothing is ever
/// turned away. The machinery is exercised on native and by
/// [`AdmissionLedger::decide`]'s injected arm in tests, and it is waiting here
/// for the day the constant moves.
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

/// **How often a sentence was put on the glass, and how often it landed on
/// one that was already there.**
///
/// Separate from the verdict counters above because they answer a different
/// question and could not answer this one. A user reported a notice that
/// "appeared and went away"; nothing in this tree could confirm or refute it,
/// because **no per-act refusal is logged anywhere** — the only refusal text
/// the application emits is the cumulative
/// `admission asked N admitted N would refuse N refused N` group on
/// `budget state:`, and [`AdmissionLedger::raise_notice`] draws without
/// logging. A hypothesis about re-stamping was therefore unfalsifiable on
/// every log this application can produce.
///
/// [`NOTICES_RAISED_LIVE`] is the whole instrument: a notice raised while one
/// is **still showing** replaces it and restarts its six seconds, which is what
/// a reader sees as flicker. A notice raised after the previous one aged out
/// is just the next notice. The two are indistinguishable in a total and
/// opposite in what they mean, so they are counted apart.
static NOTICES_RAISED: AtomicU32 = AtomicU32::new(0);
/// See [`NOTICES_RAISED`]: raised while a notice was already on the glass.
static NOTICES_RAISED_LIVE: AtomicU32 = AtomicU32::new(0);
/// **Stamps this module's own re-offer path put up** ([`AdmissionLedger::revisit`]).
///
/// Counted apart so it can be **subtracted**, not to be admired. A re-offer is
/// a stamp like any other, so without this field a wish resolving on the
/// telemetry tick would be indistinguishable from the re-stamped refusal the
/// counter exists to find — this lane's own new code masquerading as the
/// phantom it was written to measure. `raised - reoffered` is the
/// refusal-driven figure.
static NOTICES_REOFFERED: AtomicU32 = AtomicU32::new(0);

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
    /// **Sentences put on the glass** — see [`NOTICES_RAISED`]. Every
    /// [`AdmissionLedger::raise_notice`], whatever raised it.
    pub raised: u32,
    /// **Of those, the ones that landed on a notice still showing**, which is
    /// what a reader sees as a notice appearing and vanishing. See
    /// [`NOTICES_RAISED_LIVE`].
    pub raised_live: u32,
    /// **Of [`Self::raised`], the ones this module's re-offer path put up.**
    /// Subtract it to get the refusal-driven figure — see
    /// [`NOTICES_REOFFERED`].
    pub reoffered: u32,
}

/// The counters, as running totals from boot.
pub fn totals() -> Totals {
    Totals {
        asked: ASKED.load(Relaxed),
        admitted: ADMITTED.load(Relaxed),
        would_refuse: WOULD_REFUSE.load(Relaxed),
        refused: REFUSED.load(Relaxed),
        raised: NOTICES_RAISED.load(Relaxed),
        raised_live: NOTICES_RAISED_LIVE.load(Relaxed),
        reoffered: NOTICES_REOFFERED.load(Relaxed),
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
            raised: self.raised.saturating_sub(earlier.raised),
            raised_live: self.raised_live.saturating_sub(earlier.raised_live),
            reoffered: self.reoffered.saturating_sub(earlier.reoffered),
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

/// **One tick's spare, spent once** — the debited total two ledgers holding
/// the same table share.
///
/// There are two [`AdmissionLedger`]s in a running application and there has
/// to be: the App-side door (`App::handle_enable_loop`, `accept_scan_listing`)
/// asks the same question the UI doors ask, and it runs *inside* the App's own
/// `self.gui.panes_and_overlays_mut()` borrow, so it cannot reach the UI's
/// ledger even if the App-pokes-Gui coupling ceiling had room for it.
///
/// **What they must not have two of is the debit.** [`AdmissionLedger::spare`]
/// is the published spare less what has been admitted against it, and a copy
/// each means a burst mixing UI acts with loop arms inside one telemetry tick
/// is compared against one tick's spare **twice** — the scene admitted is up
/// to double what the device was priced as having. That direction costs the
/// user their process rather than a rung, which is the direction this whole
/// admission system exists for.
///
/// So the total lives here, behind a handle both ledgers hold, and the App
/// hands the UI its copy over the frame seam
/// ([`crate::shell_api::FrameInputs::admission_debit`]) — a field on the
/// inputs, computed in `squallar-app`, which is the seam's own rule and adds
/// no reach into the UI at all.
///
/// **The generation is in the cell, not beside it.** Both ledgers adopt the
/// same table and each would otherwise zero the total on adopting it, so the
/// second to arrive would wipe what the first had already spent. Keyed on the
/// generation the cell itself last saw, the first adopter resets and the
/// second finds nothing to do.
///
/// Atomics rather than a `Cell` so the ledger stays `Send`; the two ledgers
/// are on one thread and nothing here is a synchronisation point.
#[derive(Debug, Default)]
struct Debit {
    generation: std::sync::atomic::AtomicU64,
    gpu_bytes: std::sync::atomic::AtomicU64,
    host_bytes: std::sync::atomic::AtomicU64,
}

/// A handle on the shared debited total. See [`Debit`].
#[derive(Clone, Debug, Default)]
pub struct SharedDebit(std::sync::Arc<Debit>);

impl SharedDebit {
    /// What has been admitted against the table in force.
    fn spent(&self) -> Increment {
        Increment {
            gpu_bytes: self.0.gpu_bytes.load(Relaxed),
            host_bytes: self.0.host_bytes.load(Relaxed),
        }
    }

    /// Debit an admitted act, so the next door in the same tick — through
    /// **either** ledger — sees the smaller spare.
    fn spend(&self, want: Increment) {
        self.0.gpu_bytes.fetch_add(want.gpu_bytes, Relaxed);
        self.0.host_bytes.fetch_add(want.host_bytes, Relaxed);
    }

    /// Start `generation` if this cell has not already been started on it.
    /// The second ledger to adopt one table must not re-zero what the first
    /// has spent against it.
    fn start(&self, generation: u64) {
        if self.0.generation.swap(generation, Relaxed) == generation {
            return;
        }
        self.0.gpu_bytes.store(0, Relaxed);
        self.0.host_bytes.store(0, Relaxed);
    }

    /// Whether these two handles name the same total.
    pub fn is_shared_with(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// **The per-application ledger**: the table the App published, what has been
/// admitted against it since, and the notice a refusal left.
#[derive(Debug, Default)]
pub struct AdmissionLedger {
    costs: AdmissionCosts,
    /// **Increments admitted since [`AdmissionCosts::generation`] last moved.**
    /// Debited from the published spare, so six acts inside one telemetry tick
    /// are compared against six shrinking spares rather than one standing
    /// figure.
    ///
    /// **Shared with the App's ledger, not copied to it** — see
    /// [`SharedDebit`], which is the whole reason this is behind a handle.
    spent: SharedDebit,
    /// Depth of an open batch. While non-zero the inner doors charge nothing:
    /// the batch asked for the whole and the transitions it priced are the
    /// ones they will make.
    batch: u32,
    /// The last refusal, for the glass.
    notice: Option<AdmissionNotice>,
    /// **Every act already refused against the table in force**, with the
    /// refusal it drew.
    ///
    /// A door can be re-driven by something that is not a fresh table. The
    /// loop door is: `App::hydrate_parked_panes` runs on every
    /// `RedrawRequested` and a loop waiting on its transport comes straight
    /// back onto that queue, which on the web build logged the same refusal
    /// about **forty times in the first seven seconds** (Tier-2 `long` leg,
    /// Chromium, 2026-09-07). Forty identical lines are not forty verdicts,
    /// and a notice re-stamped every redraw never ages off the glass.
    ///
    /// So a question already answered against this table is answered from
    /// here: same verdict, no second log line, no re-stamped notice, and no
    /// second count. **The answer cannot have changed** — [`Self::spare`] is
    /// the published spare less what has been admitted since, and admissions
    /// only ever shrink it, so a refusal stays a refusal until the App
    /// publishes a new table. Cleared by [`Self::adopt`] when it does.
    ///
    /// **The `want` behind a key is not quite as fixed as the spare**, and
    /// saying so is cheaper than the reader finding out. Since the pane door
    /// became a batch, `Act::Panes`' increment includes what
    /// `Gui::default_layers_increment` sums over the panes in hand, so
    /// curating a default layer back onto a pane shrinks it *within* one
    /// generation — and a refusal held here goes on answering the smaller
    /// question with the bigger one's verdict. Bounded by the composition
    /// cadence (a couple of seconds), and it errs by refusing something that
    /// has just started fitting rather than by admitting something that does
    /// not. **Only refusals are held**, so a `want` that grows is re-asked in
    /// full and cannot be waved through by this.
    ///
    /// Bounded by the distinct `(act, pane)` pairs a scene can produce — at
    /// most one per act per visible pane — so it needs no eviction.
    ///
    /// **This is a memo, not retention**, and the difference is the point of
    /// [`Self::pending`] beside it: what is held here is one table's answer to
    /// one question, and it is *correct* that it dies with the table. What
    /// must not die with the table is the user's wish.
    refused: Vec<(Act, Option<usize>, Refusal)>,
    /// **What was refused and is still wanted** — the wishes that outlive the
    /// generation, re-asked on every table [`Self::adopt`] takes. See
    /// [`PendingWish`], and [`Recovery`] for what happens to one that starts
    /// fitting.
    ///
    /// **Held per ledger, unlike the debit.** [`SharedDebit`] is shared
    /// because two ledgers spending one tick's spare twice over-admits the
    /// scene; a wish is the opposite shape - it is the record of a door's own
    /// refusal, and the door that refused it is the door that can re-drive or
    /// re-offer it. The App's ledger holds the loop wishes and the UI's holds
    /// the layer and layout ones, which is exactly where each is answerable.
    ///
    /// Bounded twice over: by the `(act kind, pane)` key [`place`] replaces on
    /// — the ceiling is [`PENDING_CAP`] — and in time by [`WISH_LIFETIME`].
    pending: Vec<PendingWish>,
    /// **Wishes that fit again and whose act the application re-drives
    /// itself** — [`Recovery::Silent`], filled by [`Self::revisit`] and
    /// drained by [`Self::take_granted`].
    ///
    /// A queue rather than a callback because the ledger cannot perform an
    /// act: it holds no panes, no registry and no layer stack, and giving it
    /// any of those would put pricing and doing in one object. The caller that
    /// owns the doors drains it — `Gui::replay_granted_admissions`.
    ///
    /// Keyed the same way [`Self::pending`] is, so a caller that never drains
    /// holds at most the same key space rather than one entry per table.
    granted: Vec<PendingWish>,
    /// **This ledger's own verdict counts.** The `static`s above are the
    /// process-wide figures the App prints; these are per-application, which
    /// is what a test can assert on without racing every other test in the
    /// binary. Both move on the same verdict, so they cannot disagree about
    /// what happened — only about the denominator.
    counts: Totals,
}

/// **A cloned ledger is a second application, not a second holder of one
/// debit.** Derived, the [`SharedDebit`] handle would come along and the copy
/// would spend the original's total — which is the defect this type exists to
/// close, running backwards. The clone gets its own cell at the same reading.
impl Clone for AdmissionLedger {
    fn clone(&self) -> Self {
        let spent = SharedDebit::default();
        spent.start(self.costs.generation);
        spent.spend(self.spent.spent());
        Self {
            costs: self.costs.clone(),
            spent,
            batch: self.batch,
            notice: self.notice.clone(),
            refused: self.refused.clone(),
            // A second application wants the same things: a wish is the
            // user's, not the cell's, and the reason the debit may not be
            // copied does not reach it.
            pending: self.pending.clone(),
            granted: self.granted.clone(),
            counts: self.counts,
        }
    }
}

impl AdmissionLedger {
    /// Take a freshly published table. Clears what has been spent when the
    /// generation moved, since the new spare already accounts for it, and
    /// re-asks every wish a refusal left behind.
    ///
    /// **This is the tick that republishes the spare**, on both sides of the
    /// seam: `App::compose_admission_costs` calls it from the telemetry
    /// cadence, and `Gui::apply_frame_inputs` calls it on the one frame that
    /// first sees the new generation. Everything below the generation compare
    /// therefore runs at most once per table, never per frame.
    pub fn adopt(&mut self, costs: &AdmissionCosts) {
        self.adopt_at(costs, web_time::Instant::now());
    }

    /// [`Self::adopt`] with the clock passed in, so a test can age a wish
    /// without sleeping - the shape [`Self::notice`] and
    /// [`Self::raise_notice`] already have, and for the same reason.
    pub fn adopt_at(&mut self, costs: &AdmissionCosts, now: web_time::Instant) {
        if costs.generation == self.costs.generation {
            return;
        }
        self.costs = costs.clone();
        // Keyed on the generation the shared cell last saw, so the second
        // ledger to adopt one table does not wipe what the first spent
        // against it. See `SharedDebit`.
        self.spent.start(costs.generation);
        // A fresh table is a fresh answer: whatever was refused against the
        // old spare gets asked again, which is what makes a refusal something
        // the user can act on and retry rather than a dead end for the
        // session.
        self.refused.clear();
        // **After the table is in force, never before**: `revisit` compares
        // every wish against `Self::spare`, and that reads `self.costs`.
        self.revisit(now);
    }

    /// **Ask every retained wish again, against the table just adopted.**
    ///
    /// The other half of the memo clear above: clearing `refused` lets a door
    /// that is re-driven ask again, and this asks for the doors that are
    /// **not** re-driven — a click nobody will make twice, a preset applied
    /// once, a slider already back where it was.
    ///
    /// Four things end a wish, and only one of them is "it fits":
    ///
    /// - **The pane it named is gone.** A wish addressed to pane 3 in a
    ///   two-pane layout is not a wish any more, and re-offering it would name
    ///   a pane that is not on the glass. Dropped before the fit test, so a
    ///   closed pane never resolves a wish it took the room from.
    /// - **It aged out** ([`WISH_LIFETIME`]).
    /// - **It fits**, and then [`Recovery`] says what that means.
    /// - Or the same question is asked again, which replaces it in
    ///   [`place`] rather than here.
    ///
    /// **The fit test is against the wish's own recorded price, which is an
    /// estimate by now.** `AdmissionLedger::refused`'s own doc records why: a
    /// door's increment can move inside a generation. So this is a filter and
    /// not a grant — a `Silent` wish goes back through its real door, which
    /// re-prices against the table in force and refuses again if it has to,
    /// and a `Reoffer` wish commits nothing at all. Nothing here debits
    /// [`Self::spent`]: no act has happened yet, and spending for one that
    /// may never be asked for would hold bytes against a scene nobody has.
    fn revisit(&mut self, now: web_time::Instant) {
        if self.pending.is_empty() {
            return;
        }
        let held = std::mem::take(&mut self.pending);
        // **One notice for the whole tick, and it is the newest wish's.**
        // `place` pushes at the back, so this set reads oldest-first and the
        // last wish to resolve is the one the reader asked for most recently
        // - the one they can still attribute a sentence to. Collected and
        // raised once after the walk rather than inside it, so a tick that
        // resolves three wishes stamps the notice once instead of three
        // times; a door that refuses later in the same frame overwrites it,
        // which is the newer fact and the right one to show.
        let mut reoffer: Option<Act> = None;
        for wish in held {
            if wish
                .pane
                .is_some_and(|idx| self.costs.panes.get(idx).is_none())
            {
                continue;
            }
            if now.duration_since(wish.wished_at) >= WISH_LIFETIME {
                continue;
            }
            if !self.wish_fits(&wish) {
                self.pending.push(wish);
                continue;
            }
            match wish.act.recovery() {
                Recovery::Silent => place(&mut self.granted, wish),
                Recovery::Reoffer => reoffer = Some(wish.act),
                // Never retained, so never seen here. Spelled rather than
                // wildcarded so a `recovery` that changes has to be read
                // against this walk too.
                Recovery::SelfDriven => {}
            }
        }
        if let Some(act) = reoffer {
            // **Counted before it is raised, so this lane's own stamp cannot
            // read as the phantom re-stamp the counter exists to find.** See
            // `NOTICES_REOFFERED`.
            NOTICES_REOFFERED.fetch_add(1, Relaxed);
            self.counts.reoffered = self.counts.reoffered.saturating_add(1);
            self.raise_notice(reoffer_text(act).to_string(), now);
        }
    }

    /// **Would the table in force take this wish now** — in the unit the door
    /// that refused it decides in, which is the only unit the answer means
    /// anything in.
    fn wish_fits(&self, wish: &PendingWish) -> bool {
        match wish.want {
            Want::Bytes(want) => verdict(self.spare(), want).is_admit(),
            // The listing door's question, re-asked exactly as
            // `Self::decide_loop_frames` asks it: a count against the frames
            // this pane's loop may hold over a scene with its own loop taken
            // out. A wish with no pane cannot be one of these and does not
            // fit by default rather than by accident.
            Want::Frames(frames) => wish
                .pane
                .is_some_and(|idx| frames <= self.pane(idx).loop_frames_allowed),
        }
    }

    /// **The wishes still waiting on room.** The figure a test asserts the
    /// bound on, and what a diagnostics row would read.
    pub fn pending(&self) -> &[PendingWish] {
        &self.pending
    }

    /// **Take the wishes that fit again and are the application's to
    /// re-drive.** Empty on every tick but the one that resolved one, and
    /// empty for the whole session on an arm where [`ENFORCING`] is `false` —
    /// nothing was turned away there, so nothing was retained.
    ///
    /// The caller re-drives each act through its **real door**, which asks
    /// again against the table in force. See `Gui::replay_granted_admissions`.
    pub fn take_granted(&mut self) -> Vec<PendingWish> {
        std::mem::take(&mut self.granted)
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
    /// everything admitted since it published it — **through either ledger**,
    /// which is what [`SharedDebit`] is for.
    pub fn spare(&self) -> Spare {
        let spent = self.spent.spent();
        let debit =
            |published: Option<u64>, spent: u64| published.map(|bytes| bytes.saturating_sub(spent));
        Spare {
            gpu_bytes: debit(self.costs.spare.gpu_bytes, spent.gpu_bytes),
            host_bytes: debit(self.costs.spare.host_bytes, spent.host_bytes),
            joint_bytes: debit(self.costs.spare.joint_bytes, spent.joint_bytes()),
        }
    }

    /// **The handle on this ledger's debited total**, for the App to publish
    /// to the UI's ledger over the frame seam. See [`SharedDebit`].
    pub fn debit(&self) -> &SharedDebit {
        &self.spent
    }

    /// **Spend against `debit` from here on**, rather than against a total of
    /// this ledger's own.
    ///
    /// Idempotent and re-stated every frame: the App publishes its handle on
    /// [`crate::shell_api::FrameInputs::admission_debit`] and this adopts it
    /// once. Adopting carries what this ledger has already spent across, so
    /// the frame in which the two ledgers meet does not forget it — without
    /// that the handshake would itself be a one-off over-admission of exactly
    /// the kind it exists to prevent.
    ///
    /// **It never resets the total it is joining**, whatever generation this
    /// ledger is on. The cell's generation is [`Self::adopt`]'s to move, and
    /// a ledger joining one tick behind the App would otherwise zero a total
    /// the App had already spent against.
    pub fn share_debit(&mut self, debit: &SharedDebit) {
        if self.spent.is_shared_with(debit) {
            return;
        }
        debit.spend(self.spent.spent());
        self.spent = debit.clone();
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
    ///
    /// **It raises no notice and retains no wish.** Both are
    /// [`Self::act_on`]'s, and a caller that only asks has not turned anything
    /// away - so there is nothing to announce and nothing to ask again for.
    /// Every production door goes through [`Self::enforce`] or
    /// [`Self::admit_loop_frames`], which do.
    pub fn ask(&mut self, act: Act, pane: Option<usize>, want: Increment) -> Verdict {
        self.ask_tracked(act, pane, want).0
    }

    /// [`Self::ask`]'s body, also saying whether this was a **fresh** answer
    /// or one repeated from [`Self::refused`].
    ///
    /// The distinction is the caller's, not the verdict's: a repeat is the
    /// same answer to the same question and must act the same way, but it
    /// must not log a second line, re-stamp the notice or move a counter.
    /// Every figure this ledger publishes is therefore a count of **verdicts
    /// taken**, never of how often a door happened to be re-driven.
    fn ask_tracked(&mut self, act: Act, pane: Option<usize>, want: Increment) -> (Verdict, bool) {
        if want.is_zero() || self.in_batch() || self.costs.generation == 0 {
            return (Verdict::Admit, true);
        }
        if let Some(held) = self.held_answer(act, pane) {
            return (held, false);
        }
        let v = verdict(self.spare(), want);
        if v.is_admit() {
            self.spent.spend(want);
        }
        (self.record(act, pane, v), true)
    }

    /// The answer this table already gave to `(act, pane)`, if it gave one.
    fn held_answer(&self, act: Act, pane: Option<usize>) -> Option<Verdict> {
        self.refused
            .iter()
            .find(|(held_act, held_pane, _)| *held_act == act && *held_pane == pane)
            .map(|(_, _, refusal)| Verdict::Refuse(*refusal))
    }

    /// **Count a verdict, log a refusal, and remember it** — the half of an
    /// ask that is the same whether the verdict came from comparing bytes
    /// against [`Self::spare`] or frames against
    /// [`PaneAdmission::loop_frames_allowed`].
    ///
    /// Spelled once so the two doors cannot come to count, log or memoise
    /// differently. It does not debit [`Self::spent`]: what an admission
    /// spends is the byte door's business, and the listing door adds no bytes
    /// to a scene that is already carrying its loop.
    fn record(&mut self, act: Act, pane: Option<usize>, v: Verdict) -> Verdict {
        ASKED.fetch_add(1, Relaxed);
        self.counts.asked = self.counts.asked.saturating_add(1);
        match v {
            Verdict::Admit => {
                ADMITTED.fetch_add(1, Relaxed);
                self.counts.admitted = self.counts.admitted.saturating_add(1);
            }
            Verdict::Refuse(refusal) => {
                WOULD_REFUSE.fetch_add(1, Relaxed);
                self.counts.would_refuse = self.counts.would_refuse.saturating_add(1);
                self.refused.push((act, pane, refusal));
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

    /// **The listing door: may this pane's loop hold the frames its listing
    /// named?**
    ///
    /// Asked once, when a listing lands and says the site's cadence, and
    /// **before the first frame is downloaded** — the moment the frame count
    /// stops being a guess and the moment before any of it is committed.
    /// `wanted` is the frame list the listing produced, already held to
    /// `LoopFrames::frames` at the pane's own span.
    ///
    /// **A count, not an increment, and that is the whole design.** The
    /// question at a listing was never "does one more thing fit" — the loop
    /// is already in the scene by then — it is "how many frames may this
    /// hold", which is what [`PaneAdmission::loop_frames_allowed`] answers
    /// over a scene with this pane's own loop taken out. Asking it as a byte
    /// increment against [`Self::spare`] cannot work: that spare is the
    /// allowance less a need that already carries this loop at the render
    /// budget's ceiling, floored at zero, so it refuses every frame count
    /// there is.
    ///
    /// One verdict for the whole loop, taken before a grant exists, so
    /// **rulings 13 and 15 hold**: nothing here shortens a granted loop or
    /// decimates one pane's frames on another's behalf. It admits or it
    /// refuses.
    ///
    /// A refusal is counted, logged, memoised and **shown** exactly as a byte
    /// door's is, and the caller is expected to leave loop mode and download
    /// nothing.
    pub fn admit_loop_frames(&mut self, pane_idx: usize, wanted: usize) -> bool {
        self.decide_loop_frames(pane_idx, wanted, ENFORCING)
    }

    /// [`Self::admit_loop_frames`]'s body with the arm's policy passed in, so
    /// both arms are reachable from one test binary — [`Self::decide`]'s
    /// reason, and the wasm arm is again the one no test here executes.
    fn decide_loop_frames(&mut self, pane_idx: usize, wanted: usize, enforcing: bool) -> bool {
        let act = Act::LoopFrames;
        let pane_key = Some(pane_idx);
        if self.in_batch() || self.costs.generation == 0 {
            return true;
        }
        let pane = self.pane(pane_idx);
        // A pane the table has not seen asks for nothing, the same way every
        // other door reads an absent row: refusing on a figure that does not
        // exist is how an admission system becomes a wall at startup.
        if self.costs.panes.get(pane_idx).is_none() || wanted <= pane.loop_frames_allowed {
            return true;
        }
        let (v, fresh) = match self.held_answer(act, pane_key) {
            Some(held) => (held, false),
            None => {
                // The same shortfall in bytes, because that is the unit the
                // memory settings and the notice are in. `spare_bytes` is
                // what the allowed frames are worth, not a pool reading:
                // the door compared counts and the sentence says so in the
                // units a reader can act on.
                let reserve = pane.loop_frame_reserve_bytes;
                let refusal = Refusal {
                    pool: Pool::Host,
                    wanted_bytes: (wanted as u64).saturating_mul(reserve),
                    spare_bytes: (pane.loop_frames_allowed as u64).saturating_mul(reserve),
                };
                (self.record(act, pane_key, Verdict::Refuse(refusal)), true)
            }
        };
        self.act_on(act, pane_key, Want::Frames(wanted), v, fresh, enforcing)
    }

    /// **Whether `act` was already refused against the table in force.**
    ///
    /// For a caller whose own re-drive is cheaper to skip than to re-ask:
    /// `App::hydrate_parked_panes` runs every redraw and would otherwise put
    /// a refused loop back through the whole door on each one. The answer
    /// cannot change until the App publishes a new table, so a caller that
    /// sees `true` should hold its request and re-drive on the next
    /// generation.
    pub fn already_refused(&self, act: Act, pane: Option<usize>) -> bool {
        self.refused
            .iter()
            .any(|(held_act, held_pane, _)| *held_act == act && *held_pane == pane)
    }

    /// **What arming `pane_idx`'s loop costs, where that is knowable at all**
    /// — `None` before a listing has said the site's cadence.
    ///
    /// The App prices [`PaneAdmission::arm_loop`] off the pane's prospective
    /// scene, and with no cadence the model's frame count is the render
    /// budget's ceiling rather than the span's own answer
    /// (`squallar_device_profile::admit::LoopFrames::priceable`, which
    /// carries the measurement). A door spending that figure refuses loops
    /// that would have fitted, so it does not get to: `None` means **admit
    /// and ask again where the number lands**, which is the listing, still
    /// before a frame is downloaded.
    pub fn arm_loop(&self, pane_idx: usize) -> Option<Increment> {
        let pane = self.pane(pane_idx);
        squallar_device_profile::admit::prices_a_loop(pane.cadence_secs).then_some(pane.arm_loop)
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
    pub fn enforce(&mut self, act: Act, pane: Option<usize>, want: Increment) -> bool {
        self.decide(act, pane, want, ENFORCING)
    }

    /// [`Self::enforce`]'s body, with the arm's policy passed in rather than
    /// read from the `const`.
    ///
    /// Both arms are then reachable from one test binary. A `cfg` in the body
    /// would leave whichever arm this build did not compile with no test at
    /// all, and the advisory arm is the wasm one — the arm no test in this
    /// workspace executes.
    fn decide(&mut self, act: Act, pane: Option<usize>, want: Increment, enforcing: bool) -> bool {
        let (v, fresh) = self.ask_tracked(act, pane, want);
        self.act_on(act, pane, Want::Bytes(want), v, fresh, enforcing)
    }

    /// **Act on a verdict**: the arm's policy, the refused count and the
    /// notice.
    ///
    /// Shared by the byte doors and the listing door so a refusal reaches the
    /// glass by one path however it was decided. Returns whether the caller
    /// may proceed.
    ///
    /// **Seven arguments counting `self`, which is exactly clippy's ceiling.**
    /// `too_many_arguments` fires at *more* than seven, so this passes - but
    /// the next parameter added here is a lint failure and not a style
    /// question. Bundle `act`/`pane`/`want` into a struct at that point rather
    /// than raising a threshold: they are one thing (the question that was
    /// asked) already spelled as three.
    fn act_on(
        &mut self,
        act: Act,
        pane: Option<usize>,
        want: Want,
        v: Verdict,
        fresh: bool,
        enforcing: bool,
    ) -> bool {
        let Verdict::Refuse(refusal) = v else {
            return true;
        };
        if fresh {
            // One reading for the wish and the notice, so a wish cannot be
            // stamped a tick apart from the sentence that announced it.
            let now = web_time::Instant::now();
            if enforcing {
                REFUSED.fetch_add(1, Relaxed);
                self.counts.refused = self.counts.refused.saturating_add(1);
                // **Only where the act was actually turned away.** On the
                // advisory arm it proceeded, and retaining a wish for
                // something that already happened would re-drive it - which
                // is a worse defect than the refusal that did not occur.
                self.retain(act, pane, want, now);
            }
            // **The notice goes up on BOTH arms**, and the two say different
            // things because different things happened.
            //
            // On the advisory arm the act proceeds, so the enforcing sentence
            // would be a false statement on the glass -- it names a refusal
            // that did not occur. What is true there is that the scene went
            // past what the device will give it and may fail, and that is
            // worth saying: on the web build this is the only warning between
            // a loop the page cannot hold and the trap that ends the tab. The
            // reader can act on it -- the same share, the same scene levers --
            // which is what separates a warning from a defect.
            //
            // Until this landed the wasm refusal was invisible by
            // construction: computed, logged to a console nobody has open,
            // and never once put in front of the person whose page was about
            // to die.
            let text = refusal_text(act, refusal, self.costs.requested_percent, enforcing);
            self.raise_notice(text, now);
        }
        !enforcing
    }

    /// **Keep what was refused, so the next table can ask again.**
    ///
    /// Only a *fresh* verdict reaches this: a repeat inside one generation is
    /// answered from [`Self::refused`] without a verdict, so a door
    /// `App::hydrate_parked_panes` re-drives sixty times a second registers
    /// nothing and refreshes no clock. See [`WISH_LIFETIME`] on why the clock
    /// is the last refusal's rather than the first's.
    fn retain(&mut self, act: Act, pane: Option<usize>, want: Want, now: web_time::Instant) {
        if matches!(act.recovery(), Recovery::SelfDriven) {
            // A standing loop already holds this wish; a copy here would be a
            // second one. See `Recovery::SelfDriven`.
            return;
        }
        place(
            &mut self.pending,
            PendingWish {
                act,
                pane,
                want,
                wished_at: now,
            },
        );
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

    /// **Put a notice up, and count that it went up** — see
    /// [`NOTICES_RAISED`].
    ///
    /// The `raised_live` half is read **before** the write, because after it
    /// there is always a notice showing: what is being counted is whether this
    /// stamp replaced a sentence the reader could still see, which is the
    /// difference between a notice that flickers and one that follows another.
    ///
    /// Every stamp goes through here — a refusal on either arm, a re-offer,
    /// and [`Self::adopt_remote_notice`] carrying the App door's sentence
    /// across the seam. That last one is already guarded by a text compare, so
    /// a notice re-stated unchanged every frame does not count and does not
    /// re-stamp; a *changed* remote sentence does both, which is true.
    pub fn raise_notice(&mut self, text: String, now: web_time::Instant) {
        let onto_a_live_one = self.notice(now).is_some();
        NOTICES_RAISED.fetch_add(1, Relaxed);
        self.counts.raised = self.counts.raised.saturating_add(1);
        if onto_a_live_one {
            NOTICES_RAISED_LIVE.fetch_add(1, Relaxed);
            self.counts.raised_live = self.counts.raised_live.saturating_add(1);
        }
        self.notice = Some(AdmissionNotice {
            text,
            raised_at: now,
        });
    }
}

/// **Put `wish` in `set` under its own key**, replacing whatever question it
/// repeats.
///
/// The key is `(Act::retention_key, pane)`: one entry per question, the later
/// ask winning, because a user who asked for three panes and then for two
/// wants two. That is what bounds both sets - see [`PENDING_CAP`], which is
/// the backstop behind this rather than the bound itself.
///
/// Pushed at the back, so the set reads oldest-first and
/// [`AdmissionLedger::revisit`] can take the newest resolved wish as the one
/// to name on the glass.
///
/// The cap drops the **oldest** wish, which is the one whose gesture is
/// furthest away. It is unreachable while the key holds, and a wish dropped
/// under it is a wish the user is not told about - so it is a backstop against
/// an unbounded `Vec`, and never a policy.
fn place(set: &mut Vec<PendingWish>, wish: PendingWish) {
    set.retain(|held| {
        held.act.retention_key() != wish.act.retention_key() || held.pane != wish.pane
    });
    if set.len() >= PENDING_CAP {
        set.remove(0);
    }
    set.push(wish);
}

/// **What a wish that started fitting says**, for the acts the application
/// will not perform on the user's behalf ([`Recovery::Reoffer`]).
///
/// Three things, in the order a reader needs them: that the wall is gone, what
/// it was about, and the gesture that gets it. The last is what separates this
/// from a status line — a sentence saying only "there is room now" is a
/// notification about something the reader has no move to make on.
///
/// **It never claims the act happened.** The whole reason these acts are
/// re-offered rather than re-driven is that the application did not do them,
/// and a sentence in the past tense here would be the same false statement on
/// the glass the advisory arm's opening clause exists to avoid.
///
/// Every variant has an arm, including the ones [`Act::recovery`] never sends
/// here, so a change of policy on any act finds a sentence already written
/// rather than a wildcard.
const fn reoffer_text(act: Act) -> &'static str {
    match act {
        Act::ShowLayer => "There is room for this layer now - turn it on again.",
        Act::DefaultLayers => "There is room for this pane's layers now.",
        Act::AdoptLayers => "There is room for the linked panes' layers now.",
        Act::Panes { .. } => "There is room for another pane now - try the split again.",
        Act::Preset => "There is room for this preset now - apply it again.",
        Act::ArmLoop | Act::LoopFrames => {
            "There is room for this loop now - turn the loop back on."
        }
        Act::LoopSpan => "There is room for a longer lookback now - move the slider again.",
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
///
/// `enforced` selects the opening clause. Both arms raise a notice - see
/// [`AdmissionLedger::decide`] - and only the enforcing one may say the act
/// was refused, because only there was it.
fn refusal_text(act: Act, refusal: Refusal, percents: (u8, u8), enforced: bool) -> String {
    let short = refusal.short_bytes().div_ceil(1000 * 1000);
    let (gpu, host) = percents;
    let movable = |percent: u8| percent < SHARE_MAX_PERCENT;
    let (gpu_movable, host_movable) = match refusal.pool {
        Pool::Gpu => (movable(gpu), false),
        Pool::Host => (false, movable(host)),
        Pool::Joint => (movable(gpu), movable(host)),
    };
    // **The opening clause is the only part that differs by arm**, and the
    // tail - what the reader can move - is the same either way, because the
    // levers do not depend on whether the door turned the act away.
    //
    // The enforcing sentence states a refusal. On the advisory arm no refusal
    // happened, so saying one did would be a false statement on the glass;
    // what is true there is that the scene went past what the device will
    // give it and was let through regardless.
    let head = |pool: &str| {
        if enforced {
            format!(
                "Not enough {pool}memory for {} - {short} MB short.",
                act.noun(),
            )
        } else {
            format!(
                "Over the {pool}memory budget for {} by {short} MB - allowed \
                 anyway, and the page may fail.",
                act.noun(),
            )
        }
    };
    let after = follow_up(act, enforced);
    if !gpu_movable && !host_movable {
        return format!(
            "{} This device has no more to give it: {}.{after}",
            head(""),
            scene_lever(act),
        );
    }
    match refusal.pool {
        Pool::Gpu => format!(
            "{} Raise \"GPU memory\" in Settings > Memory (now {gpu} %).{after}",
            head("GPU "),
        ),
        Pool::Host => format!(
            "{} Raise \"System memory\" in Settings > Memory (now {host} %).{after}",
            head("system "),
        ),
        // One memory, and whichever share is still short of its stop is the
        // one that moves the wall. Naming a share already at 100 % beside a
        // movable one would send the reader to the dead control half the time.
        Pool::Joint if gpu_movable && host_movable => format!(
            "{} This machine shares one pool between the display and the \
             system: raise \"GPU memory\" (now {gpu} %) or \"System memory\" \
             (now {host} %) in Settings > Memory.{after}",
            head(""),
        ),
        Pool::Joint if gpu_movable => format!(
            "{} This machine shares one pool between the display and the \
             system: raise \"GPU memory\" in Settings > Memory (now {gpu} %).{after}",
            head(""),
        ),
        Pool::Joint => format!(
            "{} This machine shares one pool between the display and the \
             system: raise \"System memory\" in Settings > Memory (now {host} %).{after}",
            head(""),
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
        Act::ArmLoop | Act::LoopFrames | Act::LoopSpan => {
            "shorten the lookback, or turn off a layer"
        }
        Act::Panes { .. } | Act::Preset => "close a pane, or turn off a layer",
        Act::ShowLayer | Act::DefaultLayers | Act::AdoptLayers => {
            "turn off another layer, shorten a lookback, or close a pane"
        }
    }
}

/// **What the reader must do after the lever, where the act is not re-asked
/// for them.**
///
/// The loop *arm* is re-asked for the user the moment the App publishes a
/// table with room — `App::hydrate_parked_panes` re-drives it off its own
/// parked queue and the memo clearing is what releases it — so for that act
/// "lower the lookback" is the whole instruction: the user changes it and the
/// loop arrives. The listing door is not re-driven, deliberately — that would
/// put a fresh frame listing on the network on every table — so lowering the
/// lookback alone does **nothing** visible, and a notice stopping there would
/// leave the reader having done exactly what they were told with no result.
/// That is a worse experience than the refusal it followed.
///
/// What the listing door gets instead is the other end of the same sentence:
/// its wish is retained, and [`reoffer_text`] tells the reader when the room
/// they freed is enough. See [`Recovery::Reoffer`].
///
/// **Empty on the advisory arm**, where the loop was let through and is
/// already running: telling someone to turn back on a thing that is playing
/// is the same false statement in the other direction.
const fn follow_up(act: Act, enforced: bool) -> &'static str {
    match act {
        Act::LoopFrames if enforced => " Then turn the loop back on.",
        _ => "",
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
        Act::LoopFrames => "listing a loop",
        Act::LoopSpan => "widening the lookback",
    }
}

#[path = "admission/tests.rs"]
#[cfg(test)]
mod tests;
