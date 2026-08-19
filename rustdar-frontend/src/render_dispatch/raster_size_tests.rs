//! What a finished raster's bytes are read as, and what the dispatcher offers
//! a render before it starts.

use super::*;

/// Every side a render can come back at converts, and each is read at its own
/// side rather than at a constant.
///
/// The third row is the one that used to be the only row. The others are the
/// ends the size cascade added — a browser's loop frame below the base size, a
/// long-range static render above it — and the last two are what a
/// device-derived ceiling adds: a side that is in no constant anywhere, and is
/// not a power of two, because a real surveillance cut asks for 7362 px.
#[test]
fn every_raster_size_this_build_renders_converts_at_its_own_side() {
    for (side, what) in [
        (
            rustdar_device_profile::constants::LOOP_IMAGE_SIZE,
            "a loop frame",
        ),
        (rustdar_radar::types::IMAGE_SIZE, "a render at the floor"),
        (
            rustdar_device_profile::constants::LONG_RANGE_IMAGE_SIZE,
            "a long-range render",
        ),
        (
            rustdar_device_profile::budget::BudgetLimits::for_target().raster_side_ceiling_px,
            "a render at the largest side this build's bracket allows",
        ),
        (
            (rustdar_device_profile::budget::BudgetLimits::for_target().raster_side_ceiling_px * 9
                / 10)
                | 1,
            "an odd side no constant names",
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
/// Not defensive padding. `ColorImage::from_rgba_premultiplied` asserts on a
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
    let long_range = rustdar_device_profile::constants::LONG_RANGE_IMAGE_SIZE;
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

/// The device decides the ceiling, and before there is a device the answer is
/// the size every device holds.
///
/// A dispatcher exists before a device does — the frame loop returns before
/// `dispatch_pane_renders` while `AppState` is `None`, so nothing in the
/// shipped app dispatches through the default — and the direction the default
/// falls in is the whole point: the base size is a correct picture on any
/// device, where a size the GPU refuses is a blank pane behind a swallowed
/// error.
///
/// The middle assertion is the one that used to be impossible. The ceiling was
/// a `bool` turned back into `LONG_RANGE_IMAGE_SIZE`, so a device offering
/// 8192 and a device offering exactly 4096 were dispatched identically; here
/// the number the device gave is the number that comes back out.
#[test]
fn a_static_render_takes_the_ceiling_the_device_reported_and_no_other() {
    let mut dispatcher = RenderDispatcher::new();
    assert_eq!(
        dispatcher.static_side_ceiling_px(),
        rustdar_radar::types::IMAGE_SIZE,
        "before a device exists the answer must be the size every device holds",
    );

    for side in [
        rustdar_radar::types::IMAGE_SIZE,
        rustdar_device_profile::constants::LONG_RANGE_IMAGE_SIZE,
        8192,
    ] {
        dispatcher.set_raster_side_ceiling_px(side);
        assert_eq!(
            dispatcher.static_side_ceiling_px(),
            side,
            "the dispatcher must offer what the device said, not a constant",
        );
    }

    // And a device that reports less closes it again — a lost surface rebuilds
    // `AppState`, so this is not a one-way latch.
    dispatcher.set_raster_side_ceiling_px(rustdar_radar::types::IMAGE_SIZE);
    assert_eq!(
        dispatcher.static_side_ceiling_px(),
        rustdar_radar::types::IMAGE_SIZE,
    );
}
