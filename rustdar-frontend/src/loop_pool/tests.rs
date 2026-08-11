use super::*;
use crate::constants::{
    DESKTOP_LOOP_IMAGE_SIZE, DESKTOP_LOOP_POOL_CEILING_BYTES, DESKTOP_LOOP_POOL_FLOOR_BYTES,
    DESKTOP_MAX_LOOP_RENDER_BUDGET, DESKTOP_MAX_LOOP_VOLUME_FRAMES, DESKTOP_VOLUME_GRID_CELLS,
    MOBILE_LOOP_IMAGE_SIZE, MOBILE_LOOP_POOL_CEILING_BYTES, MOBILE_LOOP_POOL_FLOOR_BYTES,
    MOBILE_MAX_LOOP_RENDER_BUDGET, MOBILE_MAX_LOOP_VOLUME_FRAMES, MOBILE_VOLUME_GRID_CELLS,
    WASM_LOOP_IMAGE_SIZE, WASM_LOOP_POOL_CEILING_BYTES, WASM_LOOP_POOL_FLOOR_BYTES,
    WASM_MAX_LOOP_RENDER_BUDGET, WASM_MAX_LOOP_VOLUME_FRAMES, WASM_VOLUME_GRID_CELLS,
};
use rustdar_egui::config_store::MemoryConfigStore;
use rustdar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

/// One device class, with both halves of every question a host build cannot
/// otherwise reach.
///
/// The shape `constants::tests::arms()` uses, for the reason it gives: every
/// constant here is `cfg`-selected in production and this workspace runs
/// `cargo test` on exactly one of three arms, so a pool rule checked against
/// `for_target()` alone would be checked on one row and left free on two.
struct Arm {
    name: &'static str,
    model: LoopFrameModel,
    limits: LoopPoolLimits,
    /// This class's `MAX_PANES`, from `rustdar_egui::pane` — the other half of
    /// the multiplication that started all of this.
    max_panes: usize,
    /// The 3D frame count this class ships, which the pool must reproduce for a
    /// single loop at the floor.
    volume_loop_frames: usize,
}

/// `loop_image_size` is the side a **loop** frame renders at, which on the web
/// is not the side a static one takes — see [`LoopFrameModel::plan_view`]. The
/// section width is passed rather than derived from it for the same reason:
/// `xsect` pins it per target, and the two agreeing on every arm this ships is
/// a fact to be read from the constants, not one to be assumed here.
fn model(
    loop_image_size: usize,
    section_width: usize,
    grid: [u32; 3],
    render_budget: usize,
) -> LoopFrameModel {
    LoopFrameModel {
        plan_view: loop_image_size * loop_image_size * 4,
        section: section_width * (section_width / 2) * 4,
        grid: crate::volume::raymarch::grid_bytes_with_mips(grid).expect("a shipped grid shape")
            + crate::constants::VOLUME_LUT_BYTES,
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
            max_panes: rustdar_egui::pane::MAX_PANES_DESKTOP,
            volume_loop_frames: WASM_MAX_LOOP_VOLUME_FRAMES,
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
            max_panes: rustdar_egui::pane::MAX_PANES_MOBILE,
            volume_loop_frames: MOBILE_MAX_LOOP_VOLUME_FRAMES,
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
            max_panes: rustdar_egui::pane::MAX_PANES_DESKTOP,
            volume_loop_frames: DESKTOP_MAX_LOOP_VOLUME_FRAMES,
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
                });
            }
        }
    }
    out
}

/// **The pool actually bounds the sum.**
///
/// The claim the whole change exists to make, and the one nothing could have
/// made before: `MAX_PANES × LOOP_TEXTURE_BUDGET_BYTES` was 3.0 GiB on desktop
/// and 1.0 GiB on a phone, and no test put those two halves side by side
/// because they lived in different crates.
///
/// Checked over **every reachable mix of loop kinds** on every arm at both
/// bounds of the pool, not over one layout: the failure this catches is a
/// division rule that is right for one pane and wrong for six.
///
/// # The two loop kinds are bounded by two different things, and that is real
///
/// The raster kinds have **no runtime enforcement at all** — nothing measures a
/// cached loop frame against a budget — so for them the division *is* the
/// bound, and it has to hold unconditionally. That is the first assertion.
///
/// The 3D kind does have one: `VolumeStore::enforce_budget` runs every frame
/// against [`LoopAllocation::volume_reserve_bytes`] and evicts oldest-first
/// until the resident grids fit. So the bound that matters there is the
/// *reserve*, not the frame count — and the reserve is one share per set, which
/// is the pool by construction. This test asserts the reserve, which is what is
/// actually enforced.
///
/// The distinction is not a loophole, it is the reason the frame count is
/// allowed to be floored at [`MIN_LOOP_FRAMES_PER_PANE`]: six *distinct* 3D
/// loops on a browser cannot each be given a frame, so the plan asks for two
/// apiece and the store keeps whatever the reserve pays for. A short loop is a
/// worse answer than a long one; a blank loop is a worse answer than either.
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

                // And the whole allocation together, except where the minimum
                // had to win — which is stated rather than absorbed, and is
                // reachable only for 3D loops whose grids cost more than a
                // share. `the_floor_seats_every_pane_without_blanking_one`
                // rules the raster case out on every target this ships.
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

