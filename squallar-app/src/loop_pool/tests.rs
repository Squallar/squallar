use super::*;
use squallar_device_profile::constants::{
    DESKTOP_LOOP_IMAGE_SIZE, DESKTOP_LOOP_POOL_CEILING_BYTES, DESKTOP_LOOP_POOL_FLOOR_BYTES,
    DESKTOP_MAX_LOOP_RENDER_BUDGET, DESKTOP_VOLUME_GRID_CELLS, MOBILE_LOOP_IMAGE_SIZE,
    MOBILE_LOOP_POOL_CEILING_BYTES, MOBILE_LOOP_POOL_FLOOR_BYTES, MOBILE_MAX_LOOP_RENDER_BUDGET,
    MOBILE_VOLUME_GRID_CELLS, WASM_LOOP_IMAGE_SIZE, WASM_LOOP_POOL_CEILING_BYTES,
    WASM_LOOP_POOL_FLOOR_BYTES, WASM_MAX_LOOP_RENDER_BUDGET, WASM_VOLUME_GRID_CELLS,
};
use squallar_device_profile::quality::DeviceClass;
use squallar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

/// One device class, with both halves of every question a host build cannot otherwise
/// reach.
struct Arm {
    name: &'static str,
    model: LoopFrameModel,
    limits: LoopPoolLimits,
    /// This class's `MAX_PANES`, from `squallar_egui::pane` — the other half of the
    /// multiplication that started all of this.
    max_panes: usize,
    /// The 3D frame count this class ships, which the pool must reproduce for a single loop
    /// at the floor.
    volume_loop_frames: usize,
}

/// `loop_image_size` is the side a **loop** frame renders at, which on the web is not the
/// side a static one takes — see [`LoopFrameModel::plan_view`].
fn model(
    loop_image_size: usize,
    section_width: usize,
    grid: [u32; 3],
    render_budget: usize,
) -> LoopFrameModel {
    LoopFrameModel {
        plan_view: loop_image_size * loop_image_size * 4,
        section: section_width * (section_width / 2) * 4,
        grid: squallar_volumetric::raymarch::resident_grid_bytes(grid)
            .expect("a shipped grid shape"),
        overlay: crate::loop_pool::nominal_overlay_frame_bytes(),
        render_budget,
    }
}

fn arms() -> [Arm; 3] {
    [
        Arm {
            name: "wasm32",
            model: model(
                WASM_LOOP_IMAGE_SIZE,
                WASM_SECTION_WIDTH,
                WASM_VOLUME_GRID_CELLS,
                WASM_MAX_LOOP_RENDER_BUDGET,
            ),
            limits: LoopPoolLimits {
                floor: WASM_LOOP_POOL_FLOOR_BYTES,
                ceiling: WASM_LOOP_POOL_CEILING_BYTES,
            },
            max_panes: squallar_device_profile::budget::MAX_PANES_DESKTOP,
            volume_loop_frames: 11,
        },
        Arm {
            name: "mobile",
            model: model(
                MOBILE_LOOP_IMAGE_SIZE,
                NATIVE_SECTION_WIDTH,
                MOBILE_VOLUME_GRID_CELLS,
                MOBILE_MAX_LOOP_RENDER_BUDGET,
            ),
            limits: LoopPoolLimits {
                floor: MOBILE_LOOP_POOL_FLOOR_BYTES,
                ceiling: MOBILE_LOOP_POOL_CEILING_BYTES,
            },
            max_panes: squallar_device_profile::budget::MAX_PANES_MOBILE,
            volume_loop_frames: 17,
        },
        Arm {
            name: "desktop",
            model: model(
                DESKTOP_LOOP_IMAGE_SIZE,
                NATIVE_SECTION_WIDTH,
                DESKTOP_VOLUME_GRID_CELLS,
                DESKTOP_MAX_LOOP_RENDER_BUDGET,
            ),
            limits: LoopPoolLimits {
                floor: DESKTOP_LOOP_POOL_FLOOR_BYTES,
                ceiling: DESKTOP_LOOP_POOL_CEILING_BYTES,
            },
            max_panes: squallar_device_profile::budget::MAX_PANES_DESKTOP,
            volume_loop_frames: 14,
        },
    ]
}

/// Every mix of loop kinds that fits on `max_panes` panes, plus the empty one.
fn reachable_demands(max_panes: usize) -> Vec<LoopDemand> {
    let mut out = Vec::new();
    for plan_view_loops in 0..=max_panes {
        for section_loops in 0..=(max_panes - plan_view_loops) {
            for volume_sets in 0..=(max_panes - plan_view_loops - section_loops) {
                out.push(LoopDemand {
                    plan_view_loops,
                    section_loops,
                    volume_sets,
                    ..LoopDemand::default()
                });
            }
        }
    }
    out
}

/// The claim the whole change exists to make, and the one nothing could have made before:
/// `MAX_PANES × LOOP_TEXTURE_BUDGET_BYTES` was 3.0 GiB on desktop and 1.0 GiB on a phone,
/// and no test put those two halves side by side because they lived in different crates.
#[test]
fn the_pool_actually_bounds_the_sum() {
    for arm in arms() {
        for bytes in [arm.limits.floor, arm.limits.ceiling] {
            let pool = LoopPool::new(bytes, arm.limits);
            for demand in reachable_demands(arm.max_panes) {
                let allocation = pool.plan(arm.model, demand);

                // The raster kinds, whose division is the only bound there is.
                let raster =
                    demand.plan_view_loops * allocation.plan_view_frames * arm.model.plan_view
                        + demand.section_loops * allocation.section_frames * arm.model.section;
                assert!(
                    raster <= pool.bytes(),
                    "{}: {demand:?} at a {} MiB pool caches {} MiB of loop \
                     frames, and nothing at runtime will take it back",
                    arm.name,
                    pool.bytes() / (1024 * 1024),
                    raster / (1024 * 1024),
                );

                // The 3D kind, whose bound is the reserve the store is held to.
                assert!(
                    allocation.volume_reserve_bytes() <= pool.bytes(),
                    "{}: {demand:?} at a {} MiB pool reserves {} MiB of resident \
                     grids",
                    arm.name,
                    pool.bytes() / (1024 * 1024),
                    allocation.volume_reserve_bytes() / (1024 * 1024),
                );

                // And the whole allocation together, except where the minimum had to win —
                // which is stated rather than absorbed, and is reachable only for 3D loops
                // whose grids cost more than a share.
                let total = allocation.bytes(arm.model, demand);
                assert!(
                    total <= pool.bytes()
                        || (demand.volume_sets > 0
                            && allocation.volume_frames == MIN_LOOP_FRAMES_PER_PANE),
                    "{}: {demand:?} at a {} MiB pool allocates {} MiB with no \
                     loop at the minimum — the pool does not bound the sum",
                    arm.name,
                    pool.bytes() / (1024 * 1024),
                    total / (1024 * 1024),
                );
            }
        }
    }
}

