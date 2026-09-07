//! One sweep, one texture, however many panes are looking at it.

use super::*;
use crate::app::tests::{drain_uploads, n_pane_app};
use squallar_radar::types::RadarProduct;
use squallar_source::id::known;

const SITE: &str = "KTLX";
const OTHER_SITE: &str = "KMPX";
const TILT: f32 = 0.5;

/// The raster size these tests use — deliberately not `IMAGE_SIZE`.
const SIDE: usize = 4;

/// Pixels whose bytes depend on `seed`, so a pane handed the wrong raster is a failed
/// comparison rather than a coincidence of two blanks.
fn raster(seed: u8) -> Arc<egui::ColorImage> {
    let mut rgba = Vec::with_capacity(SIDE * SIDE * 4);
    for i in 0..(SIDE * SIDE) as u8 {
        let a = match i % 4 {
            0 => 0,
            1 => 180,
            2 => 255,
            _ => 3,
        };
        rgba.extend_from_slice(&[seed, i.wrapping_mul(17), seed ^ i, a]);
    }
    Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [SIDE, SIDE],
        &rgba,
    ))
}

/// Aim a pane at `site` showing `product` at [`TILT`], far enough along that the broadcast
/// will accept it and `apply_render_to_pane` will not bail out.
fn point_at(app: &mut crate::app::App, pane_idx: usize, site: &str, product: RadarProduct) {
    point_at_tilts(app, pane_idx, site, product, &[TILT]);
}

/// As [`point_at`], but with a volume offering more than one tilt — which is what makes a
/// tilt change reachable at all, since `PaneState::get_rendering_params` snaps the
/// selection onto a tilt the scan actually carries.
fn point_at_tilts(
    app: &mut crate::app::App,
    pane_idx: usize,
    site: &str,
    product: RadarProduct,
    tilts: &[f32],
) {
    let radar = squallar_radar::sites::get_radar_site(site)
        .unwrap_or_else(|| panic!("{site} is a real radar"))
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, tilts.to_vec());
    let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
    pane.set_site(site.to_string());
    pane.set_selected_product(squallar_radar::fields::spec(product).id.clone());
    pane.set_selected_elevation(tilts[0]);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx,
            info: squallar_radar::types::ScanInfo {
                site: radar,
                site_source: squallar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
                    .unwrap()
                    .and_hms_opt(18, 30, 0)
                    .unwrap(),
                vcp_number: 212,
                available_products: vec![product],
                product_elevations,
                status: String::new(),
            },
        });
}

/// A finished render landing on the channel for `pane_idx`, then drained.
fn deliver(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    pane_idx: usize,
    product: RadarProduct,
    image: Arc<egui::ColorImage>,
) {
    deliver_at(app, ctx, pane_idx, product, TILT, image);
}

/// As [`deliver`], at a tilt the caller names.
fn deliver_at(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    pane_idx: usize,
    product: RadarProduct,
    elevation: f32,
    image: Arc<egui::ColorImage>,
) {
    post(app, pane_idx, product, elevation, image);
    app.poll_render_results(ctx);
}

/// Put a finished render on the channel **without** draining it.
fn post(
    app: &mut crate::app::App,
    pane_idx: usize,
    product: RadarProduct,
    elevation: f32,
    image: Arc<egui::ColorImage>,
) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                image,
                max_range_km: 230.0,
                hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
            }),
            product,
            elevation,
            generation: app.render.render_generation,
            pane_idx,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
}

/// The texture id pane `pane_idx` is drawing its radar image with.
fn placed(app: &mut crate::app::App, pane_idx: usize) -> egui::TextureId {
    app.gui
        .pane_mut(pane_idx)
        .expect("pane exists")
        .overlay_cache_mut(&known::RADAR)
        .current()
        .expect("this pane was served a radar texture")
        .texture
        .id()
}

