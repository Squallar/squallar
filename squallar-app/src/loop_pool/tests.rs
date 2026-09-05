use super::*;
use squallar_device_profile::constants::{
    DESKTOP_LOOP_IMAGE_SIZE, DESKTOP_LOOP_POOL_CEILING_BYTES, DESKTOP_LOOP_POOL_FLOOR_BYTES,
    DESKTOP_MAX_LOOP_FRAMES, DESKTOP_MAX_LOOP_RENDER_BUDGET, DESKTOP_VOLUME_GRID_CELLS,
    MOBILE_LOOP_IMAGE_SIZE, MOBILE_LOOP_POOL_CEILING_BYTES, MOBILE_LOOP_POOL_FLOOR_BYTES,
    MOBILE_MAX_LOOP_FRAMES, MOBILE_MAX_LOOP_RENDER_BUDGET, MOBILE_VOLUME_GRID_CELLS,
    WASM_LOOP_IMAGE_SIZE, WASM_LOOP_POOL_CEILING_BYTES, WASM_LOOP_POOL_FLOOR_BYTES,
    WASM_MAX_LOOP_FRAMES, WASM_MAX_LOOP_RENDER_BUDGET, WASM_VOLUME_GRID_CELLS,
};
use squallar_device_profile::quality::DeviceClass;
use squallar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

const MIB: usize = 1024 * 1024;

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
    list_cap: usize,
) -> LoopFrameModel {
    LoopFrameModel {
        plan_view: loop_image_size * loop_image_size * 4,
        section: section_width * (section_width / 2) * 4,
        grid: squallar_volumetric::raymarch::resident_grid_bytes(grid)
            .expect("a shipped grid shape"),
        overlay: crate::loop_pool::nominal_overlay_frame_bytes(),
        render_budget,
        list_cap,
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
                WASM_MAX_LOOP_FRAMES,
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
                MOBILE_MAX_LOOP_FRAMES,
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
                DESKTOP_MAX_LOOP_FRAMES,
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

/// The desktop model alone, for the pinned figures.
fn desktop() -> LoopFrameModel {
    arms()[2].model
}

fn desktop_limits() -> LoopPoolLimits {
    arms()[2].limits
}

/// A loop of `kind` on `pane` whose listing has not landed: no cadence, so its base is
/// the render budget and it can hold no more than that — what every loop asks for before
/// its listing says otherwise, and the demand every arm's shipped figures were derived
/// with.
fn bare(pane: usize, kind: LoopKind, model: &LoopFrameModel) -> LoopNeed {
    LoopNeed {
        key: LoopKey { pane },
        kind,
        span_secs: 2 * 60 * 60,
        cadence_secs: None,
        frame_bytes: model.price(kind),
        base_frames: model.render_budget,
        max_frames: model.render_budget,
    }
}

/// A radar plan-view loop on `pane` over a **six-hour lookback at a 300 s cadence** on
/// the desktop bracket — the shape every pinned figure below is stated for. The lookback
/// lists 73 scans; the base is 25 (two hours, the rung's span, at 300 s, as
/// `Budgets::frames_for_span_of` answers it); the ceiling is `min(73, MAX_LOOP_FRAMES)`.
fn six_hours(pane: usize) -> LoopNeed {
    let span_secs = 6 * 60 * 60;
    LoopNeed {
        key: LoopKey { pane },
        kind: LoopKind::PlanView,
        span_secs,
        cadence_secs: Some(300),
        frame_bytes: desktop().plan_view,
        base_frames: 25,
        max_frames: loop_ceiling_frames(
            Some(73),
            span_secs,
            Some(300),
            25,
            DESKTOP_MAX_LOOP_FRAMES,
        ),
    }
}

fn demand_of(needs: impl IntoIterator<Item = LoopNeed>) -> LoopDemand {
    let mut demand = LoopDemand::default();
    for need in needs {
        demand.push(need);
    }
    demand
}

/// `n` of `six_hours`, on panes `0..n`.
fn six_hours_on(n: usize) -> LoopDemand {
    demand_of((0..n).map(six_hours))
}

fn frames_of(allocation: &LoopAllocation, pane: usize) -> usize {
    allocation
        .frames_for_pane(pane)
        .unwrap_or_else(|| panic!("pane {pane} asked for a loop and got no grant"))
}

/// Every mix of loop kinds that fits on `max_panes` panes, plus the empty one — each loop
/// bare, on its own pane.
fn reachable_demands(max_panes: usize, model: &LoopFrameModel) -> Vec<LoopDemand> {
    let mut out = Vec::new();
    for plan_view_loops in 0..=max_panes {
        for section_loops in 0..=(max_panes - plan_view_loops) {
            for volume_sets in 0..=(max_panes - plan_view_loops - section_loops) {
                let mut demand = LoopDemand::default();
                let mut pane = 0;
                for (kind, n) in [
                    (LoopKind::PlanView, plan_view_loops),
                    (LoopKind::CrossSection, section_loops),
                    (LoopKind::Volume, volume_sets),
                ] {
                    for _ in 0..n {
                        demand.push(bare(pane, kind, model));
                        pane += 1;
                    }
                }
                out.push(demand);
            }
        }
    }
    out
}

/// The claim the whole change exists to make, and the one nothing could have made before:
/// `MAX_PANES × LOOP_TEXTURE_BUDGET_BYTES` was 3.0 GiB on desktop and 1.0 GiB on a phone,
/// and no test put those two halves side by side because they lived in different crates.
/// Every grant together charges no more than the pool — except where the two-frame floor
/// had to win, which is stated rather than absorbed.
#[test]
fn the_pool_actually_bounds_the_sum() {
    for arm in arms() {
        for bytes in [arm.limits.floor, arm.limits.ceiling] {
            let pool = LoopPool::new(bytes, arm.limits);
            for demand in reachable_demands(arm.max_panes, &arm.model) {
                let allocation = pool.plan(arm.model, &demand);
                let at_floor = allocation
                    .grants()
                    .iter()
                    .any(|g| g.frames == MIN_LOOP_FRAMES_PER_PANE);
                assert!(
                    allocation.bytes() <= pool.bytes() || at_floor,
                    "{}: {} loops at a {} MiB pool charge {} MiB with no loop at the \
                     minimum — the pool does not bound the sum",
                    arm.name,
                    demand.shares(),
                    pool.bytes() / MIB,
                    allocation.bytes() / MIB,
                );
                // The 3D kind's bound is the reserve the store is held to, and it is
                // part of the same sum.
                assert!(
                    allocation.volume_reserve_bytes() <= allocation.bytes(),
                    "{}: the 3D reserve is not part of what the plan charged",
                    arm.name,
                );
                // Nothing is granted past what exists to show.
                for (grant, need) in allocation.grants().iter().zip(demand.needs()) {
                    assert!(
                        grant.frames <= need.max_frames.max(MIN_LOOP_FRAMES_PER_PANE),
                        "{}: pane {} was granted {} frames of a listing that holds {}",
                        arm.name,
                        grant.key.pane,
                        grant.frames,
                        need.max_frames,
                    );
                    assert!(grant.frames <= arm.model.list_cap.max(MIN_LOOP_FRAMES_PER_PANE));
                }
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
            let demand =
                demand_of((0..loops).map(|pane| bare(pane, LoopKind::PlanView, &arm.model)));
            frames_of(&pool.plan(arm.model, &demand), 0)
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
        // And the recovery is the same plan read backwards, which is what makes closing a
        // pane give the length back rather than needing a separate rule that could
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
            &demand_of([bare(0, LoopKind::Volume, &arm.model)]),
        );
        // Two panes on one volume: the second is an alias of the first, so the demand is
        // one loop and so is the plan — and both panes read the one grant.
        let mut shared = demand_of([bare(0, LoopKind::Volume, &arm.model)]);
        shared.alias(1, 0);
        assert_eq!(
            shared.shares(),
            1,
            "{}: a second pane on the same volume was counted as a second set",
            arm.name,
        );
        let planned = pool.plan(arm.model, &shared);
        assert_eq!(
            planned.grants(),
            one_set.grants(),
            "{}: two panes on one volume were charged two loops",
            arm.name,
        );
        assert_eq!(
            planned.frames_for_pane(1),
            planned.frames_for_pane(0),
            "{}: the aliased pane does not read its owner's grant",
            arm.name,
        );
        assert_eq!(
            planned.volume_reserve_bytes(),
            one_set.volume_reserve_bytes()
        );

        // Two panes on two volumes really are two sets, and really do divide.
        let distinct = demand_of([
            bare(0, LoopKind::Volume, &arm.model),
            bare(1, LoopKind::Volume, &arm.model),
        ]);
        assert_eq!(distinct.count(LoopKind::Volume), 2, "{}", arm.name);
        let two_sets = pool.plan(arm.model, &distinct);
        assert!(
            frames_of(&two_sets, 0) < frames_of(&one_set, 0),
            "{}: two distinct 3D loops were not divided",
            arm.name,
        );
        // And the store is held to the sum across sets, not to one of them —
        // `enforce_budget` evicts oldest-first over one store, so a bound naming only one
        // set would evict the older loop's frames for ever.
        assert_eq!(
            two_sets.volume_reserve_bytes(),
            two_sets
                .grants()
                .iter()
                .map(LoopGrant::bytes)
                .sum::<usize>(),
            "{}",
            arm.name,
        );
    }
}

/// A single 3D loop at the floor holds exactly the count this target ships: the floor
/// less one live grid, in grids.
#[test]
fn the_pool_reproduces_the_shipped_3d_frame_count() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.floor, arm.limits);
        let allocation = pool.plan(
            arm.model,
            &demand_of([bare(0, LoopKind::Volume, &arm.model)]),
        );
        assert_eq!(
            frames_of(&allocation, 0),
            arm.volume_loop_frames,
            "{}: the pool gives a single 3D loop {} grids where this target ships {}",
            arm.name,
            frames_of(&allocation, 0),
            arm.volume_loop_frames,
        );
        assert_eq!(
            allocation.volume_frames, arm.volume_loop_frames,
            "{}",
            arm.name
        );
    }
}