/// The behaviour the pool was asked for, stated as the property rather than as the formula.
#[test]
fn a_loop_shortens_when_a_pane_arrives_and_recovers_when_it_goes() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.floor, arm.limits);
        let frames = |loops: usize| {
            pool.plan(
                arm.model,
                LoopDemand {
                    plan_view_loops: loops,
                    ..LoopDemand::default()
                },
            )
            .plan_view_frames
        };
        let alone = frames(1);
        let crowded = frames(arm.max_panes);
        assert!(
            crowded < alone,
            "{}: a loop is {crowded} frames alone and {alone} frames beside \
             {} others — the pool is not being divided",
            arm.name,
            arm.max_panes - 1,
        );
        assert!(
            crowded >= MIN_LOOP_FRAMES_PER_PANE,
            "{}: a full screen cuts a loop to {crowded} frames, under the \
             {MIN_LOOP_FRAMES_PER_PANE}-frame minimum — this is the cliff, not \
             the degradation",
            arm.name,
        );
        for loops in 1..arm.max_panes {
            assert!(
                frames(loops + 1) <= frames(loops),
                "{}: going from {loops} loops to {} lengthens a loop from {} to \
                 {} frames",
                arm.name,
                loops + 1,
                frames(loops),
                frames(loops + 1),
            );
        }
        // And the recovery is the same division read backwards, which is what makes closing
        // a pane give the length back rather than needing a separate rule that could
        // disagree with this one.
        assert_eq!(frames(1), alone, "{}", arm.name);
    }
}

/// A 3D loop's frames are resident grids in a single application-wide `VolumeStore` keyed
/// by `VolumeTarget`, so two panes orbiting one volume from two angles already share one
/// build, one upload and one resident set.
#[test]
fn the_3d_set_is_not_double_counted_across_two_panes() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.floor, arm.limits);
        let one_set = pool.plan(
            arm.model,
            LoopDemand {
                volume_sets: 1,
                ..LoopDemand::default()
            },
        );
        // Two panes on one volume: `LoopDemand::add` is told the key was already counted,
        // so the demand is unchanged and so is the share.
        let mut demand = LoopDemand::default();
        demand.add(RenderView::Volume, false);
        demand.add(RenderView::Volume, true);
        assert_eq!(
            demand.volume_sets, 1,
            "{}: a second pane on the same volume was counted as a second set",
            arm.name,
        );
        assert_eq!(
            pool.plan(arm.model, demand),
            one_set,
            "{}: two panes on one volume were charged two shares",
            arm.name,
        );

        // Two panes on two volumes really are two sets, and really do divide.
        let mut distinct = LoopDemand::default();
        distinct.add(RenderView::Volume, false);
        distinct.add(RenderView::Volume, false);
        assert_eq!(distinct.volume_sets, 2, "{}", arm.name);
        let two_sets = pool.plan(arm.model, distinct);
        assert!(
            two_sets.share_bytes < one_set.share_bytes,
            "{}: two distinct 3D loops were not divided",
            arm.name,
        );
        // And the store is held to the sum across sets, not to one of them —
        // `enforce_budget` evicts oldest-first over one store, so a bound naming only one
        // set would evict the older loop's frames for ever.
        assert_eq!(
            two_sets.volume_reserve_bytes(),
            two_sets.share_bytes * 2,
            "{}",
            arm.name,
        );
    }
}

/// A single 3D loop at the floor holds exactly the count this target ships.
#[test]
fn the_pool_reproduces_the_shipped_3d_frame_count() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.floor, arm.limits);
        let allocation = pool.plan(
            arm.model,
            LoopDemand {
                volume_sets: 1,
                ..LoopDemand::default()
            },
        );
        assert_eq!(
            allocation.volume_frames, arm.volume_loop_frames,
            "{}: the pool gives a single 3D loop {} grids where this target \
             ships {}",
            arm.name, allocation.volume_frames, arm.volume_loop_frames,
        );
    }
}

/// A full 3D loop leaves room for a live grid at every pool size.
#[test]
fn a_full_3d_loop_leaves_room_for_a_live_grid_at_every_pool_size() {
    for arm in arms() {
        let mut bytes = arm.limits.ceiling;
        while bytes >= arm.limits.floor {
            let pool = LoopPool::new(bytes, arm.limits);
            for sets in 1..=arm.max_panes {
                let allocation = pool.plan(
                    arm.model,
                    LoopDemand {
                        volume_sets: sets,
                        ..LoopDemand::default()
                    },
                );
                let resident = sets * allocation.volume_frames * arm.model.grid;
                assert!(
                    resident + arm.model.grid <= allocation.volume_reserve_bytes()
                        // The minimum wins over the byte rule, deliberately: a loop cut to
                        // nothing is worse than one that makes the store evict.
                        || allocation.volume_frames == MIN_LOOP_FRAMES_PER_PANE,
                    "{}: {sets} loop(s) of {} grids at a {} MiB pool leave no \
                     room for a live grid, so the store evicts the loop's own \
                     oldest frame and rebuilds it for ever",
                    arm.name,
                    allocation.volume_frames,
                    bytes / (1024 * 1024),
                );
            }
            if bytes == arm.limits.floor {
                break;
            }
            bytes = (bytes / 2).max(arm.limits.floor);
        }
    }
}

/// No share ever buys more frames than the dispatcher would texture.
#[test]
fn no_share_buys_more_frames_than_the_dispatcher_textures() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.ceiling, arm.limits);
        let allocation = pool.plan(
            arm.model,
            LoopDemand {
                plan_view_loops: 1,
                section_loops: 1,
                volume_sets: 1,
                ..LoopDemand::default()
            },
        );
        for frames in [
            allocation.plan_view_frames,
            allocation.section_frames,
            allocation.volume_frames,
        ] {
            assert!(
                frames <= arm.model.render_budget,
                "{}: a share bought {frames} frames against a render budget of {}",
                arm.name,
                arm.model.render_budget,
            );
        }
    }
}

