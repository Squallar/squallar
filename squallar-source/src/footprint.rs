//! **What a value owns on the heap, in bytes** — the pricing vocabulary the
//! overlay item-data heap-census families are built from.
//!
//! # What "owned" means, said once
//!
//! [`ItemFootprint::owned_bytes`] is the bytes a value holds **beyond its own
//! `size_of`**. A `Vec`'s three words live wherever the `Vec` sits — inside a
//! handler struct, inside another `Vec`'s buffer — and are already counted by
//! whoever counted that; what this adds is the buffer it points at, and
//! whatever the elements in that buffer own in turn. So a figure built from
//! this is the allocator's, and a handler holding an empty `Vec` prices at
//! zero rather than at 24.
//!
//! # An `Arc` body is priced by ONE holder
//!
//! [`Arc`] is deliberately **not** given a blanket impl. Every `Arc` in the
//! overlay layer is there because two structures share one allocation — the
//! alert list and the paint snapshot built from it hold the same
//! `Arc<Vec<OverlayFeature>>` — and a blanket impl would price that body once
//! per holder inside a single census figure, which is the double count the
//! census module forbids. The holder that *created* the body prices it
//! ([`arc_body`]); every other holder prices the pointer alone, which is
//! already inside its own `size_of`, and says so where it does.
//!
//! # It is a walk, and it runs where a walk is already being paid for
//!
//! Pricing a list is O(items). Nothing on a frame path may call these: the
//! figures they produce are computed **once, where the data is installed or
//! built**, and stored as a `u64` the census read is a load of. See
//! [`crate::handler::OverlayState::install`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Bytes this value owns on the heap **beyond its own `size_of`**.
///
/// Implemented for the shapes overlay data is made of. A scalar owns nothing
/// and answers `0` — that is a real answer, not a stub, and
/// [`impl_pod_footprint!`] is how a type states it.
pub trait ItemFootprint {
    fn owned_bytes(&self) -> u64;
}

/// **What an `Arc<T>`'s allocation costs its creator**: the two reference
/// counts, the `T` inside them, and whatever that `T` owns.
///
/// Called by the ONE holder that made the body. A second holder of the same
/// `Arc` must not call this — see the module note.
pub fn arc_body<T: ItemFootprint>(arc: &Arc<T>) -> u64 {
    // `T::owned_bytes`, spelled as a path: method syntax would resolve to the
    // `Arc` impl below and recurse until the stack runs out, which is exactly
    // what it did the first time this was written.
    (2 * size_of::<usize>() + size_of::<T>()) as u64 + T::owned_bytes(arc)
}

/// **The creating holder's price**, which is what almost every `Arc` in a
/// list of items is.
///
/// A structure that holds a SECOND clone of a body some other structure
/// created must not reach this: it prices the pointer alone, by hand, and
/// says so where it does. The two such holders in this workspace are the
/// alert paint snapshot and the storm-report row set.
impl<T: ItemFootprint> ItemFootprint for Arc<T> {
    fn owned_bytes(&self) -> u64 {
        arc_body(self)
    }
}

/// Declare that a type owns nothing on the heap.
///
/// The point of spelling it rather than defaulting it: a type that gains a
/// `String` and keeps its `impl_pod_footprint!` line is a wrong figure, and
/// the macro name is what a reviewer sees beside the new field.
#[macro_export]
macro_rules! impl_pod_footprint {
    ($($t:ty),* $(,)?) => {
        $(
            impl $crate::footprint::ItemFootprint for $t {
                fn owned_bytes(&self) -> u64 {
                    0
                }
            }
        )*
    };
}

impl_pod_footprint!(
    u8,
    u16,
    u32,
    u64,
    usize,
    i8,
    i16,
    i32,
    i64,
    isize,
    f32,
    f64,
    bool,
    char,
    chrono::NaiveDateTime,
    chrono::NaiveDate,
);

impl ItemFootprint for String {
    /// `capacity`, not `len`: the allocator is holding the capacity.
    fn owned_bytes(&self) -> u64 {
        self.capacity() as u64
    }
}

impl<T: ItemFootprint> ItemFootprint for Vec<T> {
    fn owned_bytes(&self) -> u64 {
        let buffer = (self.capacity() * size_of::<T>()) as u64;
        self.iter()
            .fold(buffer, |sum, item| sum.saturating_add(item.owned_bytes()))
    }
}

impl<T: ItemFootprint> ItemFootprint for Option<T> {
    fn owned_bytes(&self) -> u64 {
        self.as_ref().map_or(0, T::owned_bytes)
    }
}

impl<T: ItemFootprint> ItemFootprint for Box<T> {
    fn owned_bytes(&self) -> u64 {
        size_of::<T>() as u64 + T::owned_bytes(self)
    }
}

