//! What a finished raster's bytes are read as, and what the dispatcher offers a render
//! before it starts.

use super::*;

/// A frame decoded off the worker wire hands its buffer to the `ColorImage`
/// **by move**: the pixels vec IS the vec the decode materialized, and its
/// content is byte-for-byte what the copying constructor produces. On wasm
/// the deliver runs on the page thread, which is what makes the copy this
/// pins away worth pinning away.
#[test]
fn a_wire_decoded_frame_gives_its_buffer_to_the_image_without_a_copy() {
    use squallar_radar::frame::{RasterImage, RenderedFrame};

    let side = squallar_radar::types::IMAGE_SIZE;
    let rgba: Vec<u8> = (0..side * side * 4).map(|i| (i % 251) as u8).collect();
    let frame = RenderedFrame {
        image: RasterImage::Bytes(rgba.clone()),
        max_range_km: 230.0,
        polar: Default::default(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    };

    // Encoded exactly as the frame reply codec writes it, decoded through the
    // same `from_parts` the wire path runs.
    let mut head = Vec::new();
    frame.write_head(&mut head);
    let tails = vec![frame.polar.to_bytes(), rgba.clone()];
    let decoded = RenderedFrame::from_parts(&head, tails).expect("the frame reply decodes");

    let RasterImage::Pixels(pixels) = &decoded.image else {
        panic!(
            "a wire-decoded frame arrived as bytes, not as egui's layout — \
             the consumer's ColorImage will copy it on the thread the reply \
             lands on"
        );
    };
    let decode_ptr = pixels.as_ptr();

    let expected = egui::ColorImage::from_rgba_premultiplied([side, side], &rgba);
    let image = rendered_image_from(decoded)
        .expect("the frame becomes an image")
        .image;
    assert_eq!(image.size, expected.size);
    assert_eq!(
        image.pixels, expected.pixels,
        "the moved buffer is not the picture the copying constructor makes",
    );
    assert!(
        std::ptr::eq(image.pixels.as_ptr(), decode_ptr),
        "the image's pixels vec is not the decode's vec: a copy happened \
         between decode and image",
    );
}

/// Every side a render can come back at converts, and each is read at its own side rather
/// than at a constant.
#[test]
fn every_raster_size_this_build_renders_converts_at_its_own_side() {
    for (side, what) in [
        (
            squallar_device_profile::constants::LOOP_IMAGE_SIZE,
            "a loop frame",
        ),
        (squallar_radar::types::IMAGE_SIZE, "a render at the floor"),
        (
            squallar_device_profile::constants::LONG_RANGE_IMAGE_SIZE,
            "a long-range render",
        ),
        (
            squallar_device_profile::budget::BudgetLimits::for_target()
                .raster_side_ceiling_px
                .ceiling,
            "a render at the largest side this build's bracket allows",
        ),
        (
            (squallar_device_profile::budget::BudgetLimits::for_target()
                .raster_side_ceiling_px
                .ceiling
                * 9
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
    let long_range = squallar_device_profile::constants::LONG_RANGE_IMAGE_SIZE;
    let base = squallar_radar::types::IMAGE_SIZE;
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
        squallar_radar::types::IMAGE_SIZE,
        "before a device exists the answer must be the size every device holds",
    );

    for side in [
        squallar_radar::types::IMAGE_SIZE,
        squallar_device_profile::constants::LONG_RANGE_IMAGE_SIZE,
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
    dispatcher.set_raster_side_ceiling_px(squallar_radar::types::IMAGE_SIZE);
    assert_eq!(
        dispatcher.static_side_ceiling_px(),
        squallar_radar::types::IMAGE_SIZE,
    );
}

/// **The ceiling `AppState::new` computes is not the ceiling a promoted browser
/// is owed, so `update_device_profile` must re-derive it.**
///
/// Two figures come off the same adapter report, and they differ:
///
/// * `AppState::new` runs `budgets.raster_side_for_adapter(..)` against the
///   budgets `App::new` resolved from `DeviceProfile::for_target()` —
///   which carries `AdapterCeilings::WEBGL2_GUARANTEE` and so resolves
///   `Promotion::Floor` on every target, browser or not;
/// * `update_device_profile` re-resolves against the adapter that has since
///   answered, and on the web arm that is the only signal separating a
///   workstation GPU from a blocklisted driver.
///
/// Before WS1 the two agreed on the web arm because the bracket was pinned, so
/// the missing re-push cost nothing and nothing noticed it. It costs the whole
/// promotion now. This test is that gap written down.
///
/// **What this test does NOT do**, said plainly because the distinction is the
/// whole value of it: it reproduces the two computations from the same inputs,
/// it does not execute `App::update_device_profile`. That function needs a live
/// wgpu device, and no test in this crate has one. So deleting the re-derivation
/// block would leave this green — what goes red is re-pinning the bracket, which
/// is the *premise* the block rests on rather than the block itself. The
/// remaining link, `update_device_profile` actually running its own re-push, is
/// covered by nothing here and by nothing in CI; it was confirmed by reading,
/// not by execution.
#[test]
fn the_ceiling_app_state_computes_first_is_not_the_one_a_promoted_browser_is_owed() {
    use squallar_device_profile::budget::{
        AdapterCeilings, BudgetLimits, DeviceProfile, Platform, Promotion, resolve,
    };

    // Firefox 153 and Chromium 151 both reported this on a real driver,
    // measured 2026-08-22 by `.github/browser-rig/run_gpu_arm.sh`. The
    // software legs of the same run are the second row.
    for (leg, two_d, three_d, promotes) in [
        ("a browser on a real driver", 32768u32, 16384u32, true),
        ("a browser on llvmpipe", 16384, 2048, false),
        ("a browser on SwiftShader", 8192, 2048, false),
    ] {
        let web = |adapter| DeviceProfile {
            platform: Platform::Web,
            limits: BudgetLimits::WASM,
            adapter,
            ..DeviceProfile::for_target()
        };
        // What `App::new` had, and so what `AppState::new` spent.
        let before = resolve(&web(AdapterCeilings::WEBGL2_GUARANTEE));
        let pre_adapter = before.raster_side_for_adapter(two_d);

        // What `update_device_profile` resolves once the adapter has answered.
        let after = resolve(&web(AdapterCeilings {
            max_texture_dimension_2d: two_d,
            max_texture_dimension_3d: three_d,
        }));
        let post_adapter = after.raster_side_for_adapter(two_d);

        assert_eq!(before.promotion, Promotion::Floor, "{leg}");
        if promotes {
            assert_eq!(after.promotion, Promotion::Ceiling, "{leg}");
            assert!(
                post_adapter > pre_adapter,
                "{leg}: {pre_adapter} px before the adapter answered and \
                 {post_adapter} px after — if these are equal the re-derivation \
                 in `update_device_profile` is unreachable and the promotion \
                 never leaves the resolver",
            );
        } else {
            assert_eq!(after.promotion, Promotion::Floor, "{leg}");
            assert_eq!(
                post_adapter, pre_adapter,
                "{leg}: a software rasteriser was moved off the ceiling it \
                 renders at today",
            );
        }

        // And the dispatcher really does take the number it is handed, at both
        // rungs, rather than re-deriving one of its own.
        let mut dispatcher = RenderDispatcher::new();
        dispatcher.set_raster_side_ceiling_px(post_adapter);
        assert_eq!(dispatcher.static_side_ceiling_px(), post_adapter, "{leg}");
    }
}
