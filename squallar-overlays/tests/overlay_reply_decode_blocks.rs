//! **What one decoded overlay reply costs, for every overlay kind** — from the
//! bytes the wire hands over to the buffer the consumer's texture takes.
//!
//! This is the web target's whole arrival path. A native job answers in
//! process and its `RasterizeOutput` is moved; a browser job crosses a worker
//! boundary as bytes and is rebuilt here, so a producer-side layout choice
//! (`rasterize_gridded` writing pixels) reaches nothing on the web — the
//! decode is what decides the layout every kind arrives in.
//!
//! An `egui::ColorImage` holds `Vec<Color32>`, and `Color32` is
//! `#[repr(align(4))]`. A `Vec` goes back to the allocator with the `Layout`
//! it came from, so a decoded `Vec<u8>` can never *become* one by move —
//! `bytemuck::allocation::try_cast_vec` refuses on exactly that ground, in
//! both directions — and a consumer handed bytes has to allocate a second
//! block the size of the picture and copy into it. So the layout is chosen
//! where the buffer is born, which on this path is the decode.
//!
//! Its own binary with a counting `#[global_allocator]`, on the terms
//! `gridded_picture_blocks.rs` sets out: the instrument counts real
//! `GlobalAlloc` calls at or above a size threshold, knows nothing about the
//! codec, and so compiles and runs against a tree where the fix is absent and
//! disagrees with it. **Observed on the byte-buffer spelling: 2 blocks, every
//! kind.**

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ecolor::Color32;
use squallar_overlays::render::jobs::JOB_CODECS;
use squallar_overlays::render::raster_buf::RasterBuf;
use squallar_overlays::render::rasterize::{AlphaMode, HitCellMap, HitCells, RasterizeOutput};
use squallar_source::job::{DescribedOut, JobCodec};

const W: usize = 2048;
const H: usize = 1024;
/// 8 MiB.
const PICTURE: usize = W * H * 4;

/// Half a picture: comfortably above the framing the codec writes (a hit-cell
/// block here is tens of bytes) and comfortably below one picture, so the only
/// thing that can trip the counter is a picture.
const LARGE: usize = PICTURE / 2;

static LARGE_BLOCKS: AtomicUsize = AtomicUsize::new(0);
/// The bytes those blocks asked the allocator for, so the figure has a
/// magnitude and not only a count.
static LARGE_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Off outside the measured window, so the fixture's own picture and the wire
/// buffer it is encoded into — both of which exist whatever the decode does —
/// are not in the figure.
static COUNTING: AtomicBool = AtomicBool::new(false);

struct PictureBlocks;

