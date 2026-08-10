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
///
/// **This is the test the stub painter cannot be.** A wrong-typed payload is
/// one `log::warn!` in `prepare` and a silent `continue` in `paint`, so
/// every headless test in `rustdar-egui` — which can only ever see an
/// `Arc<dyn Any>` — would pass against a payload that never draws a pixel.
/// This crate is the only one that can name both ends, so the downcast is
/// asserted here.
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
fn ready_grid() -> VolumeEntry {
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
        half_width_km: 40.0,
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

/// The stand-in is scoped: a pane re-aimed at another **site, moment or
/// region** must not paint its old grid under the new target's caption —
/// the one lie the swap must never tell: another site's storm under this
/// pane's caption.
///
/// The held entry is a **`Ready` grid**, and that is what makes the pin
/// bite. Only a `Ready` entry can ever stand in — held as a `Refused`
/// stub, the fallback's own `Ready`-match refuses it before any scope
/// decision is reached, so a `same_scope` that answered always-true
/// survived the whole suite. The layer this actually exercises is
/// `begin_build`'s shed: `keep_old` keeps only same-scope resolved
/// entries, and a cross-scope `Ready` hold is exactly the case that
/// reaches that clause.
#[test]
fn an_out_of_scope_grid_never_stands_in() {
    let store = VolumeStore::new();
    let refl = target(RadarProduct::Reflectivity, 0);
    assert!(!store.share(0, &refl), "the first ask owns the dispatch");
    store.begin_build(0, &refl);
    assert!(store.complete(&refl, ready_grid()));

    let velocity = target(RadarProduct::Velocity, 0);
    assert!(!store.share(0, &velocity));
    store.begin_build(0, &velocity);
    assert!(
        store.lookup_for_pane(0, &velocity).is_none(),
        "a reflectivity grid must not stand in for a velocity build",
    );
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
///
/// A callback built on the frame a volume rolled is still in egui's shape
/// list when `prepare` runs. If the store had reused the id, that callback
/// would draw the *new* volume through the old one's uniform — a picture
/// that is wrong and looks right.
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
///
/// This is the arithmetic `prepare` does and nothing else: geography in
/// points from `paint`, the frame's own pixel size from the descriptor, out
/// come the two `vec4`s the shader reprojects through. It is a free function
/// precisely so it can be pinned here — the containing `prepare` needs a
/// `wgpu::Device`, and this is where a sign or a swapped axis would live.
///
/// The gamma lane gets both arms because **both are live**:
/// `app_state::select_surface_format` prefers a non-sRGB format only on wasm
/// and takes `capabilities.formats[0]` natively, so a desktop build and a
/// browser build reach opposite branches. A wrong flag is a floor merely a
/// little too dark or too light, with no validation error anywhere.
#[test]
fn the_floor_lanes_normalise_points_against_the_frame_and_carry_the_encoding() {
    let source = FloorSource {
        // 400 points across a 1600-point-wide frame: a quarter in.
        site_points: [400.0, 300.0],
        points_per_degree_lon: 80.0,
        // Negative, because Mercator y grows north and screen y grows down.
        points_per_mercator_y: -5000.0,
        site_lat: 41.7,
        west_km: -230.0,
        south_km: -230.0,
    };
    // 3200x2400 pixels at 2 points per pixel is a 1600x1200-point frame.
    let (uv, geo) = floor_lanes(&source, [3200, 2400], 2.0, true);

    // `point x pixels_per_point / frame_pixels`: 400 x 2 / 3200 = 0.25 across,
    // 300 x 2 / 2400 = 0.25 down; 80 x 2 / 3200 = 0.05 of the mirror per degree
    // of longitude, and -5000 x 2 / 2400 = -4.1667 per unit of Mercator y.
    // Compared with a tolerance because the products are not representable:
    // 80 x 2 / 3200 comes out 0.049999997.
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

    // Halving the mirror halves nothing: the lanes are `point x
    // pixels_per_point / frame_pixels`, and the reduced-resolution path
    // halves both of those together. This is why the mirror's own size never
    // reaches this function.
    let (half_uv, _) = floor_lanes(&source, [1600, 1200], 1.0, true);
    assert_eq!(half_uv, uv, "a half-resolution mirror maps identically");

    let (_, linear) = floor_lanes(&source, [3200, 2400], 2.0, false);
    assert_eq!(linear[3], 0.0, "an sRGB swapchain leaves the mirror linear");
}

/// Every samplable moment clears the solid-block bar, and the counts here
/// are `rustdar_radar::voxel`'s own measurements — the deliberate flip of
/// the original `only_reflectivity_clears_the_fade_bar`, whose doc said a
/// widened set "is a decision someone should make on purpose rather than
/// discover". The products WP made it: each moment's transparency profile
/// is argued at `volume_alpha_scale`, measured by
/// `the_default_transparency_profile_is_measured_per_product` upstream,
/// and admitted here.
///
/// Written as literals rather than by rebuilding six grids, and that is
/// the point: the upstream test pins what the tables produce, and this
/// pins what this renderer *does* about it.
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
    // narrow profile (its clear band is honestly small — laminar flow is
    // a thin slice of its scale); everything else clears by 2x or more,
    // and a profile change eroding that should be renegotiated here.
    for (moment, see_through) in measured {
        assert!(
            see_through >= 2 * u16::from(MINIMUM_FADE_INDICES) || moment == "Spectrum Width",
            "{moment} clears the bar by less than 2x: {see_through}",
        );
    }
    // The production wiring reads the see-through measure, not the bottom
    // run: velocity's fade_band is honestly 0 (its ramp bottom is the
    // strongest inbound air), so a gate on the bottom run would refuse it
    // in production with every literal above still green. Source-scanned
    // for the same reason as
    // `the_guards_paint_cannot_be_tested_through_are_still_in_it`: no
    // test here can build a `VoxelGrid`.
    assert!(
        include_str!("../volume_bridge.rs")
            .contains("palette_refusal_for(grid.see_through_indices(), grid.product().name())"),
        "palette_refusal no longer reads the see-through measure",
    );
}

/// A refusal names the moment and says what would have to change.
///
/// The pane paints this text and nothing else, so a bare "unavailable" here
/// is a user staring at an empty box with no idea whether to wait, switch
/// product, or file a bug.
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
///
/// # Why this is a source scan and not a behavioural test
///
/// Both guards read a `VoxelGrid`, and a `VoxelGrid` has no constructor
/// outside `build_voxels` — which needs a synthetic `nexrad_model` `Scan`.
/// So the only behavioural test would be an integration test carrying a
/// scan builder, and until one exists these two guards can be deleted with
/// every test in the workspace still green. Mutation testing found exactly
/// that: removing the palette gate, and rewriting the tilt check as "the
/// index plane is all no-data", both survived.
///
/// The second of those is the one that matters. A single-tilt volume *does*
/// yield an empty grid, so the emptiness test is right almost always — and
/// wrong without warning when a cell centre lands bit-exactly on the beam's
/// height, which is measure-zero rather than impossible. It also loses the
/// reason: the user gets an empty box instead of "wait for a full scan".
///
/// A scan is a weak test and is named as one. It is here because a guard
/// nothing can fail is worse.
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

    // The soft-edge mechanism's two production lines. Mutation testing
    // proved that deleting both left the entire workspace suite green:
    // the uniform's defaults (index-0 threshold, hard edge) are a
    // renderable configuration, so nothing downstream fails — the user
    // simply gets the hard shelf rims and the wasted marching the
    // 2026-08-09 work exists to remove. No behavioural test can reach
    // `paint` with a `Ready` grid (`VoxelGrid` has no constructor outside
    // `build_voxels`), so the lines are pinned here; the *values* they
    // assign are behaviourally pinned by
    // `the_skip_threshold_separates_the_last_transparent_entry_from_the_first_visible_one`
    // and `the_soft_width_is_eight_indices_half_the_fade_bar` below, and
    // the GPU mask instrument in `tests/volume_silhouette.rs` observes
    // the default width staying hard.
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
    // deleting either leaves every host test green (the uniform's raw
    // defaults are a renderable configuration) and the user gets the
    // voxel-spiked stippled render #5 was filed about.
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
    // And the isosurface's exemption from it, pinned here for the same
    // reason: no host test can reach `paint` with a `Ready` grid, and the
    // failure is invisible in every other suite — the line's absence passed
    // 13/13 volume_gpu, 10/10 silhouette and 151/151 lib while deleting a
    // lone measured voxel from the 3D surface outright at the shipped region
    // rung. The measurement lives in
    // `an_isosurface_at_the_shipped_rung_keeps_its_sub_kernel_features`.
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
    // absence is asserted rather than merely un-tested. It read
    // `if !rustdar_radar::voxel::no_data_blends_at_ramp_bottom(...)` and
    // pinned the reconstruction to nearest for the seven products whose
    // ramp bottom is a real value — honest, and blocky. The volume texture
    // is coverage-premultiplied `Rg8Unorm` now (`volume::VOLUME_TEXTURE_FORMAT`),
    // so a filtered sample beside empty air reconstructs inside the convex
    // hull of the stored indices for every product and there is nothing left
    // to override. Reinstating a per-product reconstruction decision here
    // would be a silent regression back to a blocky march for seven of the
    // nine, so it fails.
    assert!(
        !body.contains("no_data_blends_at_ramp_bottom") && !body.contains("NEAREST"),
        "`paint` makes a per-product reconstruction decision again: the \
             coverage channel retired that split, and re-adding it sends \
             seven of the nine products back to a nearest march",
    );
    // The floor's flag-and-texture pairing: the flag must be exactly
    // "a floor is in hand and the pane asked", or the shader composites
    // a ground nobody bound (a transparent no-op that claims to draw) or
    // draws one against the pane's toggle.
    assert!(
        body.contains("uniform.map_floor = floor.is_some()"),
        "`paint` no longer ties the floor flag to the floor being in hand",
    );
    assert!(
        body.contains("frame.floor.then_some(frame.source).flatten()"),
        "`paint` no longer consults the pane's floor toggle before looking \
             a floor up, so the per-pane escape hatch is dead",
    );

    // The isosurface wiring, same untestable-through-`paint` class: the
    // lanes must be translated against the grid's own ramp, and the skip
    // threshold must drop to the index-0 default — the surface reads the
    // data, so neither the palette's band nor a Volume Alpha curve may
    // move it. Deleting the whole `if` leaves every host test green and
    // every isosurface pane silently painting the lit volume.
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
///
/// [`CLOUD_RECONSTRUCTION_LOD`] is 1.0 — the full blend into the two-cell
/// mean, chosen on the Harvey volume because below ~0.7 the spikes
/// survive as hairs, and there is no level past 1 to reach.
/// [`CLOUD_STEP_CELLS`] is half the instrument default of
/// `volume::raymarch::RAYMARCH_STEP_CELLS`, and the relation matters as
/// much as the number: the half-cell step exists to halve the per-step
/// opacity quantum, and the step ceiling (1024) was sized so that half-cell
/// steps still cover the desktop grid's 384-cell diagonal — raise this
/// without touching the ceiling and long diagonals silently fall back to
/// stretched steps.
#[test]
fn the_cloud_rung_marches_the_smoothed_field_at_half_cell_steps() {
    assert_eq!(CLOUD_RECONSTRUCTION_LOD, 1.0);
    assert_eq!(
        CLOUD_STEP_CELLS,
        crate::volume::raymarch::RAYMARCH_STEP_CELLS / 2.0,
    );
    let ceiling = crate::volume::raymarch::RAYMARCH_STEP_CEILING as f32;
    let desktop_diagonal_cells = (256.0f32 * 256.0 + 256.0 * 256.0 + 128.0 * 128.0).sqrt();
    assert!(
        desktop_diagonal_cells / CLOUD_STEP_CELLS <= ceiling,
        "the desktop grid's diagonal needs {} cloud steps against a \
             ceiling of {ceiling}; the far corner of every diagonal view \
             falls to stretched steps",
        desktop_diagonal_cells / CLOUD_STEP_CELLS,
    );
}

/// The cloud smoothing is a function of cell size: full at the region
/// rungs, **zero at the default whole-volume box**, monotone between.
///
/// The zero half is the data-honesty pin. A fixed LOD of 1.0 at the
/// default box's 1.8 km cells was measured erasing the Harvey eyewall —
/// −41% of the ≥50 dBZ mask, −81% of ≥30 dBZ — while the 2D pane showed
/// the red core (the table in [`cloud_reconstruction_lod_for`]).
/// Restoring the fixed LOD fails the third assert; inverting the taper
/// (smoothing the coarse grid instead of the fine) fails the first.
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
    // The default whole-volume box: 460 km over 256 cells, 1.797 km/cell,
    // computed through the same helper `paint` feeds the taper.
    let uniform = VolumeUniform::new([460.0, 460.0, 18.0], [256, 256, 128]);
    let default_cell = largest_cell_km(&uniform);
    assert!(
        (1.75..1.85).contains(&default_cell),
        "the default box's coarsest cell moved: {default_cell} km",
    );
    assert_eq!(
        cloud_reconstruction_lod_for(default_cell),
        0.0,
        "the default whole-volume box must march the raw field: at \
             1.8 km cells the two-cell kernel is wider than the cores it \
             lands on, and the smoothing erases them (measured, Harvey)",
    );
}

