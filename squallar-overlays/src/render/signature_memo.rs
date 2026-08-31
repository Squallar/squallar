//! Memoization for the `content_signature` folds that walk the item set.

use std::cell::{Cell, RefCell};

/// One remembered `content_signature` answer per (data generation, view key).
///
/// The signature fold is O(items) and its callers ask per pane per layer per
/// frame, while its inputs move only on a poll or a toggle. The key has two
/// halves: `generation` covers everything `data_generation` moves on, and
/// `view_key` covers the filter inputs a caller can change *without* a
/// generation bump — a pane's category set, the dismissed-id set. Rows are
/// kept per view key so two panes filtering the same layer differently do not
/// evict each other; a generation move clears them all, which is what bounds
/// the table to the views seen since the last poll.
pub(crate) struct SignatureMemo {
    generation: Cell<u64>,
    /// (view_key, signature) rows for the current generation.
    rows: RefCell<Vec<(u64, u64)>>,
}

impl SignatureMemo {
    pub fn new() -> Self {
        Self {
            generation: Cell::new(0),
            rows: RefCell::new(Vec::new()),
        }
    }

    /// The signature for `(generation, view_key)`, running `fold` only on the
    /// first ask since either moved.
    pub fn get_or_compute(
        &self,
        generation: u64,
        view_key: u64,
        fold: impl FnOnce() -> u64,
    ) -> u64 {
        if self.generation.get() != generation {
            self.rows.borrow_mut().clear();
            self.generation.set(generation);
        }
        if let Some(&(_, sig)) = self.rows.borrow().iter().find(|(key, _)| *key == view_key) {
            return sig;
        }
        let sig = fold();
        self.rows.borrow_mut().push((view_key, sig));
        sig
    }
}
