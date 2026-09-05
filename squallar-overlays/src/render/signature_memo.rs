//! Memoization for the `content_signature` folds that walk the item set, and
//! for the built job inputs `prepare_job` hands the dispatch.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use squallar_source::job::DescribedJob;

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

/// How many built inputs a [`JobMemo`] keeps per generation. A loop that
/// fills thirteen frames at thirteen instants builds thirteen distinct
/// inputs and gains nothing from remembering them all; the live pane and a
/// second pane on another view are the rows that repeat. Small on purpose:
/// every row is a whole paint input held for the generation.
const JOB_MEMO_ROWS: usize = 4;

/// The last built values per (data generation, view key), so a dispatch
/// whose inputs have not moved hands back a refcount clone and builds
/// nothing. `T` is a whole [`DescribedJob`] where every term of the input is
/// in the key ([`JobMemo`]), or the `Arc`'d row set alone where a scalar
/// that moves per dispatch — METAR's and the storm reports' `zoom`, the
/// reports' `as_of` — sits beside it in the input.
///
/// `prepare_job` runs on the frame thread once per surviving request and its
/// body is O(items) with an allocation per item — a `String` and an `Arc`
/// per alert, a polygon per discussion, every feature of every in-force
/// outlook — while what it builds moves only on a poll, a toggle or a scrub
/// that changes the admitted set. The key is the caller's: `generation` is
/// everything `data_generation` moves on and `view_key` is a fold of every
/// other term that reaches the bytes — the pane's filters, the admitted-set
/// identity at `as_of`, `device_scale`, `is_dark` where the layer reads it.
/// **`now` is never a term**: it moves every frame and no memoised layer's
/// input carries it.
///
/// A [`DescribedJob`] is already `Arc<dyn JobInput>`, so a hit is one
/// refcount increment and the worker, the wire encoder and this table all
/// share one payload.
///
/// **Rows retired by a rollover are parked, not dropped.** What parking buys
/// is that the free happens where the app chooses rather than on the frame
/// thread, and that is worth having for two different reasons depending on
/// the input's shape. An input with per-row drop glue — a row set of owned
/// strings — costs one free per row, so a thousand rows is a thousand frees
/// and the COUNT is the cost. An input that is one block with no drop glue —
/// a slab-backed row set — costs exactly one `dealloc`, but of a block that
/// may be megabytes, and a free that large can return pages to the system
/// rather than merely update a pointer, so there the SIZE is the cost. **A
/// single-block value is therefore not a poor candidate for parking**, which
/// the count argument alone would wrongly imply.
///
/// [`Self::take_retired`] hands the parked rows to the drain at the end of
/// every frame the UI runs. While anything is animating that bounds a
/// parked value to about one frame; if the app is idle or backgrounded and
/// repaints stop, it waits for whatever wakes the UI instead — small and
/// bounded, but not a frame interval. Until the drain runs, the slot holds
/// the rows of the most recent rollover only — an undrained older batch is
/// dropped when the next one arrives. Evictions inside one generation park
/// too, and the slot is capped at `JOB_MEMO_ROWS` there as well: past that
/// the oldest parked input is freed inline, which is what happened before
/// this memo existed. So the parked footprint never exceeds `JOB_MEMO_ROWS`
/// inputs by either path, drained or not.
pub(crate) struct BuiltMemo<T> {
    generation: Cell<u64>,
    /// (view_key, value) rows for the current generation, oldest first.
    rows: RefCell<Vec<(u64, T)>>,
    /// The rows the last rollover or eviction retired, awaiting
    /// [`Self::take_retired`].
    retired: RefCell<Vec<T>>,
    /// **What [`Self::retired`] is holding**, maintained as rows go in and
    /// out rather than folded on read: the price of one input is a walk of
    /// it, and the census reads this on the frame thread.
    parked_bytes: Cell<u64>,
    /// What one row of this memo owns on the heap. A `fn`, not a closure:
    /// every memo's price is a plain function of the row, and refusing
    /// captures keeps the memo the same size it was.
    price: fn(&T) -> u64,
    /// How many times the build closure ran — the mechanism's count, for the
    /// gate that an unchanged key builds nothing.
    #[cfg(test)]
    pub(crate) builds: Cell<u64>,
}

/// A memo over whole described jobs — the common case.
pub(crate) type JobMemo = BuiltMemo<DescribedJob>;

