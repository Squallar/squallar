use super::*;
use chrono::NaiveDate;
use rustdar_egui::pane::VolumeStamp;
use rustdar_radar::types::RadarProduct;

fn target(product: RadarProduct, minute: u32) -> VolumeTarget {
    VolumeTarget {
        region: None,
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: NaiveDate::from_ymd_opt(2024, 5, 6)
                .unwrap()
                .and_hms_opt(22, minute, 0)
                .unwrap(),
        },
        product,
    }
}

/// The payload the painter hands `rustdar-egui` is one `egui_wgpu` can
/// actually draw.
#[test]
fn the_payload_the_painter_hands_over_is_one_egui_wgpu_can_draw() {
    struct Nothing;
    impl egui_wgpu::CallbackTrait for Nothing {
        fn paint(
            &self,
            _info: egui::PaintCallbackInfo,
            _render_pass: &mut wgpu::RenderPass<'static>,
            _callback_resources: &egui_wgpu::CallbackResources,
        ) {
        }
    }

    let payload = paint_payload(Nothing);
    assert!(
        payload.downcast_ref::<egui_wgpu::Callback>().is_some(),
        "egui_wgpu downcasts the payload to its own `Callback`; anything else is one \
             log line and a silent `continue`, which looks exactly like a pane with no data",
    );
}

/// Open and resolve a build the way production does: dispatch, then the
/// worker's reply. `Refused` because a `VoxelGrid` has no constructor
/// outside `build_voxels`; the store treats every resolved entry alike.
fn build(store: &VolumeStore, pane: usize, t: &VolumeTarget, note: &str) {
    assert!(
        !store.share(pane, t),
        "precondition: nothing in hand for this target, a build follows"
    );
    store.begin_build(pane, t);
    assert!(
        store.complete(t, VolumeEntry::Refused(note.to_owned())),
        "precondition: the entry this just opened takes the result"
    );
}

/// A 1024-byte palette with `band` fully transparent entries above the
/// no-data index — the alpha shape `fade_band` measures — and colour
/// channels that vary per entry so a channel-order mistake cannot pass.
fn fade_lut(band: usize) -> Vec<u8> {
    let mut lut = Vec::with_capacity(256 * 4);
    for i in 0..256usize {
        let alpha = if i <= band { 0 } else { 180 };
        lut.extend_from_slice(&[i as u8, 200u8.wrapping_sub(i as u8), 37, alpha]);
    }
    lut
}

