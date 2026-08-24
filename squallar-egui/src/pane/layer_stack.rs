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
    slots: Vec<LayerSlot>,
    /// Ids this pane has excluded, in removal order. Not a `HashSet`: it
    /// persists, and a set would write a different file on every save.
    removed: Vec<RemovedLayer>,
}

impl Deref for LayerStack {
    type Target = [LayerSlot];
    fn deref(&self) -> &[LayerSlot] {
        &self.slots
    }
}

/// Mutable **element** access, not mutable *structure* access: `&mut [T]` can
/// reorder and rewrite slots but cannot insert or remove one, so the curation
/// invariant stays behind the methods below.
impl DerefMut for LayerStack {
    fn deref_mut(&mut self) -> &mut [LayerSlot] {
        &mut self.slots
    }
}

impl<'a> IntoIterator for &'a LayerStack {
    type Item = &'a LayerSlot;
    type IntoIter = std::slice::Iter<'a, LayerSlot>;
    fn into_iter(self) -> Self::IntoIter {
        self.slots.iter()
    }
}

impl<'a> IntoIterator for &'a mut LayerStack {
    type Item = &'a mut LayerSlot;
    type IntoIter = std::slice::IterMut<'a, LayerSlot>;
    fn into_iter(self) -> Self::IntoIter {
        self.slots.iter_mut()
    }
}

impl LayerStack {
    /// Empty the stack — slots and tombstones both. A stack that holds nothing
    /// has excluded nothing either; keeping the tombstones would make the next
    /// reconcile refuse to fill an empty pane.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.removed.clear();
    }

    /// A stack from a config file: the slots it named, and the removals it
    /// recorded.
    pub fn from_parts(slots: Vec<LayerSlot>, removed: Vec<RemovedLayer>) -> Self {
        Self { slots, removed }
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
        self.slots.iter().any(|slot| slot.id == *id)
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
        self.slots.push(slot);
    }

    /// Insert a slot at `pos`, bottom-relative.
    pub fn insert(&mut self, pos: usize, slot: LayerSlot) {
        self.clear_tombstone(&slot.id);
        self.slots.insert(pos.min(self.slots.len()), slot);
    }

    /// Take the slots out for a whole-list rewrite, leaving the tombstones
    /// where they are — [`PaneState::set_draw_order`] is a permutation, and a
    /// permutation does not un-remove anything.
    ///
    /// [`PaneState::set_draw_order`]: super::PaneState::set_draw_order
    pub fn take_slots(&mut self) -> Vec<LayerSlot> {
        std::mem::take(&mut self.slots)
    }

    /// Put a rewritten slot list back. Any id in it that carried a tombstone
    /// loses it: a layer that is in the stack is, by definition, not removed
    /// from it.
    pub fn set_slots(&mut self, slots: Vec<LayerSlot>) {
        for slot in &slots {
            self.clear_tombstone(&slot.id);
        }
        self.slots = slots;
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
        let pos = self.slots.iter().position(|slot| slot.id == *id)?;
        let slot = self.slots.remove(pos);
        self.clear_tombstone(&slot.id);
        self.removed.push(RemovedLayer {
            id: slot.id.clone(),
            config: slot.config.clone(),
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
        self.slots = other.slots.clone();
        self.removed = other.removed.clone();
    }
}

#[cfg(test)]
mod tests;
