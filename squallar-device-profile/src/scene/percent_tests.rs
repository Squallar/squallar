//! The user's two pool percentages: what they multiply, on which arm, and
//! what they leave alone.
//!
//! Both directions everywhere, because both are regressions. A percentage
//! that **lowers** must lower the quantity the ladder actually spends — which
//! is why the table below reads `fit::tile_cache_budget` and not
//! `host_allowance()`: an allowance nothing consumes is not a setting. And a
//! percentage at **100 %** must leave every figure byte-identical to what a
//! session before this setting existed resolved, on every arm, which is the
//! regression that protects every user who never opens the control.

use super::*;
use crate::budget::{BudgetLimits, Budgets, DeviceProfile};
use crate::fit;
use crate::scene::fixtures::{scene_table, shipped_profile, stand_in_grid_bytes};

/// Half of both pools, which is the working percentage in every table below.
const HALF: PoolPercents = PoolPercents { gpu: 50, host: 50 };
/// The lowest share the control offers — [`PoolPercents::FLOOR`] on both
/// pools, the bottom of the slider a user can actually reach.
const LOWEST: PoolPercents = PoolPercents {
    gpu: PoolPercents::FLOOR,
    host: PoolPercents::FLOOR,
};

/// The bracket every arm below is built against — the desktop limits, so the
/// presumed arm carries a real constant rather than a mobile stand-in.
fn desktop_profile() -> DeviceProfile {
    shipped_profile(BudgetLimits::DESKTOP)
}

/// **Every arm a capacity can be learned on**, each with a pool big enough
/// that halving it is observable, named the way the readout names it.
///
/// The unified rows are built through [`Capacity::unified`] rather than by
/// hand so the partition invariant under test is the shipped one; the two
/// differ in whether a driver answered, which is exactly the
/// `Measured`/`Derived` split on that constructor.
fn capacity_table() -> Vec<(&'static str, Capacity)> {
    const POOL: u64 = 16 * 1024 * 1024 * 1024;
    vec![
        (
            "measured",
            Capacity::measured(8 * 1024 * 1024 * 1024, Some(POOL)),
        ),
        ("probed", Capacity::probed(4 * 1024 * 1024 * 1024)),
        ("presumed", Capacity::presumed(&BudgetLimits::DESKTOP)),
        (
            "presumed with a host pool",
            Capacity {
                host_bytes: Some(POOL),
                ..Capacity::presumed(&BudgetLimits::DESKTOP)
            },
        ),
        (
            "derived (unified, no reader)",
            Capacity::unified(POOL, None),
        ),
        (
            "measured unified (a reader answered)",
            Capacity::unified(POOL, Some(3 * 1024 * 1024 * 1024)),
        ),
    ]
}

/// Every arm is represented, so a new `CapacitySource` cannot be added
/// without this table failing to cover it.
///
/// Not a re-read of the enum — a `match` with no wildcard is what makes a new
/// arm a compile error at the consumers; this is the weaker but different
/// claim that the *table* names each one, which a `match` cannot say.
#[test]
fn the_capacity_table_covers_every_arm() {
    let seen: Vec<CapacitySource> = capacity_table().iter().map(|(_, cap)| cap.source).collect();
    for arm in [
        CapacitySource::Measured,
        CapacitySource::Probed,
        CapacitySource::Derived,
        CapacitySource::Presumed,
    ] {
        assert!(
            seen.contains(&arm),
            "the percentage table names no {arm:?} capacity, so nothing below \
             says what a percentage buys there",
        );
    }
    assert!(
        capacity_table()
            .iter()
            .any(|(_, cap)| cap.pools == Pools::Unified),
        "the table names no unified capacity, where both percentages bind one \
         pool",
    );
}

/// **The same percentage buys the same fraction of the same quantity on every
/// arm.** The quantity is the pool figure, which is what the percentage is
/// defined over and what every allowance and every `fit` rung is derived from.
#[test]
fn a_percentage_takes_the_same_fraction_of_the_pool_on_every_arm() {
    for (name, cap) in capacity_table() {
        let half = cap.scaled_to(HALF);
        assert_eq!(
            half.gpu_bytes,
            cap.gpu_bytes / 2,
            "{name}: 50 % bought {} of a {} B GPU pool",
            half.gpu_bytes,
            cap.gpu_bytes,
        );
        assert_eq!(
            half.host_bytes,
            cap.host_bytes.map(|host| host / 2),
            "{name}: 50 % bought {:?} of a {:?} B host pool",
            half.host_bytes,
            cap.host_bytes,
        );
        assert_eq!(half.source, cap.source, "{name}: the provenance moved");
        assert_eq!(half.pools, cap.pools, "{name}: the topology moved");
    }
}

