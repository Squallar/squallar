//! Handing the peer a view onto this instance's memory instead of a copy.
//!
//! # What this buys, and what it does not
//!
//! The page and the rasterization worker each hold a wasm instance. Both
//! memories are `SharedArrayBuffer`s since WS3b, but they are two DIFFERENT
//! ones — `worker.js` instantiates the module a second time, and
//! `wasm-bindgen-rayon` shares the *worker's* memory with the worker's own
//! rayon threads, not with the page. So a reply used to be copied TWICE:
//! `Uint8Array::from(head.as_slice())` copied it out of the worker's linear
//! memory into a fresh JS `ArrayBuffer`, and `to_vec()` copied that buffer into
//! the page's linear memory. Transferring moved the middle buffer without
//! copying it, but it did not remove either end.
//!
//! A `SharedArrayBuffer` crosses `postMessage` by SHARING, not by cloning. So
//! the producer can skip its half: build a `Uint8Array` *view* onto the region
//! of its own memory where the answer already sits, post that, and let the
//! consumer copy once. **One memcpy instead of two, in each direction.**
//!
//! What it does NOT do is remove the consumer's copy. `wgpu`'s `write_texture`,
//! `egui::ColorImage` and this workspace's own `JobCodec::decode_out` all take
//! `&[u8]` / `Vec<u8>` — slices of the CALLER's linear memory. A view onto a
//! foreign `SharedArrayBuffer` is not addressable as one at any price, so the
//! second copy is a property of the type, not of the transport. Removing it
//! would take ONE linear memory across both instances, and
//! [`crate::worker_port`] records why that is not on the table.
//!
//! # The ownership protocol
//!
//! A view is a borrow of a live allocation. The lender must not free or reuse
//! that region until the borrower has finished reading it, and nothing in
//! `postMessage` says when that is. So it is said explicitly, and symmetrically
//! — the rule is the same in both directions:
//!
//! 1. The **lender** calls [`LoanBook::lend`], which takes the buffers BY VALUE
//!    and holds them. It posts the views and the loan id.
//! 2. The **borrower** copies every view into its own memory, synchronously, in
//!    the message handler. Then it posts `RELEASE` naming the loan id.
//! 3. The lender's [`LoanBook::release`] drops the buffers. That is the only
//!    place they are freed.
//! 4. A peer that will never answer again — a worker this page has replaced, a
//!    page that has navigated — leaves loans outstanding.
//!    [`LoanBook::release_all`] is what the loss path calls.
//!
//! Rust's ownership carries step 1 and step 3: `lend` consumes the buffers, so
//! there is no second owner to free them early, and `release` is the only way
//! to get them back. What Rust cannot see is step 2 — that the borrower is a
//! different agent — so the book is also the instrument: [`LoanBook::release`]
//! reports a release of an id it never issued (or has already released) rather
//! than swallowing it, and `loan_book_tests` pins that. A recycled region
//! reaching a texture upload is far worse than a copy, so the failure mode this
//! is built against is a release that arrives twice, not one that never comes:
//! a never-released loan leaks, and [`LoanBook::bytes_outstanding`] is what
//! makes a leak visible.
//!
//! # When it is unavailable
//!
//! Lending needs the memory's buffer to be a `SharedArrayBuffer` AND the agent
//! cluster to be cross-origin isolated — without isolation the buffer is still
//! shared but `postMessage` refuses to carry it. A deployment served without
//! COOP/COEP (GitHub Pages, which `dc92df4c` kept serving alongside CloudFront)
//! therefore falls back to the copying wire, exactly as it falls back to a
//! one-thread rayon pool. That fallback is a *runtime capability check*, not a
//! `cfg`: the same code runs on both, and [`can_lend`] is the one place that
//! decides.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

/// Names one outstanding loan on the wire.
///
/// Never [`NO_LOAN`] while outstanding: the wire writes 0 for "this message
/// carries copies", so a real id has to be distinguishable from the fallback
/// arm by value alone.
pub type LoanId = u32;

/// What a message that carries copies rather than views writes for its loan.
pub const NO_LOAN: LoanId = 0;

/// One buffer a loan holds down, and where its bytes live.
///
/// The two arms exist because a loan has two reasons to hold memory. An
/// encoder that BUILT a wire buffer hands it over by value and the book frees
/// it on release. A payload that was ALREADY resident — a grid the page holds
/// behind an `Arc` — is not copied into a wire buffer at all; the book holds
/// the `Arc` so the region cannot be freed while the peer reads it, and the
/// view is built onto the bytes where they already sit.
///
/// The whole point of the second arm is that it costs no memcpy. A gridded
/// overlay job's payload is the size of the message, so copying it into a wire
/// buffer is the dominant cost of the hand-off, and it runs on the FRAME
/// THREAD.
#[derive(Debug)]
pub enum Lent {
    /// Bytes the book owns and will free on release.
    Owned(Vec<u8>),
    /// Bytes owned elsewhere, viewed in place.
    ///
    /// `owner` is never read through — it is held ONLY so the allocation
    /// outlives the peer's read. `addr` and `len` are what the view is built
    /// from, and both are captured FROM the owner by [`Lent::borrowed`] so
    /// they cannot describe a different allocation than the one held.
    Borrowed {
        owner: Arc<dyn Any + Send + Sync>,
        addr: usize,
        len: usize,
    },
}