/// A section loop gets more history than a plan-view one from the same share.
#[test]
fn an_equal_share_buys_a_section_loop_more_history() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.floor, arm.limits);
        let allocation = pool.plan(
            arm.model,
            LoopDemand {
                plan_view_loops: 1,
                section_loops: 1,
                ..LoopDemand::default()
            },
        );
        assert_eq!(arm.model.section * 2, arm.model.plan_view, "{}", arm.name);
        assert!(
            allocation.section_frames >= allocation.plan_view_frames,
            "{}: a section loop got {} frames against a plan view's {}",
            arm.name,
            allocation.section_frames,
            allocation.plan_view_frames,
        );
    }
}

/// One plan-view pane looping over two hours with no cadence yet — it wants the
/// whole render budget.
fn looping_pane() -> squallar_device_profile::scene::PaneNeed {
    squallar_device_profile::scene::PaneNeed {
        px: [1920, 1080],
        view: RenderView::PlanView,
        looping: true,
        loop_span_secs: 2 * 60 * 60,
        cadence_secs: None,
        overlay_frame_bytes: 0,
        volume_grids: 0,
        ground: squallar_device_profile::quality::GroundPass::Off,
        buildings: false,
    }
}

/// `panes` of [`looping_pane`] and nothing else.
fn scene_of(panes: usize) -> Scene {
    Scene {
        panes: vec![looping_pane(); panes],
        tile_sources: Vec::new(),
        mirror_px: [0, 0],
    }
}

/// **The scene decides where between the bounds the pool sits, not the
/// class.** One two-hour loop needs exactly the floor on every arm — the floor
/// was argued as one loop's span, and this is that argument read back — and a
/// full screen of them needs the lesser of their sum and the room the static
/// renders leave under the presumed capacity, held to the bracket. An adapter
/// that says nothing and a discrete card get the same pool for the same one
/// loop: the ruling, as arithmetic. On the desktop bracket one loop is 36 x
/// 16 MiB = 576 MiB, not the 3072 MiB ceiling a discrete card used to be
/// handed, and six are min(3456, 3840 - 6 x 256) = 2304 MiB.
#[test]
fn the_scene_decides_where_between_the_bounds_the_pool_sits() {
    use crate::budget_arms::shipped_profile;
    use squallar_device_profile::budget::{BudgetLimits, DeviceProfile, FormFactor, resolve};

    const MIB: usize = 1024 * 1024;
    for limits in BudgetLimits::SHIPPED {
        let cap = Capacity::presumed(&limits);
        let mut one_loop_pools = Vec::new();
        for class in [DeviceClass::Unknown, DeviceClass::Discrete] {
            let b = resolve(&DeviceProfile {
                class,
                form_factor: Some(FormFactor::Desktop),
                ..shipped_profile(limits)
            });
            let pool_for = |panes: usize| {
                LoopPool::for_scene(&scene_of(panes), &b, &cap, LoopPoolLimits::from_budgets(&b))
                    .bytes()
            };

            let one = b.loop_render_budget * b.loop_frame_bytes();
            assert_eq!(
                pool_for(1),
                one,
                "{} / {class:?}: one loop does not get exactly its own span",
                limits.name,
            );
            assert_eq!(
                one, b.loop_pool_floor_bytes,
                "{}: the floor stopped being one loop's span budget",
                limits.name,
            );
            one_loop_pools.push(pool_for(1));

            let full = b.max_panes;
            let need = full * one;
            let room = usize::try_from(cap.allowance())
                .unwrap()
                .saturating_sub(full * b.static_frame_bytes());
            let expected = need
                .min(room)
                .clamp(b.loop_pool_floor_bytes, b.loop_pool_ceiling_bytes);
            assert_eq!(
                pool_for(full),
                expected,
                "{} / {class:?}: {full} loops want {} MiB and {} MiB of room is left \
                 under the {} MiB presumption once {full} static renders of {} MiB \
                 are paid for; the pool is the lesser, held to [{}, {}] MiB",
                limits.name,
                need / MIB,
                room / MIB,
                cap.allowance() / MIB as u64,
                b.static_frame_bytes() / MIB,
                b.loop_pool_floor_bytes / MIB,
                b.loop_pool_ceiling_bytes / MIB,
            );
        }
        assert!(
            one_loop_pools.windows(2).all(|pair| pair[0] == pair[1]),
            "{}: the class moved the pool for one and the same loop: {one_loop_pools:?}",
            limits.name,
        );
    }

    // The desktop figures, named.
    let desktop = resolve(&shipped_profile(BudgetLimits::DESKTOP));
    let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
    let pool_for = |panes: usize| {
        LoopPool::for_scene(
            &scene_of(panes),
            &desktop,
            &cap,
            LoopPoolLimits::from_budgets(&desktop),
        )
        .bytes()
    };
    assert_eq!(pool_for(1), 576 * MIB, "36 x 16 MiB");
    assert_eq!(pool_for(6), 2304 * MIB, "min(6 x 576, 3840 - 6 x 256)");
}

/// The bounds hold whatever the scene asks: nothing looping is the floor, and
/// no scene reaches past the ceiling.
#[test]
fn the_pool_is_held_inside_the_bracket_whatever_the_scene_asks() {
    for arm in arms() {
        assert_eq!(
            LoopPool::new(usize::MAX, arm.limits).bytes(),
            arm.limits.ceiling
        );
        assert_eq!(LoopPool::new(0, arm.limits).bytes(), arm.limits.floor);
    }
    let desktop = crate::budget_arms::arms()[2];
    let cap = Capacity::presumed(&squallar_device_profile::budget::BudgetLimits::DESKTOP);
    let limits = LoopPoolLimits::from_budgets(&desktop);
    assert_eq!(
        LoopPool::for_scene(&Scene::empty(), &desktop, &cap, limits).bytes(),
        limits.floor,
        "nothing looping asks for nothing, and the floor holds",
    );
    assert!(
        LoopPool::for_scene(&scene_of(60), &desktop, &cap, limits).bytes() <= limits.ceiling,
        "sixty loops reached past the ceiling",
    );
}

