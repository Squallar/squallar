#![warn(clippy::all)]
// `deny`, not `forbid`: the one `unsafe impl` below carries a scoped `allow`,
// which `forbid` cannot be overridden by. Everything else in the crate errors.
#![deny(unsafe_code)]

//! **Live host bytes, counted at the allocator.**
//!
//! Every other pressure input in the tree is a high-water mark: a wasm
//! instance's `memory().buffer().byteLength` only ever grows, and a GPU
//! `OutOfMemory` event says nothing about recovery. A governor that lowers a
//! presumption on such a signal and cannot observe the heap coming back is a
//! ratchet. This crate is the figure that can fall: what the process has
//! allocated less what it has freed, read off two `Relaxed` atomics that the
//! global allocator bumps on every call.
//!
//! # What it is, and what it is not
//!
//! [`live_bytes`] is **bytes the allocator has handed out and not yet been
//! handed back**, on the instance whose statics these are. It is the same
//! figure on native and on wasm, because the same [`Counting`] wraps
//! `std::alloc::System` on both. It is not the process's resident set (the
//! allocator's own reserve, fragmentation and the module's statics are all
//! outside it), and on wasm it is not `byteLength` — a linear memory never
//! shrinks, so `byteLength − live_bytes` is exactly the freed-but-reserved
//! headroom the high-water mark hides.
//!
//! **Per instance, not per process, on wasm.** The rasterization worker is a
//! second instantiation of the same module with its own linear memory, and a
//! wasm static lives in one memory; its counters are its own and the page's
//! are the page's. The two are never added — they are two heaps under two
//! ceilings — and the worker's figure crosses to the page on the messages
//! that already cross (`squallar_web::worker_protocol::LIVE`).
//!
//! # The counter's own rules
//!
//! * **Count only what was granted.** The counters move on a non-null return
//!   from `alloc`, `alloc_zeroed` and `realloc`, never on the request: an
//!   allocation the system refuses must not drift the live figure upward, or
//!   the one moment the figure matters most is the moment it lies.
//! * **Nothing here allocates.** The hooks are a delegation and two atomic
//!   adds; a `log::`, a `format!` or a `Vec` inside the allocator re-enters
//!   it.
//! * **Two atomics, `Relaxed`.** A reader wants a recent figure, not a
//!   synchronised one; the cost is one fetch-add on each call, which is the
//!   prediction the frame-time lane's `frame prepare` p50/p99 gate this
//!   against.
//!
//! # Installing it
//!
//! The `#[global_allocator]` declaration is not `unsafe` and lives in the
//! binary crates — the `squallar` shell and `squallar-web` — never here, so a
//! library that depends on this crate for the reading does not also install
//! it. A process that never installed it reads [`live_bytes`] as `None`: no
//! allocation has passed through the counter, which is the honest answer and
//! not a heap of zero bytes.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::alloc::System;

/// Bytes every granted allocation has ever asked for, summed. Monotone.
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Bytes every freed allocation had, summed. Monotone.
static FREED: AtomicU64 = AtomicU64::new(0);

/// `std::alloc::System`, counted. Declare it as the global allocator in a
/// binary crate:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;
/// ```
pub struct Counting;

/// A granted block of `bytes` came into being.
#[inline]
fn granted(bytes: usize) {
    ALLOCATED.fetch_add(bytes as u64, Relaxed);
}

/// A block of `bytes` went back.
#[inline]
fn returned(bytes: usize) {
    FREED.fetch_add(bytes as u64, Relaxed);
}

/// The crate is `deny(unsafe_code)`; this impl carries the scoped allow
/// because `GlobalAlloc` is an `unsafe trait` and every method of it is an
/// `unsafe fn` — the contract is the system allocator's, delegated to
/// unchanged, and the counting happens beside the pointer, never through it.
#[allow(
    unsafe_code,
    reason = "GlobalAlloc is an unsafe trait; every call is delegated to std::alloc::System unchanged"
)]
unsafe impl GlobalAlloc for Counting {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract is `GlobalAlloc::alloc`'s, and it is
        // forwarded verbatim.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            granted(layout.size());
        }
        ptr
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as `alloc`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            granted(layout.size());
        }
        ptr
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller promises `ptr` was returned by this allocator
        // with `layout`, which is `System`'s own contract.
        unsafe { System.dealloc(ptr, layout) };
        returned(layout.size());
    }

    /// A successful `realloc` is one block returned and one granted, whatever
    /// the system did underneath; a refused one changes nothing, because the
    /// old block is still the caller's.
    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as `dealloc` for `ptr`/`layout`; `new_size` is the caller's
        // to promise, as `GlobalAlloc::realloc` requires.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            returned(layout.size());
            granted(new_size);
        }
        moved
    }
}

