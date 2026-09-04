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
        AdapterCeilings, BudgetLimits, DeviceProfile, FormFactor, Platform, Promotion, resolve,
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
            // The rig's Xvfb legs run with a mouse: form factor Desktop, which the
            // ceiling asks for since the form factor is read.
            form_factor: Some(FormFactor::Desktop),
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

/// **The overlay picture sizes are placed by pane index, read off the recorded
/// plan, and absent where nothing was dispatched.**
///
/// The formatter's own tests cannot see any of that — they are handed a list.
/// This is the gate on where the list comes from, and it exists because a
/// tamper that made the reader return zeros left every formatter test green.
///
/// Position is the whole contract: `px` in the line is read positionally as
/// the pane index, so a reader that packed only the panes it found would
/// report pane 2's picture as pane 1's.
#[test]
fn overlay_picture_sizes_land_on_their_own_pane_and_nowhere_else() {
    use squallar_egui::overlay_cache::OverlayTexturePlan;

    let plan = |w: u32, h: u32| crate::app::fetch::OverlayRenderRequest {
        geo_bounds: squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        texture: OverlayTexturePlan {
            width: w,
            height: h,
            overdraw: 0.0,
            pixels_per_point: 1.0,
            pane_px: [0, 0],
        },
        data_generation: 1,
        zoom: 32,
    };

    // This build's own budgets: the reader hands back a recorded plan and
    // consults none of them, so the test asks for nothing the shipping app
    // does not already construct.
    let mut render = RenderDispatcher::new();
    // Panes 0 and 2 have dispatched; 1 and 3 have not.
    render.record_overlay_dispatch(0, &squallar_source::id::known::NWS_ALERTS, plan(2880, 1555));
    render.record_overlay_dispatch(2, &squallar_source::id::known::NWS_ALERTS, plan(1440, 780));
    // A second layer on pane 0 agrees with the first, as every layer on one
    // pane does — the plan is a function of the pane rect and the adapter's
    // limit, both of which they share.
    render.record_overlay_dispatch(
        0,
        &squallar_source::id::known::STORM_REPORTS,
        plan(2880, 1555),
    );

    assert_eq!(
        render.overlay_picture_sizes(4),
        vec![(2880, 1555), (0, 0), (1440, 780), (0, 0)],
        "a pane's picture is not reported at its own index, or an undispatched \
         pane is not reported as absent",
    );
    // A pane index past the scene is dropped rather than growing the list:
    // the list length IS the pane count the line reports as `n`.
    render.record_overlay_dispatch(9, &squallar_source::id::known::NWS_ALERTS, plan(64, 64));
    assert_eq!(render.overlay_picture_sizes(4).len(), 4);
}

/// **The resident picture load is per `(pane, layer)`, and the per-pane list
/// is not it folded up.**
///
/// These are two different questions off one record and the defect they gate
/// is reading the first as the second. `overlay_picture_sizes` answers "how
/// big is a pane's picture" — one entry per pane, which is what a surface
/// check holds a bracket's uploaded bytes against. `resident_overlay_pictures`
/// answers "how many pictures is this page carrying and what do they weigh",
/// which is what the page heap pays and what the host need model prices.
///
/// The figures are the Tier-2 `huge` leg's own: one pane of 2878 x 1611
/// physical pixels at 1.5x oversampling is a 4317 x 2416 picture of
/// 41,719,488 B, and the leg showed thirteen texture layers on it. Reported
/// per pane that is one picture of 41,719,488 B — 40 MiB, which fits three
/// quarters of a 1 GiB page heap with room to spare. Reported per picture it
/// is 542,353,344 B, 517 MiB, and with the arrival the model adds it is the
/// 557 MiB the leg could not hold. Both figures are on the line; neither is
/// the other.
#[test]
fn resident_pictures_count_every_layer_where_the_pane_list_counts_panes() {
    use squallar_egui::overlay_cache::OverlayTexturePlan;

    let huge = crate::app::fetch::OverlayRenderRequest {
        geo_bounds: squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        texture: OverlayTexturePlan {
            width: 4317,
            height: 2416,
            overdraw: 0.5,
            pixels_per_point: 1.0,
            pane_px: [2878, 1611],
        },
        data_generation: 1,
        zoom: 32,
    };

    // The leg's thirteen texture layers on one pane. Any thirteen distinct
    // ids: the record is keyed by `(pane, id)` and the count is of keys.
    let ids = [
        squallar_source::id::known::NWS_ALERTS,
        squallar_source::id::known::STORM_REPORTS,
        squallar_source::id::known::SPC_OUTLOOK,
        squallar_source::id::known::SPC_FIRE_OUTLOOK,
        squallar_source::id::known::SPC_DISCUSSIONS,
        squallar_source::id::known::MRMS,
        squallar_source::id::known::GMGSI,
        squallar_source::id::known::MODEL_DATA,
        squallar_source::id::known::LIGHTNING,
        squallar_source::id::known::METAR,
        squallar_source::id::known::CITY_LABELS,
        squallar_source::id::known::RADAR_SITES,
        squallar_source::id::known::RADAR_COVERAGE,
    ];
    let mut render = RenderDispatcher::new();
    for id in &ids {
        render.record_overlay_dispatch(0, id, huge.clone());
    }

    assert_eq!(
        render.overlay_picture_sizes(1),
        vec![(4317, 2416)],
        "the per-pane list stopped being one entry per pane",
    );
    assert_eq!(
        render.resident_overlay_pictures(),
        (13, 542_353_344),
        "the resident load is not every layer's picture: this is the figure \
         the page heap pays and the host need model prices, and reading the \
         pane list in its place is how the `huge` leg was fitted at 40 MiB \
         of pictures when it held 517",
    );
    assert_eq!(render.overlay_picture_count(0), 13);
    assert_eq!(
        render.overlay_picture_count(1),
        0,
        "a pane that has dispatched nothing was charged for a picture",
    );

    // A second pane's layers are its own on both readings.
    render.record_overlay_dispatch(1, &squallar_source::id::known::NWS_ALERTS, huge.clone());
    assert_eq!(render.overlay_picture_count(0), 13);
    assert_eq!(render.overlay_picture_count(1), 1);
    assert_eq!(render.resident_overlay_pictures().0, 14);
}
