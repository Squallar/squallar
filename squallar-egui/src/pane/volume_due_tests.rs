//! **Which volume a 3D pane is about, and whether it still needs building.**
//!
//! One question asked at two moments: by the draw loop's level-trigger while
//! the pane paints, and by the shell at the moment a volume installs
//! (WO-M14c). Both go through the three functions below, so the two paths
//! cannot name different volumes for one pane — which would build one grid
//! twice under two keys instead of the second asker attaching to the first.

use super::*;
use crate::radar_layer::CurrentVolumeStamp;
use squallar_geo::GeoPoint;
use squallar_radar::sites::RadarSite;

const SITE: &str = "KTLX";

/// The row the loop fixture below is keyed to, built here rather than read
/// out of the process-wide table.
fn site() -> RadarSite {
    RadarSite {
        name: SITE,
        network: squallar_radar::sites::RadarNetwork::of_id(SITE),
        lat: 35.33306,
        lon: -97.2775,
        heights: None,
    }
}

fn at(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
        .expect("a real date")
        .and_hms_opt(18, minute, 0)
        .expect("a real time")
}

/// The site's merge: newest data at `newest`, base volume started at `:00`.
fn current(newest: u32) -> CurrentVolumeStamp {
    CurrentVolumeStamp {
        newest: at(newest),
        base_started: Some(at(0)),
    }
}

/// A 3D pane on `SITE`, live, with no build behind it yet.
fn volume_pane() -> PaneState {
    let mut pane = PaneState::new();
    pane.set_site(SITE.to_owned());
    pane.set_view(RenderView::Volume);
    assert!(
        pane.volume().is_some(),
        "fixture precondition: the pane is in Volume mode, or every assertion \
         below passes by describing a plan-view pane",
    );
    pane
}

fn field() -> FieldId {
    radar_fields::known::REFLECTIVITY
}

/// **A live pane is about the newest data in the merge.** That stamp advances
/// with every sealed sweep, which is what makes the 3D view rebuild in step
/// with the map beside it rather than freezing on the volume it opened with.
#[test]
fn a_live_pane_is_about_the_newest_data_in_the_merge() {
    let pane = volume_pane();
    assert!(
        pane.viewing_live,
        "fixture precondition: a fresh pane is on the live feed",
    );
    let (stamp, base_started) = pane
        .volume_stamp(Some(current(6)))
        .expect("a site with a merged volume gives a stamp");
    assert_eq!(stamp.site, SITE);
    assert_eq!(stamp.collected, at(6));
    assert_eq!(
        base_started,
        Some(at(0)),
        "the caption's base-volume start must come from the merge, not from \
         the newest sweep",
    );
}

/// **A navigated pane is about the scan it stepped back to**, and a live
/// arrival is not its business. This is what stops the arrival path dragging
/// a pane that is looking at 18:00 forward onto the volume that just landed.
#[test]
fn a_navigated_pane_is_about_the_scan_it_stepped_back_to() {
    let mut pane = volume_pane();
    pane.viewing_live = false;
    pane.scan_info = Some(ScanInfo {
        site: site(),
        site_source: squallar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp: at(3),
        vcp_number: 212,
        available_products: Vec::new(),
        product_elevations: HashMap::new(),
        status: String::new(),
    });
    let merge = current(6);
    assert_ne!(
        at(3),
        merge.newest,
        "fixture precondition: the navigated scan and the merge's newest must \
         differ, or this test cannot tell which one was chosen",
    );

    let (stamp, base_started) = pane
        .volume_stamp(Some(merge))
        .expect("a navigated pane still names a volume");
    assert_eq!(
        stamp.collected,
        at(3),
        "the pane was dragged forward onto a volume it is not looking at",
    );
    assert_eq!(
        base_started,
        Some(at(3)),
        "a navigated pane's caption is about the scan it navigated to",
    );
}

/// **A site with no merged volume yet is about nothing**, and the pane says
/// the first download is in flight rather than drawing an empty box.
#[test]
fn a_site_with_no_volume_yet_names_no_stamp() {
    assert!(volume_pane().volume_stamp(None).is_none());
}

