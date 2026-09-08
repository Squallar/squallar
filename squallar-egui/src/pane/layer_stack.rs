//! **The pane's own layer stack — a curated list, not a view of the registry.**
//!
//! [`OverlayRegistry`](squallar_overlays::render::overlay_state::OverlayRegistry)
//! is the *build's* catalogue: every layer this binary can draw, fixed at
//! compile time by which crates registered a `SourceHandler`. [`LayerStack`] is
//! the *user's* list: which of those layers **this pane** draws, in the order
//! the user put them in.
//!
//! Before WO-SITE-CURATE the two were the same list wearing two names. Every
//! pane held a slot for every registered handler, permanently:
//! `Gui::initialize_pane_enabled` and `PaneState::adopt_handler_state` between
//! them re-derived the pane's stack from the registry on every load and every
//! toggle, so the stack was a *complete projection* of the catalogue and the
//! only per-layer state a user could express was the eye. That made three
//! things untrue at once — the catalogue could not "add" anything, the stack
//! could not lose anything, and the panel's length was a function of how many
//! source crates the build happened to link, which the architecture guarantees
//! only ever grows ([`LAYER_ID_LEDGER`](squallar_source::id::LAYER_ID_LEDGER) is
//! append-only, and adding a source is one crate's work).
//!
//! The split is this type. The registry answers *what exists*; the stack
//! answers *what this pane draws*, and the two are reconciled by a rule
//! ([`LayerStack::admits`]) rather than by assignment.

use std::ops::{Deref, DerefMut};

use squallar_source::id::LayerId;

use super::LayerSlot;

/// **What this pane pays to find out *where* a layer sits in its own stack** —
/// the `slot lookups` family, always on and thread-local.
///
/// The sibling of
/// [`lookup_ledger`](squallar_overlays::render::overlay_state::lookup_ledger),
/// which prices the same question asked of the *registry*. Three quantities
/// with three different denominators, never added:
///
/// - a **lookup** is one `&LayerId -> Option<usize>` resolution asked of
///   [`LayerStack::position_of`]. Every `PaneState::slot`, `slot_mut`,
///   `layer_ref`, `is_overlay_enabled`, `layer_opacity`, `time_state` and
///   `PaneView::layer` is one, so this counts the walk's *questions of the
///   pane*.
/// - a **mark** is one 8-byte fingerprint compared while answering one. The
///   fingerprints are a dense `Vec<u64>` beside the slot list, so a mark is a
///   register compare over a contiguous array rather than a stride over a
///   `LayerSlot`.
/// - a **compare** is one full `LayerId == LayerId`, which is the string
///   compare the scan used to do per slot. A fingerprint can reject but never
///   accept, so every answer this returns has been confirmed by one of these,
///   and **`compares / lookups` is 1.00 on a hit and 0 on a miss** — never the
///   list's length.
///
/// Thread-local rather than atomic, for the reason the registry's is: the walk
/// runs on the frame thread and a `lock xadd` per mark would cost more than the
/// mark. A reader sees **its own thread's** figures.
///
/// **What the counting costs**, since a ledger with no figure for itself is the
/// defect it exists to find: one thread-local read-modify-write per *lookup* —
/// not per mark and not per counter, because the three numbers share one
/// `Cell`. The marks and compares of a whole scan are summed in registers and
/// handed over once.
pub mod slot_ledger {
    use std::cell::Cell;

    /// The three counters, in one cell so one TLS access reaches all of them.
    #[derive(Clone, Copy, Default)]
    struct Counts {
        lookups: u64,
        marks: u64,
        compares: u64,
    }

    thread_local! {
        static COUNTS: Cell<Counts> = const {
            Cell::new(Counts {
                lookups: 0,
                marks: 0,
                compares: 0,
            })
        };
    }

