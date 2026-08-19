//! One sweep, one texture, however many panes are looking at it.
//!
//! Every assertion here is a count of uploads read off egui's own texture delta
//! (`app::tests::drain_uploads`) rather than a timing, because the cost being
//! removed is a `queue.write_texture` of the whole raster and the delta is
//! exactly what produces one. A test that only checked that each pane *has* a
//! texture passed before this change and after it.

use super::*;
use crate::app::tests::{drain_uploads, n_pane_app};
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::types::RadarProduct;

const SITE: &str = "KTLX";
const OTHER_SITE: &str = "KMPX";
const TILT: f32 = 0.5;

/// The raster size these tests use — deliberately not `IMAGE_SIZE`.
///
/// Nothing here is timing anything, and a 2048² blank costs 16 MiB per fixture
/// and a visible fraction of a second to build. What is being counted is
/// uploads, and an upload of a small picture is one upload.
const SIDE: usize = 4;

/// Pixels whose bytes depend on `seed`, so a pane handed the wrong raster is a
/// failed comparison rather than a coincidence of two blanks.
///
/// The alphas walk the three arms of `Color32::from_rgba_unmultiplied` for the
/// reason `overlay_upload_tests` gives: `0` and `255` are early returns and
/// `palette.rs`'s `TRANSPARENCY = 180` is the one nearly every data pixel takes.
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

/// Aim a pane at `site` showing `product` at [`TILT`], far enough along that
/// the broadcast will accept it and `apply_render_to_pane` will not bail out.
fn point_at(app: &mut crate::app::App, pane_idx: usize, site: &str, product: RadarProduct) {
    point_at_tilts(app, pane_idx, site, product, &[TILT]);
}

/// As [`point_at`], but with a volume offering more than one tilt — which is
/// what makes a tilt change reachable at all, since
/// `PaneState::get_rendering_params` snaps the selection onto a tilt the scan
/// actually carries.
fn point_at_tilts(
    app: &mut crate::app::App,
    pane_idx: usize,
    site: &str,
    product: RadarProduct,
    tilts: &[f32],
) {
    let radar = rustdar_radar::sites::get_radar_site(site)
        .unwrap_or_else(|| panic!("{site} is a real radar"))
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, tilts.to_vec());
    let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
    pane.site = site.to_string();
    pane.selected_product = product;
    pane.selected_elevation = tilts[0];
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx,
            info: rustdar_radar::types::ScanInfo {
                site: radar,
                site_source: rustdar_radar::site_position::SitePositionSource::Table,
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
///
/// The drain is what scopes `PlanViewUploads`, so a test that posts twice and
/// polls twice never puts two rasters in front of one memo — which is exactly
/// the arrangement a memo keyed on anything at all survives. Separating the
/// post from the poll is what lets a test choose.
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
                hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
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
        .overlay_cache_mut(&OverlayKind::Radar.id())
        .current()
        .expect("this pane was served a radar texture")
        .texture
        .id()
}

/// A split on one site is one upload, not one per pane.
///
/// Run at every pane count a user can reach on this path and over products
/// from both datasources, because the broadcast is keyed on site, product and
/// elevation and a fix that only held for Level II would be invisible against
/// the Level III panes beside it.
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
///
/// Sharing a handle is only sound if the handle is of *these* pixels, and the
/// delta is the one place that can be read back — a `TextureHandle` offers an
/// id and a size and never its contents.
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

/// Two panes on **different** sites get two textures, because they are two
/// pictures — and the memo has to hold both at once to be asked the question.
///
/// The control for the test above: an upload count of one is only evidence of
/// sharing if a case that must not share still costs two. A memo keyed on
/// something coarser than the buffer's own identity — the size, the product,
/// the pane count — passes every assertion above and paints one site's ground
/// on another site's pane here.
///
/// # Both responses are posted before the single drain, and that is the test
///
/// `PlanViewUploads` is created **per drain**. A version of this that posted
/// and polled twice — which is what `deliver` does — gave each raster its own
/// memo, so no memo ever held two entries and the key was never consulted at
/// all. Mutating `Arc::ptr_eq(seen, image)` to `seen.size == image.size` left
/// that version green: two same-sized rasters, two lookups that never had to
/// discriminate. It is green for the same reason a lock is never contended in
/// a single-threaded test.
///
/// One drain over two rasters is the only arrangement in which the key does
/// work, and it is also the arrangement production is in — the render channel
/// is drained once per frame and two sites finishing in the same frame is
/// ordinary, not contrived.
#[test]
fn two_sites_in_one_drain_do_not_share_a_texture() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(2, SITE);
    point_at(&mut app, 0, SITE, RadarProduct::Reflectivity);
    point_at(&mut app, 1, OTHER_SITE, RadarProduct::Reflectivity);
    let _ = drain_uploads(&ctx);

    // Same dimensions, different pixels: a key that compared shape rather than
    // identity would call these one raster.
    post(&mut app, 0, RadarProduct::Reflectivity, TILT, raster(1));
    post(&mut app, 1, RadarProduct::Reflectivity, TILT, raster(2));
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
    // And the pixels, not merely the handles: a shared handle is only wrong
    // because of what it paints.
    let on_pane = |idx: usize, seed: u8| {
        let want = raster(seed);
        let got = app
            .render
            .pane_render
            .get(idx)
            .and_then(|prs| prs.cached_render.as_ref())
            .expect("this pane was served")
            .image
            .clone();
        assert_eq!(got.pixels, want.pixels, "pane {idx} holds the wrong raster");
    };
    on_pane(0, 1);
    on_pane(1, 2);
}