/// **The target carries the pane's own region**, so a pane aimed at a dragged
/// box and a pane on the default box name two different grids — and the same
/// pane names one grid from both paths.
#[test]
fn the_target_carries_the_panes_own_region() {
    let mut pane = volume_pane();
    let (stamp, _) = pane.volume_stamp(Some(current(6))).expect("a stamp");
    assert_eq!(
        pane.volume_target_for(&field(), stamp.clone()).region,
        None,
        "a pane nobody aimed must ask for the default box",
    );

    let region = VolumeRegion::new(
        GeoPoint {
            lat: 35.33,
            lon: -97.28,
        },
        squallar_radar::voxel::HalfExtentKm {
            east_km: 30.0,
            north_km: 20.0,
        },
    )
    .expect("a fixture region must be on Earth with a finite extent");
    pane.volume_mut().expect("a 3D pane").region = Some(region);
    let target = pane.volume_target_for(&field(), stamp);
    assert_eq!(
        target.region,
        Some(region),
        "the aimed box was dropped, so the grid built would cover a different \
         patch of ground than the one on screen",
    );
    assert_eq!(target.product, field());
    assert_eq!(target.volume.collected, at(6));
}

/// **A pane already rendered for the target is not due** — this is the
/// off-switch that makes the level-trigger quiesce once an eager build has
/// landed, rather than re-asking every frame for ever.
#[test]
fn a_pane_already_rendered_for_the_target_is_not_due() {
    let mut pane = volume_pane();
    let (stamp, _) = pane.volume_stamp(Some(current(6))).expect("a stamp");
    let target = pane.volume_target_for(&field(), stamp);
    assert!(
        pane.volume_build_due(&target),
        "a pane with no build behind it must be due, or the assertion below \
         cannot fail",
    );

    pane.volume_mut().expect("a 3D pane").rendered_for = Some(target.clone());
    assert!(!pane.volume_build_due(&target));

    let (later, _) = pane.volume_stamp(Some(current(11))).expect("a stamp");
    assert!(
        pane.volume_build_due(&pane.volume_target_for(&field(), later)),
        "the next volume must reopen the ask; a pane that stops asking after \
         one build never updates again",
    );
}

/// **A pane playing a 3D loop is not due a live build.** Its playhead's own
/// frame grid is what is on screen, so the live volume would be a grid
/// nothing puts up — and the loop would be interrupted to build it.
#[test]
fn a_pane_playing_a_3d_loop_is_not_due_a_live_build() {
    let mut pane = volume_pane();
    let (stamp, _) = pane.volume_stamp(Some(current(6))).expect("a stamp");
    let target = pane.volume_target_for(&field(), stamp);
    assert!(
        pane.volume_build_due(&target),
        "precondition: without a loop the same pane is due",
    );

    let mut loop_state = crate::radar_layer::begin_loop(3600, &site(), RenderView::Volume);
    loop_state.frames = vec![LoopFrame {
        timestamp: at(2),
        image: Some(LoopFrameImage::Volume(VolumeFrameGrid {
            id: 7,
            target: target.clone(),
        })),
        render_in_flight: false,
        render_failed: false,
    }];
    loop_state.current_frame = 0;
    *pane.time_state_mut(&known::RADAR) = loop_state;
    assert!(
        pane.active_volume_frame().is_some(),
        "fixture precondition: the playhead really is on a resident grid",
    );

    assert!(
        !pane.volume_build_due(&target),
        "a playing 3D loop was interrupted to build the live volume, which \
         nothing would have drawn",
    );
}

/// **A pane not in Volume mode is never due.** The boundary WO-M14c is drawn
/// at: the arrival path moves the same work earlier and never invents work
/// for a mode a pane merely might switch to.
#[test]
fn a_pane_not_in_volume_mode_is_never_due() {
    let volume = volume_pane();
    let (stamp, _) = volume.volume_stamp(Some(current(6))).expect("a stamp");
    let target = volume.volume_target_for(&field(), stamp);
    assert!(
        volume.volume_build_due(&target),
        "precondition: the same target on a 3D pane is due",
    );

    let mut plan = PaneState::new();
    plan.set_site(SITE.to_owned());
    plan.set_view(RenderView::PlanView);
    assert!(
        !plan.volume_build_due(&target),
        "a plan-view pane was owed a 3D build it would never draw",
    );
}