/// A split on one site is one upload, not one per pane.
#[test]
fn every_pane_on_one_sweep_shares_a_single_upload() {
    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::CorrelationCoefficient,
        RadarProduct::EchoTops,
    ] {
        for panes in [1, 2, 4] {
            let ctx = egui::Context::default();
            let mut app = n_pane_app(panes, SITE);
            for pane_idx in 0..panes {
                point_at(&mut app, pane_idx, SITE, product);
            }
            let _ = drain_uploads(&ctx);

            deliver(&mut app, &ctx, 0, product, raster(panes as u8));

            let uploads = drain_uploads(&ctx);
            assert_eq!(
                uploads.len(),
                1,
                "{product:?}, {panes} panes: {} uploads of one sweep. \
                 The panes hold the same `Arc<ColorImage>` — the render cache \
                 shares it — so every upload past the first is 16 MiB of \
                 duplicate VRAM and a whole `queue.write_texture` on the frame \
                 thread.",
                uploads.len(),
            );

            let first = placed(&mut app, 0);
            for pane_idx in 1..panes {
                assert_eq!(
                    placed(&mut app, pane_idx),
                    first,
                    "{product:?}, {panes} panes: pane {pane_idx} was given its \
                     own texture rather than a clone of the handle pane 0 \
                     already holds",
                );
            }
        }
    }
}

/// The shared texture holds the renderer's pixels, unchanged.
#[test]
fn the_shared_texture_holds_the_renderers_own_pixels() {
    for product in [RadarProduct::Reflectivity, RadarProduct::EchoTops] {
        for panes in [1, 2, 4] {
            let ctx = egui::Context::default();
            let mut app = n_pane_app(panes, SITE);
            for pane_idx in 0..panes {
                point_at(&mut app, pane_idx, SITE, product);
            }
            let _ = drain_uploads(&ctx);
            let expected = raster(product.wire_code() as u8);

            deliver(&mut app, &ctx, 0, product, Arc::clone(&expected));

            let uploads = drain_uploads(&ctx);
            assert_eq!(uploads.len(), 1, "{product:?}, {panes} panes");
            assert_eq!(
                uploads[0].size, expected.size,
                "{product:?}, {panes} panes: the uploaded texture is the wrong \
                 shape"
            );
            assert_eq!(
                uploads[0].pixels, expected.pixels,
                "{product:?}, {panes} panes: the pixels on the GPU are not the \
                 ones the renderer produced"
            );
        }
    }
}

/// Two panes on **different** sites get two textures, because they are two pictures — and
/// the memo has to hold both at once to be asked the question.
#[test]
fn two_sites_in_one_drain_do_not_share_a_texture() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(2, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity);
    point_at(&mut app, 1, OTHER_SITE, RadarProduct::Reflectivity);
    let _ = drain_uploads(&ctx);

    // Same dimensions, different pixels: a key that compared shape rather than identity
    // would call these one raster. Kept, so the identity assertions below have
    // something to compare against.
    let first_raster = raster(1);
    let second_raster = raster(2);
    post(
        &mut app,
        0,
        RadarProduct::Reflectivity,
        TILT,
        Arc::clone(&first_raster),
    );
    post(
        &mut app,
        1,
        RadarProduct::Reflectivity,
        TILT,
        Arc::clone(&second_raster),
    );
    app.poll_render_results(&ctx);

    assert_eq!(
        drain_uploads(&ctx).len(),
        2,
        "two sites in one drain are two textures"
    );
    assert_ne!(
        placed(&mut app, 0),
        placed(&mut app, 1),
        "a pane on {OTHER_SITE} is drawing {SITE}'s texture"
    );
    // And *which buffer* each pane was shown, not merely that the handles
    // differ: a shared handle is only wrong because of what it paints. Buffer
    // identity is the stronger half of the old pixel comparison — two rasters
    // could compare equal by accident, but only one allocation is the one this
    // pane's texture was uploaded from.
    assert!(
        app.render.pane_render[0].shows_buffer(&first_raster),
        "pane 0 was shown a raster other than the one delivered for it",
    );
    assert!(
        app.render.pane_render[1].shows_buffer(&second_raster),
        "pane 1 was shown a raster other than the one delivered for it",
    );
    assert!(
        !app.render.pane_render[0].shows_buffer(&second_raster),
        "pane 0 claims the other site's buffer",
    );
}