/// A full 3D loop leaves room for a live grid at every pool size: the reserve the store is
/// held to is the loop's grids **and** the live grid beside them, which is what every 3D
/// loop is charged.
#[test]
fn a_full_3d_loop_leaves_room_for_a_live_grid_at_every_pool_size() {
    for arm in arms() {
        let mut bytes = arm.limits.ceiling;
        while bytes >= arm.limits.floor {
            let pool = LoopPool::new(bytes, arm.limits);
            for sets in 1..=arm.max_panes {
                let demand =
                    demand_of((0..sets).map(|pane| bare(pane, LoopKind::Volume, &arm.model)));
                let allocation = pool.plan(arm.model, &demand);
                let resident: usize = allocation
                    .grants()
                    .iter()
                    .map(|g| g.frames * arm.model.grid)
                    .sum();
                assert!(
                    resident + sets * arm.model.grid <= allocation.volume_reserve_bytes(),
                    "{}: {sets} loop(s) at a {} MiB pool leave no room for their live \
                     grids, so the store evicts a loop's own oldest frame and rebuilds it \
                     for ever",
                    arm.name,
                    bytes / MIB,
                );
                // And the charge is inside the pool, except where the two-frame floor won.
                assert!(
                    allocation.volume_reserve_bytes() <= pool.bytes()
                        || allocation
                            .grants()
                            .iter()
                            .any(|g| g.frames == MIN_LOOP_FRAMES_PER_PANE),
                    "{}: {sets} 3D loop(s) reserve {} MiB of a {} MiB pool with none at \
                     the minimum",
                    arm.name,
                    allocation.volume_reserve_bytes() / MIB,
                    bytes / MIB,
                );
            }
            if bytes == arm.limits.floor {
                break;
            }
            bytes = (bytes / 2).max(arm.limits.floor);
        }
    }
}

