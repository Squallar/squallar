use super::*;
use crate::budget::{BudgetLimits, DeviceProfile, resolve};
use crate::quality::DeviceClass;
use crate::scene::fixtures::{
    huge, plan_pane, scene_table, shipped_profile, stand_in_grid_bytes, two_panes_one_loop,
};
use crate::scene::{Capacity, PaneNeed, Pools, Scene, TileNeed};

const MIB: u64 = 1024 * 1024;
const HD: [u32; 2] = [1920, 1080];
const TWO_HOURS: usize = 2 * 60 * 60;
/// The WSR-88D precipitation cadence, measured.
const PRECIP: Option<u32> = Some(259);

fn desktop() -> crate::budget::Budgets {
    resolve(&shipped_profile(BudgetLimits::DESKTOP))
}

fn scene_of(panes: Vec<PaneNeed>) -> Scene {
    Scene {
        panes,
        ..Scene::empty()
    }
}

/// A split capacity with `gpu` and `host` of spare and nothing else bounding
/// it — the shape a discrete card's session carries.
fn split(gpu: u64, host: u64) -> Spare {
    Spare {
        gpu_bytes: Some(gpu),
        host_bytes: Some(host),
        joint_bytes: None,
    }
}

// ── The primitive ─────────────────────────────────────────────────────────

/// **Zero costs nothing and is always admitted**, on a pool that is empty and
/// on one that has nothing left. The second half is the load-bearing one: the
/// doors that *shed* go through the same seam as the doors that add, so a
/// refusal of a free act would make a scene that is already too large
/// impossible to reduce.
#[test]
fn a_free_act_is_admitted_even_on_an_empty_pool() {
    assert_eq!(verdict(split(0, 0), Increment::ZERO), Verdict::Admit);
    assert_eq!(
        verdict(
            Spare {
                gpu_bytes: Some(0),
                host_bytes: Some(0),
                joint_bytes: Some(0),
            },
            Increment::ZERO,
        ),
        Verdict::Admit,
    );
}

/// **Both arms of the compare, at the boundary.** Equal fits; one byte more
/// does not. Over-firing is the worse direction, so the admit arm is asserted
/// on the exact figure the refuse arm is one byte above.
#[test]
fn the_boundary_admits_what_exactly_fits_and_refuses_one_byte_more() {
    let spare = split(100, 100);
    assert_eq!(
        verdict(
            spare,
            Increment {
                gpu_bytes: 100,
                host_bytes: 100
            }
        ),
        Verdict::Admit,
        "a scene that exactly fills the spare fits",
    );
    assert_eq!(
        verdict(
            spare,
            Increment {
                gpu_bytes: 101,
                host_bytes: 0
            }
        ),
        Verdict::Refuse(Refusal {
            pool: Pool::Gpu,
            wanted_bytes: 101,
            spare_bytes: 100,
        }),
    );
    assert_eq!(
        verdict(
            spare,
            Increment {
                gpu_bytes: 0,
                host_bytes: 101
            }
        ),
        Verdict::Refuse(Refusal {
            pool: Pool::Host,
            wanted_bytes: 101,
            spare_bytes: 100,
        }),
    );
}

/// A pool this session has no figure for refuses nothing — the same reading
/// `fit::over` gives a capacity with no host allowance. `None` is "unknown",
/// never "empty": read the other way, a native arm no RAM reader answered on
/// would refuse every act.
#[test]
fn an_unknown_pool_is_never_over() {
    let huge_want = Increment {
        gpu_bytes: u64::MAX,
        host_bytes: u64::MAX,
    };
    assert_eq!(verdict(Spare::default(), huge_want), Verdict::Admit);
    assert_eq!(
        verdict(
            Spare {
                gpu_bytes: Some(0),
                host_bytes: None,
                joint_bytes: None,
            },
            Increment::host(u64::MAX),
        ),
        Verdict::Admit,
        "a host-only act against an unknown host pool",
    );
}