/// **A resume puts every pane back with one upload** — off the render cache,
/// through the ordinary dispatch, since 2026-09-07.
///
/// The panes used to hold a CPU copy of the raster apiece for exactly this
/// moment, and `restore_cached_render` re-uploaded from it. They hold no
/// pixels now: the teardown clears every `last_rendered`, and the `Dispatch`
/// row finds each pane wanting a picture and answers it from the shared
/// render cache. The observable is unchanged and that is the point — one
/// upload for four panes, one texture handle between them, and the pixels that
/// were on the glass.
#[test]
fn a_resume_puts_four_panes_back_with_one_upload() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(4, SITE);
    for pane_idx in 0..4 {
        point_at(&mut app, pane_idx, SITE, RadarProduct::Reflectivity);
    }
    let expected = raster(9);
    deliver(
        &mut app,
        &ctx,
        0,
        RadarProduct::Reflectivity,
        Arc::clone(&expected),
    );
    for pane_idx in 0..4 {
        assert!(
            app.render.pane_render[pane_idx].shows_buffer(&expected),
            "precondition: pane {pane_idx} must be showing the delivered raster"
        );
    }
    assert!(
        app.render
            .get_cached_render(
                SITE,
                RadarProduct::Reflectivity,
                squallar_radar::types::RenderView::PlanView,
                TILT,
            )
            .is_some(),
        "precondition: the render cache must be holding the raster the resume \
         is expected to come back from",
    );

    // What a suspend, a display change or a surface loss does: every handle
    // released, every pane's dedupe cleared, the shared render cache kept.
    app.gui.clear_graphics_state();
    app.render.clear_last_rendered();
    let _ = drain_uploads(&ctx);

    app.restore_cached_render(&ctx);
    assert!(
        drain_uploads(&ctx).is_empty(),
        "the restore uploaded a plan view itself. It is the section panes' \
         path now; a plan view comes back through the dispatch below, which is \
         what lets the pane stop holding one",
    );
    app.dispatch_pane_renders(&ctx);

    let uploads = drain_uploads(&ctx);
    assert_eq!(
        uploads.len(),
        1,
        "a four-pane resume cost {} uploads of one cached raster",
        uploads.len(),
    );
    assert_eq!(
        uploads[0].pixels, expected.pixels,
        "the restored pixels are not the ones that were on the glass"
    );
    let first = placed(&mut app, 0);
    for pane_idx in 1..4 {
        assert_eq!(
            placed(&mut app, pane_idx),
            first,
            "pane {pane_idx} came back from the resume with a texture of its own"
        );
    }
}

fn tilt_independent() -> Vec<RadarProduct> {
    RadarProduct::all()
        .iter()
        .copied()
        .filter(|p| p.tilt_independent_plan_view())
        .collect()
}

/// What a pane's radar texture says it depicts.
fn stamped_elevation(app: &mut crate::app::App, pane_idx: usize) -> f32 {
    app.gui
        .pane_mut(pane_idx)
        .expect("pane exists")
        .overlay_cache_mut(&known::RADAR)
        .current()
        .expect("this pane was served a radar texture")
        .radar_meta
        .as_ref()
        .expect("a radar texture describes itself")
        .elevation
}

/// Clicking to another tilt on a tilt-independent pane costs no upload — and still moves
/// the label.
#[test]
fn a_tilt_click_on_a_tilt_independent_pane_reuploads_nothing() {
    let products = tilt_independent();
    assert!(
        !products.is_empty(),
        "precondition: there are tilt-independent products to test"
    );
    for product in products {
        let ctx = egui::Context::default();
        let mut app = n_pane_app(1, SITE);
        point_at_tilts(&mut app, 0, SITE, product, &[TILT, 3.4]);
        deliver(&mut app, &ctx, 0, product, raster(5));
        let before = placed(&mut app, 0);
        assert_eq!(stamped_elevation(&mut app, 0), TILT, "{product:?}");
        let _ = drain_uploads(&ctx);

        app.gui
            .pane_mut(0)
            .expect("pane exists")
            .set_selected_elevation(3.4);
        app.dispatch_pane_renders(&ctx);

        assert!(
            drain_uploads(&ctx).is_empty(),
            "{product:?}: a tilt click re-uploaded the whole raster. The cache \
             collapses this product onto one slot, so the buffer handed back is \
             the buffer already on the GPU.",
        );
        assert_eq!(
            placed(&mut app, 0),
            before,
            "{product:?}: the pane swapped textures without uploading one"
        );
        assert_eq!(
            stamped_elevation(&mut app, 0),
            3.4,
            "{product:?}: the picture stayed and its label did not follow, so \
             `stale_image_on_screen` now disowns a correct image",
        );
    }
}