/// **Two loops of unequal cost and one lookback reach the same temporal resolution from
/// the same surplus** — not the same bytes. A plan-view frame is 16 MiB and a section
/// frame 8 MiB; over a 6 h lookback at 300 s both have a base of 25, and a 1200 MiB pool
/// pays both bases (600 MiB) and balloons both to **50 frames** (50 x 24 MiB = 1200 MiB
/// exactly). The equal-bytes split this replaces gave the section loop twice the frames.
#[test]
fn two_loops_of_unequal_cost_get_equal_temporal_resolution_from_the_same_surplus() {
    let model = desktop();
    let section = LoopNeed {
        key: LoopKey { pane: 1 },
        kind: LoopKind::CrossSection,
        frame_bytes: model.section,
        ..six_hours(1)
    };
    let demand = demand_of([six_hours(0), section]);
    let pool = LoopPool::new(1200 * MIB, desktop_limits());
    let allocation = pool.plan(model, &demand);
    assert_eq!(
        model.section * 2,
        model.plan_view,
        "a section frame is half a plan-view frame"
    );
    assert_eq!(frames_of(&allocation, 0), 50, "the 16 MiB loop");
    assert_eq!(frames_of(&allocation, 1), 50, "the 8 MiB loop");
    assert_eq!(
        allocation.bytes(),
        1200 * MIB,
        "the surplus is spent to the byte"
    );
    assert_eq!(
        allocation.balloon_bytes(),
        25 * model.plan_view + 25 * model.section
    );
    assert!(
        frames_of(&allocation, 1) != 2 * frames_of(&allocation, 0),
        "the equal-bytes split is back: the cheaper frame bought its loop twice the history",
    );
}

/// **Base first, balloon second.** With exactly the bases' bytes every loop holds its
/// base and no balloon exists; one more frame's worth goes to the loop whose frames stand
/// for the most seconds, and a loop already holding every scan it listed takes nothing.
#[test]
fn every_base_is_paid_before_the_first_balloon_frame() {
    let model = desktop();
    let long = six_hours(0);
    // One hour at 300 s: 13 scans listed, a base of 13, nothing to balloon into.
    let short = LoopNeed {
        key: LoopKey { pane: 1 },
        kind: LoopKind::PlanView,
        span_secs: 3600,
        cadence_secs: Some(300),
        frame_bytes: model.plan_view,
        base_frames: 13,
        max_frames: loop_ceiling_frames(Some(13), 3600, Some(300), 13, DESKTOP_MAX_LOOP_FRAMES),
    };
    let demand = demand_of([long, short]);
    let limits = LoopPoolLimits {
        floor: 0,
        ceiling: usize::MAX,
    };

    let bases = (25 + 13) * model.plan_view;
    let exact = LoopPool::new(bases, limits).plan(model, &demand);
    assert_eq!((frames_of(&exact, 0), frames_of(&exact, 1)), (25, 13));
    assert_eq!(
        exact.balloon_bytes(),
        0,
        "a pool of exactly the bases has no balloon"
    );

    let one_more = LoopPool::new(bases + model.plan_view, limits).plan(model, &demand);
    assert_eq!(
        (frames_of(&one_more, 0), frames_of(&one_more, 1)),
        (26, 13),
        "the one surplus frame went to the loop whose frames stand for the most seconds \
         (21600 / 25 = 864 s against 3600 / 13 = 277 s)",
    );
    assert_eq!(one_more.balloon_bytes(), model.plan_view);

    let ten_more = LoopPool::new(bases + 10 * model.plan_view, limits).plan(model, &demand);
    assert_eq!(
        (frames_of(&ten_more, 0), frames_of(&ten_more, 1)),
        (35, 13),
        "the short loop already holds every scan it listed and takes none of the surplus",
    );
}

/// **A lone plan-view pane at a six-hour lookback holds every scan it listed, up to the
/// list cap.** 73 scans at 300 s; the base is 25 (the rung's two-hour span); on the
/// measured arm with 17 GiB of room the pool's ceiling is retired and the loop holds
/// **60** — `MAX_LOOP_FRAMES` binds, not the pool: 73 exist and 60 is the most any loop
/// lists. On the presumed desktop arm the 3072 MiB ceiling pays min(60, 3072 / 16 = 192)
/// = **60** too. Room bought density inside the user's window, never a longer one.
#[test]
fn a_lone_pane_over_six_hours_holds_every_listed_scan_up_to_the_list_cap() {
    use squallar_device_profile::budget::BudgetLimits;
    use squallar_device_profile::scene::Capacity;

    let model = desktop();
    let demand = six_hours_on(1);
    assert_eq!(
        demand.needs()[0].max_frames,
        60,
        "73 listed, and MAX_LOOP_FRAMES is 60"
    );
    assert!(
        demand.needs()[0].max_frames < 73,
        "the list cap must bind here, or the test cannot say which bound holds",
    );

    let measured = Capacity::measured(24822 << 20, Some(64 << 30));
    let on_the_card = LoopPool::new(17 * 1024 * MIB, desktop_limits().on(&measured));
    assert_eq!(
        on_the_card.bytes(),
        17 * 1024 * MIB,
        "a measured card retires the ceiling"
    );
    let allocation = on_the_card.plan(model, &demand);
    assert_eq!(frames_of(&allocation, 0), 60);
    assert_eq!(
        allocation.balloon_bytes(),
        35 * model.plan_view,
        "60 - 25 frames of balloon"
    );

    let presumed = Capacity::presumed(&BudgetLimits::DESKTOP);
    let on_the_presumption = LoopPool::new(usize::MAX, desktop_limits().on(&presumed));
    assert_eq!(on_the_presumption.bytes(), DESKTOP_LOOP_POOL_CEILING_BYTES);
    assert_eq!(
        frames_of(&on_the_presumption.plan(model, &demand), 0),
        60,
        "min(60, 3072 MiB / 16 MiB = 192) is the list cap again",
    );
}