/// **A pool or a frame model that moves under the same demand is re-planned
/// after the dwell, like a demand that moves.** The pool follows the scene's
/// need and room, and the model follows a re-fit of the budgets; an
/// allocation planned against the old one would cap frames the new one cannot
/// pay for. A flicker inside the dwell still changes nothing, and a shrink is
/// taken with no dead band.
#[test]
fn a_pool_or_model_that_moves_under_the_same_demand_is_replanned_after_the_dwell() {
    use squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES;

    let model = model(
        DESKTOP_LOOP_IMAGE_SIZE,
        NATIVE_SECTION_WIDTH,
        DESKTOP_VOLUME_GRID_CELLS,
        DESKTOP_MAX_LOOP_RENDER_BUDGET,
    );
    let limits = LoopPoolLimits {
        floor: DESKTOP_LOOP_POOL_FLOOR_BYTES,
        ceiling: DESKTOP_LOOP_POOL_CEILING_BYTES,
    };
    let wide = LoopPool::new(limits.ceiling, limits);
    let narrow = LoopPool::new(limits.floor, limits);
    let one = LoopDemand {
        plan_view_loops: 1,
        ..LoopDemand::default()
    };
    let mut state = LoopPoolState::new(wide, model);
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(wide, model, one);
    }
    let settled = state.allocation();
    assert_eq!(settled.share_bytes, wide.bytes());

    // The pool halves and comes back on alternate frames: nothing moves.
    for frame in 0..LOOP_POOL_DWELL_FRAMES * 8 {
        let pool = if frame % 2 == 0 { narrow } else { wide };
        assert_eq!(
            state.observe(pool, model, one),
            settled,
            "the allocation moved on frame {frame} of a pool flicker",
        );
    }

    // Held for the dwell, the narrower pool is taken at once: a shrink.
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(narrow, model, one);
    }
    let shrunk = state.allocation();
    assert_eq!(shrunk.share_bytes, narrow.bytes());
    assert!(shrunk.plan_view_frames <= settled.plan_view_frames);
    assert_eq!(
        shrunk.plan_view_frames, DESKTOP_MAX_LOOP_RENDER_BUDGET,
        "the floor was argued as one loop's whole span: 36 frames",
    );

    // The model moves under the same pool and demand — a re-fit halved the
    // render budget — and after the dwell the frames are capped by the new one.
    let halved = LoopFrameModel {
        render_budget: DESKTOP_MAX_LOOP_RENDER_BUDGET / 2,
        ..model
    };
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(narrow, halved, one);
    }
    assert_eq!(
        state.allocation().plan_view_frames,
        DESKTOP_MAX_LOOP_RENDER_BUDGET / 2,
        "a halved render budget did not reach the allocation in force",
    );
}

/// A pane that appears and vanishes inside the dwell costs nothing at all.
#[test]
fn a_pane_that_flickers_inside_the_dwell_changes_nothing() {
    let model = model(
        DESKTOP_LOOP_IMAGE_SIZE,
        NATIVE_SECTION_WIDTH,
        DESKTOP_VOLUME_GRID_CELLS,
        30,
    );
    let limits = LoopPoolLimits {
        floor: DESKTOP_LOOP_POOL_FLOOR_BYTES,
        ceiling: DESKTOP_LOOP_POOL_CEILING_BYTES,
    };
    let pool = LoopPool::new(limits.floor, limits);
    let one = LoopDemand {
        plan_view_loops: 1,
        ..LoopDemand::default()
    };
    let two = LoopDemand {
        plan_view_loops: 2,
        ..LoopDemand::default()
    };

    let mut state = LoopPoolState::new(pool, model);
    for _ in 0..squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, one);
    }
    let settled = state.allocation();
    assert_eq!(
        settled.plan_view_frames,
        pool.plan(model, one).plan_view_frames
    );

    // A second pane appears and vanishes on alternate frames for many times the dwell.
    for frame in 0..squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES * 8 {
        let demand = if frame % 2 == 0 { two } else { one };
        assert_eq!(
            state.observe(pool, model, demand),
            settled,
            "the allocation moved on frame {frame} of a flicker",
        );
    }

    // Held for the dwell, it is taken — and it is shorter, not blank.
    for _ in 0..squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, two);
    }
    let shared = state.allocation();
    assert!(shared.plan_view_frames < settled.plan_view_frames);
    assert!(shared.plan_view_frames >= MIN_LOOP_FRAMES_PER_PANE);
}

/// A shrink is taken after the dwell; a growth also has to clear the dead band.
#[test]
fn a_growth_has_to_clear_the_dead_band_but_a_shrink_does_not() {
    let model = model(
        DESKTOP_LOOP_IMAGE_SIZE,
        NATIVE_SECTION_WIDTH,
        DESKTOP_VOLUME_GRID_CELLS,
        30,
    );
    let limits = LoopPoolLimits {
        floor: DESKTOP_LOOP_POOL_FLOOR_BYTES,
        ceiling: DESKTOP_LOOP_POOL_CEILING_BYTES,
    };
    let pool = LoopPool::new(limits.ceiling, limits);
    let loops = |n: usize| LoopDemand {
        plan_view_loops: n,
        ..LoopDemand::default()
    };
    let settle = |state: &mut LoopPoolState, demand| {
        for _ in 0..squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES {
            state.observe(pool, model, demand);
        }
        state.allocation()
    };

    let mut state = LoopPoolState::new(pool, model);
    let six = settle(&mut state, loops(6));

    // Five of six: 6/5 = 1.2x, inside the band.
    let five = settle(&mut state, loops(5));
    assert_eq!(five, six, "a 1.2x growth was taken");
    for _ in 0..squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES * 4 {
        assert_eq!(state.observe(pool, model, loops(5)), six);
    }

    // Four of six: 6/4 = 1.5x, past the band.
    let four = settle(&mut state, loops(4));
    assert!(
        four.share_bytes > six.share_bytes,
        "a 1.5x growth was refused",
    );

    // And a shrink straight back to six is taken with no band at all, because the pool is a
    // bound.
    let back = settle(&mut state, loops(6));
    assert_eq!(back, six, "a shrink was held off by the dead band");
}

