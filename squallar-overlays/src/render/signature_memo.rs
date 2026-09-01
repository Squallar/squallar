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

/// **Which items an as-of filter admits, as one number** — the body of
/// [`SourceHandler::as_of_signature`] for every layer whose picture at an
/// instant is "the items in force then".
///
/// `admitted` yields one `bool` per item, in the layer's own storage order,
/// and this folds the *positions* of the `true`s. Positions rather than item
/// ids because an id is a `String` on both layers that implement this and
/// hashing a few thousand of them once per pane per frame is the cost this
/// whole mechanism exists to avoid; the order of a layer's storage is stable
/// for as long as its `data_generation` is, and that generation is already an
/// input to the token this value is mixed into. So two instants collide here
/// only if they admit the same items, which is exactly the contract.
///
/// The fold is multiplicative and therefore **order-sensitive**: a plain XOR
/// would give `{a, b}` and `{b, a}` the same value, and — worse — would give
/// the empty set and any pair `{i, i}` the same value as each other. Nothing
/// here allocates, hashes a `String`, or takes a lock.
pub(crate) fn as_of_identity(admitted: impl Iterator<Item = bool>) -> u64 {
    let mut folded = 0u64;
    for (idx, keep) in admitted.enumerate() {
        if keep {
            folded = folded.wrapping_mul(0x0000_0100_0000_01b3) ^ (idx as u64 + 1);
        }
    }
    folded
}

#[cfg(test)]
mod as_of_identity_tests {
    use super::as_of_identity;

    /// The sets a sweeping clock actually walks through are **neighbours** —
    /// one item joins or leaves — so those are the pairs that must not
    /// collide. A collision here is not a wasted raster: it is a picture the
    /// pane never rebuilds, so an alert that issued stays off the glass.
    #[test]
    fn neighbouring_valid_sets_do_not_share_an_identity() {
        let of = |keep: &[bool]| as_of_identity(keep.iter().copied());

        // Nothing valid, and the empty answer must not equal any real set.
        let empty = of(&[false, false, false]);
        assert_ne!(empty, of(&[true, false, false]), "one item joined");
        assert_ne!(empty, of(&[false, true, false]), "a different item joined");

        // One item joining or leaving — every position, against its
        // predecessor and against every other single-item set.
        let singles: Vec<u64> = (0..8)
            .map(|i| {
                let mut keep = [false; 8];
                keep[i] = true;
                of(&keep)
            })
            .collect();
        for (i, a) in singles.iter().enumerate() {
            for (j, b) in singles.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "item {i} and item {j} alone are one picture");
                }
            }
        }

        // A superset must differ from its subset: the case where an alert
        // issues while the ones already up stay up, which is the ordinary
        // shape of a warning sweep.
        assert_ne!(
            of(&[true, false, true, false]),
            of(&[true, true, true, false]),
            "a warning issued between two that were already valid",
        );
        assert_ne!(
            of(&[true, true, true, false]),
            of(&[true, true, false, false]),
            "a warning expired and left the two before it standing",
        );
    }

    /// The same set is the same number, however often it is asked for — the
    /// half that makes the whole mechanism a saving rather than a wash.
    #[test]
    fn the_same_valid_set_is_the_same_identity() {
        let keep = [true, false, true, true, false];
        assert_eq!(
            as_of_identity(keep.iter().copied()),
            as_of_identity(keep.iter().copied()),
        );
    }

    /// **Order-sensitivity, which is why the fold multiplies rather than
    /// XORs.** A plain XOR gives `{0, 1}` and `{1, 0}` the same value — and,
    /// worse, gives any `{i, i}` the value of the empty set.
    #[test]
    fn the_fold_is_not_a_bare_xor() {
        // Two items at positions 2 and 5, admitted in the layer's order, must
        // not equal the pair read the other way round.
        let forward = as_of_identity([false, false, true, false, false, true].into_iter());
        let backward = as_of_identity([false, true, false, false, true, false].into_iter());
        assert_ne!(forward, backward, "positions must not commute away");
    }
}