    /// One resolution, having compared `marks` fingerprints and made
    /// `compares` full id comparisons doing it.
    pub(super) fn note(marks: u64, compares: u64) {
        COUNTS.with(|c| {
            let mut counts = c.get();
            counts.lookups = counts.lookups.wrapping_add(1);
            counts.marks = counts.marks.wrapping_add(marks);
            counts.compares = counts.compares.wrapping_add(compares);
            c.set(counts);
        });
    }

    /// `(lookups, marks, compares)` on **this thread** since the last
    /// [`reset`].
    pub fn read() -> (u64, u64, u64) {
        COUNTS.with(|c| {
            let counts = c.get();
            (counts.lookups, counts.marks, counts.compares)
        })
    }

    /// Zero this thread's three counters.
    pub fn reset() {
        COUNTS.with(|c| c.set(Counts::default()));
    }
}

/// **A layer this pane used to hold and no longer does**, with what it held.
///
/// Two facts, and both are load-bearing:
///
/// * **The id.** A removal that is not written down is not a removal: the
///   reconcile rule would hand the layer straight back on the next frame,
///   because "registered, default-on, and this pane has no slot for it" is
///   exactly the shape of a layer that has just been *registered*. The
///   tombstone is what tells those two apart, and it is why removal persists
///   as a list of its own rather than as an absence.
/// * **The config.** A layer carries per-pane settings — an outlook's day, a
///   lightning window, a model parameter — and throwing them away on removal
///   would make an accidental click cost work that cannot be undone. Re-adding
///   from the catalogue restores them.
#[derive(Clone, Debug, PartialEq)]
pub struct RemovedLayer {
    pub id: LayerId,
    /// The slot's `config` as it stood when the layer left, `Null` for a layer
    /// that had saved nothing.
    pub config: serde_json::Value,
    /// The slot's opacity as it stood when the layer left, `None` for a layer
    /// still at its default. Kept for the same reason `config` is: a removal
    /// the user undoes should cost them nothing.
    pub opacity: Option<f32>,
}

/// **One pane's curated layer stack**: the slots it draws, bottom to top, plus
/// the tombstones of the layers it has been curated to exclude.
///
/// The vector's order **is** the draw order — the invariant [`LayerSlot`]'s own
/// doc describes — and every mutation that can break the curation invariant
/// goes through a method here. Reads go through [`Deref`] to `[LayerSlot]`,
/// which is why the fifty-odd `pane.layers.iter()` sites did not have to move:
/// reading the stack was never the problem.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LayerStack {
    slots: guarded::Slots,
    /// Ids this pane has excluded, in removal order. Not a `HashSet`: it
    /// persists, and a set would write a different file on every save.
    removed: Vec<RemovedLayer>,
}

/// **The slot list and its position index, behind a wall the rest of this file
/// cannot reach over.**
///
/// The index answers "where does `id` sit" without striding the slot list, and
/// a *stale* one is a wrong answer rather than a slow one — so the thing that
/// must be true is not "every mutation site remembers to invalidate" but "no
/// mutation site can fail to". Both fields are private **to this module**, so
/// the only way any code outside it — including the rest of `LayerStack` —
/// can touch a slot mutably is [`Slots::as_mut`], and that invalidates on the
/// way through. A door that has not been written yet invalidates too.
///
/// The index is a `Vec<u64>` of **fingerprints**, one per slot, rebuilt lazily
/// on the first read after an invalidation. A fingerprint packs the id's
/// length and its first, middle and last byte, so it can *reject* a candidate
/// without touching the string but can never *accept* one: every position this
/// returns has been confirmed by a full `LayerId` comparison. That is the
/// whole saving — the scan that used to make N string comparisons over a
/// `LayerSlot`-strided list now makes N register comparisons over 8-byte
/// elements and exactly one string comparison.
///
/// **What a rebuild costs**, because an index whose maintenance is unpriced is
/// the optimisation this campaign keeps finding: one pass over the slots, four
/// byte loads each, into a `Vec` whose allocation is reused. At the eighteen
/// slots the shipped registry fills that is one of the scans it replaces, and
/// the walk makes scores of them between mutations.
mod guarded {
    use std::cell::{Cell, RefCell};