/// `LoopDemand::add` classifies by view exhaustively, like everything else in this
/// workspace that switches on one.
#[test]
fn the_demand_counts_each_view_in_its_own_column() {
    let mut demand = LoopDemand::default();
    demand.add(RenderView::PlanView, false);
    demand.add(RenderView::CrossSection, false);
    demand.add(RenderView::Volume, false);
    // The fourth column is not a view at all: it is the pane whose loops are
    // all some layer other than radar's, which the three above cannot see.
    demand.add_overlay_pane();
    assert_eq!(
        demand,
        LoopDemand {
            plan_view_loops: 1,
            section_loops: 1,
            volume_sets: 1,
            overlay_loops: 1,
        },
    );
    assert_eq!(
        demand.shares(),
        4,
        "an overlay-only pane is a way the pool is split; before WB-7 it was \
         invisible here and was handed a share sized as though it were alone",
    );
    assert_eq!(LoopDemand::default().shares(), 0);
}

/// Nothing looping is not a division by zero, and it does not blank anything either — the
/// allocation a fresh application starts with is the one a single loop would get.
#[test]
fn an_empty_demand_is_the_single_loop_answer() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.floor, arm.limits);
        let empty = pool.plan(arm.model, LoopDemand::default());
        let one = pool.plan(
            arm.model,
            LoopDemand {
                plan_view_loops: 1,
                ..LoopDemand::default()
            },
        );
        assert_eq!(empty.share_bytes, pool.bytes(), "{}", arm.name);
        assert_eq!(empty.plan_view_frames, one.plan_view_frames, "{}", arm.name);
        // And no 3D reserve at all, which is what lets the caller floor the store's bound
        // at the live-grid figure instead of at zero.
        assert_eq!(empty.volume_reserve_bytes(), 0, "{}", arm.name);
    }
}

/// **What an overlay loop frame costs, pinned to its arithmetic and not to the
/// function that computes it.**
///
/// `LoopFrameModel::overlay` is the one price in the model that is not a fixed
/// side: it is the pane's own raster, so the figure is `plan_overlay_texture`
/// run on the default window (`RENDER_WIDTH` × `RENDER_HEIGHT` = 1920 × 1080
/// **physical pixels**) with **no class ceiling applied** — see
/// `nominal_overlay_frame_bytes` for why the overlay planner is bounded by the
/// adapter's own `max_texture_side` and never by `raster_side_ceiling_px`. The
/// whole `OVERDRAW_FRACTION` of 0.25 is afforded, so 1920 × 1.5 = 2880 and
/// 1080 × 1.5 = 1620 → 2880 × 1620 × 4 = **18,662,400 B**, on every arm.
///
/// **The figure no longer varies by arm, so pinning it per arm would be one
/// assertion written three times.** The two regressions it has to catch are
/// named as values instead, which is the whole point of the test:
///
/// * **A class ceiling put back.** Planning against the wasm arm's 2048 gives
///   2048 × 1152 × 4 = 9,437,184 B, and that was the defect: a 1.98×
///   under-price on every browser whose adapter clears 2880 px, which is every
///   browser on a real driver. The wrong answer is asserted against by name
///   rather than left to be unreachable.
/// * **An overlay priced as radar.** Before WB-7 an overlay frame was priced as
///   a radar plan-view frame — `Budgets::loop_frame_bytes()` — and every
///   consumer of the overlay arm reads it back off the model, so a suite that
///   only compared the model with itself would pass with the two identical. The
///   figures are 4 MiB on wasm and 16 MiB on both native arms against
///   18.66 MB, and they must stay different numbers.
///
/// **Floor — `price_the_overlay_as_radar`: make `LoopFrameModel::from_budgets`
/// and `for_target` answer `budgets.loop_frame_bytes()`.** Both blocks red on
/// all three arms. Applied and observed — and the whole rest of the suite
/// stayed green under it, which is why this test exists.
#[test]
fn an_overlay_frame_is_priced_by_the_planner_and_is_not_a_radar_frame() {
    use crate::loop_pool::nominal_overlay_frame_bytes;

    assert_eq!(
        nominal_overlay_frame_bytes(),
        2880 * 1620 * 4,
        "1920x1080 physical pixels at the full 0.25 overdraw is 2880x1620",
    );
    assert_ne!(
        nominal_overlay_frame_bytes(),
        2048 * 1152 * 4,
        "an overlay frame is being planned against a 2048 texture limit again. \
         WebGL2's 2048 is a guarantee the adapter accepts at least that, not a \
         cap that it accepts at most that, and `device_limits` copies the \
         adapter's own resolution verbatim — so this under-prices by 1.98x \
         every browser whose adapter clears 2880 px",
    );

    for arm in arms() {
        assert_ne!(
            arm.model.overlay, arm.model.plan_view,
            "{}: an overlay loop frame is priced as a radar plan-view frame \
             ({} B). That is what the fallback did before WB-7, and it is the \
             one substitution every other assertion in this crate survives, \
             because they all read the price back off this same field.",
            arm.name, arm.model.plan_view,
        );
    }
}

/// The compiled target's model is one of the three the table above names.
#[test]
fn the_compiled_model_is_one_of_the_named_arms() {
    let compiled = LoopFrameModel::for_target();
    assert!(
        arms().iter().any(|arm| arm.model == compiled),
        "LoopFrameModel::for_target() is {compiled:?}, which is none of the \
         three arms",
    );
    let limits = LoopPoolLimits::for_target();
    assert!(arms().iter().any(|arm| arm.limits == limits));
    assert!(limits.floor <= limits.ceiling);
}

/// The budget-agreement proofs that bridge to this module's planner — moved here from the
/// floor crate's `constants::tests` at WO-RD.
mod budget_agreement {
    use crate::budget_arms::{SHIPPED_VOLUME_LOOP_FRAMES, arms, volume_bytes};

