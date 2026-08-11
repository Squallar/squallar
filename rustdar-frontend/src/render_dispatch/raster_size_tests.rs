//! What a finished raster's bytes are read as, and what the dispatcher offers
//! a render before it starts.

use super::*;

/// The three sizes a render can come back at all convert, and each one is read
/// at its own side rather than at a constant.
///
/// The middle row is the one that used to be the only row. The others are the
/// two ends the size cascade added: a browser's loop frame below the base size
/// and a long-range static render above it.
#[test]
fn every_raster_size_this_build_renders_converts_at_its_own_side() {
    for (side, what) in [
        (crate::constants::LOOP_IMAGE_SIZE, "a loop frame"),
        (rustdar_radar::types::IMAGE_SIZE, "a render at the floor"),
        (
            crate::constants::LONG_RANGE_IMAGE_SIZE,
            "a long-range render",
        ),
    ] {
        let image = plan_view_image(&vec![0u8; side * side * 4])
            .unwrap_or_else(|| panic!("{what} at {side} px must convert"));
        assert_eq!(image.size, [side, side], "{what}");
        assert_eq!(image.pixels.len(), side * side, "{what}");
    }
}

/// A length no render produces is refused rather than converted.
///
/// Not defensive padding. `ColorImage::from_rgba_unmultiplied` asserts on a
/// mismatch and this runs on a native render thread: a panic there sends no
/// `RenderResponse` at all, so `render_in_flight` never clears and the pane
/// stops asking for renders for the rest of its life. `None` routes the same
/// buffer down the "no matching sweep" path, which the dispatcher retires
/// cleanly.
///
/// The odd lengths are the ones that would get *furthest*: a buffer one pixel
/// short of a real raster is a plausible truncation, and a length that is not
/// a multiple of four is not even a whole number of pixels.
#[test]
fn a_length_no_render_produces_is_refused_rather_than_asserted_on() {
    let long_range = crate::constants::LONG_RANGE_IMAGE_SIZE;
    let base = rustdar_radar::types::IMAGE_SIZE;
    for (len, why) in [
        (0, "an empty buffer"),
        (1, "one byte"),
        (7, "not a whole number of pixels"),
        (base * base * 4 - 4, "one pixel short of the base raster"),
        (
            long_range * long_range * 4 - 1,
            "one byte short of the long-range raster",
        ),
        (
            long_range * long_range * 4 + 4,
            "one pixel past the long-range raster",
        ),
    ] {
        assert!(
            plan_view_image(&vec![0u8; len]).is_none(),
            "{why} ({len} bytes) must be refused",
        );
    }
}

/// The device gate decides `full_res`, and it starts closed.
///
/// A dispatcher exists before a device does — the frame loop returns before
/// `dispatch_pane_renders` while `AppState` is `None`, so nothing in the
/// shipped app dispatches through the default — and the direction the default
/// falls in is the whole point: base size is a correct picture on any device,
/// where a size the GPU refuses is a blank pane behind a swallowed error.
#[test]
fn a_static_render_takes_the_long_range_raster_only_once_the_device_has_said_so() {
    let mut dispatcher = RenderDispatcher::new();
    assert!(
        !dispatcher.static_full_res(),
        "before a device exists the answer must be the size every device holds",
    );

    dispatcher.set_long_range_raster_ok(true);
    assert!(dispatcher.static_full_res());

    // And a device that reports less than the long-range side closes it again
    // — a lost surface rebuilds `AppState`, so this is not a one-way latch.
    dispatcher.set_long_range_raster_ok(false);
    assert!(!dispatcher.static_full_res());
}