/// The control: a pane handed genuinely different pixels still uploads them, and retires
/// the texture it was showing — **once the new one is whole**.
#[test]
fn a_pane_handed_a_different_raster_uploads_it() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at_tilts(&mut app, 0, SITE, RadarProduct::Reflectivity, &[TILT, 3.4]);

    deliver(&mut app, &ctx, 0, RadarProduct::Reflectivity, raster(1));
    let first = placed(&mut app, 0);
    let _ = drain_uploads(&ctx);

    let second_raster = raster(2);
    app.gui
        .pane_mut(0)
        .expect("pane exists")
        .set_selected_elevation(3.4);
    deliver_at(
        &mut app,
        &ctx,
        0,
        RadarProduct::Reflectivity,
        3.4,
        Arc::clone(&second_raster),
    );

    let uploads = drain_uploads(&ctx);
    assert_eq!(uploads.len(), 1, "a new sweep must reach the GPU");
    assert_eq!(
        uploads[0].pixels, second_raster.pixels,
        "the pane uploaded something other than the sweep it was given"
    );
    // Uploaded, and *not yet shown*: the pane keeps a whole picture rather than filling a
    // new one in top-down over the frames its bands take.
    assert_eq!(
        placed(&mut app, 0),
        first,
        "the pane swapped onto a raster whose pixels had not all arrived"
    );
    assert!(
        ctx.tex_manager().read().meta(first).is_some(),
        "the picture still on screen was freed while it was the only whole one \
         the pane had"
    );

    app.deliver_held_rasters();
    assert_ne!(
        placed(&mut app, 0),
        first,
        "the pane is still drawing the previous sweep's texture"
    );
    // And the one it stopped drawing is *gone*, not parked.
    assert!(
        ctx.tex_manager().read().meta(first).is_none(),
        "the replaced texture is still allocated after the pane stopped drawing \
         it, so two generations of the same overlay are resident at once"
    );
}

/// **The pane keeps the buffer's identity and not the buffer.**
///
/// This is the falsifiable half of the `cached renders` census family, which
/// can only read zero and so cannot fail on its own. Until 2026-09-07 a pane
/// held `Arc::clone` of every raster it was shown, for restore: at the 7362 px
/// side a desktop plan view reaches that is 216,796,176 B a pane, held for the
/// life of the process, and the *sole* holder of those bytes the moment the
/// render cache dropped its own entry — which the FLOOR legs measured
/// happening at t=339 s and t=327 s with `live_bytes` not moving.
///
/// So: hand the app a raster, then let go of every other holder there is — the
/// texture manager's delta, the render cache entry, and this test's own clone
/// — and require the allocation to be gone. A pane that still holds pixels
/// fails here.
#[test]
fn a_pane_does_not_keep_the_pixels_it_was_shown() {
    // Deliberately not `SIDE`, and the reason is the whole point of the
    // precondition below: a 4 px raster is 64 B, which is also what an `Arc`
    // header costs, so a holder that had shrunk from a picture to a handle
    // would price the same as the picture and slide through. At 64 px the two
    // figures cannot be confused.
    const RELEASED_SIDE: usize = 64;
    const RELEASED_BYTES: usize =
        RELEASED_SIDE * RELEASED_SIDE * std::mem::size_of::<egui::Color32>();

    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity);
    let _ = drain_uploads(&ctx);

    let shown = Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [RELEASED_SIDE, RELEASED_SIDE],
        &vec![7u8; RELEASED_SIDE * RELEASED_SIDE * 4],
    ));
    let pixels = Arc::downgrade(&shown);
    deliver(
        &mut app,
        &ctx,
        0,
        RadarProduct::Reflectivity,
        Arc::clone(&shown),
    );

    // **Preconditions, named in bytes rather than in "not empty".** A release
    // assertion is only as good as the proof that there was something of a
    // known size to release: if the delivery upstream of this ever stops
    // producing a raster -- a render that is no longer dispatched at all, say
    // -- the allocation below would be dead because nothing ever held it, and
    // this test would pass while proving nothing. So: the pane took THIS
    // buffer, the shared cache is holding a raster of exactly the expected
    // byte size, and the allocation is alive at this instant.
    assert!(
        app.render.pane_render[0].shows_buffer(&shown),
        "precondition: the pane must have taken this raster",
    );
    let held = app
        .render
        .get_cached_render(
            SITE,
            RadarProduct::Reflectivity,
            squallar_radar::types::RenderView::PlanView,
            TILT,
        )
        .expect("precondition: the render cache must be holding the delivered raster");
    assert_eq!(
        held.image.pixels.len() * std::mem::size_of::<egui::Color32>(),
        RELEASED_BYTES,
        "precondition: the holder is not holding {RELEASED_BYTES} B of pixels, \
         so the release below is a release of something else -- or of nothing",
    );
    assert!(
        pixels.upgrade().is_some(),
        "precondition: the allocation must be ALIVE here, or `is_none()` below \
         is satisfied by a raster that was never built",
    );

    // Every other holder, let go of one at a time so a survivor is nameable.
    drop(drain_uploads(&ctx)); // egui's texture delta
    let (evicted, _) = app.render.clear_render_cache();
    drop(evicted); // the shared render cache entry

    // **Counted before it is dropped**, which is the sharper half of the two
    // assertions: with every named holder released, the only strong reference
    // left must be this test's own. `strong_count` says that directly, and it
    // says it about the app rather than about the `Weak` -- a survivor here is
    // a holder this test can then hunt by name, where a live `Weak` after the
    // final drop only says that somebody, somewhere, still has it. Adopted
    // from the holder-counting pattern the disabled-render lane is using.
    assert_eq!(
        Arc::strong_count(&shown),
        1,
        "with egui's delta and the render cache released, {} strong \
         reference(s) remain instead of this test's one -- something in the \
         app is still holding the raster",
        Arc::strong_count(&shown),
    );

    drop(shown); // this test's own

    assert!(
        pixels.upgrade().is_none(),
        "the pane is still holding the {RELEASED_BYTES} B it was shown. At the \
         shipped raster side that is 216,796,176 B a pane, held against a \
         graphics loss that may never come, and with the render cache emptied \
         above the pane is the only holder there is",
    );
    assert!(
        app.render.pane_render[0].uploaded_from.is_some(),
        "the pane forgot which buffer its texture came from. That identity is \
         what stops a tilt click on a tilt-independent product re-uploading a \
         raster already on the GPU; dropping it trades resident bytes for an \
         upload of the same size",
    );
}