    use squallar_source::id::LayerId;

    use super::LayerSlot;
    use super::slot_ledger;

    /// No slot remembered — [`Slots::last`]'s empty value.
    const NO_SLOT: usize = usize::MAX;

    pub(super) struct Slots {
        slots: Vec<LayerSlot>,
        /// One fingerprint per slot, in `slots`' own order. Meaningful only
        /// while `fresh`.
        marks: RefCell<Vec<u64>>,
        /// Whether `marks` describes `slots` as it stands.
        fresh: Cell<bool>,
        /// **The position the last lookup resolved to**, or [`NO_SLOT`].
        ///
        /// The walk asks the pane three or four questions about the *same*
        /// layer in a row — enabled, then opacity, then its `PaneRef`, then
        /// its texture — so a one-entry memo answers most lookups with a
        /// single comparison and no scan at all. Self-verifying: the id at
        /// the remembered position is compared before it is believed, so the
        /// memo can only ever be a shortcut, never an answer.
        last: Cell<usize>,
    }

    /// A dense stand-in for one id: its length and its first, middle and last
    /// byte. Rejects on any of the four differing; accepts nothing on its own.
    fn mark(id: &LayerId) -> u64 {
        let bytes = id.as_str().as_bytes();
        let len = bytes.len();
        ((len as u64) << 32)
            | (u64::from(bytes.first().copied().unwrap_or(0)) << 16)
            | (u64::from(bytes.get(len / 2).copied().unwrap_or(0)) << 8)
            | u64::from(bytes.last().copied().unwrap_or(0))
    }

    impl Slots {
        pub(super) fn from_vec(slots: Vec<LayerSlot>) -> Self {
            Self {
                slots,
                marks: RefCell::new(Vec::new()),
                fresh: Cell::new(false),
                last: Cell::new(NO_SLOT),
            }
        }

        pub(super) fn as_slice(&self) -> &[LayerSlot] {
            &self.slots
        }

        /// **The one mutable door.** Every structural change and every element
        /// rewrite in this crate reaches the slots through here, and the index
        /// is dropped on the way in — before the caller has had a chance to
        /// move anything.
        pub(super) fn as_mut(&mut self) -> &mut Vec<LayerSlot> {
            self.fresh.set(false);
            self.last.set(NO_SLOT);
            &mut self.slots
        }

        /// Where `id` sits, or `None` for an id this stack does not hold —
        /// [`Iterator::position`]'s answer, by construction: the first
        /// confirmed match wins.
        pub(super) fn position_of(&self, id: &LayerId) -> Option<usize> {
            // The memo, checked before anything is built. One comparison, and
            // it is the same comparison the scan would have ended on.
            let remembered = self.last.get();
            if let Some(slot) = self.slots.get(remembered)
                && slot.id == *id
            {
                slot_ledger::note(0, 1);
                return Some(remembered);
            }
            let wanted = mark(id);
            let mut marks = self.marks.borrow_mut();
            if !self.fresh.get() {
                marks.clear();
                marks.extend(self.slots.iter().map(|slot| mark(&slot.id)));
                self.fresh.set(true);
            }
            let mut scanned = 0u64;
            let mut compares = 0u64;
            let mut found = None;
            for (idx, held) in marks.iter().enumerate() {
                scanned += 1;
                if *held == wanted {
                    compares += 1;
                    if self.slots[idx].id == *id {
                        found = Some(idx);
                        break;
                    }
                }
            }
            slot_ledger::note(scanned, compares);
            if let Some(idx) = found {
                self.last.set(idx);
            }
            found
        }
    }

    /// **The index is derived state and never travels.** A clone comes up with
    /// nothing built, because a clone's `marks` would describe the original's
    /// slots and the first read rebuilds them anyway.
    impl Clone for Slots {
        fn clone(&self) -> Self {
            Self::from_vec(self.slots.clone())
        }
    }

