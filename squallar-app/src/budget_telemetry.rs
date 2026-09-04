//! **What the machine told the budget system, and what the budgets are**, as
//! one telemetry sentence.
//!
//! The `loop state:` line prices what the loops hold; this one names the
//! bracket and rung the whole budget set was resolved at, every host signal
//! the profile carries beside it, and the capacity those signals amounted to,
//! so a row can say *which machine* it was measured on and *which arm* the
//! budgets were fitted on without a log somebody kept. `resolve` spends none
//! of the signals — the class rung is the adapter's and the form factor's —
//! and what spends a reading is `fit`, through the capacity the floor crate
//! derives from them (`DeviceProfile::capacity`), which is why the line
//! prints that capacity and its source beside the raw readings.
//!
//! **Denominators, stated once, never added.** `bracket` is the compile-time
//! set (`Budgets::name`) and `rung` the promotion it was resolved at, `steps`
//! the ladder rungs `fit` shed to make the scene fit. `pool` is the **live**
//! loop pool — what the loops need, capped by the room the scene leaves, the
//! same figure `loop state:` prints in bytes, here in MiB — and `ceiling` is
//! the whole-application texture ceiling (`app_texture_ceiling_bytes`), the
//! bracket's constant and on the presumed arm the capacity itself.
//! `vram`, `ram` and `declared` come from three different sources (measured
//! VRAM, measured RAM, a browser's `deviceMemory` declaration) and are never
//! summed or read as one figure; `threads` is what the host reports, not what
//! any pool was built with. `linear` is the wasm page instance's heap over
//! the rasterization worker's — two instances, two ceilings, never one
//! figure. `cap` is the **capacity in force this session** — the measured
//! figure where the readings amount to one, the bracket's presumption where
//! they do not, held to what pressure has taught this session — and the
//! integer after it is how it was learned: 0 presumed, 1 measured, 2 probed.
//! `cap` is not `vram`: a unified-memory part's capacity is half its `ram`
//! with `vram` unread, a rasteriser's is the presumption with `vram` read.
//! `probe` is where the browser's WebGPU probe stands ([`gpu_probe_code`]):
//! carried here, on the level line, because the probe's own lines are said
//! once and the browser console's bounded ring evicts them within seconds,
//! so a scrape reading them as absent could not tell "evicted" from "never
//! ran". `balloon` is what the loops hold **above their base** — the bytes
//! the pool's planner spent on density past what `fit` charged, summed over
//! every loop, 0 when every loop holds its base or less; it is a subset of
//! `pool` and is never added to it.
//!
//! **After everything the rig reads**, in this order: `spare gpu <n> MiB host
//! <n> MiB`, what each pool has left for one more pane or layer, `none` where
//! the pool itself is unknown; then `live <page>/<worker> MiB`, what each wasm
//! instance's allocator is holding — the one host figure here that can FALL,
//! where `linear` only grows; then, LAST, one group per visible pane —
//! `pane<i> gpu <n> MiB host <n> MiB shared <n> MiB own <n> MiB`: what the
//! pane COSTS on each axis at the budgets in force
//! (`fit::need_terms_for_pane`), then what its stores HOLD, split by whether
//! another pane holds the same bytes — two families, priced and held, never
//! added to each other.
//!
//! **That order is a rule, not a history: fixed-width fields first, the
//! variable-arity group last.** Anything positioned BEHIND a group whose
//! length varies is what a positional reader cannot find — with N pane rows
//! in front of it, a fixed field sits at a different separator index on every
//! scene. So the pane rows go where nothing follows them, and a new fixed
//! field goes in front of them. Every byte figure is MiB by integer division,
//! because the rig's probe reads these sentences with `(\d+)` groups.
//!
//! Product telemetry, not a campaign instrument: it rides
//! `report_frame_telemetry`'s existing 2 s tick, and no figure it prints
//! gates CI.

use crate::platform::{GpuProbeReport, LinearMemory};
use squallar_device_profile::budget::{Budgets, DeviceProfile, FormFactor, Promotion};
use squallar_device_profile::scene::{Capacity, CapacitySource};

/// The integer the line prints for where the WebGPU probe stands: 0 absent
/// (every native bridge, or not asked yet), 1 skipped (a WebGL2 page),
/// 2 pending, 3 empty (ran, held nothing), 4 found at the device's refusal,
/// 5 found at the probe's own bound (`capped` — the figure is a floor). A
/// `cap N 2` beside 4 or 5 is the probe's figure in force; `cap 288 0`
/// beside 1 is a WebGL2 page on its presumption, never a probe that failed.
pub(crate) fn gpu_probe_code(report: GpuProbeReport) -> u8 {
    match report {
        GpuProbeReport::Absent => 0,
        GpuProbeReport::Skipped => 1,
        GpuProbeReport::Pending => 2,
        GpuProbeReport::Empty => 3,
        GpuProbeReport::Found(probe) if probe.capped => 5,
        GpuProbeReport::Found(_) => 4,
    }
}

/// The integer the line prints for how a capacity was learned: 0 presumed,
/// 1 measured, 2 probed — in ascending order of trust, so a reader who sorts
/// by it sorts by that.
pub(crate) fn capacity_source_code(source: CapacitySource) -> u8 {
    match source {
        CapacitySource::Presumed => 0,
        CapacitySource::Measured => 1,
        CapacitySource::Probed => 2,
    }
}

/// The same, as the word the prose log lines use.
pub(crate) fn capacity_source_word(source: CapacitySource) -> &'static str {
    match source {
        CapacitySource::Presumed => "presumed",
        CapacitySource::Measured => "measured",
        CapacitySource::Probed => "probed",
    }
}