/// **100 % is the identity on every arm, byte for byte.** Every figure a
/// consumer can read off a capacity, not just the two the scaling touches.
#[test]
fn a_hundred_percent_leaves_every_figure_byte_identical() {
    let need = Need {
        gpu_bytes: 64 * 1024 * 1024,
        host_bytes: 64 * 1024 * 1024,
    };
    for (name, cap) in capacity_table() {
        let full = cap.scaled_to(PoolPercents::FULL);
        assert_eq!(full, cap, "{name}: the capacity itself moved at 100 %");
        assert_eq!(full.allowance(), cap.allowance(), "{name}: allowance moved");
        assert_eq!(
            full.host_allowance(),
            cap.host_allowance(),
            "{name}: host allowance moved",
        );
        assert_eq!(
            full.economy_allowance(need),
            cap.economy_allowance(need),
            "{name}: economy allowance moved",
        );
        assert_eq!(
            full.joint_allowance(),
            cap.joint_allowance(),
            "{name}: joint allowance moved",
        );
    }
}

/// The default is [`PoolPercents::FULL`] — the neutral value — and not a
/// policy. Every install with nothing written down, every config written
/// before the field existed and every reset lands here.
#[test]
fn the_default_percentages_are_neutrality() {
    assert_eq!(PoolPercents::default(), PoolPercents::FULL);
    assert_eq!(PoolPercents::FULL.gpu, 100);
    assert_eq!(PoolPercents::FULL.host, 100);
}

/// A value from a hand-edited file, or from a build whose range was wider,
/// costs the user nothing.
#[test]
fn a_percentage_from_disk_is_held_inside_the_offered_range() {
    assert_eq!(
        PoolPercents::clamped(0, 250),
        PoolPercents { gpu: 10, host: 100 }
    );
    assert_eq!(
        PoolPercents::clamped(100, 100),
        PoolPercents::FULL,
        "the neutral pair must survive the clamp unchanged",
    );
    assert_eq!(
        PoolPercents::clamped(PoolPercents::FLOOR, PoolPercents::FLOOR).gpu,
        PoolPercents::FLOOR,
        "the floor is offered, not clamped away",
    );
}

/// **On a unified adapter the lower percentage binds both shares**, because
/// there is one memory and both settings are taken of it.
#[test]
fn a_unified_adapter_takes_the_lower_of_the_two_percentages() {
    const POOL: u64 = 16 * 1024 * 1024 * 1024;
    let cap = Capacity::unified(POOL, None);
    for (gpu, host) in [(25u8, 75u8), (75, 25)] {
        let scaled = cap.scaled_to(PoolPercents { gpu, host });
        let lower = u64::from(gpu.min(host));
        assert_eq!(
            scaled.gpu_bytes,
            cap.gpu_bytes * lower / 100,
            "gpu {gpu} host {host}: the GPU share did not take the lower percent",
        );
        assert_eq!(
            scaled.host_bytes,
            cap.host_bytes.map(|bytes| bytes * lower / 100),
            "gpu {gpu} host {host}: the host share did not take the lower percent",
        );
    }
}

/// **A scaled unified capacity's two shares still cannot sum past its one
/// pool** — the property `Capacity::unified` is built for, held across the
/// scaling by `floor(a) + floor(b) <= floor(a + b)`.
///
/// Odd pools and odd percentages on purpose: an even split rounds exactly and
/// could not distinguish a correct floor from a wrong one.
#[test]
fn a_scaled_unified_adapters_two_shares_never_sum_past_its_scaled_pool() {
    for pool in [1u64, 3, 7, 1023, 86_197_547_521, u64::MAX / 4] {
        for percent in [PoolPercents::FLOOR, 33, 50, 67, 99, 100] {
            let cap = Capacity::unified(pool, None);
            let scaled = cap.scaled_to(PoolPercents {
                gpu: percent,
                host: percent,
            });
            let ceiling = u64::try_from(u128::from(pool) * u128::from(percent) / 100)
                .expect("the scaled pool fits a u64");
            let sum = scaled
                .gpu_bytes
                .saturating_add(scaled.host_bytes.unwrap_or(0));
            assert!(
                sum <= ceiling,
                "a {pool} B pool at {percent} % authorised {sum} B over {ceiling} B",
            );
        }
    }
}

