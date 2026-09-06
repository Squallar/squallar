use squallar_device_profile::constants::LOOP_IMAGE_SIZE;

use super::*;

/// A well-formed buffer converts, and keeps the dimensions the rest of the loop
/// machinery assumes.
#[test]
fn a_full_size_buffer_converts() {
    let rgba = vec![0u8; LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE * 4];
    let image =
        loop_frame_image(&rgba, LOOP_IMAGE_SIZE).expect("a correctly sized buffer must convert");
    assert_eq!(image.size, [LOOP_IMAGE_SIZE, LOOP_IMAGE_SIZE]);
    assert_eq!(image.pixels.len(), LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE);
}

/// The reason the guard exists: on the worker thread the assert inside
/// `from_rgba_premultiplied` would kill the thread silently.
#[test]
fn a_malformed_buffer_is_rejected_rather_than_panicking() {
    let short = LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE * 4 - 4;
    let long = LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE * 4 + 4;
    assert!(
        loop_frame_image(&vec![0u8; short], LOOP_IMAGE_SIZE).is_none(),
        "short buffer"
    );
    assert!(
        loop_frame_image(&vec![0u8; long], LOOP_IMAGE_SIZE).is_none(),
        "long buffer"
    );
    assert!(
        loop_frame_image(&[], LOOP_IMAGE_SIZE).is_none(),
        "empty buffer"
    );
    let long_range = squallar_device_profile::constants::LONG_RANGE_IMAGE_SIZE;
    assert!(
        loop_frame_image(&vec![0u8; long_range * long_range * 4], LOOP_IMAGE_SIZE).is_none(),
        "a long-range static raster is not a loop frame",
    );
}

/// Pixel values survive the conversion — a frame that converted to transparent
/// black would render as nothing and look exactly like a frame that never rendered.
#[test]
fn pixel_values_survive_the_conversion() {
    let mut rgba = vec![0u8; LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE * 4];
    let painted = egui::Color32::from_rgba_unmultiplied(10, 20, 30, 180);
    rgba[0..4].copy_from_slice(&painted.to_array());
    let image = loop_frame_image(&rgba, LOOP_IMAGE_SIZE).unwrap();
    assert_eq!(image.pixels[0], painted);
    assert_ne!(image.pixels[0], egui::Color32::TRANSPARENT);
}

/// **The transition gate**: a frame is measured against the side it was
/// dispatched at, not against a constant — so a buffer that is right for one
/// ceiling and wrong for another converts under its own and is refused under
/// the other. This is what keeps lowering the texture ceiling from silently
/// voiding every frame already in flight.
#[test]
fn a_frame_is_measured_against_the_side_it_was_dispatched_at() {
    let small = 512usize;
    let rgba = vec![0u8; small * small * 4];
    let image = loop_frame_image(&rgba, small).expect("its own side converts");
    assert_eq!(image.size, [small, small]);
    assert!(
        loop_frame_image(&rgba, LOOP_IMAGE_SIZE).is_none(),
        "the same buffer measured against a different side is refused",
    );
    let owned = vec![egui::Color32::TRANSPARENT; small * small];
    assert_eq!(
        loop_frame_image_owned(owned.clone(), small)
            .expect("its own side converts")
            .size,
        [small, small],
    );
    assert!(
        loop_frame_image_owned(owned, LOOP_IMAGE_SIZE).is_none(),
        "the owned arm refuses on the same terms",
    );
}

/// A zero side is refused rather than dividing a buffer by nothing: the
/// budget field it comes from is a `usize` and nothing in the type stops a
/// future fit handing back zero.
#[test]
fn a_zero_side_converts_nothing() {
    assert!(loop_frame_image(&[], 0).is_none());
    assert!(loop_frame_image_owned(Vec::new(), 0).is_none());
}