impl<T: Clone> BuiltMemo<T> {
    /// `price` answers what freeing one row would give back — see
    /// [`crate::render::footprint`], where every memo's is written. It is
    /// **required**, not defaulted: a memo constructed without one would
    /// publish a silent zero into a census family whose whole purpose is to
    /// find what nobody is counting.
    pub fn new(price: fn(&T) -> u64) -> Self {
        Self {
            generation: Cell::new(0),
            rows: RefCell::new(Vec::new()),
            retired: RefCell::new(Vec::new()),
            parked_bytes: Cell::new(0),
            price,
            #[cfg(test)]
            builds: Cell::new(0),
        }
    }

    /// Move [`Self::parked_bytes`] and the module level together, so the
    /// per-memo figure and the census level cannot drift apart.
    fn set_parked(&self, bytes: u64) {
        move_parked(self.parked_bytes.get(), bytes);
        self.parked_bytes.set(bytes);
    }

    /// What this memo's parked slot is holding right now.
    #[cfg(test)]
    pub fn parked_bytes(&self) -> u64 {
        self.parked_bytes.get()
    }

    /// The value for `(generation, view_key)`, running `build` only on the
    /// first ask since either moved. A `build` that answers `None` is not
    /// remembered: the layer had nothing to draw, and its next ask is as
    /// cheap as this one was.
    pub fn get_or_build(
        &self,
        generation: u64,
        view_key: u64,
        build: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        if self.generation.get() != generation {
            let stale = std::mem::take(&mut *self.rows.borrow_mut());
            if !stale.is_empty() {
                let batch: Vec<T> = stale.into_iter().map(|(_, value)| value).collect();
                let bytes = batch
                    .iter()
                    .fold(0u64, |sum, row| sum.saturating_add((self.price)(row)));
                *self.retired.borrow_mut() = batch;
                self.set_parked(bytes);
            }
            self.generation.set(generation);
        }
        if let Some((_, value)) = self.rows.borrow().iter().find(|(key, _)| *key == view_key) {
            return Some(value.clone());
        }
        #[cfg(test)]
        self.builds.set(self.builds.get() + 1);
        let value = build()?;
        let mut rows = self.rows.borrow_mut();
        if rows.len() >= JOB_MEMO_ROWS {
            let (_, evicted) = rows.remove(0);
            let mut parked = self.parked_bytes.get();
            let mut retired = self.retired.borrow_mut();
            if retired.len() >= JOB_MEMO_ROWS {
                // The slot is full and nothing has drained it. Free the
                // oldest here, which is exactly what happened before this
                // memo existed, rather than holding a parked input per
                // eviction for the whole generation.
                let freed = retired.remove(0);
                parked = parked.saturating_sub((self.price)(&freed));
            }
            parked = parked.saturating_add((self.price)(&evicted));
            retired.push(evicted);
            drop(retired);
            self.set_parked(parked);
        }
        rows.push((view_key, value.clone()));
        Some(value)
    }

    /// **Park every live row**, for a layer that has just let go of the data
    /// they were built from.
    ///
    /// Same shape as a rollover: the batch replaces whatever was parked, so
    /// the slot still holds one generation's rows at most. Without this a
    /// switched-off layer would keep its built inputs for the life of the
    /// process — nothing dispatches it any more, so no `get_or_build` would
    /// ever see the new generation and retire them.
    pub fn retire_live_rows(&self) {
        let live = std::mem::take(&mut *self.rows.borrow_mut());
        if live.is_empty() {
            return;
        }
        let batch: Vec<T> = live.into_iter().map(|(_, value)| value).collect();
        let bytes = batch
            .iter()
            .fold(0u64, |sum, row| sum.saturating_add((self.price)(row)));
        *self.retired.borrow_mut() = batch;
        self.set_parked(bytes);
    }

    /// The inputs a rollover or an eviction retired since the last drain —
    /// for the app's discard seam, so their frees happen where it chooses.
    /// Empty when nothing has retired.
    ///
    /// Drained once a frame by the handler's
    /// [`SourceHandler::take_retired`](squallar_source::handler::SourceHandler::take_retired),
    /// which the app files against the worker's discard pool. It must be a
    /// PER-FRAME drain and not a per-arrival one: these rows retire on the
    /// next `get_or_build` that sees a new generation, which is the dispatch
    /// and not the delivery.
    pub fn take_retired(&self) -> Vec<T> {
        let batch = std::mem::take(&mut *self.retired.borrow_mut());
        self.set_parked(0);
        batch
    }