/// A real, tiny grid, for the tests whose subject is what may *stand in*
/// on screen — only a `Ready` entry ever does, so a `Refused` stub cannot
/// exercise them. Built through `build_voxels` because that is the one
/// constructor a `VoxelGrid` has.
pub(crate) fn ready_grid() -> VolumeEntry {
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };
    let sweep = |number: u8, elevation: f32| {
        let radials = (0..8u16)
            .map(|i| {
                Radial::new(
                    1_760_000_000_000 + i64::from(i),
                    i + 1,
                    f32::from(i) * 45.0,
                    45.0,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(MomentData::from_fixed_point(
                        4,
                        2125,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![120, 140, 160, 180],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    };
    let cut = |angle: f64| {
        nexrad_model::data::ElevationCut::new(
            angle,
            nexrad_model::data::ChannelConfiguration::ConstantPhase,
            nexrad_model::data::WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    };
    let scan = Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5), sweep(2, 1.5)],
    );
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (35.33, -97.27),
        half_extent_km: Some(rustdar_radar::voxel::HalfExtentKm::square(40.0)),
        base_km_msl: 0.0,
        top_km_msl: 10.0,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::WASM_SHAPE,
        values_wanted: false,
    };
    let grid = rustdar_radar::voxel::build_voxels(&scan, &request, 35.33, -97.27)
        .expect("the fixture volume resamples");
    VolumeEntry::Ready(Arc::new(grid))
}

/// **The worker-path dedupe.** `PrepareVolume` is level-triggered — the
/// pane re-asks every frame — and with the build asynchronous there is no
/// result in hand to stop it for hundreds of milliseconds. The `Building`
/// entry is what answers: the same pane's next frame, and any second pane,
/// attach to it instead of dispatching again.
#[test]
fn a_build_in_flight_absorbs_every_further_ask_for_its_target() {
    let store = VolumeStore::new();
    let t = target(RadarProduct::Reflectivity, 0);

    assert!(!store.share(0, &t), "the first ask owns the dispatch");
    store.begin_build(0, &t);

    assert!(
        store.share(0, &t),
        "the same pane's next frame must attach, not dispatch a second build",
    );
    assert!(
        store.share(1, &t),
        "a second pane on the same target must attach, not dispatch",
    );
    assert_eq!(store.live_ids().len(), 1, "one target, one entry");

    assert!(
        store.complete(&t, VolumeEntry::Refused("stub".to_owned())),
        "the one build resolves for everyone",
    );
    assert!(
        !store.complete(&t, VolumeEntry::Refused("again".to_owned())),
        "a duplicate reply has nothing to resolve and is dropped",
    );
}

/// Refcounting is by target: two panes on one volume share one entry, and it
/// survives until the second lets go.
#[test]
fn two_panes_on_one_volume_share_one_build() {
    let store = VolumeStore::new();
    let t = target(RadarProduct::Reflectivity, 0);

    build(&store, 0, &t, "stub");
    assert!(
        store.share(1, &t),
        "a second pane on the same volume must not trigger a second build",
    );
    assert_eq!(store.live_ids().len(), 1, "one target, one entry");

    store.release(0);
    assert_eq!(
        store.live_ids().len(),
        1,
        "the entry must survive while the second pane still holds it",
    );
    store.release(1);
    assert!(
        store.live_ids().is_empty(),
        "the last pane letting go must drop the entry",
    );
}

/// A pane moving to a volume **another pane already built** lets go of the
/// one it was holding — `share` on a resolved entry is a switch, not a
/// swap-in-progress, so nothing old is kept.
#[test]
fn a_pane_joining_a_volume_someone_else_built_drops_what_it_held() {
    let store = VolumeStore::new();
    let held = target(RadarProduct::Reflectivity, 0);
    let shared = target(RadarProduct::Velocity, 6);

    build(&store, 0, &held, "held");
    build(&store, 1, &shared, "shared");
    assert_eq!(
        store.live_ids().len(),
        2,
        "precondition: two volumes in hand"
    );

    assert!(store.share(0, &shared), "the build is shared, not repeated");
    assert!(
        store.lookup(&held).is_none(),
        "the volume pane 0 was holding is nobody's now and must be gone",
    );
    assert_eq!(store.live_ids().len(), 1);
}

/// **The seamless swap's ledger.** While a rebuild of the same site,
/// moment and region is in flight, the old grid stays attached and
/// answers `lookup_for_pane`; the moment the build lands, the old grid is
/// gone. Two entries mid-swap, one after — never an accumulation.
#[test]
fn the_old_grid_stands_in_while_its_replacement_builds_and_then_leaves() {
    let store = VolumeStore::new();
    let first = target(RadarProduct::Reflectivity, 0);
    let second = target(RadarProduct::Reflectivity, 6);

    // A real grid, because only a `Ready` entry may stand in: an old
    // *refusal* painted under a new target's caption would be a stale
    // explanation of the wrong volume.
    assert!(!store.share(0, &first));
    store.begin_build(0, &first);
    assert!(store.complete(&first, ready_grid()));
    let old_id = store.lookup(&first).expect("resolved").id;

    assert!(!store.share(0, &second), "a new stamp needs a new build");
    store.begin_build(0, &second);
    assert_eq!(
        store.live_ids().len(),
        2,
        "mid-swap: the old grid and the building entry coexist",
    );
    let standing_in = store
        .lookup_for_pane(0, &second)
        .expect("the old grid answers while the new one builds");
    assert_eq!(
        standing_in.id, old_id,
        "what stands in must be the pane's previous grid, not the building entry",
    );

    assert!(store.complete(&second, VolumeEntry::Refused("new picture".to_owned())));
    assert_eq!(
        store.live_ids().len(),
        1,
        "the swap must retire the old grid the moment the new one lands",
    );
    assert!(store.lookup(&first).is_none(), "the old grid is gone");
    assert_eq!(
        store
            .lookup_for_pane(0, &second)
            .expect("the new entry answers")
            .id,
        store.lookup(&second).expect("stored").id,
    );
}

/// The stand-in is scoped: a pane re-aimed at another **radar or product**
/// must not paint its old grid under the new target's caption — the one lie
/// the swap must never tell: another site's storm under this pane's caption.
#[test]
fn an_out_of_scope_grid_never_stands_in() {
    let elsewhere = VolumeTarget {
        volume: VolumeStamp {
            site: "KFWS".to_owned(),
            ..target(RadarProduct::Reflectivity, 0).volume
        },
        ..target(RadarProduct::Reflectivity, 0)
    };
    for other in [target(RadarProduct::Velocity, 0), elsewhere] {
        let store = VolumeStore::new();
        let refl = target(RadarProduct::Reflectivity, 0);
        assert!(!store.share(0, &refl), "the first ask owns the dispatch");
        store.begin_build(0, &refl);
        assert!(store.complete(&refl, ready_grid()));

        assert!(!store.share(0, &other));
        store.begin_build(0, &other);
        assert!(
            store.lookup_for_pane(0, &other).is_none(),
            "a KTLX reflectivity grid must not stand in for {} at {}",
            other.product.code(),
            other.volume.site,
        );
    }
}

/// The `ready_grid` fixture's own site, and the half-width it was resampled
/// over. Anything derived from these has to agree with the grid, or the crop
/// below is measuring itself.
const FIXTURE_SITE: (f64, f64) = (35.33, -97.27);
const FIXTURE_HALF_KM: f64 = 40.0;

/// A target over a box centred on the fixture's site, `half_width_km` either
/// side — a pane that has zoomed to exactly that much ground.
fn box_target(half_width_km: f64) -> VolumeTarget {
    VolumeTarget {
        region: Some(
            rustdar_egui::pane::VolumeRegion::new(
                rustdar_geo::GeoPoint {
                    lat: FIXTURE_SITE.0,
                    lon: FIXTURE_SITE.1,
                },
                rustdar_radar::voxel::HalfExtentKm::square(half_width_km),
            )
            .expect("a finite in-range half-width on a real centre is a region"),
        ),
        ..target(RadarProduct::Reflectivity, 0)
    }
}

/// **Zooming a 3D pane must not take its picture away.**
#[test]
fn a_zoom_keeps_the_grid_the_pane_is_already_painting() {
    let store = VolumeStore::new();
    let wide = box_target(FIXTURE_HALF_KM);
    let tight = box_target(FIXTURE_HALF_KM / 2.0);

    store.begin_build(0, &wide);
    assert!(store.complete(&wide, ready_grid()));
    let held = store.lookup(&wide).expect("resolved").id;

    assert!(!store.share(0, &tight), "a new box needs a new build");
    store.begin_build(0, &tight);

    let standing_in = store
        .lookup_for_pane(0, &tight)
        .expect("the pane's own grid must answer while the zoomed box builds");
    assert_eq!(
        standing_in.id, held,
        "the picture a zoom leaves on screen must be the grid the pane was \
         already painting",
    );
    assert!(
        matches!(standing_in.entry, VolumeEntry::Ready(_)),
        "the in-flight placeholder is not a picture; answering with it would \
         blank the pane exactly as before while looking like a stand-in",
    );

    assert!(store.complete(&tight, ready_grid()));
    assert_eq!(
        store.live_ids().len(),
        1,
        "the zoomed grid retires the one it stood in for; two boxes may \
         coexist through a rebuild and must never accumulate",
    );
}

/// The crop's algebra, at both zoom directions and at rest.
#[test]
fn the_drawn_box_is_the_one_asked_for_and_the_crop_finds_it_in_the_grid() {
    let VolumeEntry::Ready(grid) = ready_grid() else {
        panic!("the fixture is a resolved grid");
    };
    assert_eq!(
        (grid.x_range_km(), grid.y_range_km()),
        (
            (-FIXTURE_HALF_KM, FIXTURE_HALF_KM),
            (-FIXTURE_HALF_KM, FIXTURE_HALF_KM)
        ),
        "precondition: the fixture is centred on its own site, so the boxes \
         below are symmetric and an inverted offset cannot cancel out",
    );

    // At rest the pane asks for the box it already has, and the affine is the
    // identity — bit-exactly, because both boxes came out of one function.
    let settled = DrawnBox::for_target(&box_target(FIXTURE_HALF_KM), &grid)
        .expect("a picked region always places");
    assert_eq!(settled, DrawnBox::settled(&grid));
    assert_eq!(
        (settled.scale, settled.offset, settled.bounded),
        ([1.0; 3], [0.0; 3], false),
    );

    // Zoomed in: half the ground, so the middle half of the grid, magnified.
    // Nothing can fall outside it, so no bounds test is asked for.
    let inner = DrawnBox::for_target(&box_target(FIXTURE_HALF_KM / 2.0), &grid)
        .expect("a picked region always places");
    assert_eq!((inner.x_km, inner.y_km), ((-20.0, 20.0), (-20.0, 20.0)));
    assert_eq!(inner.scale, [0.5, 0.5, 1.0]);
    assert_eq!(inner.offset, [0.25, 0.25, 0.0]);
    assert!(!inner.bounded);

    // Zoomed out: twice the ground, so the grid fills the middle quarter and
    // the rest must read as air rather than as the sampler's clamped rim.
    let outer = DrawnBox::for_target(&box_target(FIXTURE_HALF_KM * 2.0), &grid)
        .expect("a picked region always places");
    assert_eq!((outer.x_km, outer.y_km), ((-80.0, 80.0), (-80.0, 80.0)));
    assert_eq!(outer.scale, [2.0, 2.0, 1.0]);
    assert_eq!(outer.offset, [-0.5, -0.5, 0.0]);
    assert!(
        outer.bounded,
        "a box reaching past the grid must ask for the bounds test, or the \
         edge texels smear across ground the radar never reported",
    );

    // The vertical is the grid's own on all three: a region cannot re-cut the
    // column, so the stand-in must not introduce a vertical pop when the real
    // build lands.
    for drawn in [settled, inner, outer] {
        assert_eq!(drawn.z_km_msl, grid.z_range_km_msl());
        assert_eq!((drawn.scale[2], drawn.offset[2]), (1.0, 0.0));
    }
}

/// A pane that re-aims mid-build supersedes its own build: the orphaned
/// `Building` entry is gone, so the stale reply finds nothing and drops.
#[test]
fn a_superseded_builds_reply_is_dropped() {
    let store = VolumeStore::new();
    let first = target(RadarProduct::Reflectivity, 0);
    let second = target(RadarProduct::Reflectivity, 6);

    assert!(!store.share(0, &first));
    store.begin_build(0, &first);
    assert!(!store.share(0, &second));
    store.begin_build(0, &second);

    assert!(
        !store.complete(&first, VolumeEntry::Refused("stale".to_owned())),
        "the superseded build's reply must be dropped, not stored",
    );
    assert!(
        store.complete(&second, VolumeEntry::Refused("current".to_owned())),
        "the current build's reply must land",
    );
    assert_eq!(store.live_ids().len(), 1);
}

/// Ids are never reused, so a stale callback cannot address a new upload.
#[test]
fn a_released_id_is_never_handed_out_again() {
    let store = VolumeStore::new();
    let first = target(RadarProduct::Reflectivity, 0);
    build(&store, 0, &first, "a");
    let first_id = store.lookup(&first).expect("stored").id;
    store.release(0);

    let second = target(RadarProduct::Velocity, 0);
    build(&store, 0, &second, "b");
    assert_ne!(
        store.lookup(&second).expect("stored").id,
        first_id,
        "ids must not be reused",
    );
}

/// The floor's uniform lanes, both ways the mirror can be encoded.
#[test]
fn the_floor_lanes_normalise_points_against_the_mirror_and_carry_the_encoding() {
    let mirror = [1600.0, 1200.0];
    let source = FloorSource {
        // 400 points across a 1600-point-wide mirror: a quarter in.
        site_points: [400.0, 300.0],
        points_per_degree_lon: 80.0,
        // Negative, because Mercator y grows north and screen y grows down.
        points_per_mercator_y: -5000.0,
        site_lat: 41.7,
        west_km: -230.0,
        south_km: -230.0,
        mirror_size_points: mirror,
    };
    let (uv, geo) = floor_lanes(&source, mirror, true);

    // `point / mirror_points`: 400 / 1600 = 0.25 across, 300 / 1200 = 0.25
    // down; 80 / 1600 = 0.05 of the mirror per degree of longitude, and
    // -5000 / 1200 = -4.1667 per unit of Mercator y.
    for (lane, (got, want)) in [
        "u at the site",
        "v at the site",
        "u per degree of longitude",
        "v per unit of Mercator y",
    ]
    .into_iter()
    .zip(uv.into_iter().zip([0.25, 0.25, 0.05, -4.166_667]))
    {
        assert!((got - want).abs() < 1e-5, "{lane}: got {got}, want {want}");
    }
    assert_eq!(geo, [41.7, -230.0, -230.0, 1.0], "geo lanes, gamma-encoded");

    // A mirror grown to hold a floor strip moves every v lane and no u lane.
    let (with_strip, _) = floor_lanes(&source, [1600.0, 2400.0], true);
    assert_eq!(
        [with_strip[0], with_strip[2]],
        [uv[0], uv[2]],
        "growing the mirror downwards must not move anything across",
    );
    for (lane, (tall, square)) in ["v at the site", "v per unit of Mercator y"]
        .into_iter()
        .zip([(with_strip[1], uv[1]), (with_strip[3], uv[3])])
    {
        assert!(
            (tall - square / 2.0).abs() < 1e-5,
            "{lane}: a mirror twice as tall must halve it, got {tall} against {square}",
        );
    }

    let (_, linear) = floor_lanes(&source, mirror, false);
    assert_eq!(linear[3], 0.0, "an sRGB swapchain leaves the mirror linear");
}

/// Every samplable moment clears the solid-block bar, and the counts here are
/// `rustdar_radar::voxel`'s own measurements — the deliberate flip of the
/// original `only_reflectivity_clears_the_fade_bar`, whose doc said a widened
/// set "is a decision someone should make on purpose rather than discover".
#[test]
fn every_samplable_moments_default_table_clears_the_gate() {
    let measured = [
        ("Reflectivity", 64u16),
        ("Velocity", 41),
        ("Spectrum Width", 18),
        ("Differential Reflectivity", 53),
        ("Differential Phase", 255),
        ("Correlation Coefficient", 35),
    ];
    let refused: Vec<&str> = measured
        .iter()
        .filter(|(moment, see_through)| palette_refusal_for(*see_through, moment).is_some())
        .map(|(moment, _)| *moment)
        .collect();
    assert_eq!(
        refused,
        Vec::<&str>::new(),
        "a samplable moment stopped clearing the solid-block bar",
    );
    // The bar still has teeth: a wall-to-wall opaque table is refused.
    assert!(
        palette_refusal_for(0, "Anything").is_some(),
        "an all-opaque table must still be refused",
    );
    // And a bar's-edge clearance is called out: spectrum width is the one
    // narrow profile (its clear band is honestly small — laminar flow is a thin
    // slice of its scale); everything else clears by 2x or more, and a profile
    // change eroding that should be renegotiated here.
    for (moment, see_through) in measured {
        assert!(
            see_through >= 2 * u16::from(MINIMUM_FADE_INDICES) || moment == "Spectrum Width",
            "{moment} clears the bar by less than 2x: {see_through}",
        );
    }
    // The production wiring reads the see-through measure, not the bottom run:
    // velocity's fade_band is honestly 0 (its ramp bottom is the strongest
    // inbound air), so a gate on the bottom run would refuse it in production
    // with every literal above still green.
    assert!(
        include_str!("../volume_bridge.rs")
            .contains("palette_refusal_for(grid.see_through_indices(), grid.product().name())"),
        "palette_refusal no longer reads the see-through measure",
    );
}

/// A refusal names the moment and says what would have to change.
#[test]
fn a_refusal_names_the_moment_and_says_why() {
    let why = palette_refusal_for(0, "Velocity").expect("an opaque palette is refused");
    assert!(
        why.starts_with("Velocity"),
        "the moment must be named: {why}"
    );
    assert!(
        why.contains("opaque"),
        "the reason must name the property that caused it: {why}",
    );
    assert!(
        why.contains("solid block"),
        "the reason must say what the render would degenerate into: {why}",
    );
    assert!(
        why.contains("profile"),
        "the message must point at the thing that regressed: {why}",
    );
}

/// The two guards inside `paint` that no headless test can reach are still
/// in it, and the single-tilt one is still on the **count**.
#[test]
fn the_guards_paint_cannot_be_tested_through_are_still_in_it() {
    let source = include_str!("../volume_bridge.rs");
    let start = source
        .find("impl VolumePainter for BridgeVolumePainter {")
        .expect("the painter impl is no longer where this test looks for it");
    let body = &source[start..];
    let end = body
        .find("\n}\n")
        .expect("the painter impl has no closing brace");
    let body = &body[..end];

    assert!(
        body.contains("grid.tilt_count() == 1"),
        "`paint` no longer branches on the tilt count",
    );
    assert!(
        !body.contains("all(|&i|") && !body.contains("iter().all("),
        "`paint` looks like it tests the index plane for emptiness; \
             a single-tilt volume must be recognised by its tilt count, because \
             emptiness is measure-zero rather than an invariant",
    );
    assert!(
        body.contains("palette_refusal(&grid)"),
        "`paint` no longer consults the palette gate, so a moment whose colour \
             table is opaque at the bottom of its ramp would render as a solid block",
    );

    // The soft-edge mechanism's two production lines.
    assert!(
        body.contains(
            "empty_index_threshold_for(effective_fade_band(grid.fade_band(), frame.alpha.as_ref()))"
        ),
        "`paint` no longer anchors the skip threshold at the EFFECTIVE fade \
             boundary — the palette's band through `effective_fade_band`, or the \
             user's Volume Alpha curve's when one is applied. Anchoring on the \
             palette alone erases the bottom of a curve that paints into the \
             band and pays full sample cost through a curve that strips it; \
             anchoring on nothing reverts to skipping only index 0",
    );
    assert!(
        body.contains("uniform.edge_soft_width = EDGE_SOFT_WIDTH"),
        "`paint` no longer widens the opacity ramp, so every shelf and echo \
             top reverts to the hard one-LUT-step rim the soft edge dissolves",
    );
    // The cloud rung's two production lines, pinned for the same reason:
    // deleting either leaves every host test green (the uniform's raw defaults
    // are a renderable configuration) and the user gets the voxel-spiked
    // stippled render #5 was filed about.
    assert!(
        body.contains("uniform.reconstruction_lod = cloud_reconstruction_lod_for(largest_cell_km"),
        "`paint` no longer selects the cell-size-tapered smoothed \
             reconstruction on the cloud rung: a fixed LOD erases coarse-grid \
             cores (the Harvey table in `cloud_reconstruction_lod_for`), and \
             no LOD at all brings the single-voxel spikes and tilt-shelf \
             cliffs back",
    );
    assert!(
        body.contains("uniform.step_cells = CLOUD_STEP_CELLS"),
        "`paint` no longer halves the march step on the cloud rung, so the \
             jitter's per-step opacity residual returns as a visible stipple",
    );
    // And the isosurface's exemption from it, pinned here for the same reason:
    // no host test can reach `paint` with a `Ready` grid, and the failure is
    // invisible in every other suite — the line's absence passed 13/13
    // volume_gpu, 10/10 silhouette and 151/151 lib while deleting a lone
    // measured voxel from the 3D surface outright at the shipped region rung.
    let isosurface_arm = body
        .split_once("VolumeViewMode::Isosurface")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    assert!(
        isosurface_arm.contains("uniform.reconstruction_lod = 0.0"),
        "`paint` marches the isosurface at the cloud rung's smoothed \
             reconstruction. An isosurface is a level set of the field, so the \
             smoothing moves the surface rather than softening its rendering, \
             and `volume.wgsl`'s COVERAGE_FLOOR of 0.5 is a statement about \
             the RAW tent: above level 0 a lone measured voxel reconstructs to \
             coverage 0.125 and a one-cell sheet to 0.502, and the cut erases \
             them. Both shipped region rungs take the full LOD",
    );
    // The boundary-honesty override that used to sit here is GONE, and its
    // absence is asserted rather than merely un-tested.
    assert!(
        !body.contains("no_data_blends_at_ramp_bottom") && !body.contains("NEAREST"),
        "`paint` makes a per-product reconstruction decision again: the \
             coverage channel retired that split, and re-adding it sends \
             seven of the nine products back to a nearest march",
    );
    // The floor's flag-and-texture pairing: the flag must be exactly "a floor
    // is in hand and the pane asked", or the shader composites a ground nobody
    // bound (a transparent no-op that claims to draw) or draws one against the
    // pane's toggle.
    assert!(
        body.contains("uniform.map_floor = floor.is_some()"),
        "`paint` no longer ties the floor flag to the floor being in hand",
    );
    assert!(
        body.contains("frame.floor.then_some(frame.source).flatten()"),
        "`paint` no longer consults the pane's floor toggle before looking \
             a floor up, so the per-pane escape hatch is dead",
    );

    // The isosurface wiring, same untestable-through-`paint` class: the lanes
    // must be translated against the grid's own ramp, and the skip threshold
    // must drop to the index-0 default — the surface reads the data, so neither
    // the palette's band nor a Volume Alpha curve may move it.
    assert!(
        body.contains("grid.iso_uniform_params(frame.iso_threshold)"),
        "`paint` no longer translates the isosurface threshold against \
             the grid's ramp",
    );
    assert!(
        body.contains("uniform.empty_index_threshold = empty_index_threshold_for(0)"),
        "`paint` no longer pins the isosurface's skip threshold to the \
             no-data index — a Volume Alpha curve could then move where the \
             surface sits, which the UI promises it cannot",
    );
}

/// The cloud rung's two constants, by value.
#[test]
fn the_cloud_rung_marches_the_smoothed_field_at_half_cell_steps() {
    assert_eq!(CLOUD_RECONSTRUCTION_LOD, 1.0);
    assert_eq!(CLOUD_STEP_CELLS, crate::raymarch::RAYMARCH_STEP_CELLS / 2.0,);
    let ceiling = crate::raymarch::RAYMARCH_STEP_CEILING as f32;
    let desktop_diagonal_cells = (256.0f32 * 256.0 + 256.0 * 256.0 + 128.0 * 128.0).sqrt();
    assert!(
        desktop_diagonal_cells / CLOUD_STEP_CELLS <= ceiling,
        "the desktop grid's diagonal needs {} cloud steps against a \
             ceiling of {ceiling}; the far corner of every diagonal view \
             falls to stretched steps",
        desktop_diagonal_cells / CLOUD_STEP_CELLS,
    );
}

/// The ground reach a WSR-88D's surveillance cut has, in km.
///
/// [`rustdar_radar::voxel::volume_reach_km`]'s units, so the cosine of the
/// sweep's median elevation is already folded in — which is the whole of the
/// difference between this and the 460.125 km slant range the gate arithmetic
/// starts from. Across 150 archive volumes from 53 sites every WSR-88D reports
/// the same figure with no variance at all; the table in
/// [`rustdar_radar::voxel::box_half_width_km`] is where that survey lives.
const REFLECTIVITY_REACH_KM: f64 = 460.109;

/// The same, for the Doppler cut — velocity, spectrum width, ZDR, ΦDP and
/// ρHV, which is five of the six products a 3D pane can show.
const DOPPLER_REACH_KM: f64 = 300.114;

/// What a discrete desktop adapter reports for `max_texture_dimension_3d`.
const DISCRETE_DESKTOP_MAX_AXIS: u32 = 2048;

/// The uniform a desktop pane really hands the shader for the whole-volume box
/// of a product reaching `reach_km`.
fn whole_volume_uniform(reach_km: f64) -> VolumeUniform {
    let half_width_km = rustdar_radar::voxel::box_half_width_km(reach_km) as f32;
    let shape = rustdar_device_profile::constants::volume_grid_shape(DISCRETE_DESKTOP_MAX_AXIS);
    let height_km = (rustdar_radar::voxel::DEFAULT_TOP_KM_MSL
        - rustdar_radar::voxel::DEFAULT_BASE_KM_MSL) as f32;
    VolumeUniform::new(
        [half_width_km * 2.0, half_width_km * 2.0, height_km],
        [shape.nx as u32, shape.ny as u32, shape.nz as u32],
    )
}

/// The cloud smoothing is a function of cell size: full at the region
/// rungs, **zero at the reflectivity whole-volume box**, monotone between.
#[test]
fn the_cloud_smoothing_tapers_with_cell_size_and_spares_the_default_box() {
    // The desktop region rungs: 60 km and 160 km boxes over 256 cells.
    for cell_km in [60.0 / 256.0, 160.0 / 256.0] {
        assert_eq!(
            cloud_reconstruction_lod_for(cell_km),
            CLOUD_RECONSTRUCTION_LOD,
            "a {cell_km:.3} km/cell region box must get the full cloud \
                 smoothing — its cells outresolve the features",
        );
    }
    // Between the knees the taper is a real intermediate value, so a
    // future box between the rungs degrades rather than jumps.
    let mid = cloud_reconstruction_lod_for(1.2);
    assert!(
        mid > 0.0 && mid < CLOUD_RECONSTRUCTION_LOD,
        "the taper must pass through intermediate levels, got {mid}",
    );

    // Reflectivity's whole-volume box, through the same helper `paint` feeds
    // the taper. Both rules are called, so this reads 1.797 only while the box
    // rule and the shape rule between them still produce 1.797.
    let default_cell = largest_cell_km(&whole_volume_uniform(REFLECTIVITY_REACH_KM));
    assert!(
        (1.75..1.85).contains(&default_cell),
        "the reflectivity box's coarsest cell moved: {default_cell} km",
    );
    assert_eq!(
        cloud_reconstruction_lod_for(default_cell),
        0.0,
        "the reflectivity whole-volume box must march the raw field: at \
             {default_cell:.3} km cells the two-cell kernel is wider than the \
             cores it lands on, and the smoothing erases them (measured, Harvey)",
    );
    // How much room that has, stated rather than left implicit.
    let margin = (default_cell - CLOUD_SMOOTHING_RAW_CELL_KM) / CLOUD_SMOOTHING_RAW_CELL_KM;
    assert!(
        (0.0..0.05).contains(&margin),
        "the reflectivity box now clears the taper's {CLOUD_SMOOTHING_RAW_CELL_KM} km zero by \
         {:.1}%, not the ~2.7% every claim about the default view assumes. Under 0 the default \
         view has started smoothing (Harvey); far over it, this test has stopped being the \
         tripwire it is documented as.",
        100.0 * margin,
    );

    // The Doppler cut's box, which is the other five products.
    let doppler_cell = largest_cell_km(&whole_volume_uniform(DOPPLER_REACH_KM));
    assert!(
        doppler_cell < default_cell,
        "the Doppler box ({doppler_cell} km/cell) is sized on a shorter reach than the \
         reflectivity one ({default_cell} km/cell) and must be finer",
    );
    assert!(
        cloud_reconstruction_lod_for(doppler_cell) > 0.0,
        "velocity, spectrum width, ZDR, ΦDP and ρHV reach 300 km, so their whole-volume box is \
         {doppler_cell:.3} km/cell and the taper is live there. A change that took this to zero \
         would be a rendering change for five of six products, not a no-op.",
    );
}

/// The coarse mip level is allocated exactly where the taper would read it,
/// and nowhere else.
#[test]
fn the_coarse_level_is_built_only_where_something_will_sample_it() {
    use rustdar_device_profile::quality::{
        MOBILE_PLATFORM_CEILING, VolumeQuality, WASM_PLATFORM_CEILING,
    };

    // Live: a discrete desktop GPU, lit volume, region box.
    for cell_km in [60.0 / 256.0, 160.0 / 256.0] {
        assert_eq!(
            coarse_level_for(true, cell_km),
            CoarseLevel::Built,
            "at {cell_km:.3} km/cell the taper reads the coarse level, so it \
             has to exist",
        );
    }

    // Reflectivity's whole-volume box, on that same discrete GPU.
    let default_cell = largest_cell_km(&whole_volume_uniform(REFLECTIVITY_REACH_KM));
    assert_eq!(
        cloud_reconstruction_lod_for(default_cell),
        0.0,
        "the premise moved: see the taper test above",
    );
    assert_eq!(
        coarse_level_for(true, default_cell),
        CoarseLevel::Omitted,
        "the reflectivity whole-volume box marches the raw field, so its 4 MiB \
         coarse level is 4 MiB nothing samples",
    );

    // The Doppler cut's whole-volume box, same device, same lit mode.
    let doppler_cell = largest_cell_km(&whole_volume_uniform(DOPPLER_REACH_KM));
    assert!(
        cloud_reconstruction_lod_for(doppler_cell) > 0.0,
        "the premise moved: see the taper test above",
    );
    assert_eq!(
        coarse_level_for(true, doppler_cell),
        CoarseLevel::Built,
        "velocity, spectrum width, ZDR, ΦDP and ρHV reach 300 km, so their \
         whole-volume box is {doppler_cell:.3} km/cell and the smoothing reads \
         the coarse level there. Omitting it would leave the shader sampling a \
         level that was never allocated.",
    );

    // Every adapter on wasm32 and mobile, and every desktop adapter below
    // discrete: shading off, so `flags.y` is never raised and no cell size
    // can bring the level back.
    for ceiling in [WASM_PLATFORM_CEILING, MOBILE_PLATFORM_CEILING] {
        assert!(
            !VolumeQuality::BEST.capped_by(ceiling).shading.is_on(),
            "the platform ceiling {ceiling:?} now admits shading, so this \
             target can reach the coarse level and the row below is wrong",
        );
    }
    for cell_km in [60.0 / 256.0, 1.2, default_cell, doppler_cell] {
        assert_eq!(
            coarse_level_for(false, cell_km),
            CoarseLevel::Omitted,
            "with shading off the shader never reads a nonzero LOD, so no \
             box may allocate the level",
        );
    }
}

/// [`empty_index_threshold_for`] sits strictly between the last fully
/// transparent palette entry and the first visible one, for every band.
#[test]
fn the_skip_threshold_separates_the_last_transparent_entry_from_the_first_visible_one() {
    for band in 0..=u8::MAX {
        let threshold = empty_index_threshold_for(band);
        assert!(
            f32::from(band) / 255.0 <= threshold,
            "at band {band} the last transparent entry clears the skip \
                 threshold, so the march samples — and shades — cells whose \
                 fetch is guaranteed invisible",
        );
        if band < u8::MAX {
            assert!(
                f32::from(band) / 255.0 + 1.0 / 255.0 > threshold,
                "at band {band} the first visible entry is under the skip \
                     threshold, so the march erases the bottom of the ramp",
            );
        }
    }
    // And the anchor is the midpoint, not merely inside the gap: the
    // EDGE_SOFT_WIDTH ramp rises from it, so where it sits inside the gap
    // decides the opacity the first visible index fades in at (~1% at the
    // midpoint, ~9% one index lower).
    assert_eq!(empty_index_threshold_for(64), 64.5 / 255.0);
}

/// **The untouched Volume Alpha editor is bit-exact, by construction.**
#[test]
fn an_untouched_editor_uploads_the_grids_own_bytes() {
    let lut = fade_lut(64);
    let out = effective_lut(&lut, None);
    assert!(
        matches!(out, Cow::Borrowed(_)),
        "no curve must mean the grid's own bytes travel to the GPU, not a rewrite",
    );
    assert!(
        std::ptr::eq(out.as_ptr(), lut.as_ptr()),
        "the borrowed table must be the very slice the grid handed over",
    );
    assert_eq!(&*out, &lut[..], "and byte-identical, trivially");
}

/// A curve replaces the LUT's alpha channel and nothing else: colours are
/// the palette's at every entry, alpha is the curve's, and entry 0 stays
/// transparent whatever anyone claims.
#[test]
fn a_curve_replaces_only_the_alpha_channel() {
    use rustdar_egui::volume_alpha::{AlphaCurve, CURVE_LEN};

    let lut = fade_lut(64);
    let mut alphas = [0u8; CURVE_LEN];
    for (i, slot) in alphas.iter_mut().enumerate() {
        *slot = (255 - i) as u8; // a curve unlike any palette's
    }
    let curve = AlphaCurve::from_alphas(alphas);
    let out = effective_lut(&lut, Some(&curve));

    for (i, (got, want)) in out.chunks_exact(4).zip(lut.chunks_exact(4)).enumerate() {
        assert_eq!(
            got[..3],
            want[..3],
            "entry {i}: the colour channels must stay the palette's",
        );
        let expected = if i == 0 { 0 } else { curve.alphas()[i] };
        assert_eq!(got[3], expected, "entry {i}: the alpha must be the curve's");
    }
}

/// **The skip threshold follows the effective curve, exactly.**
#[test]
fn the_skip_threshold_follows_the_effective_curve() {
    use rustdar_egui::volume_alpha::{AlphaCurve, CURVE_LEN};

    let palette = fade_lut(64);
    let palette_band = 64u8;

    // The canonical gesture (strip the low end to 120), its inverse
    // (paint alpha into the palette's fade band), an untouched editor,
    // and the extremes: everything transparent, everything opaque.
    let curves: Vec<Option<AlphaCurve>> = vec![
        None,
        Some(AlphaCurve::from_alphas({
            let mut a = [0u8; CURVE_LEN];
            a[120..].fill(200);
            a
        })),
        Some(AlphaCurve::from_alphas({
            let mut a = [0u8; CURVE_LEN];
            a[1..].fill(30);
            a
        })),
        Some(AlphaCurve::from_alphas([0u8; CURVE_LEN])),
        Some(AlphaCurve::from_alphas([255u8; CURVE_LEN])),
    ];

    for curve in &curves {
        let band = effective_fade_band(palette_band, curve.as_ref());
        let threshold = empty_index_threshold_for(band);
        let uploaded = effective_lut(&palette, curve.as_ref());

        for (i, entry) in uploaded.chunks_exact(4).enumerate() {
            let index_value = i as f32 / 255.0;
            if index_value <= threshold {
                assert_eq!(
                    entry[3], 0,
                    "curve {curve:?}: entry {i} is under the skip threshold \
                         but visible in the uploaded table — the march would \
                         erase it",
                );
            }
        }
        if band < u8::MAX {
            let first_visible = usize::from(band) + 1;
            assert!(
                first_visible as f32 / 255.0 > threshold,
                "curve {curve:?}: the first entry past the band must clear \
                     the threshold, or the ramp's foot sits a shell too low",
            );
            assert_ne!(
                uploaded[first_visible * 4 + 3],
                0,
                "curve {curve:?}: the entry past the band must actually be \
                     visible in the uploaded table — the two halves of the seam \
                     have drifted apart",
            );
        } else {
            // The all-transparent curve: the threshold sits above every
            // representable index, so the march samples nothing — an
            // honestly empty pane, with no division anywhere on the path.
            assert!(
                threshold > 1.0,
                "an all-transparent curve must put the threshold above \
                     every index the grid can encode",
            );
        }
    }

    // And by value, the two directions the doc names: stripping to 120
    // raises the anchor to 119.5/255; painting index 1 drops it to 0.5/255.
    assert_eq!(
        effective_fade_band(palette_band, curves[1].as_ref()),
        119,
        "stripping the low end must raise the effective band",
    );
    assert_eq!(
        effective_fade_band(palette_band, curves[2].as_ref()),
        0,
        "painting into the palette's fade band must lower it",
    );
    assert_eq!(
        effective_fade_band(palette_band, None),
        palette_band,
        "no curve must mean the palette's own band, untouched",
    );
}

/// The curve is applied on the upload path and only through the staleness
/// comparison — the same source-scan arrangement as the painter's guards, and
/// for the same reason: the upload needs a `wgpu::Device`, so no host test can
/// reach it.
#[test]
fn the_upload_applies_the_curve_through_the_staleness_gate() {
    let source = include_str!("../volume_bridge.rs");
    let start = source
        .find("    pub fn ensure_upload(")
        .expect("the upload step is no longer where this test looks for it");
    let body = &source[start..];
    let end = body
        .find("\n    }\n")
        .expect("the upload step has no closing brace");
    let body = &body[..end];

    assert!(
        body.contains("if upload.applied_alpha.as_ref() != alpha {"),
        "the LUT rewrite is no longer gated on the curve changing — either \
             an edit stopped applying to an already-uploaded grid, or the 1 KiB \
             table is being rewritten every frame",
    );
    assert!(
        body.matches("effective_lut(palette, alpha)").count() >= 2,
        "both upload paths — first upload and in-place rewrite — must build \
             the table through `effective_lut`, or one of them ships the wrong \
             alpha",
    );

    // And the caller still hands it the grid's own table and this frame's
    // curve. Without this the scan above would stay green over an
    // `ensure_upload` nothing called with a curve at all.
    let start = source
        .find("impl egui_wgpu::CallbackTrait for VolumeCallback {")
        .expect("the callback impl is no longer where this test looks for it");
    let callback = &source[start..];
    let end = callback
        .find("\n}\n")
        .expect("the callback impl has no closing brace");
    let callback = &callback[..end];
    assert!(
        callback.contains("self.grid.lut(),") && callback.contains("self.alpha.as_ref(),"),
        "`prepare` no longer hands the upload the grid's own palette and the \
             frame's curve, so the seam above is reached with neither",
    );
}

/// The production ramp is eight indices wide — half the fade bar, and not
/// the uniform's hard-edged default.
#[test]
fn the_soft_width_is_eight_indices_half_the_fade_bar() {
    // Pinning the value pins it away from zero too: a zero production
    // width is the hard alpha cliff the soft edge exists to dissolve.
    assert_eq!(
        EDGE_SOFT_WIDTH,
        f32::from(MINIMUM_FADE_INDICES) / 2.0 / 255.0,
        "EDGE_SOFT_WIDTH is no longer eight palette indices; the 4/8/16 \
             Harvey comparison behind the number is in its doc comment",
    );
}

/// The bar is inclusive, and a table one index short of it is refused.
#[test]
fn the_fade_bar_is_inclusive_and_bites_one_index_below_it() {
    assert!(palette_refusal_for(u16::from(MINIMUM_FADE_INDICES), "x").is_none());
    assert!(palette_refusal_for(u16::from(MINIMUM_FADE_INDICES) - 1, "x").is_some());
}

/// **The byte-bounded eviction actually bounds.** A set holder is exempt from
/// every shed in this file, so this is the only thing standing between a 3D
/// loop and an unbounded store.
#[test]
fn the_store_eviction_actually_bounds() {
    let store = VolumeStore::new();
    let one = match ready_grid() {
        VolumeEntry::Ready(grid) => crate::raymarch::resident_grid_bytes([
            u32::try_from(grid.shape().nx).unwrap(),
            u32::try_from(grid.shape().ny).unwrap(),
            u32::try_from(grid.shape().nz).unwrap(),
        ])
        .expect("a fixture grid cannot overflow"),
        _ => unreachable!("ready_grid is Ready"),
    };
    assert!(one > 0, "precondition: a resident grid costs something");

    let targets: Vec<VolumeTarget> = (0..4)
        .map(|m| target(RadarProduct::Reflectivity, m))
        .collect();
    for t in &targets {
        assert!(!store.share_held(0, t, Hold::Set), "each target is new");
        store.begin_build_held(0, t, Hold::Set);
        assert!(store.complete(t, ready_grid()), "the build resolves");
    }
    assert_eq!(
        store.texture_bytes(),
        one * 4,
        "precondition: four resident grids, and the byte count is per grid — \
         if this is 0 the eviction below has nothing to measure and passes \
         vacuously",
    );

    let evicted = store.enforce_budget(one * 2);
    assert_eq!(evicted, 2, "the eviction stopped short of the budget");
    assert!(
        store.texture_bytes() <= one * 2,
        "the store is still over its budget after enforcing it: {} bytes \
         against {}",
        store.texture_bytes(),
        one * 2,
    );
    // Oldest-first, by build order: the two the pane asked for first are the
    // two that went. In production those are the pane's live grid and the
    // oldest loop frame, which is exactly the intended order.
    assert!(
        store.lookup(&targets[0]).is_none() && store.lookup(&targets[1]).is_none(),
        "the two oldest entries survived a budget they caused to be exceeded, \
         so the eviction is not oldest-first",
    );
    assert!(
        store.lookup(&targets[3]).is_some(),
        "the newest entry was evicted, so the eviction is not oldest-first and \
         a playing loop would lose the frame it had just built",
    );
}

/// A set holder keeps its whole set through the events that shed a single
/// holder's other grids.
#[test]
fn a_set_holder_keeps_its_whole_set_through_a_build_landing() {
    let store = VolumeStore::new();
    let targets: Vec<VolumeTarget> = (0..3)
        .map(|m| target(RadarProduct::Reflectivity, m))
        .collect();

    for t in &targets {
        store.begin_build_held(0, t, Hold::Set);
        assert!(store.complete(t, ready_grid()), "the build resolves");
    }
    assert_eq!(
        store.live_ids().len(),
        3,
        "a set holder lost grids as its own later builds landed, which is the \
         single-holder swap rule applied to a set",
    );
    for t in &targets {
        assert!(
            store.lookup(t).is_some(),
            "one of the set's grids is gone: {:?}",
            t.volume.collected,
        );
    }

    // The same three targets held the *old* way lose all but the last, which
    // is what makes the assertions above about `Hold::Set` rather than about
    // the store having stopped shedding altogether.
    let single = VolumeStore::new();
    for t in &targets {
        single.begin_build(1, t);
        assert!(single.complete(t, ready_grid()), "the build resolves");
    }
    assert_eq!(
        single.live_ids().len(),
        1,
        "a single holder no longer sheds, so the set holder's exemption above \
         is not being tested",
    );
}

/// `retain_set` is the set holder's shed: what it does not name, it lets go
/// of — and an empty set is the release-before-build rule.
#[test]
fn retain_set_states_the_whole_set_and_release_set_gives_it_all_back() {
    let store = VolumeStore::new();
    let targets: Vec<VolumeTarget> = (0..3)
        .map(|m| target(RadarProduct::Reflectivity, m))
        .collect();
    for t in &targets {
        store.begin_build_held(0, t, Hold::Set);
        assert!(store.complete(t, ready_grid()), "the build resolves");
    }

    let dropped = store.retain_set(0, &targets[1..]);
    assert_eq!(dropped, 1, "the unnamed grid was not let go of");
    assert!(store.lookup(&targets[0]).is_none());
    assert!(store.lookup(&targets[2]).is_some());

    assert_eq!(store.release_set(0), 2, "the whole remaining set goes");
    assert_eq!(store.texture_bytes(), 0, "the store still holds bytes");

    // And the exemption that makes `release_set` safe to call for every pane
    // whose loop is not active: a live 3D pane holds one grid, is not a set
    // holder, and must not lose it.
    let live = target(RadarProduct::Velocity, 9);
    store.begin_build(1, &live);
    assert!(store.complete(&live, ready_grid()), "the build resolves");
    assert_eq!(
        store.release_set(1),
        0,
        "release_set took a live pane's only grid, so a 3D pane with no loop \
         would rebuild it every frame",
    );
    assert!(store.lookup(&live).is_some());
}

/// GPU texture bytes one [`ready_grid`] costs the store, by the same arithmetic
/// `StoredVolume::texture_bytes` uses — so a test asserting that bytes went
/// away is asserting about a real allocation rather than about a count.
fn one_grid_texture_bytes() -> usize {
    match ready_grid() {
        VolumeEntry::Ready(grid) => {
            let shape = grid.shape();
            crate::raymarch::resident_grid_bytes([
                u32::try_from(shape.nx).unwrap(),
                u32::try_from(shape.ny).unwrap(),
                u32::try_from(shape.nz).unwrap(),
            ])
            .expect("a fixture grid cannot overflow")
        }
        _ => unreachable!("ready_grid is Ready"),
    }
}

/// Host bytes one [`ready_grid`] costs the store.
fn one_grid_host_bytes() -> usize {
    match ready_grid() {
        VolumeEntry::Ready(grid) => grid.memory_bytes(),
        _ => unreachable!("ready_grid is Ready"),
    }
}

/// **A pane the layout stopped showing is named, and nothing else is.**
#[test]
fn hidden_holders_names_the_panes_the_layout_dropped_and_their_bytes_go() {
    let store = VolumeStore::new();
    let one = one_grid_texture_bytes();
    let host_one = one_grid_host_bytes();
    assert!(
        one > 0 && host_one > 0,
        "precondition: a resident grid costs something on both sides, or every \
         byte assertion below passes vacuously",
    );

    // Pane 0: a *visible* 3D loop holding a set of two.
    let kept: Vec<VolumeTarget> = (0..2)
        .map(|m| target(RadarProduct::Reflectivity, m))
        .collect();
    for t in &kept {
        store.begin_build_held(0, t, Hold::Set);
        assert!(store.complete(t, ready_grid()), "the build resolves");
    }
    // Pane 1: hidden, holding one live grid the ordinary way.
    let single = target(RadarProduct::Reflectivity, 2);
    store.begin_build(1, &single);
    assert!(store.complete(&single, ready_grid()), "the build resolves");
    // Pane 2: hidden, and a set holder — the case nothing else bounds, because
    // `dispatch_loop_renders` never walks a hidden pane and so never restates
    // its `retain_set`.
    let stranded: Vec<VolumeTarget> = (3..5)
        .map(|m| target(RadarProduct::Reflectivity, m))
        .collect();
    for t in &stranded {
        store.begin_build_held(2, t, Hold::Set);
        assert!(store.complete(t, ready_grid()), "the build resolves");
    }

    assert_eq!(
        store.texture_bytes(),
        one * 5,
        "precondition: five resident grids",
    );
    assert_eq!(
        store.memory_bytes(),
        host_one * 5,
        "precondition: host side"
    );

    assert!(
        store.hidden_holders(3).is_empty(),
        "a layout showing all three panes has nothing to release, and naming a \
         visible pane would take a live volume away mid-frame",
    );
    assert_eq!(
        store.hidden_holders(1),
        vec![1, 2],
        "the layout shows one pane, so panes 1 and 2 are gone from it — a set \
         holder among them, which is the one nothing else bounds",
    );

    for pane in store.hidden_holders(1) {
        store.release(pane);
    }

    assert_eq!(
        store.texture_bytes(),
        one * 2,
        "the hidden panes' grids are still resident: {} bytes where two grids \
         is {}",
        store.texture_bytes(),
        one * 2,
    );
    assert_eq!(
        store.memory_bytes(),
        host_one * 2,
        "the host bytes did not go with the GPU ones",
    );

    // The other direction, and the one a careless release breaks: the visible
    // loop's resident set is untouched.
    for t in &kept {
        assert!(
            store.lookup(t).is_some(),
            "releasing a hidden pane tore down the visible loop's set: {:?} is \
             gone",
            t.volume.collected,
        );
    }
    assert!(
        store.holds_set(0),
        "the visible loop stopped being a set holder, so the next build to land \
         will shed the frames it is animating",
    );
    assert!(
        !store.holds_set(2),
        "a released pane is still on the set-holder list, so coming back as a \
         single holder it would be exempt from every shed there is",
    );

    assert!(
        store.hidden_holders(1).is_empty(),
        "the sweep is not edge-triggered: it names the same panes again with \
         nothing left to give, so the frame path would do this work for ever",
    );
}

/// A pane marked a set holder but holding **no entry** is named too, so the
/// mark goes with the release.
#[test]
fn a_hidden_pane_still_marked_a_set_holder_is_named_so_the_mark_goes_too() {
    let store = VolumeStore::new();
    let t = target(RadarProduct::Reflectivity, 0);
    store.begin_build_held(1, &t, Hold::Set);
    assert!(store.complete(&t, ready_grid()), "the build resolves");
    assert_eq!(store.release_set(1), 1, "the set goes");
    assert!(
        store.holds_set(1),
        "precondition: `retain_set` leaves the mark behind, which is the state \
         this test is about",
    );
    assert_eq!(store.texture_bytes(), 0, "precondition: no entry is left");

    assert_eq!(
        store.hidden_holders(1),
        vec![1],
        "a holder with a mark and no entry is invisible to a scan of the \
         entries alone",
    );
    store.release(1);
    assert!(!store.holds_set(1), "the release did not take the mark");
    assert!(store.hidden_holders(1).is_empty());
}