/// **A loop shortens when a pane arrives and recovers when it goes.**
///
/// The behaviour the pool was asked for, stated as the property rather than as
/// the formula. A per-pane allowance passes nothing here: it would return the
/// same frame count for one pane and for six.
///
/// Monotone in the pane count as well as merely different, because "shorter"
/// has to mean shorter every time and not only at the ends — a division that
/// happened to be non-monotone would make opening a pane sometimes *lengthen*
/// a neighbour's loop, which is the same confusion as the cliff, arrived at
/// from the other side.
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
        // And the recovery is the same division read backwards, which is what
        // makes closing a pane give the length back rather than needing a
        // separate rule that could disagree with this one.
        assert_eq!(frames(1), alone, "{}", arm.name);
    }
}

/// **Two 3D panes on one volume are one share, not two.**
///
/// A 3D loop's frames are resident grids in a single application-wide
/// `VolumeStore` keyed by `VolumeTarget`, so two panes orbiting one volume from
/// two angles already share one build, one upload and one resident set. The
/// pool is therefore divided per *loop* rather than per pane, and
/// `App::loop_demand` deduplicates 3D panes by site, product and
/// `VolumeLoopKey` before counting them.
///
/// A naive per-pane split fails this outright: it would halve the share of the
/// one loop kind that cannot re-render its way out of being short, for a second
/// pane that costs nothing.
///
/// Two panes on two *different* volumes are correctly two sets and do divide,
/// which is the other half of the claim and is what stops this being a rule
/// that simply ignores 3D panes.
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
        // Two panes on one volume: `LoopDemand::add` is told the key was
        // already counted, so the demand is unchanged and so is the share.
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
        // `enforce_budget` evicts oldest-first over one store, so a bound
        // naming only one set would evict the older loop's frames for ever.
        assert_eq!(
            two_sets.volume_reserve_bytes(),
            two_sets.share_bytes * 2,
            "{}",
            arm.name,
        );
    }
}

/// A single 3D loop at the floor holds exactly the count this target ships.
///
/// The continuity property for the loop kind whose frame list *is* its resident
/// set: `MAX_LOOP_VOLUME_FRAMES` was chosen as the tighter of what the budget
/// admits beside one live grid and `MAX_LOOP_RENDER_BUDGET`, and
/// `LoopPool::plan` has to reproduce that number rather than merely come close
/// to it — the dispatcher, the store and the readiness check all read the pool
/// now, and a count one frame different is a rebuild treadmill.
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

/// **A full 3D loop leaves room for one live grid beside it — at every pool
/// size.**
///
/// A second 3D pane showing a live volume is one more grid in the same store,
/// and `VolumeStore::enforce_budget` evicts *oldest first*: the loop started
/// first, so what goes is the loop's own frame 0, which the dispatcher re-plans
/// on the next pass and rebuilds at ~89 ms of resample, for ever.
///
/// The constant this replaces was tuned against one budget figure. The
/// subtraction is inside `plan` now, so the property holds at the floor, at the
/// ceiling and at every back-off step between them — which is the only way it
/// can hold at all once the pool is discovered rather than compiled in.
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
                        // The minimum wins over the byte rule, deliberately: a
                        // loop cut to nothing is worse than one that makes the
                        // store evict. Only reachable at pane counts the floor
                        // does not seat, which is a case
                        // `the_floor_seats_every_pane_without_blanking_one`
                        // rules out for the raster kinds and which is stated
                        // here for the 3D one.
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
///
/// `evict_textures_outside_render_set` strips the texture off every frame
/// outside the render set on every dispatch, so frames past
/// `MAX_LOOP_RENDER_BUDGET` are memory spent on pictures nothing keeps. A pool
/// large enough to pay for them must not, which is what makes the ceiling a
/// bound on *memory* rather than a licence to grow the history without limit.
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
///
/// Not a bonus: it is what dividing *bytes* means, given that a section frame
/// is exactly half a plan-view one. Pinned so that a future change to the
/// section raster's aspect has to come and re-argue it rather than quietly
/// making a section loop the largest thing on the screen.
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