/// **A split adapter's two percentages are independent**: lowering the GPU
/// side leaves the host figure exactly where it was, and the other way round.
#[test]
fn a_split_adapters_two_percentages_do_not_reach_each_other() {
    let cap = Capacity::measured(8 * 1024 * 1024 * 1024, Some(16 * 1024 * 1024 * 1024));
    let gpu_only = cap.scaled_to(PoolPercents { gpu: 25, host: 100 });
    assert_eq!(gpu_only.gpu_bytes, cap.gpu_bytes / 4);
    assert_eq!(gpu_only.host_bytes, cap.host_bytes);

    let host_only = cap.scaled_to(PoolPercents { gpu: 100, host: 25 });
    assert_eq!(host_only.gpu_bytes, cap.gpu_bytes);
    assert_eq!(host_only.host_bytes, cap.host_bytes.map(|bytes| bytes / 4));
}

/// **The percentage reaches the presumed arm too**, which is where every
/// browser and every unread native adapter is. `allowance()` does not apply
/// `NEED_FRACTION` there, so a percentage applied after it would have left
/// those users with an inert control.
#[test]
fn the_percentage_moves_the_presumed_arms_allowance() {
    let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
    assert_eq!(
        cap.allowance(),
        cap.gpu_bytes,
        "premise: the presumed arm's constant is its own allowance, so this \
         test is about the scaling and not about NEED_FRACTION",
    );
    let half = cap.scaled_to(HALF);
    assert_eq!(half.allowance(), cap.gpu_bytes / 2);
    assert!(half.allowance() < cap.allowance());
}

/// **`allowance()` itself is untouched on the presumed arm.** The
/// `NEED_FRACTION`-on-every-arm decision is deferred, not taken here: the
/// presumed constant is still its own allowance after any scaling.
#[test]
fn scaling_does_not_apply_the_need_fraction_to_a_presumed_figure() {
    for percent in [PoolPercents::FLOOR, 50, 100] {
        let cap = Capacity::presumed(&BudgetLimits::DESKTOP).scaled_to(PoolPercents {
            gpu: percent,
            host: percent,
        });
        assert_eq!(
            cap.allowance(),
            cap.gpu_bytes,
            "a presumed capacity at {percent} % took a fraction of its own \
             constant — the deferred NEED_FRACTION change landed by accident",
        );
    }
}

/// **The host percentage moves `host_allowance()` on a native profile whose
/// GPU reader answered nothing.** That is the whole of a machine with no
/// readable card: the capacity falls to the presumed arm carrying the host
/// pool beside it, and the RAM control has to work there or it does nothing
/// at all on a large class of Linux desktops.
#[test]
fn the_ram_percentage_moves_the_host_allowance_with_no_gpu_reader() {
    const POOL: u64 = 16 * 1024 * 1024 * 1024;
    let profile = DeviceProfile {
        // The reader answered nothing; the pool reader did.
        vram_bytes: None,
        host_pool_bytes: Some(POOL),
        ..desktop_profile()
    };
    let cap = profile.capacity();
    assert_eq!(
        cap.source,
        CapacitySource::Presumed,
        "premise: no GPU reader answered, so this is the presumed arm",
    );
    assert_eq!(cap.host_bytes, Some(POOL), "premise: the pool reader did");

    let full = cap
        .host_allowance()
        .expect("a host figure has an allowance");
    let half = cap
        .scaled_to(HALF)
        .host_allowance()
        .expect("scaling keeps the host figure");
    assert!(
        half < full,
        "50 % of a {POOL} B pool left the host allowance at {half} B against \
         {full} B — the RAM control is inert on the arm it matters most on",
    );
    assert_eq!(half, full / 2, "the halved allowance is not half");
    assert_eq!(
        cap.scaled_to(PoolPercents::FULL).host_allowance(),
        Some(full),
        "100 % moved a figure it must leave alone",
    );
}