/// **What each arm of `CapacitySource` buys the loop pool** — the app-side half
/// of `squallar_device_profile::fit::tests::what_each_capacity_source_arm_buys`,
/// and the consumer the incident ran through.
///
/// The bracket ceiling is the only bound in this crate that is ever removed,
/// and `hold` is `clamp(floor, ceiling.max(floor))`, so an arm that reads
/// `usize::MAX` here has no upper bound at all. A reading removes it, a
/// presumption keeps it, and a **derived** figure keeps it — which is the
/// correction. On a Framework 13 with 86.2 GiB of RAM the derived figure was
/// wearing the measured arm's label, so a machine with no VRAM reader at all
/// had an unbounded loop pool.
#[test]
fn what_each_capacity_source_arm_buys_the_loop_pool() {
    use squallar_device_profile::scene::{Capacity, CapacitySource, Pools};

    let limits = desktop_limits();
    assert_eq!(limits.ceiling, DESKTOP_LOOP_POOL_CEILING_BYTES);
    assert!(limits.floor < limits.ceiling, "or the table says nothing");

    // source | the bracket's ceiling still binds
    for (source, bounded) in [
        (CapacitySource::Measured, false),
        (CapacitySource::Probed, false),
        (CapacitySource::Derived, true),
        (CapacitySource::Presumed, true),
    ] {
        let cap = Capacity {
            gpu_bytes: 43 << 30,
            host_bytes: Some(43 << 30),
            source,
            pools: Pools::Unified,
        };
        let on = limits.on(&cap);
        assert_eq!(
            on.floor, limits.floor,
            "{source:?}: the floor holds on every arm",
        );
        assert_eq!(on.ceiling == limits.ceiling, bounded, "{source:?}");
        assert_eq!(
            LoopPool::new(usize::MAX, on).bytes(),
            if bounded {
                DESKTOP_LOOP_POOL_CEILING_BYTES
            } else {
                usize::MAX
            },
            "{source:?}: what an unbounded ask resolves to",
        );
        // The floor still wins a pool below it, on every arm.
        assert_eq!(LoopPool::new(0, on).bytes(), limits.floor, "{source:?}");
    }
}