/// The classification is the only signal there is, and it is spent this way.
///
/// A discrete GPU has memory nothing else is competing for and takes the
/// ceiling — which is exactly what this application could already reach, so no
/// machine that has the memory loses anything. Everything else is a device that
/// either shares its memory with the whole system or cannot say what it is, and
/// **every browser is the latter**: WebGL2 exposes no device type at all.
#[test]
fn the_device_class_decides_where_between_the_bounds_the_pool_sits() {
    for arm in arms() {
        let at = |class| LoopPool::for_device(class, None, arm.limits).bytes();
        assert_eq!(
            at(DeviceClass::Discrete),
            arm.limits.ceiling,
            "{}",
            arm.name
        );
        assert_eq!(
            at(DeviceClass::Integrated),
            (arm.limits.floor * 2).min(arm.limits.ceiling),
            "{}",
            arm.name,
        );
        for blind in [
            DeviceClass::Unknown,
            DeviceClass::Virtual,
            DeviceClass::Software,
        ] {
            assert_eq!(
                at(blind),
                arm.limits.floor,
                "{}: a {blind:?} adapter must take the floor — this is the arm \
                 every browser lands in",
                arm.name,
            );
        }
        // A device that says it is discrete does not get to claim more than the
        // ceiling, which is the misread this bound exists for.
        assert!(at(DeviceClass::Discrete) <= arm.limits.ceiling);
    }
}

/// What a session remembered outranks what the adapter is.
///
/// A pool arrived at by watching *this machine* refuse an allocation is better
/// evidence than any classification, and honouring it is also what keeps a
/// reopen 1:1 — without it, a user who had backed off would see a different
/// loop length on every start.
#[test]
fn a_remembered_pool_outranks_the_classification() {
    let limits = LoopPoolLimits {
        floor: 64 * 1024 * 1024,
        ceiling: 512 * 1024 * 1024,
    };
    let remembered = 128 * 1024 * 1024;
    assert_eq!(
        LoopPool::for_device(DeviceClass::Discrete, Some(remembered), limits).bytes(),
        remembered,
        "a discrete adapter overrode what this machine had already learned",
    );
    // Still held to the bounds: a memo written by a build with different ones
    // is evidence about the machine, not a licence to leave this build's.
    assert_eq!(
        LoopPool::for_device(DeviceClass::Discrete, Some(usize::MAX), limits).bytes(),
        limits.ceiling,
    );
    assert_eq!(
        LoopPool::for_device(DeviceClass::Discrete, Some(1), limits).bytes(),
        limits.floor,
    );
}

/// Backing off halves toward the floor and stops there.
///
/// The behavioural half of the sizing, and on wasm32 the only half: nothing in
/// a browser reports memory, and Chrome answers exhaustion by destroying the
/// rendering context rather than by failing a call.
#[test]
fn backing_off_halves_toward_the_floor_and_stops() {
    let limits = LoopPoolLimits {
        floor: 64 * 1024 * 1024,
        ceiling: 512 * 1024 * 1024,
    };
    let mut pool = LoopPool::new(limits.ceiling, limits);
    let mut seen = vec![pool.bytes()];
    while pool.back_off(limits) {
        seen.push(pool.bytes());
        assert!(seen.len() < 16, "back-off did not terminate: {seen:?}");
    }
    assert_eq!(
        seen,
        vec![
            512 * 1024 * 1024,
            256 * 1024 * 1024,
            128 * 1024 * 1024,
            64 * 1024 * 1024,
        ],
    );
    // At the floor it reports that nothing moved, which is the caller's cue not
    // to write the same value to the config store on every subsequent loss.
    assert!(!pool.back_off(limits));
    assert_eq!(pool.bytes(), limits.floor);
}

/// A pane that appears and vanishes inside the dwell costs nothing at all.
///
/// The anti-thrash rule's whole point: re-planning a loop means re-fetching and
/// re-rendering its frames, so a transient must not reach the allocation. This
/// is `MirrorRungs`' shape — a pending demand that has to survive
/// [`LOOP_POOL_DWELL_FRAMES`] before it is taken.
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
    for _ in 0..crate::constants::LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, one);
    }
    let settled = state.allocation();
    assert_eq!(
        settled.plan_view_frames,
        pool.plan(model, one).plan_view_frames
    );

    // A second pane appears and vanishes on alternate frames for many times the
    // dwell. Neither demand ever holds long enough to be taken.
    for frame in 0..crate::constants::LOOP_POOL_DWELL_FRAMES * 8 {
        let demand = if frame % 2 == 0 { two } else { one };
        assert_eq!(
            state.observe(pool, model, demand),
            settled,
            "the allocation moved on frame {frame} of a flicker",
        );
    }

    // Held for the dwell, it is taken — and it is shorter, not blank.
    for _ in 0..crate::constants::LOOP_POOL_DWELL_FRAMES {
        state.observe(pool, model, two);
    }
    let shared = state.allocation();
    assert!(shared.plan_view_frames < settled.plan_view_frames);
    assert!(shared.plan_view_frames >= MIN_LOOP_FRAMES_PER_PANE);
}

