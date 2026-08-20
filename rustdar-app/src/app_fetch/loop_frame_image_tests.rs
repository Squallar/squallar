use super::*;

/// A well-formed buffer converts, and keeps the dimensions the rest of the loop
/// machinery assumes.
#[test]
fn a_full_size_buffer_converts() {
    let rgba = vec![0u8; LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE * 4];
    let image = loop_frame_image(&rgba).expect("a correctly sized buffer must convert");
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
        loop_frame_image(&vec![0u8; short]).is_none(),
        "short buffer"
    );
    assert!(loop_frame_image(&vec![0u8; long]).is_none(), "long buffer");
    assert!(loop_frame_image(&[]).is_none(), "empty buffer");
    let long_range = rustdar_device_profile::constants::LONG_RANGE_IMAGE_SIZE;
    assert!(
        loop_frame_image(&vec![0u8; long_range * long_range * 4]).is_none(),
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
    let image = loop_frame_image(&rgba).unwrap();
    assert_eq!(image.pixels[0], painted);
    assert_ne!(image.pixels[0], egui::Color32::TRANSPARENT);
}