fn note(size: usize) {
    if size >= LARGE && COUNTING.load(Ordering::Relaxed) {
        LARGE_BLOCKS.fetch_add(1, Ordering::Relaxed);
        LARGE_BYTES.fetch_add(size, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for PictureBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow past the bar counts: a `Vec` that reallocates its way up to a
    /// picture has taken a fresh block that size, whatever the call was named.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.size() < new_size {
            note(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: PictureBlocks = PictureBlocks;

/// A picture whose every byte is a function of `seed` and its own offset, so a
/// reply decoded from the wrong row's bytes, truncated, or shifted by a byte
/// fails the comparison rather than matching by luck.
///
/// Premultiplied by construction — alpha is the largest of the four — because
/// that is what the wire carries: the run funnel premultiplies in its output
/// stage, before `encode_out`.
fn a_picture(seed: u8) -> Vec<u8> {
    (0..PICTURE)
        .map(|i| {
            let v = (i as u32)
                .wrapping_mul(2_654_435_761)
                .wrapping_add(seed as u32);
            match i % 4 {
                // Alpha: kept at the top of the range so every pixel is a
                // legal premultiplied one.
                3 => 0xC0 | (v as u8 & 0x3F),
                _ => (v >> 13) as u8 & 0xBF,
            }
        })
        .collect()
}

/// Three occupied cells on the quarter-resolution grid, so the framing before
/// the pixels is not empty on every row.
fn some_cells() -> HitCells {
    let mut cells = HitCellMap::default();
    cells.insert(0u32, vec![0u32]);
    cells.insert(5u32, vec![1, 4]);
    cells.insert(7u32, vec![2]);
    HitCells {
        width: 4,
        height: 2,
        cells,
    }
}

/// What the consumer ends up holding, and what it cost, for one row.
struct Arrival {
    label: &'static str,
    blocks: usize,
    /// What those blocks asked the allocator for.
    bytes: usize,
    /// The address the decoder allocated the picture at.
    decoded_at: usize,
    /// The address the consumer's `Vec<Color32>` starts at.
    held_at: usize,
    /// Whether the decode chose the consumer's own layout.
    arrived_as_pixels: bool,
    pixels: Vec<Color32>,
    cells: Option<HitCells>,
}

/// One row's whole arrival: encode the reply the funnel would have produced,
/// then decode it **through the row's own registry entry** and take the
/// picture the way `overlay_job_deliver` takes it.
///
/// Driven through `JobCodec`'s erased `encode_out`/`decode_out` rather than
/// the typed trait, because that is the pair `deliver_encoded_reply` calls:
/// the boxing between them is part of what must not copy the picture, and a
/// row added to `JOB_CODECS` is measured here without this file being edited.
///
/// The encode is deliberately outside the counted window and the wire buffer
/// is dropped before the picture is read: what is counted is the cost the
/// *decode* adds, and what the read after the drop shows is that the decoder
/// answered with a buffer of its own rather than a view into the wire.
fn arrive(row: &JobCodec, source: &[u8], cells: HitCells) -> Arrival {
    let mut head = Vec::new();
    let mut tails = Vec::new();
    (row.encode_out)(
        DescribedOut(Box::new(RasterizeOutput {
            rgba: RasterBuf::Bytes(source.to_vec()),
            hit_cells: Some(cells),
            alpha: AlphaMode::Premultiplied,
            blank: None,
        })),
        &mut head,
        &mut tails,
    );
    assert!(
        tails.is_empty(),
        "`{}` wrote a tail; the overlay rows write none, so the decode below \
         would be reading a message this harness did not build",
        row.label,
    );

    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_BLOCKS.load(Ordering::Relaxed);
    let before_bytes = LARGE_BYTES.load(Ordering::Relaxed);

    let decoded = (row.decode_out)(&head, tails)
        .and_then(|out| out.take::<RasterizeOutput>())
        .unwrap_or_else(|| panic!("`{}` refused a reply its own `encode_out` wrote", row.label));
    let arrived_as_pixels = matches!(decoded.rgba, RasterBuf::Pixels(_));
    let decoded_at = decoded.rgba.as_bytes().as_ptr() as usize;
    let cells = decoded.hit_cells;
    // The read `overlay_job_deliver` makes, at the point it makes it.
    let pixels = decoded.rgba.into_pixels();

    let blocks = LARGE_BLOCKS.load(Ordering::Relaxed) - before;
    let bytes = LARGE_BYTES.load(Ordering::Relaxed) - before_bytes;
    COUNTING.store(false, Ordering::Relaxed);

    // The picture cannot be a view into the wire, and this is where that stops
    // being a claim about the types: the wire buffer is freed here, and every
    // assertion the caller makes reads the picture afterwards.
    let wire = head.as_ptr() as usize..head.as_ptr() as usize + head.capacity();
    assert!(
        !wire.contains(&decoded_at),
        "`{}` answered with a picture inside the wire buffer it was handed; \
         the decoder must own what it hands back",
        row.label,
    );
    drop(head);

    Arrival {
        label: row.label,
        blocks,
        bytes,
        decoded_at,
        held_at: pixels.as_ptr() as usize,
        arrived_as_pixels,
        pixels,
        cells,
    }
}

/// **One decoded overlay, one picture-sized block — for every overlay kind.**
///
/// The pair, stated per row:
///
/// *The cost fell.* One block, where a decode that answers bytes takes one for
/// the bytes and the consumer takes a second for the pixels it actually holds
/// — 8 MiB each at this texture, 40.79 MiB each at the measured 4317 x 2477
/// desktop case, on the thread the reply lands on.
///
/// *The work held.* The picture the consumer holds is compared **byte for
/// byte** against the picture the producer encoded — the whole buffer, not its
/// length and not a digest of it.
///
/// **Counted, never timed.** Blocks at or above 4 MiB is a property of the
/// code, not of the machine or the load, which is what makes this a gate and
/// not a reading.
///
/// The addresses are the half a count cannot make: one allocation is also what
/// copy-and-free looks like. Equal addresses say the block the decoder took is
/// the block the consumer holds, so nothing between them reproduced the
/// picture.
#[test]
fn every_overlay_kind_decodes_its_reply_into_one_picture_sized_block() {
    // Every registered overlay row, and a different picture for each, so a
    // row reading another row's bytes — or this harness comparing a row
    // against the wrong source — fails rather than matching by luck.
    let sources: Vec<Vec<u8>> = (0..JOB_CODECS.len())
        .map(|i| a_picture((i as u8).wrapping_mul(31) | 1))
        .collect();
    let cells = some_cells();

    let arrivals: Vec<Arrival> = JOB_CODECS
        .iter()
        .zip(&sources)
        .map(|(row, source)| arrive(row, source, cells.clone()))
        .collect();
    assert!(
        !arrivals.is_empty(),
        "control: `JOB_CODECS` is empty, so every assertion below runs zero \
         times and this test passes having measured nothing",
    );

    for (arrival, source) in arrivals.iter().zip(&sources) {
        let label = arrival.label;
        assert!(
            arrival.arrived_as_pixels,
            "`{label}` decoded its picture as bytes. The consumer holds \
             `Vec<Color32>` and a `Vec` cannot change the alignment it is \
             freed with, so bytes here means a second picture-sized block and \
             a full-picture copy on the thread the reply lands on",
        );
        assert_eq!(
            arrival.blocks,
            1,
            "`{label}` took {} picture-sized blocks to arrive. One is the \
             picture; a second is the copy a decoded byte buffer forces on a \
             consumer holding `Vec<Color32>` — {} MiB at this texture",
            arrival.blocks,
            PICTURE >> 20,
        );
        assert_eq!(
            arrival.bytes, PICTURE,
            "`{label}` asked the allocator for {} bytes to arrive, where the \
             picture is {PICTURE}. The decode's whole cost is the picture \
             itself; anything more is a buffer it did not have to take",
            arrival.bytes,
        );
        assert_eq!(
            arrival.held_at, arrival.decoded_at,
            "`{label}`: the buffer the consumer holds is not the one the \
             decoder allocated. A count of one block is also what \
             copy-and-free looks like; equal addresses are what say the \
             picture was handed over rather than reproduced",
        );
        assert_eq!(
            arrival.pixels.len(),
            W * H,
            "`{label}` handed the consumer a picture that is not the \
             texture's own size",
        );
        // Byte for byte, over the consumer's own buffer. `assert!` rather
        // than `assert_eq!` because the two sides are 8 MiB each and a
        // failure would print both.
        assert!(
            bytemuck::cast_slice::<Color32, u8>(&arrival.pixels) == source.as_slice(),
            "`{label}` changed the picture on its way through the wire. The \
             decode's whole claim is that it chooses a layout, not a value: \
             `Color32::from_rgba_premultiplied` stores the four bytes it is \
             given, so the buffer must compare equal to the one the producer \
             encoded",
        );
    }

    // The framing before the picture still round-trips: a decode that got the
    // picture right by reading from the wrong offset would lose it.
    for arrival in &arrivals {
        assert_eq!(
            arrival.cells.as_ref(),
            Some(&cells),
            "`{}` lost the hit cells the reply carried; hovers would go to \
             the wrong items",
            arrival.label,
        );
    }
}