impl Lent {
    /// Hold `owner` alive and view the bytes `project` picks out of it.
    ///
    /// The slice is taken from the owner **inside** this function rather than
    /// passed in beside it: that is what makes the address provably part of
    /// the allocation the `Arc` is holding down, instead of a pairing the
    /// caller has to get right.
    pub fn borrowed<T: Any + Send + Sync>(
        owner: Arc<T>,
        project: impl FnOnce(&T) -> &[u8],
    ) -> Self {
        let bytes = project(&owner);
        let (addr, len) = (bytes.as_ptr() as usize, bytes.len());
        Self::Borrowed { owner, addr, len }
    }

    /// How many bytes this buffer puts on the wire.
    pub fn len(&self) -> usize {
        match self {
            Self::Owned(bytes) => bytes.len(),
            Self::Borrowed { len, .. } => *len,
        }
    }

    /// Whether it puts none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Where the bytes are, for building the view.
    pub fn addr(&self) -> usize {
        match self {
            Self::Owned(bytes) => bytes.as_ptr() as usize,
            Self::Borrowed { addr, .. } => *addr,
        }
    }
}

/// The allocations this instance has lent out and cannot free yet.
///
/// One per thread. The page's book and the worker's book are separate objects
/// in separate linear memories that never see each other; what pairs them is
/// the loan id on the wire.
///
/// Deliberately NOT `derive(Default)`: a derived one would start
/// [`next`](Self::next) at [`NO_LOAN`], so the first loan of every book would be
/// named the value the wire reserves for "this message carries copies" — and
/// the Tier-2 assertion would pass on a transport that had reverted.
#[derive(Debug)]
pub struct LoanBook {
    /// The id the next [`lend`](Self::lend) will hand out. Wraps past
    /// [`NO_LOAN`] rather than through it.
    next: LoanId,
    outstanding: HashMap<LoanId, Vec<Lent>>,
}

impl Default for LoanBook {
    fn default() -> Self {
        Self::new()
    }
}

impl LoanBook {
    pub fn new() -> Self {
        Self {
            next: 1,
            outstanding: HashMap::new(),
        }
    }

    /// Take ownership of `buffers` and name the loan.
    ///
    /// By value, and that is the protocol's first half: after this call the
    /// caller has no handle to free the region early. The id is what the peer
    /// quotes back.
    pub fn lend(&mut self, buffers: Vec<Vec<u8>>) -> LoanId {
        self.lend_parts(buffers.into_iter().map(Lent::Owned).collect())
    }

    /// Name a loan over buffers that may be owned, borrowed, or a mix.
    ///
    /// The mixed case is the one that matters: a gridded job lends a small
    /// OWNED envelope beside a large BORROWED payload, so only the envelope
    /// was ever built.
    pub fn lend_parts(&mut self, buffers: Vec<Lent>) -> LoanId {
        let id = self.next;
        self.next = next_after(id);
        self.outstanding.insert(id, buffers);
        id
    }

    /// The peer says it is done reading. Frees the region.
    ///
    /// `None` for an id this book did not issue or has already released —
    /// **returned rather than ignored**. Both are protocol violations: the
    /// second means the peer read a region that may already have been handed
    /// to another loan, which is the failure this whole module exists to make
    /// impossible. The caller logs it; `loan_book_tests` asserts it.
    pub fn release(&mut self, id: LoanId) -> Option<Vec<Lent>> {
        self.outstanding.remove(&id)
    }

    /// The buffers behind an outstanding loan, for building the views to post.
    ///
    /// Borrowed, not cloned: the views are built from these addresses and the
    /// book keeps owning them.
    pub fn peek(&self, id: LoanId) -> Option<&[Lent]> {
        self.outstanding.get(&id).map(Vec::as_slice)
    }

    /// How many loans the peer still owes a release for.
    pub fn outstanding(&self) -> usize {
        self.outstanding.len()
    }

    /// How much memory those loans are holding down.
    ///
    /// The leak instrument: a peer that stops releasing does not corrupt
    /// anything, it just grows this without bound.
    pub fn bytes_outstanding(&self) -> usize {
        self.outstanding
            .values()
            .flat_map(|buffers| buffers.iter())
            .map(Lent::len)
            .sum()
    }