    /// Derived state again: two stacks holding the same slots are the same
    /// stack, whether or not either has built its index.
    impl PartialEq for Slots {
        fn eq(&self, other: &Self) -> bool {
            self.slots == other.slots
        }
    }

    impl std::fmt::Debug for Slots {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.slots.fmt(f)
        }
    }

    impl Default for Slots {
        fn default() -> Self {
            Self::from_vec(Vec::new())
        }
    }
}

impl Deref for LayerStack {
    type Target = [LayerSlot];
    fn deref(&self) -> &[LayerSlot] {
        self.slots.as_slice()
    }
}

/// Mutable **element** access, not mutable *structure* access: `&mut [T]` can
/// reorder and rewrite slots but cannot insert or remove one, so the curation
/// invariant stays behind the methods below. It **can** move an id, which is
/// why it goes through [`guarded::Slots::as_mut`] like everything else and
/// drops the position index on the way.
impl DerefMut for LayerStack {
    fn deref_mut(&mut self) -> &mut [LayerSlot] {
        self.slots.as_mut()
    }
}

impl<'a> IntoIterator for &'a LayerStack {
    type Item = &'a LayerSlot;
    type IntoIter = std::slice::Iter<'a, LayerSlot>;
    fn into_iter(self) -> Self::IntoIter {
        self.slots.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a mut LayerStack {
    type Item = &'a mut LayerSlot;
    type IntoIter = std::slice::IterMut<'a, LayerSlot>;
    fn into_iter(self) -> Self::IntoIter {
        self.slots.as_mut().iter_mut()
    }
}

impl LayerStack {
    /// Empty the stack — slots and tombstones both. A stack that holds nothing
    /// has excluded nothing either; keeping the tombstones would make the next
    /// reconcile refuse to fill an empty pane.
    pub fn clear(&mut self) {
        self.slots.as_mut().clear();
        self.removed.clear();
    }

    /// A stack from a config file: the slots it named, and the removals it
    /// recorded.
    pub fn from_parts(slots: Vec<LayerSlot>, removed: Vec<RemovedLayer>) -> Self {
        Self {
            slots: guarded::Slots::from_vec(slots),
            removed,
        }
    }

    /// The tombstones, for the save path.
    pub fn removed(&self) -> &[RemovedLayer] {
        &self.removed
    }

    /// Whether this pane has been curated to exclude `id`.
    pub fn is_removed(&self, id: &LayerId) -> bool {
        self.removed.iter().any(|gone| gone.id == *id)
    }

    /// Whether this pane holds a slot for `id` at all.
    pub fn holds(&self, id: &LayerId) -> bool {
        self.position_of(id).is_some()
    }

    /// **Where `id` sits in this stack**, or `None` for an id it does not
    /// hold — the one resolver. [`PaneState::slot`] and [`PaneState::slot_mut`]
    /// are both this plus an index, so a shared and a mutable lookup cannot
    /// disagree about which slot an id names and
    /// [`slot_ledger`] counts each of them exactly once.
    ///
    /// [`PaneState::slot`]: super::PaneState::slot
    /// [`PaneState::slot_mut`]: super::PaneState::slot_mut
    pub fn position_of(&self, id: &LayerId) -> Option<usize> {
        self.slots.position_of(id)
    }

    /// **The reconcile rule, in one place: may a registered handler join this
    /// stack on its own?**
    ///
    /// `default_on` is the handler's [`default_enabled`], and it is the signal
    /// on purpose. A layer that ships on is one the product asserts belongs on
    /// a fresh pane, so it arrives as a row; a layer that ships off is one the
    /// user has to ask for, and the place to ask is the catalogue. That is what
    /// keeps the panel a curated list of a dozen rows rather than an inventory
    /// of however many source crates the build links.
    ///
    /// A removed layer never rejoins, whatever it ships as: a tombstone
    /// outranks a default, or "remove" would mean "hide until the next
    /// autosave".
    ///
    /// [`default_enabled`]: squallar_source::handler::SourceHandler::default_enabled
    pub fn admits(&self, id: &LayerId, default_on: bool) -> bool {
        default_on && !self.holds(id) && !self.is_removed(id)
    }

    /// Push a slot onto the top of the stack.
    pub fn push(&mut self, slot: LayerSlot) {
        self.clear_tombstone(&slot.id);
        self.slots.as_mut().push(slot);
    }

    /// Insert a slot at `pos`, bottom-relative.
    pub fn insert(&mut self, pos: usize, slot: LayerSlot) {
        self.clear_tombstone(&slot.id);
        let slots = self.slots.as_mut();
        let at = pos.min(slots.len());
        slots.insert(at, slot);
    }

    /// Take the slots out for a whole-list rewrite, leaving the tombstones
    /// where they are — [`PaneState::set_draw_order`] is a permutation, and a
    /// permutation does not un-remove anything.
    ///
    /// [`PaneState::set_draw_order`]: super::PaneState::set_draw_order
    pub fn take_slots(&mut self) -> Vec<LayerSlot> {
        std::mem::take(self.slots.as_mut())
    }

    /// Put a rewritten slot list back. Any id in it that carried a tombstone
    /// loses it: a layer that is in the stack is, by definition, not removed
    /// from it.
    pub fn set_slots(&mut self, slots: Vec<LayerSlot>) {
        for slot in &slots {
            self.clear_tombstone(&slot.id);
        }
        *self.slots.as_mut() = slots;
    }

    /// **Curate `id` out of this pane**, keeping what it held.
    ///
    /// Returns the slot that left, or `None` for a layer this pane did not
    /// hold. The caller is what releases the layer's textures — see
    /// [`PaneState::remove_layer`], which is the door with the whole rule on
    /// it.
    ///
    /// [`PaneState::remove_layer`]: super::PaneState::remove_layer
    pub fn take_out(&mut self, id: &LayerId) -> Option<LayerSlot> {
        let pos = self.position_of(id)?;
        let slot = self.slots.as_mut().remove(pos);
        self.clear_tombstone(&slot.id);
        self.removed.push(RemovedLayer {
            id: slot.id.clone(),
            config: slot.config.clone(),
            opacity: slot.opacity,
        });
        Some(slot)
    }

    /// What `id` held when it was removed, or `Null` for a layer that was
    /// never removed or saved nothing. Read by the add path so a re-add
    /// restores settings rather than resetting them.
    pub fn saved_config_of_removed(&self, id: &LayerId) -> serde_json::Value {
        self.removed
            .iter()
            .find(|gone| gone.id == *id)
            .map_or(serde_json::Value::Null, |gone| gone.config.clone())
    }

    /// The opacity `id` held when it was removed, or `None` for a layer that
    /// was never removed or was at its default. The add path's other read.
    pub fn saved_opacity_of_removed(&self, id: &LayerId) -> Option<f32> {
        self.removed
            .iter()
            .find(|gone| gone.id == *id)
            .and_then(|gone| gone.opacity)
    }

    /// Forget a tombstone. Called by every path that puts a slot back, so
    /// "removed" cannot describe a layer that is visibly in the list.
    fn clear_tombstone(&mut self, id: &LayerId) {
        self.removed.retain(|gone| gone.id != *id);
    }

    /// Replace this stack with `other`'s — the layer-link sync's whole-stack
    /// copy. **The tombstones travel with the slots**: linked panes share a
    /// layer arrangement, and a copy that brought the slots without the
    /// removals would hand the destination pane every removed layer back on
    /// its next reconcile.
    pub fn adopt(&mut self, other: &LayerStack) {
        *self.slots.as_mut() = other.slots.as_slice().to_vec();
        self.removed = other.removed.clone();
    }
}

#[cfg(test)]
mod tests;