/// [`empty_index_threshold_for`] sits strictly between the last fully
/// transparent palette entry and the first visible one, for every band.
///
/// The behavioural half of the anchor: a Nearest-sampled LUT fetch of
/// entry `n` sees the index value `n / 255`, entries `0..=band` are
/// transparent and `band + 1` is the first visible one — so the shader's
/// `index > threshold` test must be false at `band / 255` and true at
/// `(band + 1) / 255`. The shipped off-by-one (`(band - 0.5) / 255`)
/// fails the first half of this: entry `band` itself cleared the
/// threshold, so a one-index shell of zero-alpha samples paid up to seven
/// fetches per step for nothing and the ramp's foot sat one index below
/// the palette's own fade boundary.
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
///
/// With no curve, [`effective_lut`] must answer with the grid's own bytes
/// — the same allocation, not an equal copy — because "borrowed" is the
/// one shape no alpha rewrite can quietly pass through. The mutation this
/// exists to kill is any unconditional transform at the upload seam (a
/// constant 0.5 alpha, a re-derived table): every one of them turns the
/// borrow into an owned buffer or moves the bytes, and this test dies.
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
///
/// For every curve, the threshold [`effective_fade_band`] anchors must
/// separate the last transparent entry of the **uploaded** table from its
/// first visible one — the march may never skip visible data and never
/// pay for a guaranteed-transparent shell at the ramp's foot. Checked
/// against [`effective_lut`]'s actual output rather than against the
/// curve, so the two halves of the seam (what is uploaded, what is
/// anchored) are pinned to agree with *each other*: mutating either one —
/// anchoring on the palette band while a curve strips the low end, or
/// uploading a curve the anchor ignores — breaks the agreement and fails
/// here by name.
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

