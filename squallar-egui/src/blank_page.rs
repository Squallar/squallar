//! **Textures whose first upload carries no information** — the one delta a
//! renderer may allocate for and never transfer.
//!
//! [`crate::raster_atlas`] mints a page with
//! `ColorImage::filled(page_size, TRANSPARENT)`, because `Context::load_texture`
//! takes the size from an image and there is no size-only door. On the shipped
//! 256-texel class that image is **1806 × 1806 × 4 = 13,046,544 B of
//! transparent pixels** — over the whole-crossing threshold on *both* device
//! arms (2,100,000 B with a staging ring, 4,194,304 B without), so it is filed
//! as bands, and `TextureUploads::pending` holds the whole 13 MB `Arc` alive
//! until the last of them crosses. It is charged to the `upload pending`
//! census family at its full size for every one of those frames, and it costs
//! the frame thread a real 13 MB of `write_texture` to move a page of zeros.
//!
//! **Nothing reads it.** A slot is sampled only through the `Rect` a lease
//! hands out, and a leased slot is written in full — the tile *plus* its
//! gutter — before that rect exists. An unleased slot is never sampled at all.
//! And the content the transfer delivers is `Color32::TRANSPARENT`, which is
//! the zero pattern a texture is created holding.
//!
//! So the producer says so, here, and the renderer allocates the texture and
//! transfers nothing. This is the same shape as `is_font_atlas`: the upload
//! router already varies its policy by texture id, and this is a second fact
//! about an id that only the producer can know.
//!
//! # Why a published fact and not a check
//!
//! The renderer could scan the delta and find every pixel equal. That scan is
//! `n` compares of a 13 MB buffer **on the frame thread**, and — worse — it
//! cannot be bounded: an overlay picture with a transparent margin is uniform
//! for as long as its margin lasts, so the abort-early case is exactly the
//! case that runs longest. A bounded scan cannot prove a full page uniform,
//! which is the one image this exists for. The producer knows for free.
//!
//! # Bounded by construction
//!
//! [`note`] is called once per page minted and [`take`] removes the entry, so
//! the list holds only pages minted since the last frame's deltas were filed —
//! egui files an allocation's delta on the frame after `load_texture`, so that
//! is at most one frame's worth. A page whose delta never arrives is dropped
//! by [`forget`], which the renderer calls for every id egui retires.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

/// Pages minted and not yet claimed by their allocation delta.
///
/// A `Vec` rather than a set: it holds one frame's mints, which is one or two
/// entries, and a linear scan of that beats hashing a `TextureId`.
static BLANK: Mutex<Vec<egui::TextureId>> = Mutex::new(Vec::new());

/// Record that `id`'s first whole delta carries no information.
///
/// Call it with the handle's own id the moment the texture is created, before
/// the frame that carries its delta.
pub fn note(id: egui::TextureId) {
    let mut blank = BLANK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !blank.contains(&id) {
        NOTED.fetch_add(1, Relaxed);
        blank.push(id);
    }
}

/// Whether `id` was noted, removing it.
///
/// **Once.** The second whole delta under an id is a real image — a page is
/// blank only when it is minted — so a claim that did not remove would make
/// every later re-allocation of that id transfer nothing and draw a blank
/// page.
pub fn take(id: egui::TextureId) -> bool {
    let mut blank = BLANK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match blank.iter().position(|held| *held == id) {
        Some(at) => {
            CLAIMED.fetch_add(1, Relaxed);
            blank.swap_remove(at);
            true
        }
        None => false,
    }
}

/// Drop `id` unclaimed — the retirement path, so a page freed before its delta
/// was filed does not sit in the list for the life of the session.
pub fn forget(ids: &[egui::TextureId]) {
    let mut blank = BLANK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = blank.len();
    blank.retain(|held| !ids.contains(held));
    FORGOTTEN.fetch_add(before - blank.len(), Relaxed);
}

static NOTED: AtomicUsize = AtomicUsize::new(0);
static CLAIMED: AtomicUsize = AtomicUsize::new(0);
static FORGOTTEN: AtomicUsize = AtomicUsize::new(0);
static CLAIMED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes an upload router did not transfer because [`take`] claimed the delta.
///
/// Filed by the router, which is the only place the delta's size is known, and
/// **the transfer that did not happen** rather than a resident saving: the
/// texture is still allocated at the page's size, and the `ColorImage` this
/// spares the wire is still built and still dropped.
pub fn claimed_bytes(bytes: u64) {
    CLAIMED_BYTES.fetch_add(bytes, Relaxed);
}

/// `(noted, claimed, forgotten, claimed_bytes)`.
///
/// **`claimed` is the fires-counter.** The arm it gates is a policy the
/// producer publishes and the renderer obeys; nothing else distinguishes a
/// mechanism that ran from one whose condition never held. `noted` is its
/// denominator, and `forgotten` the pages retired before their delta arrived —
/// `noted == claimed + forgotten + outstanding()` on a healthy leg.
pub fn totals() -> (usize, usize, usize, u64) {
    (
        NOTED.load(Relaxed),
        CLAIMED.load(Relaxed),
        FORGOTTEN.load(Relaxed),
        CLAIMED_BYTES.load(Relaxed),
    )
}

/// How many entries are outstanding. For the suite that holds the bound.
pub fn outstanding() -> usize {
    BLANK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len()
}

#[cfg(test)]
#[path = "blank_page/tests.rs"]
mod tests;