/// The `budget state:` line.
///
/// Every field is always present, so a real zero is a real zero. `vram`,
/// `ram`, `declared`, `threads` and both `linear` figures print `0` for an
/// unread signal only because 0 is not a possible measurement of any of them
/// — no machine has zero bytes of RAM or zero threads, and a wasm instance's
/// heap is at least its initial pages — while `form` spells unknown
/// explicitly as 0 against 1 (handheld) and 2 (desktop). `cap` is never
/// unread: every session has a capacity in force, and the integer after it
/// says which arm it is ([`capacity_source_code`]). `probe` spells its own
/// absent explicitly as 0 ([`gpu_probe_code`]). `balloon` is 0 whenever no
/// loop holds more than its base, which is a real zero: nothing was granted.
///
/// **`heap max <page>/<worker>` is the only witness there is** to the ceiling
/// each wasm instance was actually constructed with. The page picks both per
/// device before the module is instantiated (`squallar-web/heap.js`), and no
/// engine implements `WebAssembly.Memory.prototype.type()`, so nothing can
/// read a maximum back off a memory object: a leg that wants to assert the
/// page came up at 512 MiB rather than 1024 has this field and nothing else.
/// Both print 0 natively and on any bridge that reported none, which is an
/// absence and not a wall of zero. It rides at the END, after `page heap
/// acts`, for that field's own reason: `drive.py`'s `budget_state_re` and
/// `native_row.py`'s copy of it read this line by positional groups and are
/// unanchored at their end, so a trailing field is the only safe place to add
/// one.
///
/// **The spare pair, the live pair and the pane rows ride after `heap max`**,
/// for the same reason: `readout` is the budget system's own readout
/// (`squallar_egui::shell_api::BudgetReadout`), the two pools' spare and then
/// one group per visible pane, appended where the rig's unanchored regex
/// cannot see them. `none` for a spare the session has no pool figure for —
/// spelled, because 0 is a real spare.
///
/// **`live <page>/<worker>` is the one host figure on the line that can
/// fall**, and it rides LAST. What each instance's allocator has handed out
/// and not been handed back (`squallar_alloc::live_bytes`), where `linear` is
/// `byteLength` and only grows. The same two-instance shape as `linear` and
/// `heap max`, never summed: the page's is read off this process's own
/// counter — on native the same counter, the same figure — and the worker's
/// is what it last said beside its `linear` reading, 0 until it has. 0 on the
/// page is a binary that never installed the counter, not an empty heap.
///
/// **The ordering rule for everything appended here: fixed-width fields
/// first, the variable-arity group LAST.** Anything positioned *behind* a
/// group whose length varies is what a positional reader cannot find — with
/// N pane rows in front of it, a fixed field sits at a different separator
/// index on every scene — so the variable group belongs where nothing is
/// behind it. `spare` and `live` are therefore in front of the `pane<i>`
/// rows, and a future fixed field goes in front of them too.
///
/// This corrects the rationale recorded when the pane rows landed, which had
/// the rule the wrong way round and put `spare` behind them; the pair moved
/// in front here. What makes the line appendable at all is that every field
/// is **self-describing by name**, so a reader searches for its label rather
/// than counting separators — the fixed-first ordering is the belt to that
/// braces, for anyone parsing positionally anyway.
///
/// A free function returning a `String` for the reason every other telemetry
/// sentence in this tree is one: `.github/browser-rig/drive.py` scrapes it
/// with a regex in another language in another directory, and
/// `the_rig_reads_the_budget_line_the_app_actually_writes` holds the two ends
/// together.
// Ten, and the line is the reason: every field on it is a different
// subsystem's reading, and bundling any two of them into a struct would make
// a type whose only purpose is to be destructured here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn budget_state_line(
    budgets: &Budgets,
    profile: &DeviceProfile,
    linear: Option<LinearMemory>,
    pool_bytes: usize,
    balloon_bytes: usize,
    cap: &Capacity,
    probe: GpuProbeReport,
    page_heap: crate::pressure::LinearMemoryWatch,
    readout: &squallar_egui::shell_api::BudgetReadout,
    page_live_bytes: Option<u64>,
) -> String {
    use std::fmt::Write as _;

    let mib = |bytes: u64| bytes / (1024 * 1024);
    let rung = match budgets.promotion {
        Promotion::Floor => 0,
        Promotion::Step => 1,
        Promotion::Ceiling => 2,
    };
    let form = match profile.form_factor {
        None => 0,
        Some(FormFactor::Handheld) => 1,
        Some(FormFactor::Desktop) => 2,
    };
    let mut line = format!(
        "budget state: bracket {}, rung {rung}, steps {}, pool {} MiB, ceiling {} MiB, \
         vram {} MiB, ram {} MiB, declared {} MiB, threads {}, form {form}, \
         linear {}/{} MiB, cap {} {}, probe {}, balloon {} MiB, \
         page heap acts {} at {} MiB, heap max {}/{} MiB",
        budgets.name,
        budgets.steps_back,
        mib(pool_bytes as u64),
        mib(budgets.app_texture_ceiling_bytes as u64),
        mib(profile.vram_bytes.unwrap_or(0)),
        mib(profile.system_ram_bytes.unwrap_or(0)),
        mib(profile.declared_ram_bytes.unwrap_or(0)),
        profile.parallelism.unwrap_or(0),
        mib(linear.map_or(0, |l| l.page_bytes)),
        mib(linear.and_then(|l| l.worker_bytes).unwrap_or(0)),
        mib(cap.gpu_bytes),
        capacity_source_code(cap.source),
        gpu_probe_code(probe),
        mib(balloon_bytes as u64),
        page_heap.acts(),
        mib(page_heap.last_acted_at().unwrap_or(0)),
        mib(linear.map_or(0, |l| l.page_max_bytes)),
        mib(linear.map_or(0, |l| l.worker_max_bytes)),
    );
    // **Fixed-width fields first, the variable-arity group last.** See the
    // note on this function: everything BEHIND a variable group is what a
    // positional reader cannot find.
    let spare = |pool: Option<&squallar_egui::shell_api::PoolReadout>| {
        pool.and_then(|pool| pool.spare_bytes)
            .map_or_else(|| "none".to_string(), |bytes| format!("{} MiB", mib(bytes)))
    };
    // Writing into a `String` cannot fail.
    let _ = write!(
        line,
        ", spare gpu {} host {}",
        spare(Some(&readout.gpu)),
        spare(readout.host.as_ref()),
    );
    let _ = write!(
        line,
        ", live {}/{} MiB",
        mib(page_live_bytes.unwrap_or(0)),
        mib(linear.and_then(|l| l.worker_live_bytes).unwrap_or(0)),
    );
    // Last on the line, and the only group whose LENGTH varies.
    for (i, pane) in readout.panes.iter().enumerate() {
        let _ = write!(
            line,
            ", pane{i} gpu {} MiB host {} MiB shared {} MiB own {} MiB",
            mib(pane.terms.gpu_bytes()),
            mib(pane.terms.host_bytes()),
            mib(pane.shared_bytes),
            mib(pane.own_bytes),
        );
    }
    line
}