/// **One memory asks one question.** On a unified capacity the two increments
/// are summed and tested once, so an act that fits neither axis's share of the
/// partition but fits the pool is admitted — and an act that fits both shares
/// separately but not the pool together is refused. That second row is the
/// Framework 13: the picture terms are charged to the host axis alone while
/// the driver places them in memory shared with the compositor.
#[test]
fn a_unified_pool_sums_the_two_increments_and_tests_once() {
    let unified = Spare {
        gpu_bytes: Some(10),
        host_bytes: Some(10),
        joint_bytes: Some(20),
    };
    // Over the GPU half alone, inside the pool. Admitted — the partition is
    // this crate's arithmetic, not a fence the hardware keeps.
    assert_eq!(
        verdict(
            unified,
            Increment {
                gpu_bytes: 15,
                host_bytes: 5
            }
        ),
        Verdict::Admit,
    );
    // Inside both halves and inside the pool: admitted, so the row below is a
    // refusal the joint arm makes rather than one it makes of everything.
    assert_eq!(
        verdict(
            unified,
            Increment {
                gpu_bytes: 9,
                host_bytes: 9
            }
        ),
        Verdict::Admit,
    );
    // Inside both halves, over the pool. Refused, and the split reading two
    // lines down admits the same act.
    assert_eq!(
        verdict(
            unified,
            Increment {
                gpu_bytes: 11,
                host_bytes: 10
            }
        ),
        Verdict::Refuse(Refusal {
            pool: Pool::Joint,
            wanted_bytes: 21,
            spare_bytes: 20,
        }),
    );
    assert_eq!(
        verdict(
            split(10, 10),
            Increment {
                gpu_bytes: 10,
                host_bytes: 10
            }
        ),
        Verdict::Admit,
        "the same act on a SPLIT capacity of the same two halves is admitted \
         — which is the whole difference the joint arm makes",
    );
}

/// A refusal carries the arithmetic that produced it, so the notice can say
/// what the user would have to free rather than only that something failed.
#[test]
fn a_refusal_names_what_it_is_short_by() {
    let Verdict::Refuse(refusal) = verdict(split(4 * MIB, 0), Increment::host(10 * MIB)) else {
        panic!("10 MiB of host against no host spare must refuse");
    };
    assert_eq!(refusal.pool, Pool::Host);
    assert_eq!(refusal.short_bytes(), 10 * MIB);
}

// ── The pricing primitive ─────────────────────────────────────────────────

/// **An increment is a difference of the one model.** One more identical
/// looping pane costs exactly what the model says two of them cost less what
/// it says one does — asserted against `need_terms` directly, so a term that
/// moves in `fit` moves here on the same land.
#[test]
fn one_more_pane_costs_the_difference_the_model_prices() {
    let budgets = desktop();
    let one = scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]);
    let two = scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP); 2]);
    let want = increment(&one, &two, &budgets, stand_in_grid_bytes);

    let priced_one = crate::fit::need(&one, &budgets, stand_in_grid_bytes);
    let priced_two = crate::fit::need(&two, &budgets, stand_in_grid_bytes);
    assert_eq!(
        want,
        Increment {
            gpu_bytes: priced_two.gpu_bytes - priced_one.gpu_bytes,
            host_bytes: priced_two.host_bytes - priced_one.host_bytes,
        },
    );
    assert!(
        !want.is_zero(),
        "control: a second looping pane is not free, or the rows above prove \
         nothing",
    );
}

/// **An act that frees bytes prices at zero, never at a negative.** A
/// difference that could go negative would let one door's saving pay for
/// another door's growth inside a batch.
#[test]
fn an_act_that_frees_bytes_prices_at_zero() {
    let budgets = desktop();
    let two = scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP); 2]);
    let one = scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]);
    assert_eq!(
        increment(&two, &one, &budgets, stand_in_grid_bytes),
        Increment::ZERO,
    );
}