    /// The store's bound is what the sum above charges, on the arithmetic the application
    /// actually runs rather than on the reading of it.
    #[test]
    fn the_volume_store_floor_is_the_widest_the_override_can_open() {
        use crate::loop_pool::{LoopDemand, LoopFrameModel, LoopPool, LoopPoolLimits};

        for arm in arms() {
            let model = LoopFrameModel {
                plan_view: arm.loop_frame_bytes(),
                section: arm.section_frame_bytes(),
                grid: volume_bytes(&arm),
                overlay: crate::loop_pool::nominal_overlay_frame_bytes(),
                render_budget: arm.loop_render_budget,
            };
            let limits = LoopPoolLimits {
                floor: arm.loop_pool_floor_bytes,
                ceiling: arm.loop_pool_ceiling_bytes,
            };
            for pool_bytes in [arm.loop_pool_floor_bytes, arm.loop_pool_ceiling_bytes] {
                let pool = LoopPool::new(pool_bytes, limits);
                for plan_view_loops in 0..=arm.max_panes {
                    for section_loops in 0..=(arm.max_panes - plan_view_loops) {
                        for volume_sets in 0..=(arm.max_panes - plan_view_loops - section_loops) {
                            let demand = LoopDemand {
                                plan_view_loops,
                                section_loops,
                                volume_sets,
                                ..LoopDemand::default()
                            };
                            let allocation = pool.plan(model, demand);
                            // What `setup_egui_frame` hands `enforce_budget`.
                            let store = allocation
                                .volume_reserve_bytes()
                                .max(arm.volume_loop_bytes());
                            // What the raster loops may cache beside it.
                            let raster =
                                plan_view_loops * allocation.plan_view_frames * model.plan_view
                                    + section_loops * allocation.section_frames * model.section;
                            assert!(
                                raster + store
                                    <= arm.loop_pool_ceiling_bytes + arm.volume_loop_bytes(),
                                "{}: {demand:?} at a {} MiB pool caches {} MiB of raster \
                                 frames beside a {} MiB store bound — over the \
                                 `pool ceiling + volume-store floor` the app ceiling \
                                 charges",
                                arm.name,
                                pool.bytes() / (1024 * 1024),
                                raster / (1024 * 1024),
                                store / (1024 * 1024),
                            );
                        }
                    }
                }
            }
        }
    }

    /// The shipped 3D loop budget per target: frame count, one grid, the resident
    /// set it makes, and the headroom left inside the share.
    #[test]
    fn the_loop_budget_is_what_the_constants_derive() {
        const MIB: f64 = 1024.0 * 1024.0;
        // target, frames, one 3D texture, resident set, headroom, share (MiB).
        let shipped = [
            ("wasm32", 11usize, "4.598", "50.57", "5.43", 56usize),
            ("mobile", 17, "15.550", "264.35", "23.65", 288),
            ("desktop", 14, "36.598", "512.37", "63.63", 576),
        ];

        for ((arm, frames), row) in arms()
            .into_iter()
            .zip(SHIPPED_VOLUME_LOOP_FRAMES)
            .zip(shipped)
        {
            let (name, want_frames, texture, resident_mib, headroom, share) = row;
            assert_eq!(arm.name, name, "the arms are out of order");
            assert_eq!(frames, want_frames, "{name}: shipped 3D loop frame count");

            let grid = volume_bytes(&arm);
            let resident = grid * frames;
            assert_eq!(
                format!("{:.3}", grid as f64 / MIB),
                texture,
                "{name}: one 3D texture",
            );
            assert_eq!(
                format!("{:.2}", resident as f64 / MIB),
                resident_mib,
                "{name}: the resident set a full loop holds",
            );
            assert_eq!(
                format!("{:.2}", (arm.volume_loop_bytes() - resident) as f64 / MIB),
                headroom,
                "{name}: headroom left inside the share",
            );
            assert_eq!(
                arm.volume_loop_bytes() / (1024 * 1024),
                share,
                "{name}: the share itself",
            );
        }
    }

    use crate::budget_arms::shipped_profile;
    use crate::loop_pool::{GRID_BYTES, LoopFrameModel};
    use squallar_device_profile::budget::{
        AdapterCeilings, BudgetLimits, Budgets, DESKTOP_CLASS_REPORT, DeviceProfile, FormFactor,
        Promotion, resolve,
    };
    use squallar_device_profile::fit::{loop_room, need_terms};
    use squallar_device_profile::quality::DeviceClass;
    use squallar_device_profile::scene::Capacity;

    /// **A 3D pane's need is priced by the raymarch's own grid arithmetic** —
    /// the floor crate takes the function in rather than re-deriving it, and
    /// the one it is handed is the one [`LoopFrameModel::from_budgets`] reads,
    /// so a grid cannot be priced two ways. Every mip level as the backend
    /// lays it out, the colour table's texture and the jitter tile, on every
    /// arm.
    #[test]
    fn a_3d_panes_need_is_priced_by_the_raymarchs_own_grid_arithmetic() {
        use squallar_device_profile::scene::{PaneNeed, Scene};

        for arm in arms() {
            let grid = volume_bytes(&arm);
            assert_eq!(
                GRID_BYTES(arm.grid_cells),
                Some(grid),
                "{}: the pool's pricer and the raymarch's are not one function",
                arm.name,
            );
            assert_eq!(
                LoopFrameModel::from_budgets(&arm).grid,
                grid,
                "{}",
                arm.name
            );
            let scene = Scene {
                panes: vec![PaneNeed {
                    px: [1920, 1080],
                    view: squallar_radar::types::RenderView::Volume,
                    looping: true,
                    loop_span_secs: 2 * 60 * 60,
                    cadence_secs: None,
                    overlay_frame_bytes: 0,
                    volume_grids: 1,
                    ground: squallar_device_profile::quality::GroundPass::Off,
                    buildings: false,
                }],
                tile_sources: Vec::new(),
                mirror_px: [0, 0],
            };
            let terms = need_terms(&scene, &arm, GRID_BYTES);
            assert_eq!(terms.grids, grid as u64, "{}: the live grid", arm.name);
            assert_eq!(
                terms.loops,
                (arm.loop_render_budget * grid) as u64,
                "{}: a 3D loop's frames are grids at the raymarch's price",
                arm.name,
            );
            assert_eq!(
                terms.static_rasters, 0,
                "{}: a 3D pane's picture is its offscreen",
                arm.name
            );
        }
    }