/// **The Framework 13's loop pool is bounded again.** End to end from the
/// profile that machine really carries — an integrated adapter, no VRAM
/// reader, 86.2 GiB of RAM — through the capacity, the fit and the pool: the
/// pool it is handed is the bracket's 3072 MiB and not the 32 GiB of allowance
/// its capacity names.
#[test]
fn the_framework_13s_loop_pool_is_held_to_the_bracket_ceiling() {
    use crate::budget_arms::shipped_profile;
    use squallar_device_profile::budget::{BudgetLimits, DeviceProfile, resolve};
    use squallar_device_profile::fit::fit;
    use squallar_device_profile::scene::CapacitySource;

    const RAM: u64 = 862 * (1 << 30) / 10;
    let profile = DeviceProfile {
        class: DeviceClass::Integrated,
        vram_bytes: None,
        system_ram_bytes: Some(RAM),
        host_pool_bytes: Some(80 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let cap = profile.capacity();
    assert_eq!(cap.source, CapacitySource::Derived);
    // 40 GiB of GPU share, 30 GiB of allowance: the figure is unchanged, and
    // what changed is that it no longer buys an unbounded pool.
    assert_eq!(cap.gpu_bytes, 40 << 30);
    assert_eq!(cap.allowance(), 30 << 30);

    let scene = scene_of(6);
    let budgets = fit(&scene, &profile, &cap, GRID_BYTES);
    assert_eq!(
        budgets,
        resolve(&profile),
        "the scene fits with room to spare"
    );
    let pool = LoopPool::for_scene(
        &scene,
        &budgets,
        &cap,
        LoopPoolLimits::from_budgets(&budgets),
    );
    assert_eq!(
        pool.bytes(),
        DESKTOP_LOOP_POOL_CEILING_BYTES,
        "the derived arm keeps the bracket ceiling",
    );
    assert!(
        (pool.bytes() as u64) < cap.allowance(),
        "the ceiling is what binds, not the allowance",
    );
}

/// **Six such panes share one budget by water-filling.** At 3072 MiB — 192 frames of
/// 16 MiB — the six bases (150 frames) fit and the 42 spare frames go seven apiece:
/// **32 each**, 6 x 32 x 16 MiB = 3072 MiB exactly. At the presumed arm's 2304 MiB of
/// room (3840 less six 256 MiB static renders) the bases do not fit, and every pane
/// shrinks to the same resolution: **24 each**, 144 frames.
#[test]
fn six_panes_over_six_hours_share_the_pool_by_water_filling() {
    let model = desktop();
    let demand = six_hours_on(6);

    let full = LoopPool::new(3072 * MIB, desktop_limits()).plan(model, &demand);
    for pane in 0..6 {
        assert_eq!(frames_of(&full, pane), 32, "pane {pane} at 3072 MiB");
    }
    assert_eq!(full.bytes(), 3072 * MIB);
    assert_eq!(full.balloon_bytes(), 6 * 7 * model.plan_view);

    let room = LoopPool::new(2304 * MIB, desktop_limits()).plan(model, &demand);
    for pane in 0..6 {
        assert_eq!(frames_of(&room, pane), 24, "pane {pane} at 2304 MiB");
    }
    assert_eq!(room.bytes(), 2304 * MIB);
    assert_eq!(
        room.balloon_bytes(),
        0,
        "below the bases there is no balloon"
    );
}

/// **A pane joining deflates balloons before any base is cut.** 800 MiB is 50 frames:
/// alone, a pane holds 25 of base and 25 of balloon; a second takes the whole balloon back
/// and both hold exactly their base; a third cannot be paid at base, and all three shrink
/// to within a frame of one another, none below two.
#[test]
fn a_pane_joining_deflates_balloons_before_any_base_is_cut() {
    let model = desktop();
    let pool = LoopPool::new(
        800 * MIB,
        LoopPoolLimits {
            floor: 0,
            ceiling: usize::MAX,
        },
    );

    let alone = pool.plan(model, &six_hours_on(1));
    assert_eq!(frames_of(&alone, 0), 50);
    assert_eq!(alone.balloon_bytes(), 25 * model.plan_view);

    let two = pool.plan(model, &six_hours_on(2));
    assert_eq!(
        (frames_of(&two, 0), frames_of(&two, 1)),
        (25, 25),
        "both at base"
    );
    assert_eq!(
        two.balloon_bytes(),
        0,
        "the joining pane took the whole balloon, no more"
    );

    let three = pool.plan(model, &six_hours_on(3));
    let frames: Vec<usize> = (0..3).map(|pane| frames_of(&three, pane)).collect();
    assert_eq!(
        frames.iter().sum::<usize>(),
        50,
        "the pool is spent: {frames:?}"
    );
    assert!(
        frames.iter().all(|f| *f < 25),
        "every base was cut: {frames:?}"
    );
    assert!(frames.iter().all(|f| *f >= MIN_LOOP_FRAMES_PER_PANE));
    assert!(
        frames.iter().max().unwrap() - frames.iter().min().unwrap() <= 1,
        "the cut was not to one resolution: {frames:?}",
    );
    assert_eq!(three.balloon_bytes(), 0);
}

/// When the bases do not fit, every loop shrinks to the same resolution and none below
/// two — and a pool too small for two frames apiece is exceeded, in the open, rather than
/// cutting a loop to a still picture.
#[test]
fn when_the_bases_do_not_fit_every_loop_shrinks_to_one_resolution_none_below_two() {
    let model = desktop();
    let limits = LoopPoolLimits {
        floor: 0,
        ceiling: usize::MAX,
    };
    let loop_of = |pane| LoopNeed {
        key: LoopKey { pane },
        kind: LoopKind::PlanView,
        span_secs: 3600,
        cadence_secs: Some(180),
        frame_bytes: model.plan_view,
        base_frames: 20,
        max_frames: 21,
    };
    let demand = demand_of([loop_of(0), loop_of(1)]);

    let six_frames = LoopPool::new(6 * model.plan_view, limits).plan(model, &demand);
    assert_eq!(
        (frames_of(&six_frames, 0), frames_of(&six_frames, 1)),
        (3, 3)
    );
    assert_eq!(six_frames.bytes(), 6 * model.plan_view);

    let two_frames = LoopPool::new(2 * model.plan_view, limits).plan(model, &demand);
    assert_eq!(
        (frames_of(&two_frames, 0), frames_of(&two_frames, 1)),
        (2, 2),
        "the floor wins over the byte bound",
    );
    assert!(
        two_frames.bytes() > two_frames.pool_bytes(),
        "and the excess is stated: {} B charged against a {} B pool",
        two_frames.bytes(),
        two_frames.pool_bytes(),
    );
}

/// **A listing caps inflation.** Ten scans listed is ten frames, however much room there
/// is and however large the base `fit` charged for the lookback.
#[test]
fn max_frames_from_the_listing_caps_inflation() {
    let model = desktop();
    let thin = LoopNeed {
        max_frames: loop_ceiling_frames(Some(10), 6 * 3600, Some(300), 25, DESKTOP_MAX_LOOP_FRAMES),
        ..six_hours(0)
    };
    assert_eq!(thin.max_frames, 10);
    let pool = LoopPool::new(3072 * MIB, desktop_limits());
    let allocation = pool.plan(model, &demand_of([thin]));
    assert_eq!(
        frames_of(&allocation, 0),
        10,
        "a loop cannot hold scans that do not exist"
    );
    assert_eq!(
        allocation.balloon_bytes(),
        0,
        "a list shorter than the base is no balloon"
    );
    assert!(allocation.bytes() < pool.bytes());
}

/// The same pool, model and demand plan the same grants, and the order of the panes is the
/// only tie-break.
#[test]
fn the_same_inputs_plan_the_same_grants() {
    let model = desktop();
    let pool = LoopPool::new(1000 * MIB, desktop_limits());
    let demand = demand_of([
        six_hours(0),
        LoopNeed {
            key: LoopKey { pane: 1 },
            kind: LoopKind::CrossSection,
            frame_bytes: model.section,
            ..six_hours(1)
        },
        bare(2, LoopKind::Volume, &model),
    ]);
    let first = pool.plan(model, &demand);
    for _ in 0..5 {
        assert_eq!(pool.plan(model, &demand), first);
    }
    assert!(first.bytes() <= pool.bytes());
}

/// [`loop_ceiling_frames`]: the listing where one has landed, the window at the cadence
/// where not, the base where nothing is known — never past the list cap.
#[test]
fn the_ceiling_is_the_listing_then_the_window_then_the_base_never_past_the_cap() {
    assert_eq!(loop_ceiling_frames(Some(73), 21600, Some(300), 25, 60), 60);
    assert_eq!(loop_ceiling_frames(Some(40), 21600, Some(300), 25, 60), 40);
    assert_eq!(loop_ceiling_frames(Some(5), 21600, Some(300), 25, 60), 5);
    assert_eq!(
        loop_ceiling_frames(None, 21600, Some(300), 25, 60),
        60,
        "1 + 72 = 73, capped"
    );
    assert_eq!(loop_ceiling_frames(None, 3600, Some(300), 13, 60), 13);
    assert_eq!(
        loop_ceiling_frames(None, 3600, Some(0), 36, 60),
        36,
        "a zero cadence is unknown"
    );
    assert_eq!(loop_ceiling_frames(None, 3600, None, 36, 60), 36);
    assert_eq!(loop_ceiling_frames(None, 3600, None, 36, 14), 14);
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
        overlay_pictures: 0,
        picture_px: [0, 0],
        loop_scans_shared: false,
        loop_scans_resident_bytes: 0,
        loop_scans_resident_frames: 0,
        loop_scans_needed: true,
    }
}

/// `panes` of [`looping_pane`] and nothing else.
fn scene_of(panes: usize) -> Scene {
    Scene {
        panes: vec![looping_pane(); panes],
        tile_sources: Vec::new(),
        mirror_px: [0, 0],
        overlay_grids: Vec::new(),
    }
}

/// **The scene decides where between the bounds the pool sits, not the
/// class.** One two-hour loop with no cadence yet needs exactly the floor on
/// every arm — the floor was argued as one loop's span, and this is that
/// argument read back — and a full screen of them needs the lesser of their
/// sum and the room the static renders leave under the presumed capacity,
/// held to the bracket. An adapter that says nothing and a discrete card get
/// the same pool for the same one loop: the ruling, as arithmetic. On the
/// desktop bracket one loop is 36 x 16 MiB = 576 MiB, not the 3072 MiB
/// ceiling a discrete card used to be handed, and six are
/// min(3456, 3840 - 6 x 256) = 2304 MiB.
#[test]
fn the_scene_decides_where_between_the_bounds_the_pool_sits() {
    use crate::budget_arms::shipped_profile;
    use squallar_device_profile::budget::{BudgetLimits, DeviceProfile, FormFactor, resolve};

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

/// **The pool follows the loops' ceiling, not their base, when the room allows.** A pane
/// whose listing has said 300 s over a six-hour lookback has a base of 25 frames (400 MiB)
/// and a ceiling of 60 (960 MiB): on a measured 3090 the pool is the 960 MiB the balloon
/// can grow into, and six such panes on the presumed arm are held to the 2304 MiB of room
/// — the same room six two-hour loops with no cadence get, because the room does not
/// depend on what the loops ask.
#[test]
fn the_pool_follows_the_ceiling_not_the_base_when_the_room_allows() {
    use crate::budget_arms::shipped_profile;
    use squallar_device_profile::budget::{BudgetLimits, DeviceProfile, resolve};
    use squallar_device_profile::quality::DeviceClass;

    let with_cadence = |panes: usize| Scene {
        panes: vec![
            squallar_device_profile::scene::PaneNeed {
                loop_span_secs: 6 * 60 * 60,
                cadence_secs: Some(300),
                ..looping_pane()
            };
            panes
        ],
        tile_sources: Vec::new(),
        mirror_px: [0, 0],
        overlay_grids: Vec::new(),
    };
    let rtx_3090 = DeviceProfile {
        class: DeviceClass::Discrete,
        vram_bytes: Some(24822 << 20),
        system_ram_bytes: Some(64 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let b = resolve(&rtx_3090);
    assert_eq!(
        b.frames_for_span_of(6 * 60 * 60, Some(300)),
        25,
        "the base: two hours at 300 s"
    );
    let limits = LoopPoolLimits::from_budgets(&b);
    let measured = rtx_3090.capacity();
    assert_eq!(
        LoopPool::for_scene(&with_cadence(1), &b, &measured, limits).bytes(),
        60 * 16 * MIB,
        "the ceiling, 60 x 16 MiB, not the base's 25 x 16",
    );
    let presumed = Capacity::presumed(&BudgetLimits::DESKTOP);
    assert_eq!(
        LoopPool::for_scene(&with_cadence(6), &b, &presumed, limits).bytes(),
        2304 * MIB,
        "min(6 x 960, 3840 - 6 x 256): the room",
    );
    assert_eq!(
        LoopPool::for_scene(&with_cadence(1), &b, &presumed, limits).bytes(),
        960 * MIB,
        "one pane on the presumption: min(960, 3840 - 256)",
    );
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
/// room, and the model follows a re-fit of the budgets; an allocation planned
/// against the old one would hold frames the new one cannot pay for. A
/// flicker inside the dwell still changes nothing, and a shrink is taken with
/// no dead band.
#[test]
fn a_pool_or_model_that_moves_under_the_same_demand_is_replanned_after_the_dwell() {
    let model = desktop();
    let limits = desktop_limits();
    let wide = LoopPool::new(limits.ceiling, limits);
    let narrow = LoopPool::new(limits.floor, limits);
    let one = six_hours_on(1);
    let mut state = LoopPoolState::new(wide, model);
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(wide, model, one.clone());
    }
    let settled = state.allocation().clone();
    assert_eq!(
        frames_of(&settled, 0),
        60,
        "the ceiling pays every listed scan"
    );

    // The pool halves and comes back on alternate frames: nothing moves.
    for frame in 0..LOOP_POOL_DWELL_FRAMES * 8 {
        let pool = if frame % 2 == 0 { narrow } else { wide };
        assert_eq!(
            state.observe(pool, model, one.clone()),
            &settled,
            "the allocation moved on frame {frame} of a pool flicker",
        );
    }

    // Held for the dwell, the narrower pool is taken at once: a shrink.
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(narrow, model, one.clone());
    }
    let shrunk = state.allocation().clone();
    assert_eq!(
        frames_of(&shrunk, 0),
        36,
        "the floor was argued as one loop's whole span: 36 frames of 16 MiB",
    );
    assert!(frames_of(&shrunk, 0) < frames_of(&settled, 0));

    // The model moves under the same pool and demand — a re-fit halved the
    // render budget — and after the dwell the ceiling a loop the plan has not
    // seen is held to follows it, even though no grant moved.
    let halved = LoopFrameModel {
        render_budget: DESKTOP_MAX_LOOP_RENDER_BUDGET / 2,
        ..model
    };
    assert_eq!(state.allocation().frames_for(RenderView::CrossSection), 36);
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(narrow, halved, one.clone());
    }
    assert_eq!(
        state.allocation().frames_for(RenderView::CrossSection),
        DESKTOP_MAX_LOOP_RENDER_BUDGET / 2,
        "a halved render budget did not reach the allocation in force",
    );
    assert_eq!(
        frames_of(state.allocation(), 0),
        36,
        "the grant itself was untouched"
    );
}

/// A pane that appears and vanishes inside the dwell costs nothing at all.
#[test]
fn a_pane_that_flickers_inside_the_dwell_changes_nothing() {
    let model = desktop();
    let limits = desktop_limits();
    let pool = LoopPool::new(limits.floor, limits);
    let one = demand_of([bare(0, LoopKind::PlanView, &model)]);
    let two = demand_of([
        bare(0, LoopKind::PlanView, &model),
        bare(1, LoopKind::PlanView, &model),
    ]);

    let mut state = LoopPoolState::new(pool, model);
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, one.clone());
    }
    let settled = state.allocation().clone();
    assert_eq!(
        frames_of(&settled, 0),
        frames_of(&pool.plan(model, &one), 0)
    );

    // A second pane appears and vanishes on alternate frames for many times the dwell.
    for frame in 0..LOOP_POOL_DWELL_FRAMES * 8 {
        let demand = if frame % 2 == 0 { &two } else { &one };
        assert_eq!(
            state.observe(pool, model, demand.clone()),
            &settled,
            "the allocation moved on frame {frame} of a flicker",
        );
    }

    // Held for the dwell, it is taken — and it is shorter, not blank.
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, two.clone());
    }
    let shared = state.allocation();
    assert!(frames_of(shared, 0) < frames_of(&settled, 0));
    assert!(frames_of(shared, 0) >= MIN_LOOP_FRAMES_PER_PANE);
    assert!(
        shared.frames_for_pane(1).is_some(),
        "the pane that stayed got a grant"
    );
}

