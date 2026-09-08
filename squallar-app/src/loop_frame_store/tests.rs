//! The store's own contract: one picture per key, holders re-stated per pass,
//! the texture freed by the last handle and not before.

use super::*;
use crate::test_keys;
use squallar_radar::types::RadarProduct;
use std::sync::Arc;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;

fn ts(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 30)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

fn target(site: &str, product: RadarProduct, elevation: f32) -> RenderTarget {
    test_keys::key(site, &squallar_radar::fields::spec(product).id, elevation)
}

fn reflectivity(site: &str, elevation: f32) -> RenderTarget {
    target(site, RadarProduct::Reflectivity, elevation)
}

fn picture(ctx: &egui::Context) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::PlanView(squallar_egui::pane::RadarImageData {
        surface: squallar_egui::pane::RadarSurface::Raster(ctx.load_texture(
            "shared",
            image,
            egui::TextureOptions::NEAREST,
        )),
        lat: 35.33,
        lon: -97.27,
        max_range_km: 230.0,
        placed: squallar_radar::types::ImageBounds::from_radar_site(35.33, -97.27, 230.0).into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
    })
}

fn texture_id(image: &LoopFrameImage) -> egui::TextureId {
    image
        .plan_view()
        .expect("every picture these tests file is a plan view")
        .surface
        .raster()
        .expect("every picture these tests file is a raster")
        .id()
}

fn allocated(ctx: &egui::Context, id: egui::TextureId) -> bool {
    ctx.tex_manager().read().meta(id).is_some()
}

fn section_key(offset: f64) -> SectionLoopKey {
    SectionLoopKey::new(
        squallar_egui::pane::SectionLine::new(
            squallar_geo::GeoPoint {
                lat: 35.0 + offset,
                lon: -97.0,
            },
            squallar_geo::GeoPoint {
                lat: 35.5 + offset,
                lon: -97.0,
            },
        )
        .expect("two distinct points on Earth"),
        None,
        squallar_radar::srv::SrvFallback::default(),
    )
}

/// **Two holders, one texture; the last holder drops it.** The store's own
/// clone and both panes' clones are one retain-counted texture, and the GPU
/// copy outlives the store only as long as a pane still holds a handle.
#[test]
fn two_holders_share_one_texture_and_the_last_handle_frees_it() {
    let ctx = egui::Context::default();
    let mut store = LoopFrameStore::default();
    let key = LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0));

    assert!(store.insert(key.clone(), picture(&ctx), 0).is_none());
    let first = store.get(&key).cloned().expect("filed");
    assert!(store.hold(1, &key), "the second pane holds what was filed");
    let second = store.get(&key).cloned().expect("still filed");
    let id = texture_id(&first);
    assert_eq!(
        id,
        texture_id(&second),
        "two holders, two handles, one texture"
    );
    assert_eq!(store.holders(&key), 2);
    assert_eq!(
        store.shared(),
        1,
        "one picture is held by more than one pane"
    );
    assert!(allocated(&ctx, id));

    // Pane 0 scrubs away: it re-states nothing, pane 1 re-states the frame.
    store.begin_pass();
    assert!(store.hold(1, &key));
    let dropped = store.end_pass();
    assert!(dropped.is_empty(), "a frame one pane still names is kept");
    assert_eq!(store.holders(&key), 1);
    assert_eq!(store.shared(), 0, "held by one pane is not shared");

    // Nobody names it: the store lets go, and the texture lives exactly as
    // long as the panes' own handles do.
    store.begin_pass();
    let dropped = store.end_pass();
    assert_eq!(dropped.len(), 1, "the unheld frame is handed back");
    assert_eq!(store.len(), 0);
    assert!(store.get(&key).is_none());
    drop(dropped);
    assert!(
        allocated(&ctx, id),
        "the store letting go must not free a texture a pane still draws with"
    );
    drop(first);
    assert!(allocated(&ctx, id), "one pane still holds it");
    drop(second);
    assert!(
        !allocated(&ctx, id),
        "the last handle dropped and the texture is still allocated: the \
         retain count this store rests on is not what it was measured to be"
    );
}