/// A resume puts every pane back with one upload.
///
/// `apply_render_to_pane` stores `Arc::clone(&render.image)` into each served
/// pane's `cached_render`, so four panes on one site come out of a suspend
/// holding four handles on one buffer. Paying four uploads to put them back is
/// worst exactly here: on Android a resume is the frame with the least budget
/// there is, and `restore_cached_render` exists because it is the one that must
/// not drop.
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
            app.render.pane_render[pane_idx].cached_render.is_some(),
            "precondition: pane {pane_idx} must have a cached render to restore"
        );
    }

    // What a suspend, a display change or a surface loss does: every handle
    // released, every `cached_render` deliberately kept.
    app.gui.clear_graphics_state();
    app.render.clear_last_rendered();
    let _ = drain_uploads(&ctx);

    app.restore_cached_render(&ctx);

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

/// The four products whose plan view is the same picture at every tilt, read
/// off the predicate rather than restated — `render_cache_tests` is where that
/// list is pinned against the renderer's own dispatch, and a second copy here
/// would be a second thing to keep in step.
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
        .overlay_cache_mut(&OverlayKind::Radar.id())
        .current()
        .expect("this pane was served a radar texture")
        .radar_meta
        .as_ref()
        .expect("a radar texture describes itself")
        .elevation
}

/// Clicking to another tilt on a tilt-independent pane costs no upload — and
/// still moves the label.
///
/// `render_cache_key` collapses these four onto `NO_ELEVATION_SLOT`, so the
/// dispatch pass finds the render already there. But `needs_render` compares
/// the raw elevation and is still true, so `apply_render_to_pane` ran anyway
/// and put the whole raster back on the GPU to redraw a picture provably
/// already on it.
///
/// The last assertion is the half that makes the first safe:
/// `PaneState::stale_image_on_screen` reads `RadarTextureMeta::elevation` and
/// nothing else, so an upload skipped without the restamp would leave the pane
/// disowning its own correct picture for as long as it showed it.
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

        app.gui.pane_mut(0).expect("pane exists").selected_elevation = 3.4;
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

/// The control: a pane handed genuinely different pixels still uploads them,
/// and retires the texture it was showing — **once the new one is whole**.
///
/// Buffer identity is what the skip is keyed on, and this is the case that must
/// not be caught by it. A skip keyed on anything coarser — the product, the
/// pane, "this pane already has a texture" — passes the test above and freezes
/// the map here.
///
/// The upload and the swap are separate events now, and this pins both: the
/// pixels go to the GPU the moment the render lands, and the pane goes on
/// drawing the sweep it has until they have all arrived. The old texture is
/// retired by the swap and not before, which is the one thing that costs
/// anything — see the peak-residency note in `App::apply_render_to_pane`.
#[test]
fn a_pane_handed_a_different_raster_uploads_it() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at_tilts(&mut app, 0, SITE, RadarProduct::Reflectivity, &[TILT, 3.4]);

    deliver(&mut app, &ctx, 0, RadarProduct::Reflectivity, raster(1));
    let first = placed(&mut app, 0);
    let _ = drain_uploads(&ctx);

    let second_raster = raster(2);
    app.gui.pane_mut(0).expect("pane exists").selected_elevation = 3.4;
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
    // Uploaded, and *not yet shown*: the pane keeps a whole picture rather than
    // filling a new one in top-down over the frames its bands take.
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
    // And the one it stopped drawing is *gone*, not parked. Asserted against
    // egui's own texture manager rather than against any holding pen in `App`,
    // because being absent from there is the whole point: a meta entry lives
    // exactly as long as some `TextureHandle` does, so this fails whether a
    // replaced raster is kept deliberately or by accident — which is right,
    // since neither is wanted. An `old_textures` vector used to make this false
    // for a whole extra frame; see the note in `App::apply_render_to_pane` for
    // why nothing has to hold a replaced handle at all.
    assert!(
        ctx.tex_manager().read().meta(first).is_none(),
        "the replaced texture is still allocated after the pane stopped drawing \
         it, so two generations of the same overlay are resident at once"
    );
}