    /// **The promotion no longer moves the pool for the same scene** — the
    /// `LoopPool` half of the floor crate's
    /// `a_desktop_class_browser_is_promoted_and_a_spec_floor_browser_is_not`,
    /// re-argued: one loop on a promoted browser is the same 14 x 4 MiB =
    /// 56 MiB it is on a browser at the guarantee. What the promotion buys is
    /// resolution — the grid's cells and the raster's side — and what the
    /// wider raster costs is room: 288 - 64 MiB against 288 - 16.
    #[test]
    fn a_promoted_browser_holds_the_same_loop_in_the_same_pool() {
        let web = |two_d: u32, three_d: u32| DeviceProfile {
            adapter: AdapterCeilings {
                max_texture_dimension_2d: two_d,
                max_texture_dimension_3d: three_d,
            },
            // The ceiling wants the desktop form factor too; the rig's legs run with a
            // mouse, and the step a shape-less browser takes is the same numbers.
            form_factor: Some(FormFactor::Desktop),
            ..shipped_profile(BudgetLimits::WASM)
        };
        let floor = resolve(&web(
            AdapterCeilings::WEBGL2_GUARANTEE.max_texture_dimension_2d,
            AdapterCeilings::WEBGL2_GUARANTEE.max_texture_dimension_3d,
        ));
        let promoted = resolve(&web(
            DESKTOP_CLASS_REPORT.max_texture_dimension_2d,
            DESKTOP_CLASS_REPORT.max_texture_dimension_3d,
        ));
        assert_eq!(floor.promotion, Promotion::Floor);
        assert_eq!(promoted.promotion, Promotion::Ceiling);
        let cap = Capacity::presumed(&BudgetLimits::WASM);
        let one = super::scene_of(1);
        let pool = |b: &Budgets| {
            crate::loop_pool::LoopPool::for_scene(
                &one,
                b,
                &cap,
                crate::loop_pool::LoopPoolLimits::from_budgets(b),
            )
            .bytes()
        };
        assert_eq!(
            pool(&promoted),
            pool(&floor),
            "a promotion moved the pool for one and the same loop",
        );
        assert_eq!(pool(&floor), 14 * 4 * 1024 * 1024);
        let cells = |b: &Budgets| b.grid_cells.iter().map(|&n| n as usize).product::<usize>();
        assert!(
            cells(&promoted) > cells(&floor),
            "the promotion buys nothing at all, so this test compares an arm with itself",
        );
        assert_eq!(
            loop_room(&one, &floor, &cap, GRID_BYTES),
            (288 - 16) * 1024 * 1024,
            "a 2048 px static render beside the loop",
        );
        assert_eq!(
            loop_room(&one, &promoted, &cap, GRID_BYTES),
            (288 - 64) * 1024 * 1024,
            "a 4096 px static render beside the loop: the promotion costs room",
        );
    }

    /// **The pool ceiling is a presumption and does not bind a measured card;
    /// the floor holds on both arms.** Six two-hour loops need 3456 MiB; under
    /// the 3840 MiB presumption the room holds them to 2304, and under the
    /// 3090's 24822 MiB they get every byte — past the 3072 MiB the desktop
    /// bracket would spend on loops when nothing had measured the card. An
    /// empty scene sits at the 576 MiB floor on either arm.
    #[test]
    fn the_pool_ceiling_is_a_presumption_and_does_not_bind_a_measured_card() {
        use crate::loop_pool::{LoopPool, LoopPoolLimits};
        use squallar_device_profile::constants::DESKTOP_LOOP_POOL_CEILING_BYTES;
        use squallar_device_profile::fit::fit;

        let rtx_3090 = DeviceProfile {
            class: DeviceClass::Discrete,
            vram_bytes: Some(24822 << 20),
            system_ram_bytes: Some(64 << 30),
            ..shipped_profile(BudgetLimits::DESKTOP)
        };
        let measured = rtx_3090.capacity();
        let presumed = Capacity::presumed(&BudgetLimits::DESKTOP);
        let six = super::scene_of(6);
        let b = fit(&six, &rtx_3090, &measured, GRID_BYTES);
        assert_eq!(
            b,
            resolve(&rtx_3090),
            "six loops fit the card at the class rung"
        );
        let limits = LoopPoolLimits::from_budgets(&b);
        assert_eq!(limits.ceiling, DESKTOP_LOOP_POOL_CEILING_BYTES);
        assert_eq!(
            limits.on(&presumed),
            limits,
            "the presumed arm keeps its ceiling"
        );
        assert_eq!(
            limits.on(&measured),
            LoopPoolLimits {
                floor: limits.floor,
                ceiling: usize::MAX,
            },
        );

        let on_the_card = LoopPool::for_scene(&six, &b, &measured, limits).bytes();
        assert_eq!(on_the_card, 3456 << 20, "what six two-hour loops need");
        assert!(
            on_the_card > DESKTOP_LOOP_POOL_CEILING_BYTES,
            "the bracket's 3072 MiB loop ceiling bound a measured 3090",
        );
        assert_eq!(
            LoopPool::for_scene(&six, &b, &presumed, limits).bytes(),
            2304 << 20,
            "min(3456, 3840 - 6 x 256) under the presumption",
        );
        let empty = super::scene_of(0);
        for cap in [&measured, &presumed] {
            assert_eq!(
                LoopPool::for_scene(&empty, &b, cap, limits).bytes(),
                limits.floor,
                "{:?}: nothing looping sits at the floor",
                cap.source,
            );
        }
    }