impl<A: ItemFootprint, B: ItemFootprint> ItemFootprint for (A, B) {
    fn owned_bytes(&self) -> u64 {
        self.0.owned_bytes().saturating_add(self.1.owned_bytes())
    }
}

impl<A: ItemFootprint, B: ItemFootprint, C: ItemFootprint> ItemFootprint for (A, B, C) {
    fn owned_bytes(&self) -> u64 {
        self.0
            .owned_bytes()
            .saturating_add(self.1.owned_bytes())
            .saturating_add(self.2.owned_bytes())
    }
}

/// The table's own buffer plus what its entries own.
///
/// `capacity()` is what hashbrown will hold before it grows, and its buffer is
/// one `(K, V)` per slot plus a control byte per slot. That is the table's
/// real shape, not a model of it.
impl<K: ItemFootprint, V: ItemFootprint, S> ItemFootprint for HashMap<K, V, S> {
    fn owned_bytes(&self) -> u64 {
        let buffer = (self.capacity() * (size_of::<K>() + size_of::<V>() + 1)) as u64;
        self.iter().fold(buffer, |sum, (k, v)| {
            sum.saturating_add(k.owned_bytes())
                .saturating_add(v.owned_bytes())
        })
    }
}

impl<T: ItemFootprint, S> ItemFootprint for HashSet<T, S> {
    fn owned_bytes(&self) -> u64 {
        let buffer = (self.capacity() * (size_of::<T>() + 1)) as u64;
        self.iter()
            .fold(buffer, |sum, item| sum.saturating_add(item.owned_bytes()))
    }
}

/// A type that carries no data at all — the site layer's `()`-shaped rounds
/// and the doctests' placeholders reach this.
impl ItemFootprint for () {
    fn owned_bytes(&self) -> u64 {
        0
    }
}

#[cfg(test)]
#[path = "footprint/tests.rs"]
mod tests;

// ── The levels these prices feed ─────────────────────────────────────────

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// **Bytes of installed overlay item data, summed over every live
/// [`OverlayState`](crate::handler::OverlayState) on this instance.**
///
/// A *level*, in the sense `squallar_egui::heap_census` uses: bytes resident
/// right now, moved by the two events that can change them — an install
/// replacing one generation with the next, and a state being dropped — and
/// never added to. Maintained rather than folded because the fold is O(items)
/// over a list that can be six figures long, and the census read rides the
/// frame thread.
///
/// **What is NOT in it**: the gridded layers' decoded grids, which are the
/// `overlay grids` family
/// ([`SourceHandler::resident_source_bytes`](crate::handler::SourceHandler::resident_source_bytes))
/// and are priced at zero here so the two figures do not double count; the
/// pictures rasterized from any of it; and anything a worker instance holds.
static INSTALLED_ITEM_BYTES: AtomicU64 = AtomicU64::new(0);

/// [`INSTALLED_ITEM_BYTES`] as a reading.
pub fn installed_item_bytes() -> u64 {
    INSTALLED_ITEM_BYTES.load(Relaxed)
}

/// **Bytes of RETIRED overlay item data**, summed over every live
/// `OverlayState`'s park slot — a generation an install replaced and the
/// discard seam has not drained yet.
///
/// Disjoint from [`INSTALLED_ITEM_BYTES`]: a parked generation is one the
/// installed figure no longer counts, because the install moved it out of
/// `data` in the same breath.
static PARKED_ITEM_BYTES: AtomicU64 = AtomicU64::new(0);

/// [`PARKED_ITEM_BYTES`] as a reading.
pub fn parked_item_bytes() -> u64 {
    PARKED_ITEM_BYTES.load(Relaxed)
}

/// Move a level from `was` to `now`. One `Relaxed` RMW of a difference the
/// caller already has, so the two figures cannot drift the way an add here
/// and a subtract elsewhere could.
pub(crate) fn move_level(level: &AtomicU64, was: u64, now: u64) {
    level.fetch_add(now.wrapping_sub(was), Relaxed);
}

pub(crate) fn move_installed(was: u64, now: u64) {
    move_level(&INSTALLED_ITEM_BYTES, was, now);
}

pub(crate) fn move_parked(was: u64, now: u64) {
    move_level(&PARKED_ITEM_BYTES, was, now);
}

// ── The substrate's own shapes ───────────────────────────────────────────

/// The rings, and the two labels. `geo_bounds` is a derived box of scalars
/// and `hatch` is a discriminant; both live inside the feature's own
/// `size_of`.
impl ItemFootprint for crate::feature::OverlayFeature {
    fn owned_bytes(&self) -> u64 {
        self.polygons
            .owned_bytes()
            .saturating_add(self.label.owned_bytes())
            .saturating_add(self.label2.owned_bytes())
    }
}