/// **A resume the render cache cannot answer puts no picture back**, and does
/// not pretend to.
///
/// The control for `a_resume_puts_four_panes_back_with_one_upload`: with the
/// cache emptied there is nowhere left for the pixels to come from, so the
/// pane waits for a render rather than being served out of a copy of its own.
/// Without this, that test passes against a build where the pane still holds
/// the raster and the cache lookup is decoration.
#[test]
fn a_resume_the_render_cache_cannot_answer_puts_no_picture_back() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity);
    let served = raster(4);
    let served_bytes = served.pixels.len() * std::mem::size_of::<egui::Color32>();
    deliver(
        &mut app,
        &ctx,
        0,
        RadarProduct::Reflectivity,
        Arc::clone(&served),
    );
    assert!(
        holds_texture(&mut app, 0),
        "precondition: the pane must be showing a picture before it loses one",
    );
    // Named in bytes, not in "is_some": what makes the emptied cache below
    // meaningful is that it was holding a raster of a known size first.
    assert_eq!(
        app.render
            .get_cached_render(
                SITE,
                RadarProduct::Reflectivity,
                squallar_radar::types::RenderView::PlanView,
                TILT,
            )
            .map(|e| e.image.pixels.len() * std::mem::size_of::<egui::Color32>()),
        Some(served_bytes),
        "precondition: the render cache must be holding {served_bytes} B of \
         pixels, or emptying it below empties nothing",
    );

    app.gui.clear_graphics_state();
    app.render.clear_last_rendered();
    let (evicted, _) = app.render.clear_render_cache();
    drop(evicted);
    let _ = drain_uploads(&ctx);

    app.restore_cached_render(&ctx);
    app.dispatch_pane_renders(&ctx);

    assert!(
        drain_uploads(&ctx).is_empty(),
        "a picture came back from somewhere with the render cache empty — the \
         pane is holding pixels of its own again",
    );
    assert!(
        !holds_texture(&mut app, 0),
        "the pane is drawing a radar texture the render cache could not have \
         given it",
    );
}

/// Whether pane `pane_idx` is drawing a radar texture at all.
fn holds_texture(app: &mut crate::app::App, pane_idx: usize) -> bool {
    app.gui
        .pane_mut(pane_idx)
        .expect("pane exists")
        .overlay_cache_mut(&known::RADAR)
        .current()
        .is_some()
}