    /// Drop every loan, and say how many there were.
    ///
    /// For a peer that will never send another `RELEASE`. Not an error path in
    /// itself — a worker replaced mid-job owes releases it cannot send — so the
    /// count is returned for the log rather than raised.
    pub fn release_all(&mut self) -> usize {
        let count = self.outstanding.len();
        self.outstanding.clear();
        count
    }
}

/// The id after `id`, skipping [`NO_LOAN`] on the wrap.
///
/// A free function so the wrap is reachable by a test without lending 2^32
/// times and without a test-only hook into [`LoanBook`] — the case that matters
/// is the one an in-book test cannot get to.
///
/// Reusing ids past the wrap is harmless on its own: an id is only ever quoted
/// back by the peer that was handed it, and 2^32 loans outlive any page.
/// Skipping 0 is the part that is load-bearing.
fn next_after(id: LoanId) -> LoanId {
    match id.wrapping_add(1) {
        NO_LOAN => 1,
        next => next,
    }
}

/// The bookkeeping half, on the host, where a browser is not needed to run it.
#[cfg(test)]
mod loan_book_tests;

#[cfg(target_arch = "wasm32")]
mod js {
    use super::{Lent, LoanBook, LoanId, NO_LOAN};
    use std::cell::RefCell;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;

    thread_local! {
        /// This thread's outstanding loans. The page has one; the worker has
        /// its own, in its own memory.
        static BOOK: RefCell<LoanBook> = RefCell::new(LoanBook::new());
    }

    /// Whether a view onto this memory can reach the peer at all.
    ///
    /// Two conditions and both are needed. The buffer has to BE a
    /// `SharedArrayBuffer` — a plain `ArrayBuffer` would be cloned or detached
    /// by `postMessage`, which is a copy again or a corrupted lender. And the
    /// agent cluster has to be cross-origin isolated, because without it
    /// `postMessage` refuses to carry a `SharedArrayBuffer` at all even though
    /// the buffer is one. Checked, not assumed: the second is the case a
    /// deployment without COOP/COEP lands in, and it is also the negative
    /// control the Tier-2 gate uses.
    pub fn can_lend() -> bool {
        cross_origin_isolated() && memory_buffer().is_some_and(|b| b.is_shared())
    }