/// **Ruling 8, priced.** A second pane on a loop another pane already owns —
/// written into the prospective scene the way `App::loop_demand` writes an
/// alias — owes nothing at all on the GPU, where the loop's frames live. The
/// admit fixture that RESEMBLES the refuse one: the same second pane, its own
/// loop rather than a share, costs the whole loop.
#[test]
fn a_second_pane_on_a_shared_loop_owes_no_loop_bytes() {
    let budgets = desktop();
    let one = scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]);

    let aliased = increment(&one, &two_panes_one_loop(), &budgets, stand_in_grid_bytes);
    let own_loop = increment(
        &one,
        &scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP); 2]),
        &budgets,
        stand_in_grid_bytes,
    );

    // **An alias is not free — it owes its OWN cost**, which for a 2D pane is
    // its static render. What it owes nothing for is the loop, and the
    // difference between the two rows is exactly the loop term the owner
    // already pays.
    let loops_of =
        |scene: &Scene| crate::fit::need_terms(scene, &budgets, stand_in_grid_bytes).loops;
    let one_loop = loops_of(&one);
    assert_eq!(
        loops_of(&two_panes_one_loop()),
        one_loop,
        "a second pane on the same loop identity adds no loop frames at all",
    );
    assert_eq!(
        own_loop.gpu_bytes - aliased.gpu_bytes,
        loops_of(&scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP); 2])) - one_loop,
        "the whole GPU difference between an alias and its unshared twin is \
         the second loop's frames, and nothing else",
    );
    assert!(
        aliased.gpu_bytes > 0,
        "control: an alias still costs its own static render, so a zero here \
         would mean the model is pricing no panes at all",
    );
    assert!(
        aliased.host_bytes < own_loop.host_bytes,
        "an alias shares the site's decoded volumes too: {} vs {} host bytes",
        aliased.host_bytes,
        own_loop.host_bytes,
    );

    // And the verdict the two produce against a pool with room for one loop
    // and no more: the alias is ADMITTED, its unshared twin REFUSED.
    let spare = split(own_loop.gpu_bytes - 1, u64::MAX);
    assert!(
        aliased.gpu_bytes < own_loop.gpu_bytes,
        "control: the spare below has to sit between the two figures",
    );
    assert_eq!(verdict(spare, aliased), Verdict::Admit);
    assert!(
        !verdict(spare, own_loop).is_admit(),
        "the twin that owns its loop is refused at the same spare",
    );
}

/// **A preset is one increment, applied or refused whole.** The pane growth
/// and every layer it shows are summed before anything is asked, so a scene
/// at capacity refuses the preset rather than leaving half of it applied.
#[test]
fn a_preset_is_summed_before_it_is_asked() {
    let budgets = desktop();
    let before = scene_of(vec![plan_pane(HD, false, TWO_HOURS, None)]);
    let grown = scene_of(vec![plan_pane(HD, false, TWO_HOURS, None); 4]);
    let with_layers = scene_of(
        (0..4)
            .map(|_| PaneNeed {
                overlay_pictures: 3,
                picture_px: HD,
                ..plan_pane(HD, false, TWO_HOURS, None)
            })
            .collect(),
    );

    let panes_only = increment(&before, &grown, &budgets, stand_in_grid_bytes);
    let whole = increment(&before, &with_layers, &budgets, stand_in_grid_bytes);
    assert!(
        whole.host_bytes > panes_only.host_bytes,
        "the layers are the larger half of a preset: {} vs {}",
        whole.host_bytes,
        panes_only.host_bytes,
    );

    // A pool with room for the panes but not for the pictures. Asked whole,
    // the preset refuses; asked per toggle it would have admitted the growth
    // and then stalled, which is the half-applied preset.
    let spare = split(u64::MAX, panes_only.host_bytes);
    assert_eq!(verdict(spare, panes_only), Verdict::Admit);
    assert!(
        !verdict(spare, whole).is_admit(),
        "the whole preset must refuse where its first half would have been \
         admitted",
    );
}

// ── The frame-count converter ─────────────────────────────────────────────

/// [`LoopFrames::frames`] is `Budgets::frames_for_span_of`'s body, so the two
/// must agree on every row a slider can produce. A door that counted a span's
/// frames differently from the model would price a span change against frames
/// the loop never holds.
///
/// **Both halves of the pair, and both of the rulings that shaped them.** The
/// door was landed on 2026-09-06 beside WO-I, which took the compiled span
/// ceiling out of this arithmetic (ruling 13) and put the capacity's reachable
/// count in its place (ruling 15). The two lanes met here, so this checks the
/// request as well as the effective count, and checks that a lowered
/// `reachable` crosses the seam — a door reading the class figure while the
/// model read the measured one would admit a loop the machine cannot hold.
#[test]
fn the_frame_converter_is_the_budgets_own_arithmetic() {
    for limits in BudgetLimits::SHIPPED {
        let budgets = resolve(&shipped_profile(limits));
        let frames = LoopFrames::of(&budgets);
        for span_mins in [0usize, 5, 15, 45, 120, 240, 1440] {
            for cadence in [None, Some(0), Some(60), Some(259), Some(600)] {
                let span = span_mins * 60;
                assert_eq!(
                    frames.frames(span, cadence),
                    budgets.frames_for_span_of(span, cadence),
                    "{}: {span}s at {cadence:?}",
                    limits.name,
                );
                assert_eq!(
                    frames.requested(span, cadence),
                    budgets.frames_requested_for_span_of(span, cadence),
                    "{}: the REQUEST disagrees at {span}s at {cadence:?}",
                    limits.name,
                );
            }
        }

        // **Non-vacuity, and it is ruling 13's own row.** A span past this
        // bracket's own `loop_span_secs` used to be cut to it; if either side
        // still cut it, the two would agree on a figure neither the user nor
        // the model asked for. The whole 24 h at 259 s is 334 frames, above
        // every bracket's render budget, so what answers is the ceiling and
        // the request is what proves nothing shortened the span.
        let day = 24 * 60 * 60;
        assert_eq!(
            frames.requested(day, Some(259)),
            1 + day / 259,
            "{}: a day of lookback was shortened before it was converted",
            limits.name,
        );
        assert!(
            frames.requested(day, Some(259)) > frames.frames(day, Some(259)),
            "{}: nothing is clamped on this row, so it witnesses nothing",
            limits.name,
        );

        // **A lowered reachable count crosses the seam.** `LoopFrames::of`
        // reads it off the budgets, so a capacity measured too small for the
        // class figure holds the door's answer down with the model's.
        let measured = Budgets {
            loop_frames_reachable: MIN_LOOP_FRAMES_PER_PANE,
            ..budgets
        };
        assert_eq!(
            LoopFrames::of(&measured).frames(2 * 60 * 60, Some(259)),
            MIN_LOOP_FRAMES_PER_PANE,
            "{}: the door counted the class figure where the model counted \
             what the capacity reaches",
            limits.name,
        );
        assert_eq!(
            LoopFrames::of(&measured).frames(2 * 60 * 60, Some(259)),
            measured.frames_for_span_of(2 * 60 * 60, Some(259)),
            "{}",
            limits.name,
        );
    }
}