/// A shrink is taken after the dwell; a growth also has to clear the dead band — measured
/// on the loops' **frames**, the thing a re-plan changes on screen. At 3072 MiB six
/// six-hour panes hold 32 each; five would hold 38 or 39 (1.19x, inside the band) and are
/// refused; four would hold 48 (1.5x) and are taken; and back to six is a shrink, taken with
/// no band at all.
#[test]
fn a_growth_has_to_clear_the_dead_band_but_a_shrink_does_not() {
    let model = desktop();
    let limits = desktop_limits();
    let pool = LoopPool::new(limits.ceiling, limits);
    let settle = |state: &mut LoopPoolState, demand: &LoopDemand| {
        for _ in 0..LOOP_POOL_DWELL_FRAMES {
            state.observe(pool, model, demand.clone());
        }
        state.allocation().clone()
    };

    let mut state = LoopPoolState::new(pool, model);
    let six = settle(&mut state, &six_hours_on(6));
    assert_eq!(frames_of(&six, 0), 32);

    // Five of six: 39 / 32 = 1.22x, inside the band.
    let five = settle(&mut state, &six_hours_on(5));
    assert_eq!(five, six, "a 1.22x growth was taken");
    for _ in 0..LOOP_POOL_DWELL_FRAMES * 4 {
        assert_eq!(state.observe(pool, model, six_hours_on(5)), &six);
    }

    // Four of six: 48 / 32 = 1.5x, past the band.
    let four = settle(&mut state, &six_hours_on(4));
    assert_eq!(frames_of(&four, 0), 48, "a 1.5x growth was refused");

    // And a shrink straight back to six is taken with no band at all, because the pool is a
    // bound.
    let back = settle(&mut state, &six_hours_on(6));
    assert_eq!(back, six, "a shrink was held off by the dead band");
}