/// A shrink is taken after the dwell; a growth also has to clear the dead band.
///
/// The asymmetry is the point, and it is where this departs from
/// `MIRROR_RUNG_HYSTERESIS`, which is one-sided the other way. Being *over* the
/// pool is not something to be sticky about. Being under it is only a missed
/// opportunity — and closing the sixth of six panes buys each survivor 20 %
/// more share, which is not worth re-fetching every loop on screen for.
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
        for _ in 0..crate::constants::LOOP_POOL_DWELL_FRAMES {
            state.observe(pool, model, demand);
        }
        state.allocation()
    };

    let mut state = LoopPoolState::new(pool, model);
    let six = settle(&mut state, loops(6));

    // Five of six: 6/5 = 1.2x, inside the band. Refused, and not reconsidered.
    let five = settle(&mut state, loops(5));
    assert_eq!(five, six, "a 1.2x growth was taken");
    for _ in 0..crate::constants::LOOP_POOL_DWELL_FRAMES * 4 {
        assert_eq!(state.observe(pool, model, loops(5)), six);
    }

    // Four of six: 6/4 = 1.5x, past the band. Taken.
    let four = settle(&mut state, loops(4));
    assert!(
        four.share_bytes > six.share_bytes,
        "a 1.5x growth was refused",
    );

    // And a shrink straight back to six is taken with no band at all, because
    // the pool is a bound.
    let back = settle(&mut state, loops(6));
    assert_eq!(back, six, "a shrink was held off by the dead band");
}

/// The memo round-trips, and anything unreadable is simply absent.
///
/// A decimal count of MiB and nothing else, in its own `ConfigStore` key — not
/// a field on `UiConfig`, where one bad value costs *every* setting on the next
/// load. The blast radius of a corrupt entry here is one integer and one
/// re-probe.
#[test]
fn the_pool_memo_round_trips_and_survives_a_corrupt_entry() {
    let limits = LoopPoolLimits {
        floor: 64 * 1024 * 1024,
        ceiling: 512 * 1024 * 1024,
    };
    let store = MemoryConfigStore::default();
    assert_eq!(remembered(Some(&store), limits), None, "nothing yet");

    remember(Some(&store), 128 * 1024 * 1024);
    assert_eq!(
        store.load(LOOP_POOL_KEY).as_deref(),
        Some("128"),
        "the memo is a bare decimal count of MiB",
    );
    assert_eq!(remembered(Some(&store), limits), Some(128 * 1024 * 1024));

    for junk in ["", "  ", "not a number", "-1", "12.5", "{\"mib\":128}"] {
        store.store(LOOP_POOL_KEY, junk).expect("storable");
        assert_eq!(
            remembered(Some(&store), limits),
            None,
            "a {junk:?} memo should fall back to the classification",
        );
    }
    // A zero is absent rather than a pool of nothing.
    store.store(LOOP_POOL_KEY, "0").expect("storable");
    assert_eq!(remembered(Some(&store), limits), None);

    // No store at all is the wasm-without-localStorage case, and it degrades.
    assert_eq!(remembered(None, limits), None);
    remember(None, 128 * 1024 * 1024);
}

/// `LoopDemand::add` classifies by view exhaustively, like everything else in
/// this workspace that switches on one.
#[test]
fn the_demand_counts_each_view_in_its_own_column() {
    let mut demand = LoopDemand::default();
    demand.add(RenderView::PlanView, false);
    demand.add(RenderView::CrossSection, false);
    demand.add(RenderView::Volume, false);
    assert_eq!(
        demand,
        LoopDemand {
            plan_view_loops: 1,
            section_loops: 1,
            volume_sets: 1,
        },
    );
    assert_eq!(demand.shares(), 3);
    assert_eq!(LoopDemand::default().shares(), 0);
}

/// Nothing looping is not a division by zero, and it does not blank anything
/// either — the allocation a fresh application starts with is the one a single
/// loop would get.
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
        // And no 3D reserve at all, which is what lets the caller floor the
        // store's bound at the live-grid figure instead of at zero.
        assert_eq!(empty.volume_reserve_bytes(), 0, "{}", arm.name);
    }
}

/// The compiled target's model is one of the three the table above names.
///
/// Weaker than the per-arm rules and kept anyway: it is the one assertion that
/// says the `cfg` cascade this build compiled actually selected a row, rather
/// than a fourth set of numbers nothing checks.
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