// ── The report: what the doors would refuse ───────────────────────────────

/// The Framework 13 that froze: an integrated part, 86.2 GiB of RAM, no
/// driver reading — so its capacity is `Pools::Unified` and its two needs
/// face one allowance.
fn framework_13() -> DeviceProfile {
    DeviceProfile {
        class: DeviceClass::Integrated,
        vram_bytes: None,
        system_ram_bytes: Some(862 * (1 << 30) / 10),
        host_pool_bytes: Some(4 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    }
}

/// The spare a session at `cap` has for `scene`, as
/// `App::compose_budget_readout` composes it — the model's own figure, with
/// no heap reading to bound it (this crate has none).
fn spare_for(scene: &Scene, budgets: &crate::budget::Budgets, cap: &Capacity) -> Spare {
    let need = crate::fit::need(scene, budgets, stand_in_grid_bytes);
    match cap.pools {
        Pools::Unified => Spare {
            gpu_bytes: Some(cap.allowance().saturating_sub(need.gpu_bytes)),
            host_bytes: cap
                .host_allowance()
                .map(|a| a.saturating_sub(need.host_bytes)),
            joint_bytes: Some(
                cap.joint_allowance()
                    .saturating_sub(need.gpu_bytes.saturating_add(need.host_bytes)),
            ),
        },
        Pools::Split => Spare {
            gpu_bytes: Some(cap.allowance().saturating_sub(need.gpu_bytes)),
            host_bytes: cap
                .host_allowance()
                .map(|a| a.saturating_sub(need.host_bytes)),
            joint_bytes: None,
        },
    }
}

/// One more pane like the widest one on screen, before any layer is shown on
/// it — the increment `Gui::set_pane_count` sums. Its site is the active
/// pane's, so its decoded volumes are already counted.
fn one_more_pane(scene: &Scene) -> Scene {
    let mut after = scene.clone();
    let seed = scene.panes.first().copied().unwrap_or(PaneNeed {
        overlay_pictures: 0,
        ..plan_pane([0, 0], false, 0, None)
    });
    after.panes.push(PaneNeed {
        overlay_pictures: 0,
        looping: false,
        volume_grids: 0,
        loop_scans_shared: true,
        loop_scans_resident_bytes: 0,
        loop_scans_resident_frames: 0,
        ..seed
    });
    for source in &mut after.tile_sources {
        // The glass the new pane covers wants tiles of its own; the working
        // set grows by one pane's share of what is on the glass now.
        source.tiles_on_glass += source.tiles_on_glass / scene.panes.len().max(1);
    }
    after
}

/// One more whole-picture overlay layer shown on pane 0 — the increment
/// `Gui::write_pane_overlay` sums.
fn one_more_layer(scene: &Scene) -> Scene {
    let mut after = scene.clone();
    if let Some(pane) = after.panes.first_mut() {
        pane.overlay_pictures += 1;
        if pane.picture_px == [0, 0] {
            pane.picture_px = HD;
        }
    } else {
        after.panes.push(PaneNeed {
            overlay_pictures: 1,
            picture_px: HD,
            ..plan_pane([0, 0], false, 0, None)
        });
    }
    after
}

/// **The advisory report: what admission would say to each door, on every
/// scene the profiles are fitted against, on the two capacities that matter.**
///
/// Printed rather than pinned by value: the point of the table is that the
/// verdicts are *derived* from `fit`'s own model, so pinning them here would
/// re-record the model rather than check it. What IS pinned is the pair of
/// properties below the table, which is what admission promises.
///
/// Run it with `--nocapture` to read the figures.
#[test]
fn what_admission_would_say_across_the_scene_table() {
    /// One arm of the report: a name, the profile, and how that arm learns
    /// its capacity.
    type Arm = (&'static str, DeviceProfile, fn(&DeviceProfile) -> Capacity);
    let arms: [Arm; 2] = [
        (
            "desktop, presumed",
            shipped_profile(BudgetLimits::DESKTOP),
            {
                fn presumed(p: &DeviceProfile) -> Capacity {
                    Capacity::presumed(&p.limits)
                }
                presumed
            },
        ),
        ("framework 13, unified", framework_13(), {
            fn measured(p: &DeviceProfile) -> Capacity {
                p.capacity()
            }
            measured
        }),
    ];

    let mut refused_rows = 0usize;
    let mut admitted_rows = 0usize;
    for (arm, profile, capacity_of) in arms {
        let cap = capacity_of(&profile);
        println!(
            "\n=== {arm}: {} pools, allowance {} MiB gpu / {} MiB host, joint {} MiB ===",
            match cap.pools {
                Pools::Unified => "one",
                Pools::Split => "two",
            },
            cap.allowance() / MIB,
            cap.host_allowance().unwrap_or(0) / MIB,
            cap.joint_allowance() / MIB,
        );
        for (name, scene) in scene_table() {
            let budgets = crate::fit::fit(&scene, &profile, &cap, stand_in_grid_bytes);
            let spare = spare_for(&scene, &budgets, &cap);
            let rows = [
                (
                    "one more pane",
                    increment(
                        &scene,
                        &one_more_pane(&scene),
                        &budgets,
                        stand_in_grid_bytes,
                    ),
                ),
                (
                    "one more layer",
                    increment(
                        &scene,
                        &one_more_layer(&scene),
                        &budgets,
                        stand_in_grid_bytes,
                    ),
                ),
            ];
            for (door, want) in rows {
                let v = verdict(spare, want);
                if v.is_admit() {
                    admitted_rows += 1;
                } else {
                    refused_rows += 1;
                }
                println!(
                    "  {name:52} | {door:14} | gpu {:>6} MiB host {:>6} MiB | spare gpu {:>6} host {:>6} joint {:>7} | {}",
                    want.gpu_bytes / MIB,
                    want.host_bytes / MIB,
                    spare.gpu_bytes.map_or(-1i64, |b| (b / MIB) as i64),
                    spare.host_bytes.map_or(-1i64, |b| (b / MIB) as i64),
                    spare.joint_bytes.map_or(-1i64, |b| (b / MIB) as i64),
                    match v {
                        Verdict::Admit => "ADMIT".to_string(),
                        Verdict::Refuse(r) => format!(
                            "REFUSE {:?} short {} MiB",
                            r.pool,
                            r.short_bytes().div_ceil(MIB)
                        ),
                    },
                );
            }
        }
    }
    assert!(
        admitted_rows > 0 && refused_rows > 0,
        "control: the table produced {admitted_rows} admits and {refused_rows} \
         refusals — a table that is all one verdict is measuring nothing",
    );
}

/// **The `huge` leg, door by door.** The scene that OOM-traps a browser tab in
/// ten seconds, priced against the wasm bracket's own capacity, with the two
/// acts a user can take on it next.
#[test]
fn what_admission_would_say_on_the_huge_leg() {
    let profile = shipped_profile(BudgetLimits::WASM);
    let cap = Capacity::presumed(&profile.limits);
    let scene = huge(13);
    let budgets = crate::fit::fit(&scene, &profile, &cap, stand_in_grid_bytes);
    let spare = spare_for(&scene, &budgets, &cap);
    let need = crate::fit::need(&scene, &budgets, stand_in_grid_bytes);

    println!(
        "\n=== huge(13) on wasm, rung {} ===\n  need gpu {} MiB host {} MiB against allowance gpu {} MiB host {} MiB\n  spare gpu {:?} MiB host {:?} MiB",
        budgets.steps_back,
        need.gpu_bytes / MIB,
        need.host_bytes / MIB,
        cap.allowance() / MIB,
        cap.host_allowance().unwrap_or(0) / MIB,
        spare.gpu_bytes.map(|b| b / MIB),
        spare.host_bytes.map(|b| b / MIB),
    );
    for (door, after) in [
        ("one more pane", one_more_pane(&scene)),
        ("a fourteenth layer", one_more_layer(&scene)),
    ] {
        let want = increment(&scene, &after, &budgets, stand_in_grid_bytes);
        println!(
            "  {door:20} | gpu {} MiB host {} MiB | {:?}",
            want.gpu_bytes / MIB,
            want.host_bytes / MIB,
            verdict(spare, want),
        );
    }

    // The fourteenth layer is what the leg's own allocation failures were
    // asking for, and it is the row the door exists to answer.
    let fourteenth = increment(
        &scene,
        &one_more_layer(&scene),
        &budgets,
        stand_in_grid_bytes,
    );
    assert!(
        fourteenth.host_bytes > 0,
        "control: a fourteenth picture on the user's canvas is not free",
    );
}

/// **The tile working set is charged to a new pane.** A pane opened onto a
/// map costs tiles as well as its own render, and a door that priced only the
/// render would admit a split that doubles the largest host term on the
/// scene.
#[test]
fn a_new_pane_is_charged_for_the_tiles_it_puts_on_the_glass() {
    let budgets = desktop();
    let with_tiles = Scene {
        panes: vec![plan_pane(HD, false, TWO_HOURS, None)],
        tile_sources: vec![TileNeed {
            tiles_on_glass: 192,
            ancestor_net: 6,
            bytes_per_tile: 1_462_708,
        }],
        ..Scene::empty()
    };
    let without = scene_of(vec![plan_pane(HD, false, TWO_HOURS, None)]);

    let tiled = increment(
        &with_tiles,
        &one_more_pane(&with_tiles),
        &budgets,
        stand_in_grid_bytes,
    );
    let bare = increment(
        &without,
        &one_more_pane(&without),
        &budgets,
        stand_in_grid_bytes,
    );
    assert!(
        tiled.host_bytes > bare.host_bytes,
        "one more pane on a tiled map must cost more than one on a bare \
         scene: {} vs {} host bytes",
        tiled.host_bytes,
        bare.host_bytes,
    );
}

// ── Pricing a loop at the listing, not at the arm ──────────────────────────

/// The lookback the app ships as its default (`Gui`'s `loop_lookback_secs`),
/// which is what the Tier-2 `long` leg's seed inherits by naming none.
const DEFAULT_LOOKBACK_SECS: usize = 3600;

/// **KTLX at VCP 212, precipitation**, measured 2026-09-07: 16 volumes from
/// 06:24:07Z to 07:21:26Z, median gap 230 s.
const PRECIP_CADENCE: Option<u32> = Some(230);

/// **KTLX at VCP 35, clear air**, measured the same day: 8-9 volumes inside
/// 03:02Z-05:31Z, median gap 422 s.
const CLEAR_AIR_CADENCE: Option<u32> = Some(422);

/// The reserve one frame of a radar loop is priced at until its volume lands.
const RESERVE: u64 = crate::constants::LOOP_SCAN_RESERVE_BYTES;

fn wasm() -> crate::budget::Budgets {
    resolve(&shipped_profile(BudgetLimits::WASM))
}

/// **What arming one plan-view radar loop costs**, through [`increment`] —
/// this module's only pricing primitive, so the test cannot come to price a
/// loop differently from the door.
fn arm_cost(span_secs: usize, cadence: Option<u32>) -> Increment {
    let budgets = wasm();
    let before = scene_of(vec![plan_pane(HD, false, span_secs, cadence)]);
    let after = scene_of(vec![plan_pane(HD, true, span_secs, cadence)]);
    increment(&before, &after, &budgets, stand_in_grid_bytes)
}

/// **A door with no cadence has no price, and says so.**
///
/// The defect this closes, in one row. [`LoopFrames::requested`] answers
/// [`LoopFrames::ceiling`] for `cadence_secs: None` — right for a frame list,
/// wrong for a door — and the arm-loop door consumed that answer, so on the
/// web bracket every loop was priced at fourteen frames before any listing
/// existed. [`LoopFrames::priceable`] refuses to answer instead.
#[test]
fn a_loop_with_no_cadence_yet_is_not_priceable_and_is_not_guessed_at() {
    for limits in BudgetLimits::SHIPPED {
        let budgets = resolve(&shipped_profile(limits));
        let frames = LoopFrames::of(&budgets);
        assert_eq!(
            frames.priceable(DEFAULT_LOOKBACK_SECS, None),
            None,
            "{}: a door was handed a frame count for a site nobody has listed",
            limits.name,
        );
        assert_eq!(
            frames.priceable(DEFAULT_LOOKBACK_SECS, Some(0)),
            None,
            "{}: a zero cadence converts no span and is not a cadence",
            limits.name,
        );
        assert!(
            !prices_a_loop(None) && !prices_a_loop(Some(0)) && prices_a_loop(Some(230)),
            "{}: the predicate and the converter disagree about what is \
             priceable",
            limits.name,
        );

        // **Non-vacuity**: the old spelling really does answer on this row,
        // and really does answer the ceiling rather than the span. If it ever
        // stops, `priceable`'s `None` witnesses nothing.
        assert_eq!(
            frames.requested(DEFAULT_LOOKBACK_SECS, None),
            frames.frames(DEFAULT_LOOKBACK_SECS, None),
            "{}: the branch this guards is gone",
            limits.name,
        );
        assert!(
            frames.frames(DEFAULT_LOOKBACK_SECS, None) >= MIN_LOOP_FRAMES_PER_PANE,
            "{}",
            limits.name,
        );

        // And where a cadence IS known, `priceable` is the model's own answer
        // and nothing else - it adds no clamp of its own.
        for cadence in [PRECIP_CADENCE, CLEAR_AIR_CADENCE, Some(60), Some(600)] {
            assert_eq!(
                frames.priceable(DEFAULT_LOOKBACK_SECS, cadence),
                Some(budgets.frames_for_span_of(DEFAULT_LOOKBACK_SECS, cadence)),
                "{}: at {cadence:?}",
                limits.name,
            );
        }
    }
}

/// **The two arms, on the bracket and the scene that trapped the tab.**
///
/// One hour of lookback on the web bracket, at the two cadences KTLX was
/// measured at hours apart on 2026-09-07. The cadence is the only input that
/// differs, and it moves the price by 400 MiB.
///
/// * **Precipitation, 230 s.** `1 + 3600/230 = 16` frames *available* in the
///   listing, which the bracket's render budget then caps to **14** — two
///   different numbers, and the one the door consumes is the cap.
/// * **Clear air, 422 s.** `1 + 3600/422 = 9` frames, **under** the cap, so
///   nothing clamps and the loop costs what the span asks for.
///
/// Today's door prices both at 14. That is right by coincidence on the first
/// and **1.56x over** on the second, and the band between the two prices is
/// the range of spares where the door refuses a loop that would have fitted.
#[test]
fn the_web_bracket_prices_a_precipitation_loop_above_a_clear_air_one() {
    let frames = LoopFrames::of(&wasm());

    let precip = frames
        .priceable(DEFAULT_LOOKBACK_SECS, PRECIP_CADENCE)
        .expect("a listed cadence is priceable");
    let clear_air = frames
        .priceable(DEFAULT_LOOKBACK_SECS, CLEAR_AIR_CADENCE)
        .expect("a listed cadence is priceable");

    // The listing's own count, before the budget's cap: the two are different
    // things and a report naming one for the other is ambiguous.
    assert_eq!(
        frames.requested(DEFAULT_LOOKBACK_SECS, PRECIP_CADENCE),
        16,
        "the precipitation window lists 16 volumes",
    );
    assert_eq!(precip, 14, "the render budget caps the listing's 16 to 14");
    assert_eq!(
        frames.requested(DEFAULT_LOOKBACK_SECS, CLEAR_AIR_CADENCE),
        9,
        "clear air asks for 9",
    );
    assert_eq!(clear_air, 9, "9 is under the cap, so nothing clamps it");
    assert!(
        clear_air < precip,
        "the slower site must price lower, or the cadence is not being read",
    );

    // The prices, through the model. A loop displaces the still its pane was
    // parked at, so the increment is one frame's reserve short of the whole.
    let precip_cost = arm_cost(DEFAULT_LOOKBACK_SECS, PRECIP_CADENCE);
    let clear_air_cost = arm_cost(DEFAULT_LOOKBACK_SECS, CLEAR_AIR_CADENCE);
    assert_eq!(
        precip_cost.host_bytes,
        precip as u64 * RESERVE - RESERVE,
        "14 frames less the still they displace",
    );
    assert_eq!(
        precip_cost.host_bytes / MIB,
        1040,
        "the figure the Tier-2 `long` leg logged, to the MiB",
    );
    assert_eq!(clear_air_cost.host_bytes / MIB, 640);

    // **The phantom, priced.** What the door charged before it read a cadence.
    let unpriced = arm_cost(DEFAULT_LOOKBACK_SECS, None);
    assert_eq!(
        unpriced.host_bytes, precip_cost.host_bytes,
        "the no-cadence branch prices at the cap, which is the precipitation \
         price - right by coincidence, on one of the two arms",
    );
    assert!(
        unpriced.host_bytes > clear_air_cost.host_bytes,
        "the phantom must over-price the slow site, or there was no defect",
    );
    assert_eq!(
        unpriced.host_bytes - clear_air_cost.host_bytes,
        400 * MIB,
        "the clear-air over-price, which is what a door would have refused on",
    );
}

/// **The verdict, on both arms, across the band the phantom created.**
///
/// The band is `640 MiB < spare < 1040 MiB`: a session with that much host
/// spare can hold a clear-air loop and cannot hold a precipitation one, and
/// the whole value of reading the cadence is that the two now answer
/// differently there.
///
/// **Over-firing is the worse direction**, so the clear-air row is the one
/// that matters: a door that refuses a scene which would have fitted is a
/// worse product than one that refuses nothing.
#[test]
fn inside_the_band_the_cadence_decides_the_verdict_and_the_phantom_refused_both() {
    let frames = LoopFrames::of(&wasm());
    let precip_cost = arm_cost(DEFAULT_LOOKBACK_SECS, PRECIP_CADENCE);
    let clear_air_cost = arm_cost(DEFAULT_LOOKBACK_SECS, CLEAR_AIR_CADENCE);
    let unpriced = arm_cost(DEFAULT_LOOKBACK_SECS, None);

    // A spare inside the band, with the GPU pool wide open so the host is
    // unambiguously what answers.
    let inside = split(u64::MAX, 800 * MIB);
    assert!(
        !verdict(inside, precip_cost).is_admit(),
        "a precipitation loop does not fit 800 MiB and must be refused",
    );
    assert_eq!(
        verdict(inside, clear_air_cost),
        Verdict::Admit,
        "a clear-air loop DOES fit 800 MiB - refusing it is the over-fire \
         this whole change exists to stop",
    );
    assert!(
        !verdict(inside, unpriced).is_admit(),
        "the phantom refused the clear-air loop too, which is the defect",
    );

    // **The leg's own spare, and its refusal was correct.** 292 MiB was
    // measured on the failing Tier-2 `long` leg (Chromium, page instance,
    // 2026-09-07). Only four frames fit there, so BOTH cadences are over and
    // the refusal that run logged was right - the number behind it was the
    // thing that was wrong, not the answer.
    let leg = split(u64::MAX, 292 * MIB);
    assert!(!verdict(leg, precip_cost).is_admit());
    assert!(
        !verdict(leg, clear_air_cost).is_admit(),
        "at the leg's spare even the slow site is over, so this run's \
         refusal was not itself a phantom",
    );

    // Above the band nothing is refused, which is what keeps the door from
    // being a wall: the desktop brackets are not this tight.
    let roomy = split(u64::MAX, 2048 * MIB);
    assert_eq!(verdict(roomy, precip_cost), Verdict::Admit);
    assert_eq!(verdict(roomy, clear_air_cost), Verdict::Admit);

    // **What span WOULD fit the leg's 292 MiB**, at the precipitation
    // cadence: the frames its spare buys, and the lookback they cover. The
    // door is only as useful as the setting it points the reader at.
    let fits = 1 + (292 * MIB / RESERVE) as usize;
    assert_eq!(
        fits, 4,
        "292 MiB of spare buys three frames beside the still"
    );
    let span_that_fits = (fits - 1) * PRECIP_CADENCE.unwrap() as usize;
    assert_eq!(span_that_fits, 690, "11.5 minutes, not the default hour");
    assert!(
        frames.priceable(span_that_fits, PRECIP_CADENCE) == Some(fits),
        "the span and the frame count must round-trip through the converter",
    );
    assert_eq!(
        arm_cost(span_that_fits, PRECIP_CADENCE).host_bytes,
        3 * RESERVE,
        "and it fits inside 292 MiB",
    );
    assert_eq!(
        verdict(leg, arm_cost(span_that_fits, PRECIP_CADENCE)),
        Verdict::Admit,
    );
}