/// **The percentage is inert on no arm, and it moves the caches it exists to
/// move.**
///
/// `fit::tile_cache_budget` and not an allowance, because an allowance nothing
/// spends is not a setting and the tile caches are the largest thing either
/// pool's economy buys.
///
/// **The two things counted are different and are never added.** A tile move
/// is `tile_cache_budget` itself falling; a budget move is the whole fitted
/// `Budgets` falling — the offscreen, the oversample, the loop span, the grid.
/// The arms differ in which one they can show, and that is a fact about
/// `tile_cache_budget`, not about the scaling: on `Measured` and `Probed` the
/// tile budget is a split of the *economy allowance* and so tracks the
/// capacity directly, while on `Presumed` and `Derived` it short-circuits to
/// the class rung's own constants, so on those arms the percentage reaches the
/// caches only when the ladder sheds a rung. Every arm must show at least one
/// of the two on at least one scene, which is what "inert on no arm" means.
///
/// Both directions everywhere: lowering never raises either quantity, and
/// 100 % is byte-identical to the capacity with no scaling applied at all —
/// the figure every session before this setting existed resolved.
///
/// Measured 2026-09-06 over the six arms × eleven scenes: `measured` 1 budget
/// move and no tile move (its 8 GiB economy is over the tile bracket's ceiling
/// at both percentages, so the bracket holds it); `probed` 1 and 7;
/// `presumed` 1 and 0; `presumed with a host pool` 1 and 0; `derived` 1 and 0;
/// `measured unified` 1 and 7. **The floor asserted below is one apiece, not
/// those counts** — the counts move when a scene or a bracket does, and
/// pinning them would make this a change-detector for `scene_table`.
#[test]
fn the_percentage_is_inert_on_no_capacity_arm_and_never_raises_a_budget() {
    let limits = BudgetLimits::DESKTOP;
    let profile = desktop_profile();
    let budget_for =
        |cap: &Capacity, scene: &Scene| -> (Budgets, crate::budget::TileCacheBudget, u64) {
            let budgets = fit::fit(scene, &profile, cap, stand_in_grid_bytes);
            let tiles = fit::tile_cache_budget(scene, &budgets, &limits, cap, stand_in_grid_bytes);
            // **The loop pool is counted here too, and it is the quantity the
            // presumed arm moves.** Until 2026-09-06 the loop-history rung was
            // the ladder's finest step and the one a halved presumed pool
            // reliably took, so a moved `Budgets` was evidence enough. Ruling 15
            // removed it, and the ladder that is left is coarse: on the presumed
            // desktop bracket every scene in `scene_table` either fits at both
            // percentages or bottoms out at the same floor at both. What the
            // setting still buys there is the pool the loops are planned from —
            // the balloon `LoopPool::plan` spends — so it joins the counted
            // quantities rather than the arm being declared inert.
            let pool = fit::loop_pool_bytes(scene, &budgets, cap, stand_in_grid_bytes);
            (budgets, tiles, pool)
        };

    for (arm, cap) in capacity_table() {
        let mut tiles_moved = 0usize;
        let mut budgets_moved = 0usize;
        let mut pool_moved = 0usize;
        for (scene_name, scene) in scene_table() {
            let (full_budgets, full, full_pool) =
                budget_for(&cap.scaled_to(PoolPercents::FULL), &scene);
            let (base_budgets, base, base_pool) = budget_for(&cap, &scene);
            assert_eq!(
                (full_budgets, full, full_pool),
                (base_budgets, base, base_pool),
                "{arm} / {scene_name}: 100 % moved the budgets — every existing \
                 user's session changes shape on upgrade",
            );

            // **A sweep, not one percentage.** Halving alone stopped
            // discriminating on the derived unified arm when ruling 15 took
            // the loop-history rung out of the ladder: measured 2026-09-06,
            // that arm's eleven scenes each either fit at both 100 % and
            // 50 % or stood at the ladder's floor at both, and their loop
            // pools were capped by the loops' own ceiling rather than by the
            // room, so nothing moved at 50 % and the arm read inert. Lower
            // percentages still reach it. Every one of them is held to the
            // same monotonicity, so this is a wider net and not a weaker one.
            for percent in [HALF, PoolPercents { gpu: 25, host: 25 }, LOWEST] {
                let (low_budgets, low, low_pool) = budget_for(&cap.scaled_to(percent), &scene);
                assert!(
                    low.styled_bytes <= base.styled_bytes
                        && low.parsed_bytes <= base.parsed_bytes
                        && low.terrain_bytes <= base.terrain_bytes,
                    "{arm} / {scene_name}: {} % of the pools RAISED a tile \
                     cache, {low:?} against {base:?}",
                    percent.gpu,
                );
                if low.styled_bytes < base.styled_bytes {
                    tiles_moved += 1;
                }
                if low_budgets != base_budgets {
                    budgets_moved += 1;
                }
                if low_pool != base_pool {
                    pool_moved += 1;
                }
            }
        }
        assert!(
            tiles_moved + budgets_moved + pool_moved > 0,
            "{arm}: halving both pools moved neither a tile cache, a fitted \
             budget nor the loop pool on any of the {} scenes, so this arm \
             would pass against a `scaled_to` that returned `self`",
            scene_table().len(),
        );
    }
}