/// The key is the picture's identity: site, product, the tilt by its tenths
/// bucket, the instant — and nothing about which pane asked.
#[test]
fn a_key_matches_by_site_product_tilt_bucket_and_instant() {
    let key = LoopFrameKey::plan_view(reflectivity(SITE, 0.5), ts(1));
    let same = |k: LoopFrameKey| key.matches(&k);
    assert!(same(LoopFrameKey::plan_view(
        reflectivity(SITE, 0.5),
        ts(1)
    )));
    assert!(
        same(LoopFrameKey::plan_view(reflectivity(SITE, 0.54), ts(1))),
        "0.54 rounds to the 0.5 bucket the render's identity is built on"
    );
    assert!(!same(LoopFrameKey::plan_view(
        reflectivity(SITE, 0.6),
        ts(1)
    )));
    assert!(!same(LoopFrameKey::plan_view(
        reflectivity("KOUN", 0.5),
        ts(1)
    )));
    assert!(!same(LoopFrameKey::plan_view(
        target(SITE, RadarProduct::Velocity, 0.5),
        ts(1)
    )));
    assert!(!same(LoopFrameKey::plan_view(
        reflectivity(SITE, 0.5),
        ts(2)
    )));
}

/// A product whose plan view is the same picture at every tilt — the
/// tilt-independent composites — files one picture whatever tilt either pane
/// selected.
#[test]
fn a_tilt_independent_product_files_one_picture_for_every_tilt() {
    let product = RadarProduct::all()
        .iter()
        .copied()
        .find(|p| p.tilt_independent_plan_view())
        .expect("this build registers at least one tilt-independent plan view");
    let low = LoopFrameKey::plan_view(target(SITE, product, 0.5), ts(1));
    let high = LoopFrameKey::plan_view(target(SITE, product, 3.1), ts(1));
    assert!(
        low.matches(&high),
        "{product:?} at 0.5 and 3.1 is one picture"
    );
    assert!(
        !LoopFrameKey::plan_view(reflectivity(SITE, 0.5), ts(1))
            .matches(&LoopFrameKey::plan_view(reflectivity(SITE, 3.1), ts(1))),
        "control: a tilt-selecting product still keeps its tilts apart"
    );
}

/// A plan view and a section of one target at one instant are two pictures,
/// and two sections are one only on one line.
#[test]
fn a_section_is_its_own_picture_and_carries_its_line() {
    let plan = LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(1));
    let cut = LoopFrameKey::section(reflectivity(SITE, TILT), section_key(0.0), ts(1));
    assert!(!plan.matches(&cut));
    assert!(!cut.matches(&plan));
    assert!(cut.matches(&LoopFrameKey::section(
        reflectivity(SITE, TILT),
        section_key(0.0),
        ts(1)
    )));
    assert!(!cut.matches(&LoopFrameKey::section(
        reflectivity(SITE, TILT),
        section_key(0.2),
        ts(1)
    )));
    assert!(
        cut.matches(&LoopFrameKey::section(
            reflectivity(SITE, 2.4),
            section_key(0.0),
            ts(1)
        )),
        "a cut does not select by tilt, so the tilt is not in its identity"
    );
}

/// Re-filing a key replaces the picture and hands the old one back, so a
/// stale cut cannot linger under a key a fresh one was filed to.
#[test]
fn a_re_filed_key_replaces_and_hands_back_the_old_picture() {
    let ctx = egui::Context::default();
    let mut store = LoopFrameStore::default();
    let key = LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0));
    let old = picture(&ctx);
    let old_id = texture_id(&old);
    assert!(store.insert(key.clone(), old, 0).is_none());
    store.hold(1, &key);

    let new = picture(&ctx);
    let new_id = texture_id(&new);
    let replaced = store
        .insert(key.clone(), new, 0)
        .expect("the old picture comes back");
    assert_eq!(texture_id(&replaced), old_id);
    assert_eq!(store.len(), 1);
    assert_eq!(texture_id(store.get(&key).unwrap()), new_id);
    assert_eq!(
        store.holders(&key),
        1,
        "a replacement starts with the pane that filed it; every other holder \
         re-states itself on the next pass or takes the new picture at dispatch"
    );
}

/// Holding a key nobody has filed is the everyday case — a render set names
/// frames still being rendered — and files nothing.
#[test]
fn holding_an_unfiled_key_is_not_an_error_and_files_nothing() {
    let mut store = LoopFrameStore::default();
    let key = LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0));
    assert!(!store.hold(0, &key));
    assert_eq!(store.len(), 0);
    store.hold_frames(
        0,
        &reflectivity(SITE, TILT),
        RenderView::PlanView,
        None,
        [(ts(0), None), (ts(1), None)],
    );
    assert_eq!(store.len(), 0);
}