    /// How many inputs are held for the current generation.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.rows.borrow().len()
    }
}

/// **A memo that goes away takes its parked slot with it.** Without this the
/// level would climb across a process by one memo's parked batch per handler
/// ever built, and read as resident memory nobody is holding.
impl<T> Drop for BuiltMemo<T> {
    fn drop(&mut self) {
        move_parked(self.parked_bytes.get(), 0);
    }
}

/// **Bytes the built-input memos have PARKED**, summed over every memo on
/// this instance — rows a rollover or an eviction retired and nothing has
/// drained yet.
///
/// A level, in `squallar_egui::heap_census`'s sense, and a **separate** one
/// from the installed item data: a parked input is a built copy that the
/// layer's own item list does not hold, except where the price says so (the
/// alert rows share their geometry with the alert list and price only the
/// pointers). So the two figures are disjoint and may be read together.
static PARKED_INPUT_BYTES: AtomicU64 = AtomicU64::new(0);

fn move_parked(was: u64, now: u64) {
    PARKED_INPUT_BYTES.fetch_add(now.wrapping_sub(was), Relaxed);
}

/// [`PARKED_INPUT_BYTES`] as a reading.
pub(crate) fn parked_input_bytes() -> u64 {
    PARKED_INPUT_BYTES.load(Relaxed)
}

