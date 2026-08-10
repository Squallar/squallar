use super::*;

/// A well-formed buffer converts, and keeps the dimensions the rest of the loop
/// machinery assumes.
#[test]
fn a_full_size_buffer_converts() {
    let rgba = vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4];
    let image = loop_frame_image(&rgba).expect("a correctly sized buffer must convert");
    assert_eq!(image.size, [IMAGE_SIZE, IMAGE_SIZE]);
    assert_eq!(image.pixels.len(), IMAGE_SIZE * IMAGE_SIZE);
}

/// The reason the guard exists: on the worker thread the assert inside
/// `from_rgba_unmultiplied` would kill the thread silently, no response would be
/// sent, and the frame would sit `render_in_flight` forever.
#[test]
fn a_malformed_buffer_is_rejected_rather_than_panicking() {
    let short = IMAGE_SIZE * IMAGE_SIZE * 4 - 4;
    let long = IMAGE_SIZE * IMAGE_SIZE * 4 + 4;
    assert!(
        loop_frame_image(&vec![0u8; short]).is_none(),
        "short buffer"
    );
    assert!(loop_frame_image(&vec![0u8; long]).is_none(), "long buffer");
    assert!(loop_frame_image(&[]).is_none(), "empty buffer");
}

/// Pixel values survive the conversion — a frame that converted to transparent
/// black would render as nothing and look exactly like a frame that never rendered.
#[test]
fn pixel_values_survive_the_conversion() {
    let mut rgba = vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4];
    rgba[0..4].copy_from_slice(&[10, 20, 30, 255]);
    let image = loop_frame_image(&rgba).unwrap();
    assert_eq!(
        image.pixels[0],
        egui::Color32::from_rgba_unmultiplied(10, 20, 30, 255)
    );
    assert_ne!(image.pixels[0], egui::Color32::TRANSPARENT);
}