/// The pass spelling holds every stamp of one target at once, and only the
/// filed ones count.
#[test]
fn hold_frames_holds_every_filed_stamp_under_one_target() {
    let ctx = egui::Context::default();
    let mut store = LoopFrameStore::default();
    let target = reflectivity(SITE, TILT);
    for minute in [0, 2] {
        store.insert(
            LoopFrameKey::plan_view(target.clone(), ts(minute)),
            picture(&ctx),
            0,
        );
    }
    store.begin_pass();
    store.hold_frames(
        1,
        &target,
        RenderView::PlanView,
        None,
        [(ts(0), None), (ts(1), None), (ts(2), None), (ts(3), None)],
    );
    assert_eq!(
        store.holders(&LoopFrameKey::plan_view(target.clone(), ts(0))),
        1
    );
    assert_eq!(
        store.holders(&LoopFrameKey::plan_view(target.clone(), ts(2))),
        1
    );
    assert!(store.end_pass().is_empty(), "both filed frames were named");
    assert_eq!(store.len(), 2);
}

/// A picture a pane holds that the store has never seen — a restored loop, a
/// fixture — is filed on the pass, so the next pane on the identity takes it.
#[test]
fn hold_frames_files_a_picture_a_pane_already_holds() {
    let ctx = egui::Context::default();
    let mut store = LoopFrameStore::default();
    let target = reflectivity(SITE, TILT);
    let held = picture(&ctx);
    let id = texture_id(&held);

    store.begin_pass();
    store.hold_frames(
        0,
        &target,
        RenderView::PlanView,
        None,
        [(ts(0), Some(&held)), (ts(1), None)],
    );
    assert!(store.end_pass().is_empty());
    let key = LoopFrameKey::plan_view(target.clone(), ts(0));
    assert_eq!(
        store.get(&key).map(texture_id),
        Some(id),
        "the held picture was filed under the pane's target and the frame's stamp",
    );
    assert_eq!(store.holders(&key), 1, "filed under the pane that holds it");
    assert!(
        store.get(&LoopFrameKey::plan_view(target, ts(1))).is_none(),
        "a stamp with no picture files nothing",
    );
}

/// An overlay loop frame with `hit_map: None` — the shape the one
/// construction site builds — holds no host bytes. A `TextureHandle` is a GPU
/// id and a retain count, and every other field is a scalar.
#[test]
fn an_overlay_frame_with_no_hit_map_holds_nothing() {
    let ctx = egui::Context::default();
    let mut store = LoopFrameStore::default();
    store.insert(
        LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0)),
        overlay_frame(&ctx, None),
        0,
    );
    assert_eq!(store.resident_host_bytes(), 0);
}

/// **The case where the zero must stop being believable.**
///
/// `resident_host_bytes` prices an overlay frame at nothing because its one
/// host term, `hit_map`, is `None` at the only place such a frame is built.
/// That is a literal, not a type — so the arm asserts, and this is the proof
/// the assert fires rather than the figure quietly staying at zero.
///
/// It does not pin a byte figure: `HitMap` has no public size to assert
/// against, which is the missing one-liner the arm's comment names. What is
/// pinned is that the store refuses to answer zero for a frame it cannot
/// price.
/// `debug_assertions` only: the arm is a `debug_assert!`, so a release test
/// run has nothing to catch and this would fail for the wrong reason.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "an overlay loop frame carried a hit map")]
fn an_overlay_frame_carrying_a_hit_map_is_refused() {
    use squallar_overlays::render::overlay_state::HitItems;
    use squallar_overlays::render::rasterize::{HitCells, HitMap};

    let ctx = egui::Context::default();
    let hit_map = Arc::new(HitMap::from_cells(
        HitCells::new(4, 4),
        &HitItems::Rows(Vec::new()),
    ));
    let mut store = LoopFrameStore::default();
    store.insert(
        LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0)),
        overlay_frame(&ctx, Some(hit_map)),
        0,
    );
    let _ = store.resident_host_bytes();
}

/// An overlay frame pins no decoded volume — structural, since nothing in
/// `OverlayTextureData` can hold an `Arc<Scan>`.
#[test]
fn an_overlay_frame_pins_no_volume() {
    let ctx = egui::Context::default();
    let mut store = LoopFrameStore::default();
    store.insert(
        LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0)),
        overlay_frame(&ctx, None),
        0,
    );
    assert_eq!(store.pinned_volume_bytes(), 0);
}

