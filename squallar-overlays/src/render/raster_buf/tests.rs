//! What the two arms are allowed to differ in (where the bytes live) and what
//! they are not (the bytes).

use super::RasterBuf;
use ecolor::Color32;

fn sample_bytes() -> Vec<u8> {
    (0..64u16).map(|b| (b * 3 % 251) as u8).collect()
}

fn sample_pixels() -> Vec<Color32> {
    sample_bytes()
        .chunks_exact(4)
        .map(|px| Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
        .collect()
}

/// **The ownership transfer is real.** A `Pixels` buffer handed to
/// [`RasterBuf::into_pixels`] comes back as the *same allocation* — same
/// pointer, same capacity — which is the whole claim: the consumer's
/// `ColorImage` takes the picture the rasterizer wrote rather than a copy of
/// it. Pointer identity, not length or content, because those two are equal
/// across a copy as well.
#[test]
fn a_pixels_buffer_moves_into_the_consumer_rather_than_being_copied() {
    let pixels = sample_pixels();
    let addr = pixels.as_ptr() as usize;
    let cap = pixels.capacity();

    let moved = RasterBuf::Pixels(pixels).into_pixels();

    assert_eq!(
        moved.as_ptr() as usize,
        addr,
        "into_pixels reallocated a Pixels buffer; the arm exists precisely so \
         the picture reaches the texture upload without a second allocation",
    );
    assert_eq!(
        moved.capacity(),
        cap,
        "the same allocation keeps its capacity"
    );
}

/// The `Bytes` arm still answers, and answers the same picture: nothing about
/// the byte producers changed, and a consumer cannot tell which arm it was
/// handed except by where the memory is.
#[test]
fn the_two_arms_carry_the_same_picture() {
    let bytes = RasterBuf::Bytes(sample_bytes());
    let pixels = RasterBuf::Pixels(sample_pixels());

    assert_eq!(
        bytes, pixels,
        "the same picture in two layouts is one picture"
    );
    assert_eq!(
        bytes.as_bytes(),
        pixels.as_bytes(),
        "the byte view does not depend on the arm",
    );
    assert_eq!(
        bytes.clone().into_pixels(),
        pixels.clone().into_pixels(),
        "and neither does the pixel view",
    );
    assert_eq!(
        bytes.clone().into_bytes(),
        pixels.into_bytes(),
        "nor the owned bytes",
    );
    assert_eq!(bytes, sample_bytes(), "and it compares against a bare Vec");
}

/// **The premultiply's seam.** The funnel rewrites the picture through
/// [`RasterBuf::as_mut_bytes`], and a `Pixels` buffer must take that write in
/// its own memory — not in a temporary that is dropped.
#[test]
fn a_write_through_the_byte_view_lands_in_the_pixels_themselves() {
    let mut buf = RasterBuf::Pixels(sample_pixels());
    let addr = buf.as_bytes().as_ptr() as usize;

    buf.as_mut_bytes()[5] = 0xAB;

    assert_eq!(
        buf.as_bytes().as_ptr() as usize,
        addr,
        "the mutable byte view is a view, not a copy",
    );
    assert_eq!(
        buf.as_bytes()[5],
        0xAB,
        "the write is visible through the buffer",
    );
    assert_eq!(
        buf.into_pixels()[1].g(),
        0xAB,
        "and through the pixels: byte 5 is pixel 1's green channel",
    );
}

/// An empty picture is empty in both directions — what a settled blank holds.
#[test]
fn an_empty_buffer_has_no_bytes_and_no_pixels() {
    let empty = RasterBuf::empty();
    assert!(empty.as_bytes().is_empty());
    assert!(empty.into_pixels().is_empty());
}

/// **The proof `into_wire`/`from_wire` rest on.** The funnel carries a picture
/// as `squallar_source::job::PixelBuf` so it never names a colour type; that is
/// only free if the round trip is a re-label of one allocation rather than two
/// copies. Pointer and capacity, in BOTH directions, plus the size/alignment
/// equality the cast needs — because if that equality ever stops holding, the
/// `expect`s in those two methods are what fires, and this is the test that
/// says so first.
#[test]
fn a_wire_round_trip_keeps_the_same_allocation() {
    assert_eq!(
        (
            std::mem::size_of::<Color32>(),
            std::mem::align_of::<Color32>()
        ),
        (std::mem::size_of::<u32>(), std::mem::align_of::<u32>()),
        "Color32 and u32 have stopped agreeing on size or alignment, so the \
         wire carrier can no longer re-label the picture and must copy it",
    );

    let pixels = sample_pixels();
    let addr = pixels.as_ptr() as usize;
    let cap = pixels.capacity();

    let wire = RasterBuf::Pixels(pixels).into_wire();
    let back = RasterBuf::from_wire(wire);
    let RasterBuf::Pixels(round_tripped) = &back else {
        panic!("from_wire did not produce a Pixels arm");
    };
    assert_eq!(
        round_tripped.as_ptr() as usize,
        addr,
        "the wire round trip reallocated the picture; the funnel would be \
         copying every raster it carries",
    );
    assert_eq!(
        round_tripped.capacity(),
        cap,
        "the same allocation keeps its capacity"
    );
    assert_eq!(back, RasterBuf::Pixels(sample_pixels()), "the bytes moved");
}

/// **The precondition [`RasterBuf::transparent_pixels`] rests on.** That
/// constructor answers `alloc_zeroed` where it used to write
/// `Color32::TRANSPARENT` into every pixel, and the two are the same picture
/// only while `TRANSPARENT` is four zero bytes. `ecolor` deciding otherwise —
/// an opaque-black transparent, a different channel order with a sentinel —
/// would turn a blank loop frame and every transport copy destination into
/// zeros silently, with no other test in the tree noticing.
#[test]
fn transparent_is_four_zero_bytes() {
    assert_eq!(
        bytemuck::bytes_of(&Color32::TRANSPARENT),
        &[0, 0, 0, 0],
        "Color32::TRANSPARENT is no longer four zero bytes, so a zeroed \
         allocation is no longer the picture `transparent_pixels` promises",
    );
}

/// And the constructor itself answers that picture, at a length that is not a
/// round number of anything.
#[test]
fn a_transparent_buffer_is_transparent_in_both_arms() {
    let buf = RasterBuf::transparent(7);
    assert_eq!(
        buf.as_bytes(),
        [0u8; 28],
        "28 bytes, every one of them zero"
    );
    assert_eq!(
        RasterBuf::transparent_pixels(7),
        vec![Color32::TRANSPARENT; 7],
        "the pixels spelling agrees with the macro it replaced",
    );
}