    /// `globalThis.crossOriginIsolated`, read through `Reflect` because the
    /// property lives on `Window` in the page and on `WorkerGlobalScope` in the
    /// worker and this module runs in both.
    fn cross_origin_isolated() -> bool {
        js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crossOriginIsolated"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// This instance's linear memory as a buffer object, or `None` if
    /// wasm-bindgen's `memory()` is not a `WebAssembly.Memory` — which cannot
    /// happen in a build this crate produces, so the `Option` is the shape of
    /// the cast and not a case with behaviour behind it.
    fn memory_buffer() -> Option<Buffer> {
        wasm_bindgen::memory()
            .dyn_into::<js_sys::WebAssembly::Memory>()
            .ok()
            .map(|memory| Buffer(memory.buffer()))
    }

    /// The `ArrayBuffer`-or-`SharedArrayBuffer` behind a linear memory.
    struct Buffer(JsValue);

    impl Buffer {
        fn is_shared(&self) -> bool {
            self.0.is_instance_of::<js_sys::SharedArrayBuffer>()
        }

        /// `byteLength`, whichever of the two buffer kinds this is. `None`
        /// for a value that is neither — which a `WebAssembly.Memory` cannot
        /// hand back, so the `Option` is the shape of the cast.
        fn byte_length(&self) -> Option<u64> {
            if let Some(shared) = self.0.dyn_ref::<js_sys::SharedArrayBuffer>() {
                Some(u64::from(shared.byte_length()))
            } else {
                self.0
                    .dyn_ref::<js_sys::ArrayBuffer>()
                    .map(|plain| u64::from(plain.byte_length()))
            }
        }
    }

    /// How many bytes this instance's linear memory currently spans — the
    /// page's on the page, the worker's in the worker, each read against its
    /// own `--max-memory`. A level, not a high-water mark: a shared memory
    /// only ever grows, so on this target the two are the same figure.
    pub fn memory_bytes() -> Option<u64> {
        memory_buffer()?.byte_length()
    }

    /// Register `buffers` as lent and build one `Uint8Array` view per buffer.
    ///
    /// `Err(buffers)` hands them straight back when this browser cannot carry a
    /// view, and the caller copies instead — the same hand-back idiom
    /// `JobSink::send` uses, so no path can lose a payload by failing to lend
    /// it. The views alias memory the book now owns; they stay valid because a
    /// SHARED memory grows IN PLACE — a plain `ArrayBuffer` detaches on
    /// `memory.grow` and takes every outstanding view with it, which is the
    /// other half of why this is gated on the buffer being shared.
    pub fn lend(buffers: Vec<Vec<u8>>) -> Result<(LoanId, js_sys::Array), Vec<Vec<u8>>> {
        if !can_lend() {
            return Err(buffers);
        }
        lend_parts(buffers.into_iter().map(Lent::Owned).collect())
            .map_err(|parts| parts.into_iter().map(owned_or_empty).collect())
    }

    /// A buffer's bytes back, for the hand-back arm.
    ///
    /// Only the OWNED arm can be handed back to a caller that will copy
    /// instead: a borrowed region's bytes belong to the resident data and the
    /// caller must re-read them from there, which is why the only caller of
    /// the hand-back path lends owned buffers exclusively.
    fn owned_or_empty(part: Lent) -> Vec<u8> {
        match part {
            Lent::Owned(bytes) => bytes,
            Lent::Borrowed { .. } => Vec::new(),
        }
    }

    /// Register `parts` as lent and build one `Uint8Array` view per part.
    ///
    /// The mixed arm: an owned envelope beside a borrowed payload, so the
    /// payload is never copied into a wire buffer at all.
    pub fn lend_parts(parts: Vec<Lent>) -> Result<(LoanId, js_sys::Array), Vec<Lent>> {
        if !can_lend() {
            return Err(parts);
        }
        let Some(buffer) = memory_buffer().map(|b| b.0) else {
            return Err(parts);
        };
        let id = BOOK.with(|book| book.borrow_mut().lend_parts(parts));
        let views = js_sys::Array::new();
        BOOK.with(|book| {
            let book = book.borrow();
            // Present by construction — nothing releases between the two
            // borrows above — but an `expect` here would panic a worker over a
            // bookkeeping slip, so the empty array falls out instead and the
            // caller sees a loan of no buffers.
            for part in book.peek(id).unwrap_or(&[]) {
                views.push(&view_onto(&buffer, part.addr(), part.len()));
            }
        });
        Ok((id, views))
    }

    /// A `Uint8Array` over `bytes` where they already sit, without copying.
    ///
    /// `as_ptr` is a safe call and the view is built through `js_sys`, so this
    /// whole module holds under the crate's `#![forbid(unsafe_code)]`. What
    /// keeps it SOUND is the book: `bytes` is borrowed from a buffer the book
    /// owns and will not drop until the peer releases it, and the peer only
    /// ever reads.
    fn view_onto(buffer: &JsValue, addr: usize, len: usize) -> js_sys::Uint8Array {
        js_sys::Uint8Array::new_with_byte_offset_and_length(buffer, addr as u32, len as u32)
    }

    /// The peer has finished reading; free the region.
    ///
    /// A release of an id this book never issued is LOGGED, not ignored: it
    /// means the peer is quoting an id from another generation, and the next
    /// thing it would do is read a region this instance has re-lent.
    pub fn release(id: LoanId) {
        if id == NO_LOAN {
            return;
        }
        let released = BOOK.with(|book| book.borrow_mut().release(id).is_some());
        if !released {
            log::error!(
                "a peer released loan {id}, which this instance has no record of \
                 lending; it is reading a region that may have been re-lent"
            );
        }
    }

    /// Drop every outstanding loan. For a peer that will never release again.
    pub fn release_all(reason: &str) {
        let count = BOOK.with(|book| book.borrow_mut().release_all());
        if count > 0 {
            log::debug!("released {count} loans still outstanding when {reason}");
        }
    }

    /// What the loans still out are holding down, for the diagnostics line.
    pub fn bytes_outstanding() -> usize {
        BOOK.with(|book| book.borrow().bytes_outstanding())
    }

    /// Whether `value` is a typed array whose buffer is a `SharedArrayBuffer`
    /// that is NOT this instance's own memory — i.e. a genuine view into the
    /// PEER's heap, which is only true when the peer lent rather than copied.
    ///
    /// This is the page's first-hand observation, and it is what the Tier-2
    /// assertion is built on. It cannot be satisfied by a transport that
    /// reverted to copying: a copied reply arrives as a transferred plain
    /// `ArrayBuffer`, and a reply forged out of the page's own memory fails the
    /// identity check.
    pub fn is_foreign_shared(value: &js_sys::Uint8Array) -> bool {
        let theirs: JsValue = value.buffer().into();
        if !theirs.is_instance_of::<js_sys::SharedArrayBuffer>() {
            return false;
        }
        match memory_buffer() {
            // `Object.is`, so the comparison is object IDENTITY and not any
            // notion of equal contents: a different `SharedArrayBuffer` object
            // is a different linear memory, which is exactly the question.
            Some(ours) => !js_sys::Object::is(&ours.0, &theirs),
            None => false,
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use js::{
    bytes_outstanding, can_lend, is_foreign_shared, lend, lend_parts, memory_bytes, release,
    release_all,
};