fn overlay_frame(
    ctx: &egui::Context,
    hit_map: Option<Arc<squallar_overlays::render::rasterize::HitMap>>,
) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 0]);
    LoopFrameImage::Overlay(squallar_egui::overlay_cache::OverlayTextureData {
        texture: ctx.load_texture("overlay", image, egui::TextureOptions::NEAREST),
        placed: squallar_radar::types::ImageBounds::from_radar_site(35.33, -97.27, 230.0).into(),
        data_generation: 0,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map,
    })
}

/// One polar plan-view frame, holding a plane of `gates` gates.
fn fan_picture(gates: u32) -> (LoopFrameImage, usize) {
    let sweep = std::sync::Arc::new(squallar_egui::radar_fan::FanSweep {
        field: squallar_radar::fields::known::REFLECTIVITY,
        radials: 4,
        gates,
        codes: vec![0; (4 * gates) as usize],
        level_offsets: vec![0],
        lut_rgba: vec![0; squallar_egui::radar_fan::LUT_BYTES],
        edges: vec![[0.0, 90.0], [90.0, 180.0], [180.0, 270.0], [270.0, 360.0]],
        geometry: squallar_egui::radar_fan::FanGeometry {
            site_lat: 35.33,
            site_lon: -97.27,
            first_gate_slant_km: 2.125,
            gate_interval_slant_km: 0.25,
            elevation_deg: Some(0.5),
            reach_gates: gates,
            reach_km: 2.5,
            first_gate_km: 2.0,
            earth_radius_km: squallar_geo::EARTH_RADIUS_KM,
            effective_radius_km: squallar_radar::beam::RE_EFF_KM,
        },
    });
    assert!(sweep.is_well_formed(), "the fixture describes itself");
    let bytes = sweep.resident_bytes();
    let image = LoopFrameImage::PlanView(squallar_egui::pane::RadarImageData {
        surface: squallar_egui::pane::RadarSurface::Fan(std::sync::Arc::from(vec![sweep])),
        lat: 35.33,
        lon: -97.27,
        max_range_km: 230.0,
        placed: squallar_radar::types::ImageBounds::from_radar_site(35.33, -97.27, 230.0).into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
    });
    (image, bytes)
}

/// **A polar frame's plane is host bytes and this store counts them; a
/// raster's pixels are egui's and it does not.**
///
/// The term exists because the polar surface changed what a plan-view frame
/// holds. A raster frame's picture is a `TextureHandle` — a GPU id and a
/// retain count — so the store's old zero was right for it and stays right. A
/// fan frame's picture is the code plane itself, on this heap, for as long as
/// the frame lives: the renderer's residency is a `Weak` handle to that very
/// payload. Priced at zero it would be the largest thing a loop holds and
/// invisible to the census that gates this campaign.
///
/// Each arm is asserted against what IT holds — the raster against zero, each
/// fan against its own payload's `resident_bytes` — and never against a shared
/// bound, which at two different plane widths would leave the narrower one
/// unguarded up to the difference.
///
/// TAMPER: drop the surface term from the `PlanView` arm and both fan arms go
/// red while the raster arm stays green.
#[test]
fn a_polar_frames_plane_is_counted_on_the_host_and_a_rasters_pixels_are_not() {
    let ctx = egui::Context::default();

    // The raster arm, alone: the store's standing zero.
    let mut store = LoopFrameStore::default();
    store.insert(
        LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0)),
        picture(&ctx),
        0,
    );
    assert_eq!(
        store.resident_host_bytes(),
        0,
        "a raster frame's pixels are egui's and must not be counted here too"
    );

    // One fan: its own plane, to the byte.
    let (narrow, narrow_bytes) = fan_picture(2);
    let mut store = LoopFrameStore::default();
    store.insert(
        LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(0)),
        narrow,
        0,
    );
    assert_eq!(store.resident_host_bytes(), narrow_bytes as u64);

    // A second fan of a different width adds its own, so the figure is a sum
    // over the entries and not one entry's price times a count.
    let (wide, wide_bytes) = fan_picture(64);
    assert!(
        wide_bytes > narrow_bytes,
        "premise: the two fixtures differ in size ({wide_bytes} vs {narrow_bytes})"
    );
    store.insert(
        LoopFrameKey::plan_view(reflectivity(SITE, TILT), ts(1)),
        wide,
        0,
    );
    assert_eq!(
        store.resident_host_bytes(),
        (narrow_bytes + wide_bytes) as u64
    );
}