    /// The stage's whole answer on one page, pinned so that a change to any rule has to
    /// come past a table someone can read.
    ///
    /// **The pool column stopped moving with the rung.** It is what one
    /// two-hour loop needs on the bracket — 36 x 16 MiB = 576 MiB natively,
    /// 14 x 4 MiB = 56 MiB on the web — whatever the machine, which is the
    /// ruling as a column; the 3072 / 1152 / 192 a class used to be handed
    /// were capacity presumptions wearing the pool's name. What does move with
    /// the rung is the **room** column beside it: the allowance less the static
    /// renders, which the promoted raster side makes smaller.
    ///
    /// **The capacity column is what a reading buys, and it is room.** The box's
    /// RTX 3090 reads 24822 MiB, so need may take 18616.5 MiB of it: six
    /// two-hour loops beside their renders cost 4992 MiB and fit with every
    /// frame, the pool is the 3456 MiB they need — past the 3072 MiB bracket
    /// ceiling, which is a presumption — and 17080.5 MiB of room is left. A
    /// 4 GiB card allows 3072 MiB and the same six panes shed two halvings to
    /// 9 frames each: 6 x (9 x 16 + 256) = 2400 MiB, an 864 MiB pool, 1536 MiB
    /// of room, and 8 x 259 s = thirty-four minutes of the two hours asked for.
    /// An integrated desktop GPU on a 64 GiB host is unified memory, 32 GiB
    /// measured: its pool is still one loop's need, and its offscreen still the
    /// Step the class earns, because memory says nothing about fill rate. The
    /// same 3090 over GL is `Other` to wgpu; with the host's RAM read it too is
    /// unified memory at the desktop-class line, and with nothing read it is
    /// the presumption. A browser is the presumption whatever it reads.
    #[test]
    fn what_five_real_machines_get() {
        use squallar_device_profile::fit::fit;
        use squallar_device_profile::scene::CapacitySource;

        let row = |limits, class, two_d, three_d, vram: Option<u64>, ram, panes| {
            let profile = DeviceProfile {
                class,
                adapter: AdapterCeilings {
                    max_texture_dimension_2d: two_d,
                    max_texture_dimension_3d: three_d,
                },
                vram_bytes: vram,
                system_ram_bytes: ram,
                // Every machine in this table has a mouse: a build fact natively, pointer
                // media on the web. The ceiling asks for it since the form factor is read.
                form_factor: Some(FormFactor::Desktop),
                ..shipped_profile(limits)
            };
            let cap = profile.capacity();
            let scene = super::scene_of(panes);
            let b = fit(&scene, &profile, &cap, GRID_BYTES);
            let pool = crate::loop_pool::LoopPool::for_scene(
                &scene,
                &b,
                &cap,
                crate::loop_pool::LoopPoolLimits::from_budgets(&b),
            )
            .bytes();
            (
                b.promotion,
                b.grid_cells.iter().map(|&n| n as usize).product::<usize>(),
                b.offscreen_bytes / (1024 * 1024),
                pool / (1024 * 1024),
                loop_room(&scene, &b, &cap, GRID_BYTES) / (1024 * 1024),
                b.raster_side_for_adapter(two_d),
                cap.gpu_bytes / (1024 * 1024),
                cap.source,
                b.loop_render_budget,
            )
        };
        let d = BudgetLimits::DESKTOP;
        let w = BudgetLimits::WASM;
        const RTX_3090_MIB: u64 = 24822;
        let ram_64 = Some(64u64 << 30);
        use CapacitySource::{Measured, Presumed};

        // machine | rung | cells | offscreen | pool | room | raster | cap | source | frames
        assert_eq!(
            row(d, DeviceClass::Discrete, 32768, 16384, None, None, 1),
            (
                Promotion::Ceiling,
                8_388_608,
                48,
                576,
                3840 - 256,
                8192,
                3840,
                Presumed,
                36
            ),
            "RTX 3090 over Vulkan before its reader has answered",
        );
        assert_eq!(
            row(
                d,
                DeviceClass::Discrete,
                32768,
                16384,
                Some(RTX_3090_MIB << 20),
                ram_64,
                6,
            ),
            (
                Promotion::Ceiling,
                8_388_608,
                48,
                3456,
                17080,
                8192,
                RTX_3090_MIB,
                Measured,
                36,
            ),
            "RTX 3090 over Vulkan, measured: six two-hour loops at the full render \
             budget, 3456 MiB of pool, 18616.5 - 1536 = 17080.5 MiB of room",
        );
        assert_eq!(
            row(
                d,
                DeviceClass::Discrete,
                32768,
                16384,
                Some(4 << 30),
                Some(16 << 30),
                6
            ),
            (
                Promotion::Ceiling,
                8_388_608,
                20,
                864,
                1536,
                8192,
                4096,
                Measured,
                9
            ),
            "a 4 GiB discrete card: the same six panes hold nine frames each, thirty-four \
             minutes at the precipitation cadence; the offscreen is at its floor because \
             the two resolution rungs were walked on the way to the loop history",
        );
        assert_eq!(
            row(d, DeviceClass::Unknown, 32768, 16384, None, None, 1),
            (
                Promotion::Ceiling,
                8_388_608,
                48,
                576,
                3840 - 256,
                8192,
                3840,
                Presumed,
                36
            ),
            "the same RTX 3090 over GL, where the driver names it `Other` — the \
             case a class-only rule gets wrong on real hardware — with nothing read",
        );
        assert_eq!(
            row(d, DeviceClass::Unknown, 32768, 16384, None, ram_64, 1),
            (
                Promotion::Ceiling,
                8_388_608,
                48,
                576,
                24576 - 256,
                8192,
                32768,
                Measured,
                36
            ),
            "the 3090 over GL with the host's 64 GiB read: an unclassed adapter at the \
             desktop-class line is believed as unified memory, half the RAM",
        );
        assert_eq!(
            row(d, DeviceClass::Integrated, 16384, 8192, None, ram_64, 1),
            (
                Promotion::Step,
                8_388_608,
                20,
                576,
                24576 - 256,
                8192,
                32768,
                Measured,
                36
            ),
            "a desktop integrated GPU on a 64 GiB host: 32 GiB of unified memory measured, \
             the pool by need, and the offscreen at the Step — what holds it back is fill \
             rate, which no memory figure speaks to",
        );
        // **The raster column moved from 2048 to 4096 at WS1**, on the four-leg
        // adapter measurement recorded at
        // `constants::WASM_RASTER_SIDE_CEILING_PROMOTED`; the room column is
        // what that costs. The row below it — a browser at the WebGL2
        // guarantee — is unmoved, which is the half that says the software
        // path did not come with it.
        assert_eq!(
            row(w, DeviceClass::Unknown, 16384, 16384, None, None, 1),
            (
                Promotion::Ceiling,
                3_538_944,
                5,
                56,
                288 - 64,
                4096,
                288,
                Presumed,
                14
            ),
            "Firefox 153 on the RTX 3090, at what it will actually allocate",
        );
        assert_eq!(
            row(
                w,
                DeviceClass::Unknown,
                16384,
                16384,
                Some(24 << 30),
                ram_64,
                1
            ),
            (
                Promotion::Ceiling,
                3_538_944,
                5,
                56,
                288 - 64,
                4096,
                288,
                Presumed,
                14
            ),
            "the same browser handed a 24 GiB reading and 64 GiB of RAM: nothing a page \
             reports is a measurement, so the presumption stands",
        );
        assert_eq!(
            row(w, DeviceClass::Unknown, 2048, 256, None, None, 1),
            (
                Promotion::Floor,
                1_048_576,
                5,
                56,
                288 - 16,
                2048,
                288,
                Presumed,
                14
            ),
            "a browser at the WebGL2 guarantee, which keeps every byte it had",
        );
    }
}