/// A loop that starts is granted at once after the dwell, however small the change it
/// makes to everyone else: a new pane reading the kind's ceiling for ever would be the
/// old equal split by another name.
#[test]
fn a_loop_that_starts_gets_a_grant_after_the_dwell_whatever_the_band_says() {
    let model = desktop();
    let limits = desktop_limits();
    let pool = LoopPool::new(limits.ceiling, limits);
    let mut state = LoopPoolState::new(pool, model);
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, six_hours_on(5));
    }
    assert!(state.allocation().frames_for_pane(5).is_none());
    for _ in 0..LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, six_hours_on(6));
    }
    assert_eq!(state.allocation().frames_for_pane(5), Some(32));
}

/// `LoopDemand` keeps one need per pane, replaces rather than duplicates, aliases a pane
/// onto another's loop, and counts by kind.
#[test]
fn the_demand_keeps_one_need_per_pane_and_counts_by_kind() {
    let model = desktop();
    let mut demand = LoopDemand::default();
    demand.push(bare(0, LoopKind::PlanView, &model));
    demand.push(bare(1, LoopKind::CrossSection, &model));
    demand.push(bare(2, LoopKind::Volume, &model));
    demand.push(bare(3, LoopKind::Overlay, &model));
    demand.alias(4, 2);
    assert_eq!(
        demand.shares(),
        4,
        "an overlay-only pane is a loop the pool is given to"
    );
    for kind in [
        LoopKind::PlanView,
        LoopKind::CrossSection,
        LoopKind::Volume,
        LoopKind::Overlay,
    ] {
        assert_eq!(demand.count(kind), 1, "{kind:?}");
    }
    // A pane visited twice is one need, the later one.
    demand.push(LoopNeed {
        base_frames: 5,
        ..bare(0, LoopKind::PlanView, &model)
    });
    assert_eq!(demand.shares(), 4);
    assert_eq!(demand.needs()[0].base_frames, 5);
    // The alias reads its owner's grant.
    let allocation = LoopPool::new(desktop_limits().ceiling, desktop_limits()).plan(model, &demand);
    assert_eq!(allocation.frames_for_pane(4), allocation.frames_for_pane(2));
    assert!(allocation.frames_for_pane(9).is_none());
    assert_eq!(LoopDemand::default().shares(), 0);
}