/// The `overlay pictures:` line — what the whole-picture overlay rasters of
/// this frame's panes are sized at, per pane.
///
/// # Why the app says this rather than a harness computing it
///
/// `native_row.py` priced a pane's picture as `(W * 1.5) * ((H - 40) * 1.5) * 4`
/// and called the 40 "the top bar in points". The 40 is right: it is
/// `MIN_BAR_HEIGHT` (`squallar_egui`'s topbar), `2 * VERTICAL_MARGIN +
/// INTERACT_HEIGHT`, a **floor** the bar lays out on. What the model has no
/// term for is the display scale. On a headed X11 leg of 2026-09-02 winit
/// guessed 13/12 — its quantization of a scale factor to twelfths, a value
/// it guessed on those legs and not a property of the display — which puts
/// those 40 points at 43.33 physical pixels, so every scene D row read
/// `** INVALID **` by exactly 57,600 B — five texel rows.
///
/// **And the model was exact when it was written.** `run_measure.sh` records
/// it verified at three surfaces on 2026-08-31 — 1920x1080, and two web
/// canvases — and the formula still reproduces all three to the byte,
/// because all three ran at scale 1.0 where a point is a pixel. The
/// constants have not moved and neither has the bar; what differed between
/// the two dates is a scale factor nothing recorded. Every native row now
/// records it: `native_row.py` reads winit's own `Guessed window scale
/// factor:` line and prints `scale=` beside the geometry, `absent` where the
/// leg never said. `run_measure_native.sh` also pins
/// `WINIT_X11_SCALE_FACTOR=1`, which on X11 overrides winit's guess outright,
/// but the pin narrows the spread and the record is the remedy. Neither is
/// what makes this line the right source: a figure in points cannot predict
/// pixels without the scale of the surface, which a harness outside the app
/// does not see.
///
/// So this reports the size the app allocated. `px` lists every pane in
/// pane-index order; a pane with no overlay picture prints `0x0`, which is an
/// absence and not a pane of zero area. `bytes` is the RGBA total over that
/// list and is the figure a surface check compares a round's uploads against.
///
/// Re-said every telemetry period rather than emitted on change: a browser
/// console ring holds 1200 entries and a rig reads the last 60, so a line
/// that spoke once is indistinguishable from a run in which nothing was
/// rastered.
pub(crate) fn overlay_pictures_line(
    sizes: &[(u32, u32)],
    resident: (usize, u64),
    oversample_percent: u16,
) -> String {
    let px = sizes
        .iter()
        .map(|(w, h)| format!("{w}x{h}"))
        .collect::<Vec<_>>()
        .join(";");
    let bytes: u64 = sizes
        .iter()
        .map(|(w, h)| u64::from(*w) * u64::from(*h) * 4)
        .sum();
    // `resident` and `oversample` trail `bytes`, the field the rig's regex
    // ends on (`native_row.py`'s `OVERLAY_PICTURES_RE`, applied with
    // `search`), so the surface check reads exactly the fields it always
    // read and the two figures after them are additions rather than a
    // reinterpretation.
    //
    // **`n` and `resident` are different counts and are never the same
    // question.** `n` is PANES and `px` lists one size per pane, because a
    // surface check compares a bracket's uploaded bytes against the size the
    // app says a pane's picture is. `resident` is PICTURES — every pane's
    // every shown texture layer — because that is what crosses the page
    // heap, and it is the figure the host need model prices
    // (`squallar_device_profile::fit::NeedTerms::pictures_host`). One pane
    // showing thirteen layers is `n=1` and `resident 13`, and reading the
    // first as the second is how a scene needing 557 MiB of pictures was
    // fitted as though it needed 40.
    //
    // `oversample` is the ladder's rung in force, in percent per side, so a
    // leg reads which rung the pictures above were planned at rather than
    // inferring it from their sizes.
    format!(
        "overlay pictures: n={}, px={px}, bytes={bytes}, resident {} of {} B, \
         oversample {oversample_percent}",
        sizes.len(),
        resident.0,
        resident.1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_device_profile::budget::{
        AdapterCeilings, BudgetLimits, BudgetMemo, Platform, resolve,
    };
    use squallar_device_profile::fit::PaneTerms;
    use squallar_device_profile::quality::DeviceClass;
    use squallar_egui::shell_api::{BudgetReadout, PaneBudget, PoolReadout};

    /// The rig driver, read at compile time so a moved or deleted file is a
    /// build failure rather than a skipped test.
    const DRIVE_PY: &str = include_str!("../../.github/browser-rig/drive.py");

    /// The body of a `var <name> = /…/;` regex literal in `drive.py` — the
    /// same extraction the loop and frame line tests make.
    fn pattern(name: &str) -> String {
        let head = format!("var {name} = /");
        let at = DRIVE_PY.find(&head).unwrap_or_else(|| {
            panic!(
                "drive.py no longer declares `{head}…`; the rig's probe for the \
                 budget line moved and this test can no longer read it"
            )
        });
        let rest = &DRIVE_PY[at + head.len()..];
        let end = rest
            .find("/;")
            .expect("the regex literal is not closed on its own line");
        rest[..end].to_string()
    }

    /// The sentence a pattern describes, given what each capture group should
    /// capture, in order. Two group spellings — the bracket's word and plain
    /// `(\d+)` — plus the one escaped slash between the two `linear` figures;
    /// anything else regexy surviving the substitution fails the leftover
    /// check. That check is also what refuses an optional group: `(?:…)?`
    /// leaves a `?` and a `(` behind, and `native_row.py` `int()`s every group
    /// of every probe it shares, so a non-participating group there is a
    /// crash on the OLD binary's log.
    fn rendered(pattern: &str, groups: &[&str]) -> String {
        const GROUP_SPELLINGS: [&str; 2] = [r"([a-z0-9]+)", r"(\d+)"];
        let mut out = String::new();
        let mut rest = pattern;
        let mut values = groups.iter();
        while let Some((at, spelling)) = GROUP_SPELLINGS
            .iter()
            .filter_map(|g| rest.find(g).map(|at| (at, *g)))
            .min()
        {
            out.push_str(&rest[..at]);
            out.push_str(
                values
                    .next()
                    .expect("the pattern has more capture groups than values were offered"),
            );
            rest = &rest[at + spelling.len()..];
        }
        assert!(
            values.next().is_none(),
            "more values were offered than the pattern has capture groups",
        );
        out.push_str(rest);
        let out = out.replace(r"\/", "/");
        assert!(
            !out.contains(['\\', '[', ']', '*', '+', '?', '|', '^', '$', '(', ')']),
            "the pattern has a metacharacter outside its two known group \
             spellings, so substituting values into it no longer produces the \
             sentence it matches: {out:?}",
        );
        out
    }

    /// The live pool the line prints beside the bracket figures: 3 GiB, a
    /// value no shipped constant carries.
    const POOL: usize = 3 << 30;

    /// The balloon in force for the distinct line: 7 MiB, a figure no other
    /// position carries.
    const BALLOON: usize = 7 << 20;

    /// The capacity in force for the distinct line: a probed 5 GiB, which no
    /// profile produces and no other position carries, so the `cap` figure
    /// and its source code are each distinct from every neighbour. The
    /// profile below would measure 24 GiB and print `24576 1`, the same
    /// figure as `vram`; the line takes the capacity as its own argument
    /// exactly so the caller can hand in what the session holds — the
    /// presumption pressure lowered, not the raw reading.
    const CAP: Capacity = Capacity {
        gpu_bytes: 5 << 30,
        host_bytes: None,
        source: CapacitySource::Probed,
    };

    /// The probe report for the distinct line: found at the probe's own
    /// bound, which prints `5` — a code no other position carries.
    /// A page-heap watch that has never acted: the state every case below
    /// but `the_budget_line_carries_the_page_heaps_act_count` is about.
    /// The page's live bytes for the distinct line: 250 MiB, a figure no
    /// other position carries, beside the worker's 600 in [`distinct`].
    const LIVE: Option<u64> = Some(250 << 20);

    const WATCH: crate::pressure::LinearMemoryWatch =
        crate::pressure::LinearMemoryWatch::never_acted();

    /// A readout nothing has composed — no panes, no pool figures — which
    /// appends only the spare pair, both `none`. What every case below but
    /// `the_pane_rows_ride_after_everything_the_rig_reads` hands in.
    fn no_readout() -> BudgetReadout {
        BudgetReadout::default()
    }

    /// Two panes with a distinct figure in every position, and a spare on
    /// each pool no other position carries: 272 / 0 / 0 / 272 MiB on pane 0
    /// (a still 2D pane holding its own 256 MiB render and a 16 MiB frame),
    /// 33 / 41 / 16 / 17 MiB on pane 1, 3568 MiB of GPU spare, 601 of host.
    fn two_pane_readout() -> BudgetReadout {
        BudgetReadout {
            panes: vec![
                PaneBudget {
                    terms: PaneTerms {
                        static_rasters: 256 << 20,
                        loops: 16 << 20,
                        ..PaneTerms::default()
                    },
                    shared_bytes: 0,
                    own_bytes: 272 << 20,
                },
                PaneBudget {
                    terms: PaneTerms {
                        static_rasters: 33 << 20,
                        pictures_host: 41 << 20,
                        ..PaneTerms::default()
                    },
                    shared_bytes: 16 << 20,
                    own_bytes: 17 << 20,
                },
            ],
            gpu: PoolReadout {
                spare_bytes: Some(3568 << 20),
                ..PoolReadout::default()
            },
            host: Some(PoolReadout {
                spare_bytes: Some(601 << 20),
                ..PoolReadout::default()
            }),
            ..BudgetReadout::default()
        }
    }

    const PROBE: GpuProbeReport = GpuProbeReport::Found(crate::platform::ProbedCapacity {
        bytes: 5 << 30,
        failed_at: None,
        steps: 8,
        elapsed_ms: 1900,
        capped: true,
    });

    /// A profile with a distinct value in every position the line prints, so
    /// a transposed pair cannot read as a correct line. The ceiling is set
    /// directly rather than resolved, for the same reason, and the pool is
    /// [`POOL`] — the pool is the live one, handed in, not a budget field.
    fn distinct() -> (Budgets, DeviceProfile, Option<LinearMemory>) {
        let profile = DeviceProfile {
            platform: Platform::Native,
            limits: BudgetLimits::DESKTOP,
            class: DeviceClass::Integrated,
            adapter: AdapterCeilings {
                max_texture_dimension_2d: 16384,
                max_texture_dimension_3d: 8192,
            },
            vram_bytes: Some(24 << 30),
            system_ram_bytes: Some(64 << 30),
            declared_ram_bytes: Some(8 << 30),
            parallelism: Some(32),
            form_factor: Some(FormFactor::Desktop),
            linear_memory_max_bytes: None,
            memo: Some(BudgetMemo {
                loop_pool_bytes: None,
                steps_back: 3,
            }),
        };
        let budgets = Budgets {
            app_texture_ceiling_bytes: 3840 << 20,
            ..resolve(&profile)
        };
        assert_eq!(
            budgets.promotion,
            Promotion::Step,
            "an integrated GPU is the Step rung"
        );
        assert_eq!(budgets.steps_back, 3);
        let linear = Some(LinearMemory {
            page_bytes: 300 << 20,
            page_max_bytes: 900 << 20,
            worker_bytes: Some(700 << 20),
            worker_max_bytes: 1100 << 20,
            worker_live_bytes: Some(600 << 20),
        });
        (budgets, profile, linear)
    }

    /// The literal pin: every figure once, in the documented order, in MiB.
    /// `pool` is the live pool handed in, not the bracket ceiling the line
    /// once printed: a scene's loops are what it holds. `cap` is the capacity
    /// handed in and its source code, not a field of the profile; `probe` is
    /// the report handed in, as its code.
    #[test]
    fn the_budget_state_line_reads_exactly_as_pinned() {
        let (budgets, profile, linear) = distinct();
        assert_eq!(
            budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
            ),
            "budget state: bracket desktop, rung 1, steps 3, pool 3072 MiB, \
             ceiling 3840 MiB, vram 24576 MiB, ram 65536 MiB, declared 8192 MiB, \
             threads 32, form 2, linear 300/700 MiB, cap 5120 2, probe 5, \
             balloon 7 MiB, page heap acts 0 at 0 MiB, heap max 900/1100 MiB, \
             spare gpu none host none, live 250/600 MiB",
        );
        // The figure follows the pool it is handed, not a field of the budgets.
        assert!(
            budget_state_line(
                &budgets,
                &profile,
                linear,
                576 << 20,
                BALLOON,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
            )
            .contains(", pool 576 MiB,"),
        );
        // And the balloon follows what it is handed: a scene holding every
        // base and nothing more reads a real 0, last on the line.
        assert!(
            budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                0,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
            )
            .ends_with(
                ", probe 5, balloon 0 MiB, page heap acts 0 at 0 MiB, \
                 heap max 900/1100 MiB, spare gpu none host none, \
                 live 250/600 MiB"
            ),
        );
        // And the capacity follows what it is handed: this profile's own
        // measured 24 GiB reads `24576 1`, a session presumption lowered to
        // 3456 MiB reads `3456 0`.
        assert!(
            budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                &profile.capacity(),
                GpuProbeReport::Absent,
                WATCH,
                &no_readout(),
                LIVE,
            )
            .ends_with(
                ", cap 24576 1, probe 0, balloon 7 MiB, page heap acts 0 at 0 MiB, \
                 heap max 900/1100 MiB, spare gpu none host none, \
                 live 250/600 MiB"
            ),
        );
        let lowered = Capacity::presumed(&BudgetLimits::DESKTOP).held_to(Some(3456 << 20));
        assert!(
            budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                &lowered,
                GpuProbeReport::Skipped,
                WATCH,
                &no_readout(),
                LIVE,
            )
            .ends_with(
                ", cap 3456 0, probe 1, balloon 7 MiB, page heap acts 0 at 0 MiB, \
                 heap max 900/1100 MiB, spare gpu none host none, \
                 live 250/600 MiB"
            ),
        );
        assert_eq!(capacity_source_code(CapacitySource::Presumed), 0);
        assert_eq!(capacity_source_code(CapacitySource::Measured), 1);
        assert_eq!(capacity_source_code(CapacitySource::Probed), 2);
        assert_eq!(capacity_source_word(CapacitySource::Measured), "measured");
    }

    /// The probe codes, in the order the doc names them, and the one fact that
    /// splits `Found` in two: whose bound the figure stopped at.
    #[test]
    fn the_probe_code_names_every_state_the_bridge_can_be_in() {
        let found = |capped: bool| {
            GpuProbeReport::Found(crate::platform::ProbedCapacity {
                bytes: 4032 << 20,
                failed_at: (!capped).then_some(8128 << 20),
                steps: 7,
                elapsed_ms: 812,
                capped,
            })
        };
        assert_eq!(gpu_probe_code(GpuProbeReport::Absent), 0);
        assert_eq!(gpu_probe_code(GpuProbeReport::Skipped), 1);
        assert_eq!(gpu_probe_code(GpuProbeReport::Pending), 2);
        assert_eq!(gpu_probe_code(GpuProbeReport::Empty), 3);
        assert_eq!(gpu_probe_code(found(false)), 4);
        assert_eq!(gpu_probe_code(found(true)), 5);
        assert_eq!(GpuProbeReport::default(), GpuProbeReport::Absent);
        assert_eq!(found(false).bytes(), Some(4032 << 20));
        assert_eq!(GpuProbeReport::Empty.bytes(), None);
        assert!(!GpuProbeReport::Pending.is_settled());
        for settled in [
            GpuProbeReport::Absent,
            GpuProbeReport::Skipped,
            GpuProbeReport::Empty,
            found(true),
        ] {
            assert!(settled.is_settled(), "{settled:?}");
        }
    }

    /// **An unread signal prints 0, and every field is still there.** The
    /// sentinel is argued in the formatter's doc: 0 is not a possible
    /// measurement of RAM, VRAM, threads or a live heap. `form` carries its
    /// own explicit unknown, and `cap` is never unread — a profile with
    /// nothing read is on the presumed arm, and says so.
    #[test]
    fn an_unread_signal_prints_zero_and_no_field_is_dropped() {
        let profile = DeviceProfile::for_target();
        let budgets = Budgets {
            app_texture_ceiling_bytes: 3840 << 20,
            ..resolve(&profile)
        };
        let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
        let line = budget_state_line(
            &budgets,
            &profile,
            None,
            POOL,
            0,
            &cap,
            GpuProbeReport::Absent,
            WATCH,
            &no_readout(),
            None,
        );
        let (_, tail) = line
            .split_once(", vram ")
            .expect("the line carries a vram field");
        assert_eq!(
            tail,
            "0 MiB, ram 0 MiB, declared 0 MiB, threads 0, form 0, linear 0/0 MiB, \
             cap 3840 0, probe 0, balloon 0 MiB, page heap acts 0 at 0 MiB, \
             heap max 0/0 MiB, spare gpu none host none, live 0/0 MiB",
        );
        assert_eq!(
            line.matches(", ").count(),
            17,
            "eighteen comma-separated groups with no pane rows, seventeen separators: a \
             field was dropped or gained",
        );
    }

    /// The values the distinct line carries, in the rig's group order: the
    /// bracket word and the fifteen integers after it, the balloon last.
    const DISTINCT_GROUPS: [&str; 16] = [
        "desktop", "1", "3", "3072", "3840", "24576", "65536", "8192", "32", "2", "300", "700",
        "5120", "2", "5", "7",
    ];

    /// **The rig reads the budget line the app actually writes.** An extra
    /// space here turns the rig's whole budget reading into `null`, which a
    /// reader would take as "a bundle older than the line".
    ///
    /// A **prefix**, not an equality, and the difference is deliberate. The
    /// rig's `budget_state_re` is unanchored at its end, so the line may
    /// carry fields after `balloon` that the scraper does not read - the page
    /// heap's act count, which exists precisely because it must survive on a
    /// line the rig already parses, and now the two per-instance heap
    /// ceilings, which are the only witness there is to a maximum no engine
    /// will report. Both were appended rather than inserted, and the
    /// assertion below moved to match rather than the line being reshaped to
    /// keep it. What the seam still holds
    /// is everything that matters: every one of the sixteen groups the rig
    /// DOES read, in order, spelled the way it expects, with the tail pinned
    /// separately so it cannot drift unnoticed either.
    #[test]
    fn the_rig_reads_the_budget_line_the_app_actually_writes() {
        let (budgets, profile, linear) = distinct();
        let line = budget_state_line(
            &budgets,
            &profile,
            linear,
            POOL,
            BALLOON,
            &CAP,
            PROBE,
            WATCH,
            &no_readout(),
            LIVE,
        );
        let read_by_the_rig = rendered(&pattern("budget_state_re"), &DISTINCT_GROUPS);
        assert!(
            line.starts_with(&read_by_the_rig),
            "the `budget state:` line and the rig's probe have drifted:\n  app: {line}\n  rig: \
             {read_by_the_rig}",
        );
        assert_eq!(
            &line[read_by_the_rig.len()..],
            ", page heap acts 0 at 0 MiB, heap max 900/1100 MiB, spare gpu none host none, \
             live 250/600 MiB",
            "the tail the rig does not read drifted",
        );
    }

    /// **The pane rows and the spare pair ride after everything the rig
    /// reads.** The rig's `budget_state_re` — read off `drive.py` here, not
    /// copied — ends unanchored at `balloon`, so the sentence it describes is
    /// a prefix of the line whatever is appended, and the appended groups are
    /// pinned separately: one group per pane in pane order, gpu then host
    /// (priced) then shared then own (held), all MiB, then the two spares.
    /// The distinct fixture puts a different figure in every position so a
    /// transposition cannot read as correct; a pool prints `none` only where
    /// the pool itself is unknown, which the bare readout is.
    #[test]
    fn the_pane_rows_ride_after_everything_the_rig_reads() {
        let (budgets, profile, linear) = distinct();
        let read_by_the_rig = rendered(&pattern("budget_state_re"), &DISTINCT_GROUPS);
        let line = budget_state_line(
            &budgets,
            &profile,
            linear,
            POOL,
            BALLOON,
            &CAP,
            PROBE,
            WATCH,
            &two_pane_readout(),
            LIVE,
        );
        assert!(
            line.starts_with(&read_by_the_rig),
            "two pane rows moved a field the rig reads:\n  app: {line}\n  rig: {read_by_the_rig}",
        );
        assert_eq!(
            &line[read_by_the_rig.len()..],
            ", page heap acts 0 at 0 MiB, heap max 900/1100 MiB, \
             spare gpu 3568 MiB host 601 MiB, live 250/600 MiB, \
             pane0 gpu 272 MiB host 0 MiB shared 0 MiB own 272 MiB, \
             pane1 gpu 33 MiB host 41 MiB shared 16 MiB own 17 MiB",
        );
        // The rig's own guarantee, restated where the rows depend on it: no
        // `$` closes the pattern, and its last literal is the balloon.
        assert!(pattern("budget_state_re").ends_with(r"balloon (\d+) MiB"));

        // **The variable-arity group is LAST, and that is the rule.** Every
        // fixed field sits at a separator index that does not depend on how
        // many panes are open; anything placed behind the rows would not.
        // Asserted over a changing pane count, because one count cannot tell
        // "last" from "happens to be last here".
        for panes in 0..3 {
            let readout = BudgetReadout {
                panes: two_pane_readout().panes.into_iter().take(panes).collect(),
                ..two_pane_readout()
            };
            let line = budget_state_line(
                &budgets, &profile, linear, POOL, BALLOON, &CAP, PROBE, WATCH, &readout, LIVE,
            );
            let (fixed, _) = line.split_once(", pane0 ").unwrap_or((line.as_str(), ""));
            assert!(
                fixed.ends_with(", live 250/600 MiB"),
                "with {panes} pane row(s) the last fixed field is not `live`: {line}",
            );
            assert_eq!(
                line.matches(", pane").count(),
                panes,
                "the rows are not one per pane: {line}",
            );
        }
        // A pool with a figure but nothing spare prints a real zero, and a
        // pool with no figure prints its absence.
        let exhausted = BudgetReadout {
            gpu: PoolReadout {
                spare_bytes: Some(0),
                ..PoolReadout::default()
            },
            host: None,
            ..BudgetReadout::default()
        };
        assert!(
            budget_state_line(
                &budgets, &profile, linear, POOL, BALLOON, &CAP, PROBE, WATCH, &exhausted, LIVE,
            )
            .ends_with(", spare gpu 0 MiB host none, live 250/600 MiB"),
        );
    }

    /// The floor under the seam test above: `rendered` really can disagree.
    #[test]
    fn a_budget_line_that_drifted_by_one_space_is_not_accepted() {
        let (budgets, profile, linear) = distinct();
        let good = rendered(&pattern("budget_state_re"), &DISTINCT_GROUPS);
        assert!(
            budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
            )
            .starts_with(&good)
        );
        let drifted = good.replacen(" rung", "  rung", 1);
        assert_ne!(drifted, good, "the perturbation perturbed nothing");
        assert!(
            !budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
            )
            .starts_with(&drifted),
            "a line with one extra space compared equal to the real one, so the \
             seam test above cannot fail",
        );
    }

    /// **The budget line carries the page heap's act count, always.**
    ///
    /// The counter exists because every other trace of a page-heap action is
    /// evictable and was in fact evicted. `budget pressure:` is one
    /// `log::warn!` per action; the browser console ring turns over in
    /// seconds under frame telemetry and the rig reads its last 60 entries —
    /// on the Tier-2 `huge` legs of 2026-09-04 that window held 4.8 s of a
    /// 50 s leg. A search of every capture channel for `budget pressure:`
    /// came back empty on four passes whose pages sat at 993-1018 of 1024
    /// MiB, and an evicted line and an arm that never fired are the same
    /// empty search. This field is re-said every telemetry period, so the
    /// last tick of any leg answers it.
    ///
    /// It rides at the END, after `balloon`, because the rig's own scraper
    /// (`drive.py`'s `budget_state_re`) reads this line by positional groups
    /// and is unanchored at its end - the same contract the `overlay
    /// pictures:` line's trailing fields keep.
    #[test]
    fn the_budget_line_carries_the_page_heaps_act_count() {
        let profile = DeviceProfile::for_target();
        let budgets = resolve(&profile);
        let never = budget_state_line(
            &budgets,
            &profile,
            None,
            POOL,
            BALLOON,
            &CAP,
            PROBE,
            WATCH,
            &no_readout(),
            None,
        );
        assert!(
            never.ends_with(
                ", page heap acts 0 at 0 MiB, heap max 0/0 MiB, spare gpu none host none, \
                 live 0/0 MiB"
            ),
            "a watch that never acted must still print its zero: {never}",
        );

        // A watch that has acted twice, the second time at 1011 MiB - the
        // reading the `huge` legs sat at while every pressure line of theirs
        // was already out of every window.
        let mut watch = crate::pressure::LinearMemoryWatch::default();
        // The wall a desktop-classified browser is given, which is the bound
        // the module is linked with. A handheld's page is judged against 512
        // MiB and its worker against 256, which is exactly why the line now
        // ends by printing both walls.
        let max = squallar_device_profile::constants::WASM_LINEAR_MEMORY_MAX_BYTES;
        let _ = watch.observe(900 << 20, max, 0);
        let _ = watch.observe(1011 << 20, max, 0);
        assert_eq!(watch.acts(), 2);
        let acted = budget_state_line(
            &budgets,
            &profile,
            None,
            POOL,
            BALLOON,
            &CAP,
            PROBE,
            watch,
            &no_readout(),
            None,
        );
        assert!(
            acted.ends_with(
                ", page heap acts 2 at 1011 MiB, heap max 0/0 MiB, spare gpu none host none, \
                 live 0/0 MiB"
            ),
            "the act count and the mark are not both on the line: {acted}",
        );

        // The rig's scraper is unanchored at its end, which is what makes a
        // field after `balloon` safe. A pattern that grew a `$` would read
        // every line of every leg as absent, and an absent budget line is
        // reported as an older bundle rather than as a broken scraper.
        let rig = include_str!("../../.github/browser-rig/drive.py");
        assert!(
            rig.contains(r"probe (\d+), balloon (\d+) MiB/;"),
            "the rig's `budget_state_re` no longer ends unanchored at              `balloon`: a trailing field is only safe while it does",
        );
    }

    /// **The `overlay pictures:` line keeps its prefix and its field order.**
    ///
    /// A harness reads this positionally, and the whole reason it exists is
    /// that the harness previously MODELLED the figure and was quietly wrong.
    /// A rename or a reordering would null its reader — and a null that reads
    /// as a zero is how a modelled figure got believed in the first place. So
    /// the shape is pinned here: a rename reddens this board rather than the
    /// rig's row.
    #[test]
    fn the_overlay_pictures_line_keeps_its_prefix_and_field_order() {
        let line = overlay_pictures_line(&[(2880, 1555), (0, 0), (1440, 780)], (4, 29874400), 150);
        assert_eq!(
            line,
            "overlay pictures: n=3, px=2880x1555;0x0;1440x780, bytes=22406400, \
             resident 4 of 29874400 B, oversample 150",
        );
        // `resident` and the rung trail `bytes` — the field the rig's regex
        // ends on — so the scraper's positional groups are what they were,
        // and a leg that took a rung says so on the line rather than in the
        // sizes alone.
        let thinner = overlay_pictures_line(&[(2878, 1651)], (1, 19006312), 100);
        assert!(
            thinner.ends_with(", bytes=19006312, resident 1 of 19006312 B, oversample 100"),
            "{thinner}"
        );
        let rig = include_str!("../../.github/browser-rig/native_row.py");
        assert!(
            rig.contains(r#"OVERLAY_PICTURES_RE = re.compile(r"overlay pictures: n=(\d+), px=((?:\d+x\d+(?:;\d+x\d+)*)?), bytes=(\d+)")"#)
                && rig.contains("OVERLAY_PICTURES_RE.search("),
            "the rig's overlay-pictures regex moved or is no longer applied with \
             `search`: a trailing field after `bytes` is only safe while the \
             pattern is unanchored at its end",
        );
    }

    /// **`bytes` is the RGBA sum over the list, and a pane with no picture
    /// contributes nothing to it.** The figure is compared against bytes a
    /// round actually uploaded, so an absent pane counted as anything but
    /// zero would move an equality check.
    #[test]
    fn a_pane_with_no_picture_costs_nothing_and_is_still_listed() {
        let none = overlay_pictures_line(&[(0, 0)], (0, 0), 150);
        assert!(
            none.contains("n=1") && none.contains("px=0x0") && none.contains("bytes=0"),
            "a pane with no picture is not reported as an empty slot: {none}",
        );
        // Listed, not skipped: position in `px` IS the pane index.
        let mixed = overlay_pictures_line(&[(0, 0), (10, 10)], (1, 400), 150);
        assert!(
            mixed.contains("px=0x0;10x10"),
            "the empty pane was dropped from the list, so every pane after it \
             is reported under the wrong index: {mixed}",
        );
    }

    /// **An empty scene prints the absence rather than nothing.** No panes is
    /// a reading; a line that vanished would be indistinguishable from a
    /// period the scraper missed.
    #[test]
    fn a_scene_with_no_panes_still_says_so() {
        assert_eq!(
            overlay_pictures_line(&[], (0, 0), 150),
            "overlay pictures: n=0, px=, bytes=0, resident 0 of 0 B, oversample 150",
        );
    }

    /// **The byte total is `u64` arithmetic, and this is the floor under
    /// that.**
    ///
    /// NOT REACHABLE TODAY, and said so rather than dressed up: the pane grid
    /// stops at six and the largest picture is bounded by the adapter's
    /// texture side, so the real worst case is six at 8192 square = 1.61 GB,
    /// which fits a `u32` (4.29 GB) with room to spare. The first draft of
    /// this test asserted six pictures overflowed and was WRONG; its own
    /// "the case is not a case" guard caught it, which is the only reason
    /// this comment is accurate.
    ///
    /// So what is pinned here is the TYPE, not a live case: seventeen
    /// max-side pictures do overflow a `u32`, and the day the pane cap or the
    /// texture ceiling moves, the arithmetic must not wrap into a plausible
    /// small number under a harness that compares it for equality. Narrowing
    /// the sum to `u32` reddens this.
    #[test]
    fn a_byte_total_past_the_u32_ceiling_does_not_wrap() {
        let many = vec![(8192u32, 8192u32); 17];
        let expected = 17u64 * 8192 * 8192 * 4;
        assert!(
            expected > u64::from(u32::MAX),
            "the case is not a case: {expected} fits a u32",
        );
        assert!(
            overlay_pictures_line(&many, (0, 0), 150).contains(&format!("bytes={expected}")),
            "the byte total wrapped",
        );
    }
}