/// The curve is applied in `prepare` and only through the staleness
/// comparison — the same source-scan arrangement as the painter's guards,
/// and for the same reason: `prepare` needs a `wgpu::Device`, so no host
/// test can reach it. Mutation testing on the *behavioural* seam is what
/// pins the values ([`effective_lut`], [`effective_fade_band`] above);
/// this pins that `prepare` still consults them, and that the rewrite is
/// gated on the curve actually changing rather than issued per frame.
#[test]
fn prepare_applies_the_curve_through_the_staleness_gate() {
    let source = include_str!("../volume_bridge.rs");
    let start = source
        .find("impl egui_wgpu::CallbackTrait for VolumeCallback {")
        .expect("the callback impl is no longer where this test looks for it");
    let body = &source[start..];
    let end = body
        .find("\n}\n")
        .expect("the callback impl has no closing brace");
    let body = &body[..end];

    assert!(
        body.contains("if upload.applied_alpha != self.alpha {"),
        "the LUT rewrite is no longer gated on the curve changing — either \
             an edit stopped applying to an already-uploaded grid, or the 1 KiB \
             table is being rewritten every frame",
    );
    assert!(
        body.matches("effective_lut(self.grid.lut(), self.alpha.as_ref())")
            .count()
            >= 2,
        "both upload paths — first upload and in-place rewrite — must build \
             the table through `effective_lut`, or one of them ships the wrong \
             alpha",
    );
}

/// The production ramp is eight indices wide — half the fade bar, and not
/// the uniform's hard-edged default.
///
/// Flipping [`EDGE_SOFT_WIDTH`] to 0 is the one-character revert of the
/// user-visible half of the soft edge, and before this test nothing in
/// the workspace could see it.
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
///
/// Both halves matter. Written as `>` the whole set would flip on a table
/// sitting exactly at 16; written as `>=` on the wrong side, a 15-index
/// token see-through region would pass and paint the block this gate
/// exists to stop.
#[test]
fn the_fade_bar_is_inclusive_and_bites_one_index_below_it() {
    assert!(palette_refusal_for(u16::from(MINIMUM_FADE_INDICES), "x").is_none());
    assert!(palette_refusal_for(u16::from(MINIMUM_FADE_INDICES) - 1, "x").is_some());
}