/// Nothing looping is not a division by zero, and it does not blank anything either — the
/// allocation a fresh application starts with is the one a single loop would get, on every
/// kind, at the whole pool.
#[test]
fn an_empty_demand_is_the_single_loop_answer() {
    for arm in arms() {
        let pool = LoopPool::new(arm.limits.floor, arm.limits);
        let empty = pool.plan(arm.model, &LoopDemand::default());
        assert_eq!(empty.share_bytes, pool.bytes(), "{}", arm.name);
        for kind in [LoopKind::PlanView, LoopKind::CrossSection, LoopKind::Volume] {
            let one = pool.plan(arm.model, &demand_of([bare(0, kind, &arm.model)]));
            assert_eq!(
                empty.frames_for_kind(kind),
                frames_of(&one, 0),
                "{}: {kind:?}: the ceiling an unseen loop reads is not what one loop gets",
                arm.name,
            );
        }
        // An overlay loop is held to the list cap and not the render budget.
        assert!(empty.overlay_frames <= arm.model.list_cap, "{}", arm.name);
        // And no 3D reserve at all, which is what lets the caller floor the store's bound
        // at the live-grid figure instead of at zero.
        assert_eq!(empty.volume_reserve_bytes(), 0, "{}", arm.name);
        assert_eq!(empty.balloon_bytes(), 0, "{}", arm.name);
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
/// * **An overlay priced as radar.** An overlay frame was once priced as a
///   radar plan-view frame — `Budgets::loop_frame_bytes()` — and every
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
             ({} B). That is what the fallback once did, and it is the one \
             substitution every other assertion in this crate survives, \
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
    assert!(
        compiled.list_cap >= compiled.render_budget,
        "the list cap is what a balloon can raise a loop to, and it is not below the base",
    );
}

/// The budget-agreement proofs that bridge to this module's planner — moved here from the
/// floor crate's `constants::tests` at WO-RD.
mod budget_agreement {
    use crate::budget_arms::{SHIPPED_VOLUME_LOOP_FRAMES, arms, volume_bytes};

    /// The store's bound is what the sum above charges, on the arithmetic the application
    /// actually runs rather than on the reading of it.
    #[test]
    fn the_volume_store_floor_is_the_widest_the_override_can_open() {
        use crate::loop_pool::{
            LoopDemand, LoopFrameModel, LoopKey, LoopKind, LoopNeed, LoopPool, LoopPoolLimits,
        };
        use squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE;

        for arm in arms() {
            let model = LoopFrameModel {
                plan_view: arm.loop_frame_bytes(),
                section: arm.section_frame_bytes(),
                grid: volume_bytes(&arm),
                overlay: crate::loop_pool::nominal_overlay_frame_bytes(),
                render_budget: arm.loop_render_budget,
                list_cap: arm.loop_frames_held,
            };
            let limits = LoopPoolLimits {
                floor: arm.loop_pool_floor_bytes,
                ceiling: arm.loop_pool_ceiling_bytes,
            };
            let bare = |pane: usize, kind: LoopKind| LoopNeed {
                key: LoopKey { pane },
                kind,
                span_secs: arm.loop_span_secs as u64,
                cadence_secs: None,
                frame_bytes: model.price(kind),
                base_frames: model.render_budget,
                max_frames: model.render_budget,
            };
            for pool_bytes in [arm.loop_pool_floor_bytes, arm.loop_pool_ceiling_bytes] {
                let pool = LoopPool::new(pool_bytes, limits);
                for plan_view_loops in 0..=arm.max_panes {
                    for section_loops in 0..=(arm.max_panes - plan_view_loops) {
                        for volume_sets in 0..=(arm.max_panes - plan_view_loops - section_loops) {
                            let mut demand = LoopDemand::default();
                            let mut pane = 0;
                            for (kind, n) in [
                                (LoopKind::PlanView, plan_view_loops),
                                (LoopKind::CrossSection, section_loops),
                                (LoopKind::Volume, volume_sets),
                            ] {
                                for _ in 0..n {
                                    demand.push(bare(pane, kind));
                                    pane += 1;
                                }
                            }
                            let allocation = pool.plan(model, &demand);
                            // What `setup_egui_frame` hands `enforce_budget`.
                            let store = allocation
                                .volume_reserve_bytes()
                                .max(arm.volume_loop_bytes());
                            // What the raster loops may cache beside it.
                            let raster: usize = allocation
                                .grants()
                                .iter()
                                .filter(|g| g.kind != LoopKind::Volume)
                                .map(|g| g.bytes())
                                .sum();
                            let at_floor = allocation
                                .grants()
                                .iter()
                                .any(|g| g.frames == MIN_LOOP_FRAMES_PER_PANE);
                            assert!(
                                raster + store
                                    <= arm.loop_pool_ceiling_bytes + arm.volume_loop_bytes()
                                    || at_floor,
                                "{}: {plan_view_loops}/{section_loops}/{volume_sets} loops at \
                                 a {} MiB pool cache {} MiB of raster frames beside a {} MiB \
                                 store bound — over the `pool ceiling + volume-store floor` \
                                 the app ceiling charges, with no loop at the minimum",
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
                    overlay_pictures: 0,
                    picture_px: [0, 0],
                    loop_scans_shared: false,
                    loop_scans_resident_bytes: 0,
                    loop_scans_resident_frames: 0,
                    loop_scans_needed: true,
                }],
                tile_sources: Vec::new(),
                mirror_px: [0, 0],
                overlay_grids: Vec::new(),
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
    /// **derived** — the host's own RAM over the divisor, since no API was
    /// asked about the card: its pool is still one loop's need, and its
    /// offscreen still the Step the class earns, because memory says nothing
    /// about fill rate. The same 3090 over GL is `Other` to wgpu; with the
    /// host's RAM read it too is unified memory at the desktop-class line, and
    /// with nothing read it is the presumption. A browser is the presumption
    /// whatever it reads.
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
        use CapacitySource::{Derived, Measured, Presumed};

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
                Derived,
                36
            ),
            "the 3090 over GL with the host's 64 GiB read: an unclassed adapter at the \
             desktop-class line is believed as unified memory, half the RAM — DERIVED \
             and not measured, since no API was asked about this GPU; every other \
             column is unchanged, which is what says the word moved and the arithmetic \
             did not",
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
                Derived,
                36
            ),
            "a desktop integrated GPU on a 64 GiB host: 32 GiB of unified memory DERIVED \
             from the host's RAM and not measured, the pool by need, and the offscreen at \
             the Step — what holds it back is fill rate, which no memory figure speaks to",
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