/// The binder names the term actually in force, and equality falls through to
/// the weaker claim rather than the stronger one.
#[test]
fn the_binder_names_the_term_in_force() {
    assert_eq!(pool_binder(100, 100, 100), PoolBinder::Hardware);
    assert_eq!(pool_binder(100, 50, 50), PoolBinder::UserPercent);
    assert_eq!(pool_binder(100, 50, 25), PoolBinder::Governor);
    assert_eq!(
        pool_binder(100, 100, 25),
        PoolBinder::Governor,
        "a governor under an untouched percentage is the governor's doing",
    );
    assert_eq!(
        pool_binder(100, 50, 50),
        PoolBinder::UserPercent,
        "a governor sitting exactly on the user's line is not the reason the \
         line is there",
    );
    assert_eq!(
        pool_binder(0, 0, 0),
        PoolBinder::Hardware,
        "a pool nothing has priced is bound by nothing the user did",
    );
}

/// **A share nothing else has touched reads back as itself**, at every
/// percentage the control offers and on every arm.
///
/// This is why the figure rounds to nearest rather than flooring: the scaling
/// floors, so the exact reciprocal is a hair under the request, and a second
/// floor here would print every untouched setting one point low. That is the
/// commonest path there is, and the regression this test exists to hold.
#[test]
fn an_unbound_share_reads_back_as_the_number_the_user_set() {
    for (name, cap) in capacity_table() {
        for percent in PoolPercents::FLOOR..=100 {
            let scaled = cap.scaled_to(PoolPercents {
                gpu: percent,
                host: percent,
            });
            assert_eq!(
                effective_percent(cap.gpu_bytes, scaled.gpu_bytes),
                Some(percent),
                "{name}: a GPU share of {percent} % read back wrong",
            );
            if let (Some(hardware), Some(in_force)) = (cap.host_bytes, scaled.host_bytes) {
                assert_eq!(
                    effective_percent(hardware, in_force),
                    Some(percent),
                    "{name}: a host share of {percent} % read back wrong",
                );
            }
        }
    }
}

/// The plain arithmetic of the figure, at the ends and in the middle.
#[test]
fn the_effective_percentage_is_the_ratio_it_claims_to_be() {
    assert_eq!(effective_percent(1000, 1000), Some(100));
    assert_eq!(effective_percent(1000, 500), Some(50));
    assert_eq!(effective_percent(1000, 0), Some(0));
    assert_eq!(
        effective_percent(1000, 250),
        Some(25),
        "a governor at a quarter must read a quarter",
    );
    assert_eq!(
        effective_percent(0, 0),
        None,
        "a pool of nothing has no percentage to report",
    );
}

/// A pool at the top of `u64` does not wrap into a smaller one — the
/// direction that would hand out an allowance the machine cannot back.
#[test]
fn scaling_an_enormous_pool_does_not_wrap() {
    let cap = Capacity::measured(u64::MAX, Some(u64::MAX));
    let half = cap.scaled_to(HALF);
    assert_eq!(half.gpu_bytes, u64::MAX / 2);
    assert_eq!(half.host_bytes, Some(u64::MAX / 2));
    assert_eq!(cap.scaled_to(PoolPercents::FULL), cap);
}
