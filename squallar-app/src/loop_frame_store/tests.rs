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
        texture: ctx.load_texture("shared", image, egui::TextureOptions::NEAREST),
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
        .texture
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
