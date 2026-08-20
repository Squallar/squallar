//! What a finished raster's bytes are read as, and what the dispatcher offers a render
//! before it starts.

use super::*;

/// Every side a render can come back at converts, and each is read at its own side rather
/// than at a constant.
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

/// The device decides the ceiling, and before there is a device the answer is the size
/// every device holds.
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

    // And a device that reports less closes it again — a lost surface rebuilds `AppState`,
    // so this is not a one-way latch.
    dispatcher.set_raster_side_ceiling_px(rustdar_radar::types::IMAGE_SIZE);
    assert_eq!(
        dispatcher.static_side_ceiling_px(),
        rustdar_radar::types::IMAGE_SIZE,
    );
}