/// **Bytes the allocator has handed out and not yet been handed back**, on
/// this instance.
///
/// `None` when nothing has ever been granted through the counter — a process
/// or module instance that did not declare [`Counting`] as its global
/// allocator, which on a running program is the only way the total can be
/// zero. The distinction matters: a `0` printed beside a real heap reading
/// would read as an empty heap, where it means "not counted here".
///
/// Two loads, not one snapshot: a free that lands between them can read the
/// figure one allocation low, and a grant one high. That is a skew of one
/// block for one reader, which is what `Relaxed` buys and what every consumer
/// of a heap figure already tolerates; the subtraction saturates so the skew
/// can never print as a wrapped `u64`.
pub fn live_bytes() -> Option<u64> {
    let allocated = ALLOCATED.load(Relaxed);
    if allocated == 0 {
        return None;
    }
    Some(allocated.saturating_sub(FREED.load(Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test binary installs the counter, the way the two shipped
    /// binaries do, so the test observes the real global allocator and not a
    /// stand-in.
    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    /// The one large block these tests move: big enough that no other
    /// allocation in this binary's own test harness is mistaken for it.
    const BLOCK: usize = 64 << 20;

    /// **The figure rises with a grant and falls with the free**, by the
    /// block's own size — and a request the system refuses moves nothing.
    ///
    /// One test rather than three, because the counters are process-global
    /// and the harness runs tests on several threads; a second test moving
    /// the same statics would race this one's arithmetic.
    #[test]
    fn live_bytes_rise_on_a_grant_fall_on_the_free_and_hold_on_a_refusal() {
        let before = live_bytes().expect("this binary installed the counter, so it has counted");

        // `vec![0u8; N]` arrives through `alloc_zeroed`, so this also shows
        // that arm counts. `SLACK` is for what the harness itself frees
        // between the two reads (a first run measured 24 B of it): the block
        // must account for all but a few KiB of the rise.
        const SLACK: u64 = 64 << 10;
        let block = vec![0u8; BLOCK];
        let held = live_bytes().expect("still counting");
        assert!(
            held + SLACK >= before + BLOCK as u64,
            "a {BLOCK} B grant moved live bytes from {before} to {held}"
        );

        drop(block);
        let after = live_bytes().expect("still counting");
        assert!(
            after < held,
            "the free did not bring live bytes back down: {held} then {after}"
        );
        // Within one block of where it started: what the harness itself
        // allocated between the two reads is the only slack there is, and it
        // is nothing like 64 MiB.
        assert!(
            after < before + BLOCK as u64,
            "live bytes {after} did not return to within a block of {before}"
        );

        // A request the system cannot serve returns null, and the counters
        // must not have moved for it — the rule that keeps the live figure
        // from drifting upward at exactly the moment it matters.
        //
        // **A bound, not an equality.** The counters are process-global and
        // the harness runs this binary's tests on several threads, so a
        // reading taken twice around the call differs by whatever another
        // thread allocated in between — kilobytes, and an equality here was
        // observed to fail on exactly that. The refused request is
        // `isize::MAX - 4096` bytes, about nine exabytes, so a counter that
        // took it would move the figure by nineteen digits. `NOISE` is
        // therefore anything at all below that: the assertion is immune to
        // the harness and still catches the only bug it exists to catch.
        const NOISE: u64 = 1 << 30;
        let refused = Layout::from_size_align(isize::MAX as usize - 4096, 1).expect("a layout");
        let read_before = live_bytes().expect("still counting");
        // SAFETY: `alloc` has no precondition beyond a non-zero-size layout,
        // which this is; the pointer is checked and never dereferenced.
        #[allow(
            unsafe_code,
            reason = "exercising the allocator's refusal arm directly"
        )]
        let ptr = unsafe { ALLOCATOR.alloc(refused) };
        assert!(ptr.is_null(), "the system granted an isize::MAX-byte block");
        let read_after = live_bytes().expect("still counting");
        assert!(
            read_after.saturating_sub(read_before) < NOISE,
            "a refused request of {} B drifted the live figure from {read_before} to \
             {read_after}",
            refused.size(),
        );
    }

    /// The counter type is a zero-sized unit that a binary can name as a
    /// `static`; a `Counting` with state would need a constructor, and a
    /// `#[global_allocator]` static has none to call.
    #[test]
    fn the_allocator_is_a_unit() {
        assert_eq!(core::mem::size_of::<Counting>(), 0);
    }
}