/// Fold one more term into a memo key. Multiplicative with an odd constant
/// then XOR, so a term and its position both count — `(a, b)` and `(b, a)`
/// do not collide the way a bare XOR would let them.
pub(crate) fn fold_key(key: u64, term: u64) -> u64 {
    key.wrapping_mul(0x0000_0100_0000_01b3) ^ term
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
mod job_memo_tests {
    use super::{JOB_MEMO_ROWS, JobMemo};
    use squallar_source::job::DescribedJob;

    #[derive(Debug, PartialEq)]
    struct Input(u64);
    squallar_source::impl_job_input!(Input);

    fn job(v: u64) -> Option<DescribedJob> {
        Some(DescribedJob::new(Input(v)))
    }

    /// A stand-in for a real layer's price: one figure per row, so a parked
    /// count and a parked byte figure move together and either can be
    /// asserted.
    const ROW_BYTES: u64 = 64;

    fn price(_: &DescribedJob) -> u64 {
        ROW_BYTES
    }

    fn memo() -> JobMemo {
        JobMemo::new(price)
    }

    /// The whole point: a second ask under the same key builds nothing and
    /// hands back **the same allocation**, not an equal one.
    #[test]
    fn an_unchanged_key_hands_back_the_same_allocation_and_builds_nothing() {
        let memo = memo();
        let first = memo.get_or_build(3, 7, || job(1)).unwrap();
        let second = memo.get_or_build(3, 7, || job(2)).unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&first.0, &second.0),
            "a hit must be a refcount clone of the row, not a rebuild",
        );
        assert_eq!(memo.builds.get(), 1, "the closure ran once");
    }

    /// Either half of the key moving is a rebuild — and the rows of the old
    /// generation are parked, not dropped.
    #[test]
    fn a_generation_move_parks_every_row_and_rebuilds() {
        let memo = memo();
        memo.get_or_build(1, 10, || job(1));
        memo.get_or_build(1, 11, || job(2));
        assert_eq!(memo.held(), 2);
        assert!(memo.take_retired().is_empty(), "nothing retired yet");

        let fresh = memo.get_or_build(2, 10, || job(3)).unwrap();
        assert_eq!(fresh.downcast_ref::<Input>(), Some(&Input(3)));
        assert_eq!(memo.builds.get(), 3);
        assert_eq!(memo.held(), 1, "the new generation holds only its own row");
        let retired = memo.take_retired();
        assert_eq!(
            retired.len(),
            2,
            "both old rows were handed back, not freed"
        );
        assert!(memo.take_retired().is_empty(), "a drain empties the slot");
    }

    #[test]
    fn a_view_key_move_within_a_generation_is_a_second_row() {
        let memo = memo();
        memo.get_or_build(1, 10, || job(1));
        memo.get_or_build(1, 11, || job(2));
        assert_eq!(memo.builds.get(), 2);
        assert_eq!(memo.held(), 2, "two views, two rows, no eviction");
        assert!(memo.take_retired().is_empty());
    }

    /// The parked slot is bounded too. A rollover replaces it, but an
    /// eviction pushes, and nothing drains it in production yet: a gesture
    /// that walks many view keys inside one data generation must not park one
    /// input per eviction for the whole generation.
    #[test]
    fn eviction_does_not_park_without_bound_inside_one_generation() {
        let memo = memo();
        // One generation, many distinct views — a zoom gesture's quanta.
        for key in 0..64u64 {
            memo.get_or_build(7, key, || job(key));
        }
        assert_eq!(memo.held(), JOB_MEMO_ROWS, "the live table stays capped");
        let parked = memo.take_retired().len();
        assert!(
            parked <= JOB_MEMO_ROWS,
            "the parked slot holds {parked} inputs after 64 views in one \
             generation; it must never exceed {JOB_MEMO_ROWS}, the bound this \
             type's own doc claims",
        );
    }

    /// The table is bounded: past `JOB_MEMO_ROWS` views the oldest is parked.
    #[test]
    fn the_rows_are_capped_and_the_oldest_is_retired_first() {
        let memo = memo();
        for key in 0..=JOB_MEMO_ROWS as u64 {
            memo.get_or_build(1, key, || job(key));
        }
        assert_eq!(memo.held(), JOB_MEMO_ROWS);
        let retired = memo.take_retired();
        assert_eq!(retired.len(), 1);
        assert_eq!(
            retired[0].downcast_ref::<Input>(),
            Some(&Input(0)),
            "the first row in was the one evicted",
        );
        // Key 0 is gone and rebuilds; the newest is still held.
        let builds = memo.builds.get();
        memo.get_or_build(1, JOB_MEMO_ROWS as u64, || job(99));
        assert_eq!(memo.builds.get(), builds, "the newest row is still a hit");
        memo.get_or_build(1, 0, || job(0));
        assert_eq!(memo.builds.get(), builds + 1, "the evicted row rebuilds");
    }

    /// A layer with nothing to draw is not a row: its next ask is as cheap
    /// as this one, and remembering `None` would pin a generation's empty
    /// answer against a view that later has rows.
    #[test]
    fn a_none_is_not_remembered() {
        let memo = memo();
        assert!(memo.get_or_build(1, 0, || None).is_none());
        assert_eq!(memo.held(), 0);
        assert!(memo.get_or_build(1, 0, || job(1)).is_some());
        assert_eq!(memo.builds.get(), 2, "the second ask built");
    }

    /// **The parked slot's byte figure is what a drain would give back**, and
    /// it is the figure the census family reads. Asserted on both edges: it
    /// rises when a rollover parks and falls to zero when the drain takes the
    /// batch.
    #[test]
    fn the_parked_byte_figure_tracks_what_the_slot_holds() {
        let memo = memo();
        memo.get_or_build(1, 10, || job(1));
        memo.get_or_build(1, 11, || job(2));
        assert_eq!(memo.parked_bytes(), 0, "nothing has retired yet");

        memo.get_or_build(2, 10, || job(3));
        assert_eq!(
            memo.parked_bytes(),
            2 * ROW_BYTES,
            "both rows of the old generation are parked and priced",
        );
        assert_eq!(memo.take_retired().len(), 2);
        assert_eq!(memo.parked_bytes(), 0, "a drain empties the figure too");
    }

    /// The eviction path maintains the figure incrementally, and the bound on
    /// the slot is a bound on the bytes as well as on the count.
    #[test]
    fn eviction_keeps_the_byte_figure_inside_the_same_bound() {
        let memo = memo();
        for key in 0..64u64 {
            memo.get_or_build(7, key, || job(key));
        }
        assert!(
            memo.parked_bytes() <= JOB_MEMO_ROWS as u64 * ROW_BYTES,
            "the parked slot priced {} B after 64 views in one generation",
            memo.parked_bytes(),
        );
        let count = memo.take_retired().len() as u64;
        assert_eq!(memo.parked_bytes(), 0);
        assert!(count <= JOB_MEMO_ROWS as u64);
    }

    /// An undrained parked batch is replaced by the next rollover's, so the
    /// parked footprint is one generation's rows and never grows.
    #[test]
    fn an_undrained_batch_is_replaced_not_accumulated() {
        let memo = memo();
        memo.get_or_build(1, 0, || job(1));
        memo.get_or_build(2, 0, || job(2));
        memo.get_or_build(3, 0, || job(3));
        let retired = memo.take_retired();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].downcast_ref::<Input>(), Some(&Input(2)));
    }
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
