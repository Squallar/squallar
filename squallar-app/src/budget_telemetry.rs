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
//! integer after it is how it was learned: 0 presumed, 1 derived, 2 measured,
//! 3 probed. `cap` is not `vram`: a unified-memory part's capacity is half the
//! host pool with `vram` unread — **derived, not measured**, and it prints 1 —
//! and a rasteriser's is the presumption with `vram` read.
//! `probe` is where the browser's WebGPU probe stands ([`gpu_probe_code`]):
//! carried here, on the level line, because the probe's own lines are said
//! once and the browser console's bounded ring evicts them within seconds,
//! so a scrape reading them as absent could not tell "evicted" from "never
//! ran". `balloon` is what the loops hold **above their base** — the bytes
//! the pool's planner spent on density past what `fit` charged, summed over
//! every loop, 0 when every loop holds its base or less; it is a subset of
//! `pool` and is never added to it.
//!
//! **`loop over` and `loop clamped` are the two figures ruling 15 and ruling
//! 13 leave behind, and neither is a subset of anything above.** `loop over`
//! is what the loop plan charges ABOVE `pool` — non-zero only where every
//! loop stands at its base and the bases together do not fit, which is a
//! scene the ladder had nothing left to shed for and an admission door is to
//! refuse. It is not `balloon`'s opposite: `balloon` is bytes held above the
//! bases and this is bytes asked for beyond the pool, and a plan has exactly
//! one of them. `loop clamped` counts the visible panes whose loop was held
//! below what its span asked for
//! (`squallar_egui::shell_api::PaneBudget::loop_span_clamped`) — the
//! effective-beside-requested pair, counted rather than listed, since the
//! per-pane figures ride the readout the settings screen paints.
//!
//! **After everything the rig reads**, in this order: `spare gpu <n> MiB host
//! <n> MiB`, what each pool has left AT THE RUNG IN FORCE, `none` where the
//! pool itself is unknown; then `door spare gpu <n> MiB host <n> MiB joint
//! <n> MiB`, the same allowances less what the scene would cost with every
//! rung shed - **the figure an admission door actually compares against**,
//! and the only one that explains a refusal. The two are never the same
//! question and are never subtracted from each other; `joint` is `none` on
//! every split capacity and is the ONLY one a unified capacity's doors read.
//! Then `admission asked <n> admitted <n> would
//! refuse <n> refused <n>`, the scene-changing doors' running totals from
//! boot — `would refuse` is what they priced and would have turned away,
//! `refused` what they actually did; then `live <page>/<worker> MiB`, what each wasm
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
//!
//! **That tick is also what composes the readout this line prints.** The
//! pane rows and the spare pair are levels, not running totals, so the one
//! consumer sets the cadence and the frame path prices nothing for them —
//! `App::refresh_budget_readout` is called from the same tick, immediately
//! before this line, and `App::compose_budget_readout` says why.

use crate::platform::{GpuProbeReport, LinearMemory};
use squallar_device_profile::budget::{Budgets, DeviceProfile, FormFactor, Promotion};
use squallar_device_profile::scene::{Capacity, CapacitySource};

/// The integer the line prints for where the browser probe stands: 0 absent
/// (every native bridge, or not asked yet), 1 skipped (a backend no probe
/// covers), 2 pending, 3 empty (ran, reached no figure), 4 found at the
/// device's refusal, 5 found at the WebGPU probe's own bound (`capped` —
/// the figure is a floor), 6 silent to the cap (a WebGL2 walk that reached
/// its policy cap with nothing refused: unmeasured, and the presumption
/// stands). A `cap N 3` beside 4 or 5 is the probe's figure in force;
/// `cap 288 0` beside 3 or 6 is a WebGL2 page on its presumption.
pub(crate) fn gpu_probe_code(report: GpuProbeReport) -> u8 {
    match report {
        GpuProbeReport::Absent => 0,
        GpuProbeReport::Skipped => 1,
        GpuProbeReport::Pending => 2,
        GpuProbeReport::Empty => 3,
        GpuProbeReport::Found(probe) if probe.capped => 5,
        GpuProbeReport::Found(_) => 4,
        GpuProbeReport::SilentToCap { .. } => 6,
    }
}

/// The integer the line prints for how a capacity was learned: 0 presumed,
/// 1 derived, 2 measured, 3 probed — **in ascending order of trust, so a
/// reader who sorts by it sorts by that**, which is why the derived arm took
/// the rung between the bracket's constant and a reading rather than being
/// appended past both. It carries this machine's own RAM figure, so it says
/// more than a compiled constant; no API was asked about the GPU, so it says
/// less than a reading. Nothing outside this crate parses the code.
pub(crate) fn capacity_source_code(source: CapacitySource) -> u8 {
    match source {
        CapacitySource::Presumed => 0,
        CapacitySource::Derived => 1,
        CapacitySource::Measured => 2,
        CapacitySource::Probed => 3,
    }
}

/// The same, as the word the prose log lines use.
pub(crate) fn capacity_source_word(source: CapacitySource) -> &'static str {
    match source {
        CapacitySource::Presumed => "presumed",
        CapacitySource::Derived => "derived",
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
/// (`squallar_egui::shell_api::BudgetReadout`), composed by this line's own
/// tick just before the call (`App::refresh_budget_readout`) — the two pools'
/// spare and then one group per visible pane, appended where the rig's
/// unanchored regex cannot see them. `none` for a spare the session has no
/// pool figure for — spelled, because 0 is a real spare.
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
/// **`host steps/promotions/churn` is the recovery governor's always-on
/// counter set** ([`crate::recovery`]): steps of host ceiling held now,
/// promotions ever, and **promotions this session's own margin got wrong** —
/// a squeeze that landed within `HOST_RECOVERY_CHURN_READINGS` readings of a
/// promotion. The third is the one that matters and the reason the trio is on
/// this line rather than in a `log::info!`: if the promotion margin is too
/// lax in the wild, a counter that rides a sentence re-said every telemetry
/// period is the only thing that will ever say so — every other trace of a
/// promotion is one line in a console ring that turns over in seconds. Three
/// fixed-width fields, so they ride in FRONT of the pane group per the rule
/// below, and behind `heap max` so the rig's unanchored positional probe
/// (which ends at `balloon`) cannot see them either way.
///
/// **`gpu steps/restored/dwell/churn` is the card's governor**
/// ([`crate::recovery::GpuRecovery`]), and the field that matters is
/// `dwell`. The host trio above reports a governor that lifts on an OBSERVED
/// margin; this one has no falling signal to observe, so it restores a step
/// after a stretch of wall-clock quiet and doubles that quiet, to a cap,
/// every time the inference turns out wrong. `dwell 4x 120 s` is therefore
/// the mechanism stated on the line — a multiplier and a clock, not a
/// measurement of anything a driver said — and a reader can tell this axis
/// apart from the host's by it. `churn` is a squeeze that landed inside a
/// dwell of a restoration, and is the only thing that can ever say from the
/// field that `GPU_RECOVERY_DWELL` was argued too short.
///
/// **`ceiling` is the resolved rung value and is NOT always the figure that
/// binds** — read `cap` for that. `ceiling` prints
/// `Budgets::app_texture_ceiling_bytes`, which is the bracket resolved at the
/// rung the scene is on now. What `fit::over` tests a GPU need against is
/// `Capacity::gpu_bytes`, and on the presumed arm `Capacity::presumed` sets
/// that from `app_texture_ceiling_bytes.at(Promotion::Floor)` — the bracket's
/// FLOOR, whatever rung is in force.
///
/// The two coincide almost everywhere and the exception is narrow, so it is
/// spelled rather than left to be rediscovered. It needs all three of: the
/// **desktop** bracket, which is the only stepped one
/// (`Bracket::new(DESKTOP_APP_TEXTURE_BUDGET_BYTES,
/// DESKTOP_APP_TEXTURE_CEILING_BYTES)` — 3840 and 4032 MiB); the **Ceiling**
/// rung, since `Bracket::new` sets `step` equal to `floor` and only the top
/// rung departs from it; and a **presumed** capacity, since a measured or
/// probed one takes `gpu_bytes` from the driver and never from the bracket at
/// all. On a promoted desktop arm with no GPU reading, `ceiling` therefore
/// reads 4032 MiB while the figure feeding `fit::over` is 3840 — 192 MiB
/// apart, one number a reader takes for the other.
///
/// **wasm and mobile are `Bracket::pinned`**, so floor, step and ceiling are
/// one value and the two figures coincide on every rung. A reader who meets
/// "printed is not binding" and goes looking for it on the browser arm will
/// find nothing, which is why the scope is named here.
///
/// Neither figure is wrong for what it is, and the arithmetic is not touched:
/// `ceiling` is the rung's resolved budget and `cap` is the capacity in force.
/// They answer different questions, and the line carries both.
///
/// **`host allowance`, `rss` and `pool residual` are the host axis's three
/// diagnostics**, and each spells its own absence rather than printing a zero.
/// `host allowance` is `Capacity::host_allowance`, free because `cap` was
/// already a parameter — and **`none` there is the UNBOUND regime, not a small
/// allowance**: with no host figure `fit::over`'s `Pools::Split` host arm is
/// an `is_some_and` that never fires, so a leg showing `none` has byte figures
/// that are real readings rather than redistributions against a wall.
///
/// `rss` is this process's resident set (`squallar_alloc::process::resident`,
/// ~11 us and flat in RSS, `none` off Linux) and `pool residual` is
/// `rss - live` — **the term `pool` above does not have.**
/// `squallar_device_profile::scene::host_pool_bytes` adds back what the
/// allocator was asked for, where the OS had already subtracted the whole
/// resident set; the pool is under-stated by everything between them, which is
/// every mapping that never went through `malloc` — the executable, the shared
/// libraries, thread stacks, the driver's device maps. That function's doc
/// carries the magnitude, the direction, and why closing it would be the wrong
/// trade. The figure rides here rather than staying a documented constant
/// because it is one arm's number on one OS.
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
    over_pool_bytes: usize,
    cap: &Capacity,
    probe: GpuProbeReport,
    page_heap: crate::pressure::LinearMemoryWatch,
    readout: &squallar_egui::shell_api::BudgetReadout,
    page_live_bytes: Option<u64>,
    host_recovery: &crate::recovery::HostRecovery,
    gpu_recovery: &crate::recovery::GpuRecovery,
    doors: squallar_egui::admission::Totals,
    door_spare: squallar_device_profile::admit::Spare,
    resident: Option<squallar_alloc::process::Resident>,
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
         page heap acts {} at {} MiB, heap max {}/{} MiB, \
         host steps {} promotions {} churn {}, \
         gpu steps {} restored {} dwell {}x {} s churn {}, \
         loop over {} MiB, loop clamped {}",
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
        host_recovery.level(),
        host_recovery.promotions(),
        host_recovery.churn(),
        gpu_recovery.level(),
        gpu_recovery.restorations(),
        gpu_recovery.dwell_multiplier(),
        gpu_recovery.dwell().as_secs(),
        gpu_recovery.churn(),
        mib(over_pool_bytes as u64),
        readout
            .panes
            .iter()
            .filter(|pane| pane.loop_span_clamped())
            .count(),
    );
    let or_none = |bytes: Option<u64>| {
        bytes.map_or_else(|| "none".to_string(), |b| format!("{} MiB", mib(b)))
    };
    // **Which regime the reading was taken in, and what `pool` above is
    // missing.** Three fixed-width fields, so they ride in FRONT of `spare`
    // and `live` per the placement rule on this function.
    //
    // `host allowance` is [`Capacity::host_allowance`], and it costs no
    // plumbing because `cap` was already a parameter. **`none` is not a small
    // allowance, it is the UNBOUND regime**: a presumed native profile carries
    // no host figure at all, and `fit::over`'s `Pools::Split` host arm is an
    // `is_some_and`, so the host axis never fires and every byte figure beside
    // it is a real reading rather than a redistribution. Two sessions tried to
    // establish bound-versus-unbound off this line and could not, because the
    // field did not exist to grep.
    //
    // `rss` and `pool residual` are **the term `pool` does not have**.
    // `scene::host_pool_bytes` adds back `live` — what the allocator was asked
    // for — where the OS had subtracted the whole resident set, so the pool is
    // under-stated by `rss - live`: chunk headers, arena retention, and every
    // mapping that never went through `malloc` (the executable, the shared
    // libraries, thread stacks, the driver's device maps). That function's doc
    // carries the magnitude, the direction, and why closing it would be the
    // wrong trade. It is printed rather than left as a documented constant
    // because it is one arm's figure on one OS: `rss` is `none` off Linux,
    // where there is no `/proc` to read it from, and the residual is `none`
    // in a binary that never installed the counting allocator — an absence,
    // not a residual of zero.
    //
    // The residual is `checked_sub`, so a heap priced above its own residency
    // — `MADV_DONTNEED` leaves a block live and its page gone — reads `none`
    // rather than saturating to a zero that would look like perfect coverage.
    let _ = write!(
        line,
        ", host allowance {}, rss {}, pool residual {}",
        or_none(cap.host_allowance()),
        or_none(resident.map(|r| r.rss_bytes)),
        or_none(
            resident
                .map(|r| r.rss_bytes)
                .zip(page_live_bytes)
                .and_then(|(rss, live)| rss.checked_sub(live)),
        ),
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
    // **The DOOR's spare, beside the readout's, because they are two
    // questions and neither answers the other.** The pair above is the
    // allowance less what the scene costs at the rung in force - how much
    // room the picture on screen has left. These three are the allowance less
    // what it would cost with every rung of the ladder shed, which is the
    // only figure an act about to be admitted can be compared against
    // (`App::compose_admission_costs`).
    //
    // Without them a refusal's arithmetic was not on the line at all, in two
    // ways that both bit: the GPU door subtracts `volume_shortfall_bytes` and
    // the readout does not, so `spare gpu` was never the figure the GPU door
    // used; and on a `Pools::Unified` capacity - every integrated part, the
    // Framework 13 this whole campaign started on - `admit::verdict` tests
    // the summed increment against `joint` ALONE and ignores both axes, so
    // the line carried two numbers the door had not looked at and none of the
    // one it had.
    let _ = write!(
        line,
        ", door spare gpu {} host {} joint {}",
        or_none(door_spare.gpu_bytes),
        or_none(door_spare.host_bytes),
        or_none(door_spare.joint_bytes),
    );
    // **The admission doors' running totals, handed in.** Read by the caller
    // (`squallar_egui::admission::totals()`) rather than here, for the reason
    // every other moving figure on this line is a parameter: they are
    // process-wide counters, and a composer that read them itself would write
    // a different sentence every time anything else in the process opened a
    // door.
    //
    // Appended among the
    // fixed-width fields and BEFORE the variable-arity `pane<i>` rows: the
    // rig's regex is positional and stops at `balloon`, and a group of
    // varying length ahead of these would put them where nothing could name
    // them. `would refuse` is the advisory figure - what the doors WOULD have
    // turned away - and `refused` is what they actually did, which is zero
    // for the whole of WO-G by construction.
    let _ = write!(
        line,
        ", admission asked {} admitted {} would refuse {} refused {}",
        doors.asked, doors.admitted, doors.would_refuse, doors.refused,
    );
    // **What reached the GLASS, beside what the doors decided.** One group of
    // three, fixed width, among the fixed-width fields and ahead of the
    // variable-arity `pane<i>` rows, per the placement rule on this function.
    //
    // The counters above say what the doors did; none of them says whether the
    // user ever saw it, and the two are not the same event. `notices raised`
    // is every sentence put on the glass; `live` is the subset that landed on
    // a notice still showing, which is what a reader experiences as a notice
    // appearing and vanishing; `reoffered` is the subset this application put
    // up because a refused act started fitting again, carried so it can be
    // SUBTRACTED - without it, a wish resolving on the telemetry tick would be
    // indistinguishable from the re-stamped refusal the pair exists to find.
    //
    // It is here because a user reported exactly that flicker and nothing in
    // this tree could confirm or refute it: no per-act refusal is logged
    // anywhere, so the hypothesis was unfalsifiable on every log the
    // application can emit.
    let _ = write!(
        line,
        ", notices raised {} live {} reoffered {}",
        doors.raised, doors.raised_live, doors.reoffered,
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

/// **Whether a host-heap pressure signal would ever have fired on this
/// session, and on how many readings** — counted, never acted on.
///
/// # Why a counter and not a cause
///
/// No native target raises host-heap pressure at all. The two host-heap
/// causes [`crate::pressure::Pressure`] carries — the wasm page instance's
/// and the rasterization worker's — are both behind
/// `crate::platform::PlatformBridge::linear_memory`, whose trait default is
/// `None` and which no native bridge overrides; the platform memory warning
/// is iOS and Android only (winit raises `Event::MemoryWarning` from
/// `didReceiveMemoryWarning` and `onLowMemory` and nowhere else). What is
/// left on a desktop is a lost surface and a wgpu allocation failure, and
/// **neither is a reading**: both are a refusal already suffered. So on the
/// platform this application's live-bytes figure is measured on, nothing
/// sheds host memory *because host memory is high*.
///
/// Whether that gap costs anything is a measurement question, and it has been
/// argued from source rather than measured. This is the measurement: the two
/// figures such a signal would compare, and a running count of the readings
/// on which the comparison would have said "pressure".
///
/// **It pulls no lever, raises no cause and gates nothing.** It is filed here
/// beside the lines rather than in [`crate::pressure`] deliberately: a watch
/// kept in that module would read as a fifth trigger, and there are four.
///
/// # The line it would fire at
///
/// `squallar_device_profile::linear_memory::act_line(allowance, headroom)` —
/// the same function the page heap's watermark judges by, against a different
/// ceiling. **`max` there is a hard wall and here it is not**, and the
/// difference is the whole reason a desktop signal is possible at all: a
/// browser page has a declared `WebAssembly.Memory` maximum it traps at,
/// where a native process has the machine. What stands in its place is
/// `Capacity::host_allowance()` — three quarters of what the OS said was
/// available on this tick plus what this process already holds, scaled by the
/// user's own `PoolPercents::host` share. Crossing it is not a trap; it is
/// this process passing the share it declared for itself, on a pool the OS
/// re-quoted two seconds ago. That figure **recedes** as the rest of the
/// machine fills, which is the property a wall cannot have and the reason it
/// is the right line rather than a constant.
///
/// An allowance of zero is "nobody said a usable figure", exactly as
/// `linear_memory_verdict` reads a `max` of zero, and never a wall of zero:
/// without that guard every reading of every profile carrying no host figure
/// would count as over, since `act_line(0, h)` is 0 and any reading is at or
/// past it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostHeapWatch {
    /// Readings on which both figures were present and the live figure stood
    /// at or past the act line.
    over: u32,
    /// Readings on which both figures were present, whatever the verdict.
    ///
    /// **The denominator of [`Self::over`], and it is not the tick count.** A
    /// reading with no host capacity figure, or one taken in a binary that
    /// installed no counting allocator — every test binary in this workspace
    /// — is neither over nor under, and is in neither figure. The line prints
    /// both numbers for that reason.
    judged: u32,
    /// **The closest this session came to the line**, as a percentage of it,
    /// over the judged readings — 100 or more on a session that crossed.
    ///
    /// Here because a bare `over 0 of 42` cannot tell a session that ran
    /// comfortably from one that ran at 99 % and happened not to tip, and the
    /// two mean opposite things for whether this application needs a desktop
    /// host-heap signal at all. A null result with a peak of 7 % retires the
    /// question; a null result with a peak of 96 % is a near miss and re-opens
    /// it. Integer percent, and the product is taken in `u128` for the reason
    /// `linear_memory_verdict` takes its own that way: saturating in `u64`
    /// would make a nearly-full reading near the top of the range read as
    /// under.
    ///
    /// **Not `squallar_alloc::live_peak_bytes()`, and neither replaces the
    /// other.** That is a high-water mark of the NUMERATOR — the most this
    /// allocator ever held. This is a high-water mark of the RATIO, and the
    /// denominator moves too: `host_allowance()` recedes as the rest of the
    /// machine fills, and the act line falls further as the scene's next
    /// picture batch grows. A session whose live bytes plateau while the box
    /// fills around it shows a rising figure here and a flat one there, and
    /// only this one is a statement about how close the application came to
    /// the line it would have acted on.
    peak_percent: u32,
}

impl HostHeapWatch {
    /// Judge one reading and count it. `live` is
    /// `squallar_alloc::live_bytes()` and `allowance` is
    /// `Capacity::host_allowance()`, both as their callers hold them, so an
    /// absent figure stays absent rather than becoming a zero.
    pub(crate) fn observe(&mut self, live: Option<u64>, allowance: Option<u64>, headroom: u64) {
        let Some(line) = act_line_for(allowance, headroom) else {
            return;
        };
        let Some(live) = live else {
            return;
        };
        self.judged = self.judged.saturating_add(1);
        if live >= line {
            self.over = self.over.saturating_add(1);
        }
        // `line` is non-zero: `act_line_for` already refused a zero allowance,
        // and the percentage term of a non-zero allowance is only zero for an
        // allowance under 100 bytes, which the headroom bound would have to
        // agree with. Guarded anyway, because a division that cannot happen is
        // cheaper to guard than to argue.
        if line > 0 {
            let percent = (u128::from(live) * 100 / u128::from(line)).min(u128::from(u32::MAX));
            self.peak_percent = self.peak_percent.max(percent as u32);
        }
    }

    /// Readings on which a host-heap signal would have raised pressure, the
    /// readings on which it could have judged at all, and the closest approach
    /// as a percentage of the line.
    pub(crate) fn counts(self) -> (u32, u32, u32) {
        (self.over, self.judged, self.peak_percent)
    }
}

/// Where a host-heap reading would become pressure, or `None` where the
/// allowance says nothing. See [`HostHeapWatch`] for why a zero allowance is
/// an absence.
fn act_line_for(allowance: Option<u64>, headroom: u64) -> Option<u64> {
    allowance
        .filter(|bytes| *bytes > 0)
        .map(|bytes| squallar_device_profile::linear_memory::act_line(bytes, headroom))
}

/// `host heap watch: live 3266 MiB, allowance 46080 MiB, act line 40089 MiB,
/// headroom 512 MiB, over 0 of 42 readings, peak 8 percent` — the two figures
/// a host-heap pressure signal would compare and how often it would have
/// fired, said every telemetry period on every target.
///
/// **Those numbers are a shape, not a reading**, and the label is here because
/// the omission of it already cost something: the example was quoted back as
/// a measured session result within the hour. Only `live` is even the right
/// order of magnitude for this workspace's Linux arm; the rest were chosen to
/// show the field widths. What this line has actually read on a running
/// application is, at the time of writing, nothing at all — the instrument
/// landed before any leg was run through it.
///
/// **Nothing here gates anything and nothing here sheds a byte**; see
/// [`HostHeapWatch`]. Its own line and never appended to `budget state:`,
/// which is scraped by a regex whose groups are positional.
///
/// `headroom` is printed beside the line it bounds because without it a
/// reader cannot tell which of the act line's two terms is in force — the
/// percentage of the allowance, or the allowance less what the scene's next
/// picture batch is about to take. Every byte figure is MiB by integer
/// division, like every other figure in this module, and an absent one is
/// `none` rather than `0`: a profile that carries no host capacity and a
/// binary with no counting allocator are both silences, not zeroes.
pub(crate) fn host_heap_watch_line(
    live: Option<u64>,
    allowance: Option<u64>,
    headroom: u64,
    watch: HostHeapWatch,
) -> String {
    // The same spelling as `budget_state_line`'s own, and local for the same
    // reason: every byte figure in this module is MiB by integer division,
    // because the rig's probe reads these sentences with `(\d+)` groups.
    let mib = |bytes: u64| bytes / (1024 * 1024);
    let say = |bytes: Option<u64>| match bytes {
        Some(bytes) => format!("{} MiB", mib(bytes)),
        None => "none".to_string(),
    };
    let (over, judged, peak) = watch.counts();
    format!(
        "host heap watch: live {}, allowance {}, act line {}, headroom {} MiB, \
         over {over} of {judged} readings, peak {peak} percent",
        say(live),
        say(allowance),
        say(act_line_for(allowance, headroom)),
        mib(headroom),
    )
}

/// **How often a released merge base was offered its volume back, and how
/// often nothing wanted its gates.**
///
/// Counts and not bytes, deliberately. What the ask-gate on the restore side
/// separates is one volume — a measured 48.4 MiB, triple-named across `still
/// scans`, `loop scans` and `radar shared` — and at three legs an arm the
/// bytes cannot resolve it: the previous lane measured a within-arm spread
/// wider than its between-arm difference and claimed nothing either way. A
/// count of the times the clause fired has no such spread, so it is what says
/// whether the cut is live on a leg at all, and how often.
///
/// Running totals for the life of the process, never levels, which is why
/// they get a line of their own rather than a family on the census.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BaseRestoreCounts {
    offered: u32,
    declined: u32,
    restored: u32,
}

impl BaseRestoreCounts {
    /// A released base the loop download cache is holding a matching whole
    /// volume for — every condition the restore used to need, and the
    /// denominator the other two are read against.
    pub(crate) fn offered(&mut self) {
        self.offered = self.offered.saturating_add(1);
    }

    /// One of those turned away because nothing had asked for the gates.
    /// **This is the cut**: every one of these was a rebuild the old
    /// predicate made and nothing read.
    pub(crate) fn declined(&mut self) {
        self.declined = self.declined.saturating_add(1);
    }

    /// One of those carried through, because something had.
    pub(crate) fn restored(&mut self) {
        self.restored = self.restored.saturating_add(1);
    }

    pub(crate) fn counts(self) -> (u32, u32, u32) {
        (self.offered, self.declined, self.restored)
    }
}

/// `base restore: offered N, declined N, restored N` — said every telemetry
/// period, on every target.
///
/// `offered` counts released bases the loop download cache could have rebuilt;
/// `declined` those the ask-gate turned away because no pane read the gates
/// and none was owed a picture; `restored` those something had asked for.
/// The first is the denominator of the other two, and it is NOT their sum: a
/// restore that passes the ask and is then refused by
/// `VolumeInventory::restore_base_gates` on identity is offered and neither.
///
/// The fields are written as `N` on purpose. The example on
/// [`host_heap_watch_line`] above was quoted back as a measured session result
/// within the hour of it being written, so a line that has not been read on a
/// running application carries no figures at all.
///
/// Its own line and never appended to `budget state:`, which is scraped by a
/// regex whose groups are positional.
pub(crate) fn base_restore_line(counts: BaseRestoreCounts) -> String {
    let (offered, declined, restored) = counts.counts();
    format!("base restore: offered {offered}, declined {declined}, restored {restored}")
}

/// **The de-duplicated radar volume level, as a count and as bytes**, with
/// the live-site denominator it is read against and the one holder a pointer
/// union cannot reach printed beside it.
///
/// # Every figure here names its denominator
///
/// `volumes` is distinct allocations across the still store, the merge bases,
/// the per-site latest, the loop download cache and the chunk feed — the
/// number six panes are actually resident on, and the figure a per-pane claim
/// is made against. `sites` is live chunk feeds and NOT the pane count: two
/// panes on one site share one feed and one volume, so a scene whose panes
/// are all on different sites has nothing to share and reads `volumes` at
/// least `sites`. That distinction is the whole reading, and a line carrying
/// the volume count without it invites exactly the wrong conclusion.
///
/// `parked` is the chunk feed's closed-volume queue. It is printed and
/// **never added**: its per-entry price is not stored, so those allocations
/// cannot be folded into the union without walking every parked volume's
/// radials on this tick. The queue is drained one per round and is empty on an
/// ordinary leg, so a non-zero count here is the reading that says the union
/// beside it is missing something — which is the direction an instrument may
/// fail in, as long as it says so.
///
/// MiB by integer division, the spelling every byte figure in this module
/// uses, because the rig reads these sentences with `(\d+)` groups.
pub(crate) fn radar_volume_line(
    volumes: usize,
    bytes: u64,
    sites: usize,
    parked: usize,
    parked_bytes: u64,
) -> String {
    let mib = |bytes: u64| bytes / (1024 * 1024);
    format!(
        "radar volumes: union {volumes} distinct at {} MiB over {sites} live site(s); parked {parked} at {} MiB (not in the union)",
        mib(bytes),
        mib(parked_bytes),
    )
}

/// `radar dup volumes: files N (archive-less N), dup N (twin archive-less N, same allocation N),
/// arrival N MiB sole N MiB, twin N MiB sole N MiB; merged N at N MiB` — said
/// every telemetry period.
///
/// `merged` is the FIRES counter: how many resident twins were handed a
/// compressed half they never had, and what those twins are holding decoded.
/// It is an upper bound on what the merge frees and never a claim that it was
/// freed — three cuts in this campaign were binned for mechanisms that
/// executed zero times, and this is the field a reader checks first.
///
/// The two byte pairs are the two copies of ONE physical volume and are never
/// added: only one of them can be dropped, and which one is the cut's choice.
///
/// The seam counters behind [`squallar_radar::loop_downloads::IdentityDuplication`],
/// whose doc carries what each denominator is. Printed here because the
/// question it answers — is one physical volume decoded twice — cannot be
/// read off any level: a duplicate the residency pass drops between two 2 s
/// readings is a real duplicate a sampled level reports as zero.
///
/// `same allocation` is the null reading and is printed even when it is zero.
/// A merge over pointer-equal entries frees a refcount and no memory, and
/// this campaign has read `sole 0` three times on cuts whose bytes looked
/// certain; a line that could not distinguish the two would be worse than no
/// line.
pub(crate) fn radar_duplicate_volume_line(
    dup: squallar_radar::loop_downloads::IdentityDuplication,
) -> String {
    let mib = |bytes: u64| bytes / (1024 * 1024);
    format!(
        "radar dup volumes: files {} (archive-less {}), dup {} (twin archive-less {}, same allocation {}), arrival {} MiB sole {} MiB, twin {} MiB sole {} MiB; merged {} at {} MiB",
        dup.files,
        dup.archiveless_files,
        dup.duplicates,
        dup.twin_archiveless,
        dup.same_allocation,
        mib(dup.arrival_bytes),
        mib(dup.arrival_sole_bytes),
        mib(dup.twin_bytes),
        mib(dup.twin_sole_bytes),
        dup.merges,
        mib(dup.merge_bytes),
    )
}

/// **What a merge base is holding that nothing would free**, as a LEVEL read
/// on the telemetry tick.
///
/// # The denominator the release histogram does not carry
///
/// [`BaseReleaseCounts`] says which of six guards kept a base's gates. It
/// cannot say what taking them would have been WORTH, and on a scene where
/// the answer is zero that difference is the whole reading: a lever unblocked
/// against a volume some other store also names frees nothing, because both
/// halves are one allocation and a refcount is not memory.
///
/// So this line is the second half of that question, and it is three figures
/// with three different denominators:
///
/// * `bases` — sites holding a merge base **with its gates**. The
///   denominator for everything else here. A released base is not counted:
///   it has no rungs to price and no gates to free.
/// * `sole` — of those, how many name an allocation **no other holder does**
///   — not the still store, not the per-site latest, not the loop download
///   cache. This is the count of bases where dropping the base's reference
///   moves `live_bytes` at all. `bases - sole` is the count where it cannot.
/// * `superseded` — rungs the live flight has already sealed, which
///   [`squallar_radar::current::resolve`] therefore leaves out of the merged
///   volume, and what those rungs cost. Summed over every base counted in
///   `bases`, priced off the per-sweep prices the inventory took at install.
/// * `freeable` — `superseded` bytes on the `sole` bases ALONE. **The only
///   figure here a cut can bank**, and the one a rung-level release of the
///   merge base would be worth.
///
/// A byte figure and a count of rungs are both here because the campaign has
/// been burned reading one for the other: rungs say the merge has stopped
/// wanting them, bytes say what the allocator is holding, and `freeable`
/// says whether anyone could hand those bytes back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BaseHolderCensus {
    pub(crate) bases: usize,
    pub(crate) sole: usize,
    pub(crate) rungs: usize,
    pub(crate) superseded_rungs: usize,
    pub(crate) superseded_bytes: usize,
    pub(crate) freeable_bytes: usize,
}

/// Its own line, and never appended to `budget state:`, which is scraped by a
/// positional regex.
///
/// MiB by integer division, the spelling every byte figure in this module
/// uses, because the rig reads these sentences with `(\d+)` groups.
pub(crate) fn base_holder_line(census: BaseHolderCensus) -> String {
    let BaseHolderCensus {
        bases,
        sole,
        rungs,
        superseded_rungs,
        superseded_bytes,
        freeable_bytes,
    } = census;
    let mib = |bytes: usize| bytes / (1024 * 1024);
    format!(
        "base holders: {bases} base(s) with gates, {sole} held by nothing else; \
         superseded {superseded_rungs} of {rungs} rung(s) at {} MiB, freeable {} MiB",
        mib(superseded_bytes),
        mib(freeable_bytes),
    )
}

/// **What the loop's decoded cache is holding that no eviction path can
/// take**, as a LEVEL read on the telemetry tick.
///
/// # Six figures, and four of them are subsets of the one above
///
/// `evict_decoded_except` and `evict_decoded_to_ceiling` both refuse a volume
/// with no archive behind it, because their premise is that eviction costs a
/// decode and a chunk-feed volume has nothing to decode from. So the residency
/// policy can *decide* it does not want a frame's moments and still be unable
/// to act, and every family this campaign publishes reports those bytes as
/// `loop scans` with no way to say that.
///
/// * `volumes` / `bytes` — every decoded volume the loop cache holds. The
///   denominator for everything below, and the same quantity
///   `LoopDownloadManager::cached_scan_bytes` reports.
/// * `no_archive` / `no_archive_bytes` — of those, the ones with no archive
///   behind them. **Not a prize**: most of these are frames the policy still
///   wants, and wanting them is why they are here.
/// * `unwanted` / `unwanted_bytes` — of the archive-less, the ones the
///   residency predicate has *already excluded* (textured, outside the
///   decoded lookahead, not parked on by a pane, not in a settling site).
///   This is the set the policy would evict today if the guard let it.
/// * `never_archived` / `never_archived_bytes` — of the unwanted, the ones no
///   archive was **ever** filed for: chunk-feed volumes, whose S3 object this
///   process has never held. The rest of `unwanted` lost its way back to
///   `evict_archives_to_ceiling`, and for those re-obtaining the volume is a
///   download rather than data nobody can get. **This split is the ruling**:
///   only these bytes cost fidelity.
/// * `sole` / `sole_bytes` — of the unwanted, the ones whose allocation **no
///   other holder names** — not the still inventory, not a merge base, not the
///   per-site latest, not the chunk feed. `0f3de05db` is why this column
///   exists: a base's superseded rungs read 148 MB and freed nothing because
///   six bases of six were co-held. **`sole_bytes` is the only figure here a
///   cut could bank.**
/// * `oldest_unwanted_s` — how long ago the oldest volume in the `unwanted`
///   set was collected, seconds, against the wall clock. The exposure window,
///   and the figure that says whether "the archive turns up minutes later" is
///   true of this cache: an entry that is still here an hour on has been
///   un-evictable for that hour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoopDecodedCensus {
    pub(crate) volumes: usize,
    pub(crate) bytes: usize,
    pub(crate) no_archive: usize,
    pub(crate) no_archive_bytes: usize,
    pub(crate) unwanted: usize,
    pub(crate) unwanted_bytes: usize,
    pub(crate) never_archived: usize,
    pub(crate) never_archived_bytes: usize,
    pub(crate) sole: usize,
    pub(crate) sole_bytes: usize,
    pub(crate) oldest_unwanted_s: u64,
}

/// Its own line, never appended to `budget state:`, which is scraped by a
/// positional regex; MiB by integer division, as every byte figure here is.
pub(crate) fn loop_decoded_line(census: LoopDecodedCensus) -> String {
    let LoopDecodedCensus {
        volumes,
        bytes,
        no_archive,
        no_archive_bytes,
        unwanted,
        unwanted_bytes,
        never_archived,
        never_archived_bytes,
        sole,
        sole_bytes,
        oldest_unwanted_s,
    } = census;
    let mib = |bytes: usize| bytes / (1024 * 1024);
    format!(
        "loop decoded: {volumes} volume(s) at {} MiB; no archive {no_archive} at {} MiB; \
         unwanted {unwanted} at {} MiB, never archived {never_archived} at {} MiB, \
         sole {sole} at {} MiB; oldest unwanted {oldest_unwanted_s} s",
        mib(bytes),
        mib(no_archive_bytes),
        mib(unwanted_bytes),
        mib(never_archived_bytes),
        mib(sole_bytes),
    )
}

/// **Why a merge base's gates did not go**, as one running tally per reason.
///
/// # Why a histogram and not a level
///
/// `App::release_unneeded_base_gates` is the only lever in this application
/// that can withdraw a whole decoded volume — a measured 48.9 MiB median,
/// held by the base, the still store, the site's latest and the loop cache
/// off ONE allocation — and it is guarded by six conditions in a row. A
/// census family says the volumes are still resident. It does not say which
/// of the six is the one holding them, and the difference decides whether
/// there is a cut here at all or whether the lever is simply correct to
/// decline.
///
/// **This exists because the ledger has been wrong about that before.** A cut
/// projected at ~94 MiB sat banked for days in this campaign while never
/// executing once, because the precondition it needed never held on a real
/// scene and nothing counted the times it was asked. A count of each refusal
/// cannot fail that way: a reason that never fires reads 0, and a lever that
/// is never even reached reads `considered 0`, which no byte figure can say.
///
/// Counts, and `freed` in bytes beside them, because the two answer different
/// questions and the campaign has been burned quoting one for the other:
/// `fired` says the lever executes on this scene, `freed` says what it was
/// worth. Both are running totals for the life of the process, never levels,
/// which is why they get a line of their own and are never added to the
/// census.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BaseReleaseCounts {
    considered: u32,
    already_released: u32,
    gate_reader: u32,
    picture_owed: u32,
    no_collected: u32,
    no_archive: u32,
    in_flight: u32,
    release_none: u32,
    fired: u32,
    freed_holders: u32,
    freed_bytes: u64,
}

/// Which of the six guards turned one site's base away, or that none did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaseReleaseOutcome {
    /// The gates are already gone — the steady state after a release, and so
    /// the one arm here that is not a refusal to celebrate or mourn.
    AlreadyReleased,
    /// A cross-section or 3D pane on the site reads the gates.
    GateReader,
    /// A pane on the site has not been handed its picture yet.
    PictureOwed,
    /// The base has no collection time, so nothing can be keyed to it.
    NoCollected,
    /// **No archive to decode back from.** The trade this lever makes is
    /// gates for a way back, and a base the chunk feed produced has none:
    /// it is filed with `archive: None` and `archive_for_identity` finds
    /// nothing for it.
    NoArchive,
    /// A decode is already on its way to this base.
    InFlight,
    /// The store declined the withdrawal.
    ReleaseNone,
}

impl BaseReleaseCounts {
    /// One site with a merge base, reached by the pass — the denominator
    /// every arm below is read against, and the reading that separates "the
    /// lever declined" from "the lever was never asked".
    pub(crate) fn considered(&mut self) {
        self.considered = self.considered.saturating_add(1);
    }

    /// One of those turned away, by the guard that turned it.
    pub(crate) fn blocked(&mut self, outcome: BaseReleaseOutcome) {
        let slot = match outcome {
            BaseReleaseOutcome::AlreadyReleased => &mut self.already_released,
            BaseReleaseOutcome::GateReader => &mut self.gate_reader,
            BaseReleaseOutcome::PictureOwed => &mut self.picture_owed,
            BaseReleaseOutcome::NoCollected => &mut self.no_collected,
            BaseReleaseOutcome::NoArchive => &mut self.no_archive,
            BaseReleaseOutcome::InFlight => &mut self.in_flight,
            BaseReleaseOutcome::ReleaseNone => &mut self.release_none,
        };
        *slot = slot.saturating_add(1);
    }

    /// One withdrawal that carried through, with what the joint release
    /// handed to the drop path: how many holders let go, and the bytes the
    /// base itself was priced at.
    ///
    /// `holders` is the count and not a byte figure on purpose — every one of
    /// them may be a refcount on the volume already counted in `bytes`, which
    /// is exactly why dropping the base's reference alone frees nothing.
    pub(crate) fn fired(&mut self, holders: usize, bytes: u64) {
        self.fired = self.fired.saturating_add(1);
        self.freed_holders = self
            .freed_holders
            .saturating_add(u32::try_from(holders).unwrap_or(u32::MAX));
        self.freed_bytes = self.freed_bytes.saturating_add(bytes);
    }
}

/// Its own line, and never appended to `budget state:`, which is scraped by a
/// positional regex.
///
/// MiB by integer division, the same spelling every byte figure in this
/// module uses, because the rig reads these sentences with `(\d+)` groups.
pub(crate) fn base_release_line(counts: BaseReleaseCounts) -> String {
    let BaseReleaseCounts {
        considered,
        already_released,
        gate_reader,
        picture_owed,
        no_collected,
        no_archive,
        in_flight,
        release_none,
        fired,
        freed_holders,
        freed_bytes,
    } = counts;
    format!(
        "base release: considered {considered}, fired {fired} freeing {} MiB from {freed_holders} holders; blocked already-released {already_released}, gate-reader {gate_reader}, picture-owed {picture_owed}, no-archive {no_archive}, in-flight {in_flight}, no-collected {no_collected}, release-none {release_none}",
        freed_bytes / (1024 * 1024),
    )
}

/// **Why a base's way back could not be found**, as one running tally per
/// cause — the split behind `no-archive` in [`base_release_line`].
///
/// That guard is 83 % and 43 % of every blocked consideration on the two
/// probe legs of 2026-09-09, and the count alone cannot say which of three
/// states the cache was in when it fired. A base filed by the chunk feed has
/// no compressed half of its own, but the S3 object for the same physical
/// volume arrives in this process minutes later under the OTHER clock, so
/// "no way back" and "a way back the search did not reach" are different
/// findings with different repairs, and two attempts have already been binned
/// for building the wrong one.
///
/// Running totals, so a line of their own and never added to a census level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BaseWayBackCounts {
    asked: u32,
    unlearned: u32,
    archive_gone: u32,
    shadowed: u32,
    shadowed_decoded: u32,
}

impl BaseWayBackCounts {
    /// One `no-archive` refusal, against what
    /// `LoopDownloadManager::identity_way_backs` says the cache holds for the
    /// identity that was asked for.
    pub(crate) fn observe(&mut self, learned: usize, with_archive: usize, with_decoded: usize) {
        self.asked = self.asked.saturating_add(1);
        if learned == 0 {
            self.unlearned = self.unlearned.saturating_add(1);
        } else if with_archive == 0 {
            self.archive_gone = self.archive_gone.saturating_add(1);
        } else {
            self.shadowed = self.shadowed.saturating_add(1);
            if with_decoded > 0 {
                self.shadowed_decoded = self.shadowed_decoded.saturating_add(1);
            }
        }
    }
}

/// **Whether the archive spill fired, and whether anything came back.**
///
/// `None` when this target has nowhere to put an archive, so the row is absent
/// rather than a line of zeros: a reader must be able to tell "armed and did
/// not fire" from "there is no spill here", and a tree without the spill at all
/// prints no such row. A ~94 MiB cut on this campaign delivered exactly zero
/// because its precondition never held on the arm it ran on and nothing
/// noticed for a day.
///
/// `on-disk` is bytes on the MEDIUM, not host bytes, and is never added to a
/// census level — they are the bytes that left the heap. The rest are running
/// totals, so a line of their own.
pub(crate) fn archive_spill_line(
    installed: bool,
    on_disk: usize,
    resident_keys: usize,
    counts: (u64, u64, u64, u64, u64),
) -> Option<String> {
    if !installed {
        return None;
    }
    let (spilled, refused_full, store_failed, restored, restore_misses) = counts;
    Some(format!(
        "archive spill: on-disk {on_disk} B in {resident_keys}, spilled {spilled}, \
         refused-full {refused_full}, store-failed {store_failed}, restored {restored}, \
         restore-misses {restore_misses}"
    ))
}

/// Its own line, and never appended to `budget state:`, which is scraped by a
/// positional regex.
pub(crate) fn base_way_back_line(counts: BaseWayBackCounts) -> String {
    let BaseWayBackCounts {
        asked,
        unlearned,
        archive_gone,
        shadowed,
        shadowed_decoded,
    } = counts;
    format!(
        "base way-back: asked {asked}, unlearned {unlearned}, archive-gone {archive_gone}, shadowed {shadowed}, shadowed-decoded {shadowed_decoded}"
    )
}

/// **How often the archive byte ceiling was told to keep a released base's
/// only way back**, as a running total.
///
/// `held` is one count per pinned archive per pass that was actually over the
/// ceiling — never a level, and never added to the `chunk archives:` figures
/// below, which count volumes.
///
/// It is always on for the reason every counter on this path is: a stranded
/// base and a base that was never released read identically from every other
/// instrument this application has, so a pin that never fires and a pin that
/// works are indistinguishable without it. Two counters shipped reading zero
/// on this exact mechanism before it existed.
pub(crate) fn way_back_pin_line(held: u64) -> String {
    format!("way-back pins: archive ceiling held {held}")
}

/// **What the decoded residency pass traded for archives**, as running totals,
/// beside the lookahead that decided it.
///
/// `volumes` and `bytes` are different currencies and are never added; `keep`
/// is the `LOOP_DECODED_LOOKAHEAD_FRAMES` of the running binary, printed so a
/// row can be read without knowing which arm produced it — `none` is wasm's
/// `None`, an integer is `Some(n)`.
///
/// **This row is the fires-counter for that constant**, and it is always on
/// for the reason `way_back_pin_line` is. `evict_decoded_except` refuses any
/// volume with no archive behind it, so a narrower lookahead frees bytes only
/// where the trade is actually available; on a chunk-fed scene it is not, and
/// a policy that never fires and a policy that works are indistinguishable
/// from every level this application publishes. A cut on this campaign already
/// delivered exactly zero for that reason and nothing noticed for a day.
///
/// A running total and never a level: a volume traded between two telemetry
/// ticks is invisible to anything sampled, and the trade is the event.
pub(crate) fn lookahead_trade_line(volumes: u64, bytes: u64) -> String {
    let keep = match squallar_device_profile::constants::LOOP_DECODED_LOOKAHEAD_FRAMES {
        Some(frames) => frames.to_string(),
        None => "none".to_string(),
    };
    format!("decoded trades: keep {keep} ahead, traded {volumes} volumes, {bytes} B")
}

/// **The way back the chunk feed keeps for its own volumes**, as running
/// totals and one level.
///
/// `whole` is the denominator: volumes that closed whole and were therefore
/// offered to the loop cache at all. `kept` is how many carried their
/// compressed form with them, `refused` the ones whose retained bytes were
/// incomplete or would not split back into LDM records. `retained` is a LEVEL
/// — what the live assemblers are holding this instant — and is never added
/// to the two totals.
///
/// It exists because two attempts at this have already read zero. A mechanism
/// that never executes reads exactly like one that works, so the count is
/// always on and published whether or not anything gates on it.
pub(crate) fn chunk_archive_line() -> String {
    let (whole, kept, bytes, refused) = squallar_radar::chunks::chunk_archive_totals();
    format!(
        "chunk archives: {kept} of {whole} whole volume(s) kept at {} MiB, refused {refused}; retained {} MiB live",
        bytes / (1024 * 1024),
        squallar_radar::chunks::retained_chunk_bytes() / (1024 * 1024),
    )
}

/// **What the decoder did not build**, as a running total.
///
/// The clutter-filter-power moment is decoded past rather than materialised
/// (`squallar_radar::moment_drop`); this is the evidence that it happens on a
/// running app rather than only in a test. A mechanism that executes zero
/// times reads exactly like one that works, and two landed this month that
/// did — the `VolumeSkeleton` nothing stored, and the base withdrawal that
/// did not fire for a day — so the counter is always on and published whether
/// or not anything gates on it.
///
/// `re-decodes` is 0 by construction: nothing reads the moment, so nothing can
/// ask for it back. It is printed because a non-zero reading would mean that
/// premise is false, and a claim nobody can falsify from the log is not
/// evidence.
///
/// Its own line, and never appended to `budget state:`, which is scraped by a
/// positional regex. MiB by integer division, the spelling every byte figure
/// in this module uses.
pub(crate) fn moment_drop_line() -> String {
    format!(
        "moment drop: cfp dropped {}, blocks {}, freeing {} MiB; re-decodes {}",
        squallar_radar::moment_drop::dropped(),
        squallar_radar::moment_drop::blocks(),
        squallar_radar::moment_drop::bytes() / (1024 * 1024),
        squallar_radar::moment_drop::redecodes(),
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

    /// **The `moment drop:` line's shape, pinned where the format string
    /// lives**, so the reader that scrapes it can be built from a string this
    /// binary actually produced rather than from one a report retyped.
    ///
    /// Four fields and their order, because a positional reader breaks
    /// silently on a reorder and a lane quoting the wrong column would be
    /// reporting blocks as bytes.
    #[test]
    fn the_moment_drop_line_names_four_fields_in_order() {
        let line = moment_drop_line();
        assert!(line.starts_with("moment drop: "), "{line}");
        let numbers: Vec<&str> = line
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            numbers.len(),
            4,
            "four figures, got {numbers:?} from {line}"
        );
        for field in ["cfp dropped ", "blocks ", ", freeing ", "re-decodes "] {
            assert!(line.contains(field), "{line} is missing `{field}`");
        }
        let cfp = line.find("cfp dropped").expect("cfp field");
        let blocks = line.find("blocks").expect("blocks field");
        let freeing = line.find("freeing").expect("freeing field");
        let redecodes = line.find("re-decodes").expect("re-decodes field");
        assert!(
            cfp < blocks && blocks < freeing && freeing < redecodes,
            "the fields are out of order: {line}"
        );
        assert!(line.contains(" MiB"), "the byte figure has no unit: {line}");
    }

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

    /// What the loop plan is over its pool by on the distinct line: 5 MiB,
    /// a figure no other position carries. Non-zero on purpose — the field
    /// exists to say a scene was admitted over its pool, and a pinned line
    /// that read 0 there would not tell a printed zero from a missing field.
    const OVER: usize = 5 << 20;

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
        pools: squallar_device_profile::scene::Pools::Split,
    };

    /// The probe report for the distinct line: found at the probe's own
    /// bound, which prints `5` — a code no other position carries.
    /// A page-heap watch that has never acted: the state every case below
    /// but `the_budget_line_carries_the_page_heaps_act_count` is about.
    /// The page's live bytes for the distinct line: 250 MiB, a figure no
    /// other position carries, beside the worker's 600 in [`distinct`].
    const LIVE: Option<u64> = Some(250 << 20);

    /// The resident reading for the distinct line: 900 MiB resident against
    /// [`LIVE`]'s 250, so `rss` prints 900 and `pool residual` their
    /// difference, 650 — three figures no other position carries and none of
    /// them derivable from a neighbour by accident.
    ///
    /// Handed in like every other moving figure on this line rather than read
    /// here: a test binary never installs the counting allocator, and a
    /// composer that read `/proc` itself would write a different sentence on
    /// every run.
    const RESIDENT: Option<squallar_alloc::process::Resident> =
        Some(squallar_alloc::process::Resident {
            rss_bytes: 900 << 20,
            anon_bytes: 700 << 20,
            file_bytes: 180 << 20,
            shmem_bytes: 20 << 20,
            threads: 44,
        });

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
                    ..PaneBudget::default()
                },
                PaneBudget {
                    terms: PaneTerms {
                        static_rasters: 33 << 20,
                        pictures_host: 41 << 20,
                        ..PaneTerms::default()
                    },
                    shared_bytes: 16 << 20,
                    own_bytes: 17 << 20,
                    ..PaneBudget::default()
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
            host_pool_bytes: None,
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
            page_live_bytes: None,
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
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                RESIDENT,
            ),
            "budget state: bracket desktop, rung 1, steps 3, pool 3072 MiB, \
             ceiling 3840 MiB, vram 24576 MiB, ram 65536 MiB, declared 8192 MiB, \
             threads 32, form 2, linear 300/700 MiB, cap 5120 3, probe 5, \
             balloon 7 MiB, page heap acts 0 at 0 MiB, heap max 900/1100 MiB, \
             host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, \
             host allowance none, rss 900 MiB, pool residual 650 MiB, \
             spare gpu none host none, door spare gpu none host none joint none, admission asked 0 admitted 0 would refuse 0 refused 0, \
             notices raised 0 live 0 reoffered 0, \
             live 250/600 MiB",
        );
        // The figure follows the pool it is handed, not a field of the budgets.
        assert!(
            budget_state_line(
                &budgets,
                &profile,
                linear,
                576 << 20,
                BALLOON,
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                None,
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
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                None,
            )
            .ends_with(
                ", probe 5, balloon 0 MiB, page heap acts 0 at 0 MiB, \
                 heap max 900/1100 MiB, host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance none, rss none, pool residual none, \
                 spare gpu none host none, door spare gpu none host none joint none, admission asked 0 admitted 0 would refuse 0 refused 0, \
                 notices raised 0 live 0 reoffered 0, \
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
                OVER,
                &profile.capacity(),
                GpuProbeReport::Absent,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                None,
            )
            .ends_with(
                ", cap 24576 2, probe 0, balloon 7 MiB, page heap acts 0 at 0 MiB, \
                 heap max 900/1100 MiB, host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance 30720 MiB, rss none, pool residual none, \
                 spare gpu none host none, door spare gpu none host none joint none, admission asked 0 admitted 0 would refuse 0 refused 0, \
                 notices raised 0 live 0 reoffered 0, \
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
                OVER,
                &lowered,
                GpuProbeReport::Skipped,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                None,
            )
            .ends_with(
                ", cap 3456 0, probe 1, balloon 7 MiB, page heap acts 0 at 0 MiB, \
                 heap max 900/1100 MiB, host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance none, rss none, pool residual none, \
                 spare gpu none host none, door spare gpu none host none joint none, admission asked 0 admitted 0 would refuse 0 refused 0, \
                 notices raised 0 live 0 reoffered 0, \
                 live 250/600 MiB"
            ),
        );
        // Ascending trust, every arm, and the order is the assertion: the
        // derived arm took the rung above the bracket's constant and below a
        // reading, so measured and probed each moved up one.
        assert_eq!(capacity_source_code(CapacitySource::Presumed), 0);
        assert_eq!(capacity_source_code(CapacitySource::Derived), 1);
        assert_eq!(capacity_source_code(CapacitySource::Measured), 2);
        assert_eq!(capacity_source_code(CapacitySource::Probed), 3);
        assert_eq!(capacity_source_word(CapacitySource::Measured), "measured");
        assert_eq!(capacity_source_word(CapacitySource::Derived), "derived");
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
        let silent = GpuProbeReport::SilentToCap { cap_bytes: 1 << 30 };
        assert_eq!(gpu_probe_code(silent), 6);
        assert_eq!(GpuProbeReport::default(), GpuProbeReport::Absent);
        assert_eq!(found(false).bytes(), Some(4032 << 20));
        assert_eq!(GpuProbeReport::Empty.bytes(), None);
        assert_eq!(
            silent.bytes(),
            None,
            "a silent walk is not a figure, whatever cap it reached"
        );
        assert!(!GpuProbeReport::Pending.is_settled());
        for settled in [
            GpuProbeReport::Absent,
            GpuProbeReport::Skipped,
            GpuProbeReport::Empty,
            silent,
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
            OVER,
            &cap,
            GpuProbeReport::Absent,
            WATCH,
            &no_readout(),
            None,
            &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        );
        let (_, tail) = line
            .split_once(", vram ")
            .expect("the line carries a vram field");
        assert_eq!(
            tail,
            "0 MiB, ram 0 MiB, declared 0 MiB, threads 0, form 0, linear 0/0 MiB, \
             cap 3840 0, probe 0, balloon 0 MiB, page heap acts 0 at 0 MiB, \
             heap max 0/0 MiB, host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance none, rss none, pool residual none, \
             spare gpu none host none, door spare gpu none host none joint none, admission asked 0 admitted 0 would refuse 0 refused 0, \
             notices raised 0 live 0 reoffered 0, live 0/0 MiB",
        );
        assert_eq!(
            line.matches(", ").count(),
            27,
            "twenty-eight comma-separated groups with no pane rows, twenty-seven \
             separators: a field was dropped or gained. It was seventeen until \
             the recovery governor's `host steps N promotions N churn N` landed, \
             which is ONE group of three space-separated figures and so moved \
             this by one; eighteen until the admission doors' \
             `admission asked N admitted N would refuse N refused N`, one group \
             of four; twenty until ruling 15's `loop over N MiB` and \
             ruling 13's `loop clamped N` landed as two more; twenty-two \
             until the GPU governor's \
             `gpu steps N restored N dwell Nx N s churn N` landed beside the \
             host trio, one group of five space-separated figures and so one \
             more separator again; and twenty-three until `host allowance`, \
             `rss` and `pool residual` landed as three single-figure groups. \
             **Re-derived by \
             counting the line this build actually writes**, not by adding one \
             lane's figure to another's: the doors and the loop fields landed \
             from two lanes on one day and each was pinned against a line \
             without the other's field on it. **26 -> 27 is a stated decision, \
             not a re-point**: this lane added exactly ONE group, \
             `notices raised N live N reoffered N` - three space-separated \
             figures inside one comma-separated group, so one separator, and \
             the number moved by exactly the number of groups added",
        );
    }

    /// **`host allowance` says which regime the reading was taken in**, and
    /// `none` there is the UNBOUND one rather than a small allowance.
    ///
    /// A native profile with no host figure carries `host_bytes: None`, so
    /// `Capacity::host_allowance` is `None` and `fit::over`'s `Pools::Split`
    /// host arm — an `is_some_and` — never fires. The host axis is not binding
    /// at all there, and every byte figure beside it on this line is a real
    /// reading rather than a redistribution against a wall. Two sessions tried
    /// to establish bound-from-unbound off this line and could not, because
    /// the field was not on it.
    ///
    /// Both arms, because a field that only ever printed `none` would satisfy
    /// a literal pin without ever having been computed.
    #[test]
    fn the_line_says_whether_the_host_axis_is_bound() {
        let (budgets, profile, linear) = distinct();
        let line_for = |cap: &Capacity| {
            budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                OVER,
                cap,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                RESIDENT,
            )
        };
        assert!(
            CAP.host_bytes.is_none(),
            "the distinct capacity is the unbound arm; this case has no other",
        );
        assert!(
            line_for(&CAP).contains(", host allowance none, "),
            "an unbound host axis must spell itself: {}",
            line_for(&CAP),
        );
        // The bound arm prints `host_allowance`'s own three quarters, which is
        // that method's arithmetic and not this line's.
        let bound = Capacity {
            host_bytes: Some(4 << 30),
            ..CAP
        };
        assert_eq!(bound.host_allowance(), Some(3 << 30));
        assert!(
            line_for(&bound).contains(", host allowance 3072 MiB, "),
            "a bound host axis must print the allowance in force: {}",
            line_for(&bound),
        );
    }

    /// **`pool residual` is the term the host pool does not have**, printed on
    /// the line rather than left as a constant in a doc comment.
    ///
    /// `scene::host_pool_bytes` adds back `live` — what the allocator was
    /// asked for — where the OS had already subtracted the whole resident set,
    /// so the pool it returns is under-stated by `rss - live`. That difference
    /// is not a rounding error: on this workspace's discrete-GPU arm the
    /// non-heap resident set held 256.7-265.0 MiB across every condition
    /// tested. It is one arm's figure on one OS, which is why it is read here
    /// rather than written down.
    ///
    /// Every arm, because three of the four are absences and an absence that
    /// printed `0` would read as perfect coverage.
    #[test]
    fn the_line_carries_the_pool_residual_the_host_pool_omits() {
        let (budgets, profile, linear) = distinct();
        let line_for = |resident, live| {
            budget_state_line(
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                live,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                resident,
            )
        };
        assert!(
            line_for(RESIDENT, LIVE).contains(", rss 900 MiB, pool residual 650 MiB, "),
            "the residual is the resident set less the allocator's request total",
        );
        assert!(
            line_for(None, LIVE).contains(", rss none, pool residual none, "),
            "no `/proc` to read is an absence, not a process of zero bytes",
        );
        assert!(
            line_for(RESIDENT, None).contains(", rss 900 MiB, pool residual none, "),
            "a binary that never installed the counting allocator has no residual \
             to state, and every test binary here is one",
        );
        // A heap priced above its own residency is a REAL state, not an error:
        // `MADV_DONTNEED` leaves a block live and its page gone. It reads
        // `none` rather than saturating to a zero that would look like a pool
        // with nothing missing from it.
        assert!(
            line_for(RESIDENT, Some(2000 << 20)).contains(", rss 900 MiB, pool residual none, "),
            "a live figure above the resident set must not saturate to zero",
        );
    }

    /// **The line carries the DOOR's spare beside the readout's, and they are
    /// allowed to differ.**
    ///
    /// Until 2026-09-06 only the readout's pair was on the line and a reader
    /// took it for the figure a refusal was measured against. It never was.
    /// Two ways it was not, and this fixture shows both: the GPU door
    /// subtracts `volume_shortfall_bytes`, which the readout does not carry;
    /// and on a `Pools::Unified` capacity `admit::verdict` tests the summed
    /// increment against `joint` ALONE and ignores both axes, so on every
    /// integrated part - the machine this campaign started on - the line
    /// carried two numbers the door had not looked at and none of the one it
    /// had.
    #[test]
    fn the_line_carries_the_doors_own_spare_beside_the_readouts() {
        let (budgets, profile, linear) = distinct();
        let line = budget_state_line(
            &budgets,
            &profile,
            linear,
            POOL,
            BALLOON,
            OVER,
            &CAP,
            PROBE,
            WATCH,
            &two_pane_readout(),
            LIVE,
            &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare {
                gpu_bytes: Some(100 << 20),
                host_bytes: Some(200 << 20),
                joint_bytes: Some(300 << 20),
            },
            None,
        );
        assert!(
            line.contains("spare gpu 3568 MiB host 601 MiB, "),
            "the readout's own pair must stay exactly where it was - a reader \
             of `spare gpu` wants the rung in force: {line}",
        );
        assert!(
            line.contains("door spare gpu 100 MiB host 200 MiB joint 300 MiB,"),
            "the figure a refusal was actually measured against is not on the \
             line: {line}",
        );
    }

    /// A split capacity has no joint pool, and the field says `none` rather
    /// than a zero a reader would take for "no room". The same spelling the
    /// readout's own pair already uses for a pool nothing answered for.
    #[test]
    fn the_doors_joint_spare_reads_none_on_a_split_capacity() {
        let (budgets, profile, linear) = distinct();
        let line = budget_state_line(
            &budgets,
            &profile,
            linear,
            POOL,
            BALLOON,
            OVER,
            &CAP,
            PROBE,
            WATCH,
            &no_readout(),
            LIVE,
            &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare {
                gpu_bytes: Some(0),
                host_bytes: None,
                joint_bytes: None,
            },
            None,
        );
        assert!(
            line.contains("door spare gpu 0 MiB host none joint none,"),
            "an empty pool and an absent one must not read alike - `Some(0)` \
             refuses everything and `None` refuses nothing: {line}",
        );
    }

    /// The values the distinct line carries, in the rig's group order: the
    /// bracket word and the fifteen integers after it, the balloon last.
    const DISTINCT_GROUPS: [&str; 16] = [
        "desktop", "1", "3", "3072", "3840", "24576", "65536", "8192", "32", "2", "300", "700",
        // The capacity-source group: probed, which is 3 now that the derived
        // arm holds 1 and every arm above it moved up
        // ([`capacity_source_code`]).
        "5120", "3", "5", "7",
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
            OVER,
            &CAP,
            PROBE,
            WATCH,
            &no_readout(),
            LIVE,
            &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        );
        let read_by_the_rig = rendered(&pattern("budget_state_re"), &DISTINCT_GROUPS);
        assert!(
            line.starts_with(&read_by_the_rig),
            "the `budget state:` line and the rig's probe have drifted:\n  app: {line}\n  rig: \
             {read_by_the_rig}",
        );
        assert_eq!(
            &line[read_by_the_rig.len()..],
            ", page heap acts 0 at 0 MiB, heap max 900/1100 MiB, \
             host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance none, rss none, pool residual none, spare gpu none host none, door spare gpu none host none joint none, \
             admission asked 0 admitted 0 would refuse 0 refused 0, \
             notices raised 0 live 0 reoffered 0, \
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
            OVER,
            &CAP,
            PROBE,
            WATCH,
            &two_pane_readout(),
            LIVE,
            &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        );
        assert!(
            line.starts_with(&read_by_the_rig),
            "two pane rows moved a field the rig reads:\n  app: {line}\n  rig: {read_by_the_rig}",
        );
        assert_eq!(
            &line[read_by_the_rig.len()..],
            ", page heap acts 0 at 0 MiB, heap max 900/1100 MiB, \
             host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance none, rss none, pool residual none, \
             spare gpu 3568 MiB host 601 MiB, door spare gpu none host none joint none, admission asked 0 admitted 0 would refuse 0 refused 0, \
             notices raised 0 live 0 reoffered 0, \
             live 250/600 MiB, \
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
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &readout,
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                None,
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
                &budgets,
                &profile,
                linear,
                POOL,
                BALLOON,
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &exhausted,
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        )
            .ends_with(
                ", spare gpu 0 MiB host none, door spare gpu none host none joint none, admission asked 0 admitted 0 would refuse 0 refused 0, \
                 notices raised 0 live 0 reoffered 0, \
                 live 250/600 MiB"
            ),
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
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                None,
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
                OVER,
                &CAP,
                PROBE,
                WATCH,
                &no_readout(),
                LIVE,
                &crate::recovery::HostRecovery::untouched(),
                &crate::recovery::GpuRecovery::untouched(),
                squallar_egui::admission::Totals::default(),
                squallar_device_profile::admit::Spare::default(),
                None,
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
            OVER,
            &CAP,
            PROBE,
            WATCH,
            &no_readout(),
            None,
            &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        );
        assert!(
            never.ends_with(
                ", page heap acts 0 at 0 MiB, heap max 0/0 MiB, \
                 host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance none, rss none, pool residual none, spare gpu none host none, door spare gpu none host none joint none, \
                 admission asked 0 admitted 0 would refuse 0 refused 0, \
                 notices raised 0 live 0 reoffered 0, \
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
            OVER,
            &CAP,
            PROBE,
            watch,
            &no_readout(),
            None,
            &crate::recovery::HostRecovery::untouched(),
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        );
        assert!(
            acted.ends_with(
                ", page heap acts 2 at 1011 MiB, heap max 0/0 MiB, \
                 host steps 0 promotions 0 churn 0, gpu steps 0 restored 0 dwell 1x 30 s churn 0, loop over 5 MiB, loop clamped 0, host allowance none, rss none, pool residual none, spare gpu none host none, door spare gpu none host none joint none, \
                 admission asked 0 admitted 0 would refuse 0 refused 0, \
                 notices raised 0 live 0 reoffered 0, \
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

    /// **The recovery governor's three counters are on the line, each in its
    /// own position, and `churn` is the one that matters.**
    ///
    /// `host steps` is what is held now, `promotions` is what has been given
    /// back ever, and `churn` is **promotions this session's own margin got
    /// wrong** — a squeeze that landed within `HOST_RECOVERY_CHURN_READINGS`
    /// readings of a promotion. If `HOST_RECOVERY_MARGIN_DELTAS` is too lax
    /// in the wild, that third figure is the only thing that will ever say
    /// so: every other trace of a promotion is one `log::info!` in a console
    /// ring that turns over in seconds, and the rig reads the last sixty
    /// entries. This line is re-said every telemetry period, so the last tick
    /// of any leg answers it.
    ///
    /// Three DISTINCT values, because a trio pinned at `0 0 0` cannot tell a
    /// field that reads the wrong counter from one that reads the right one.
    #[test]
    fn the_budget_line_carries_the_recoverys_steps_promotions_and_churn() {
        let profile = DeviceProfile::for_target();
        let budgets = resolve(&profile);
        let mut recovery = crate::recovery::HostRecovery::untouched();
        // Two squeezes, a promotion the second undoes at once (churn), then a
        // third squeeze: two steps held, one promotion, one churn.
        recovery.squeeze(Some(1 << 30), 900 << 20);
        for _ in 0..recovery.dwell() {
            recovery.observe(true);
        }
        assert_eq!(recovery.level(), 0);
        recovery.squeeze(Some(1 << 30), 900 << 20);
        recovery.squeeze(Some(900 << 20), 800 << 20);
        assert_eq!(
            (recovery.level(), recovery.promotions(), recovery.churn()),
            (2, 1, 1),
            "the fixture no longer carries three distinct figures",
        );
        let line = budget_state_line(
            &budgets,
            &profile,
            None,
            POOL,
            BALLOON,
            OVER,
            &CAP,
            PROBE,
            WATCH,
            &no_readout(),
            None,
            &recovery,
            &crate::recovery::GpuRecovery::untouched(),
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        );
        assert!(
            line.contains(", host steps 2 promotions 1 churn 1,"),
            "the recovery counters are not on the line, or not in that order: {line}",
        );
    }

    /// **The GPU governor's line says it is INFERRING, not measuring.**
    ///
    /// The host trio beside it reports a ceiling that lifts on an observed
    /// margin; this one has no falling signal at all and lifts on a clock.
    /// A reader who cannot tell the two apart reads `gpu steps 2` as a
    /// measurement of a card, so what is pinned here is that the multiplier
    /// and the seconds are BOTH on the line: `dwell 4x 120 s` is a mechanism,
    /// where `steps 2` alone would be a claim about hardware.
    #[test]
    fn the_budget_line_states_the_gpu_governors_dwell_rather_than_a_measurement() {
        let profile = DeviceProfile::for_target();
        let budgets = resolve(&profile);
        let mut gpu = crate::recovery::GpuRecovery::untouched();
        let t0 = web_time::Instant::now();
        // One squeeze, a restoration the next event undoes at once (churn),
        // then a third squeeze: two steps held, one restoration, one churn,
        // and a dwell doubled twice.
        gpu.squeeze(900 << 20, t0);
        let dwelt = t0 + crate::recovery::GPU_RECOVERY_DWELL;
        assert!(gpu.observe(dwelt), "the fixture never restored a step");
        gpu.squeeze(900 << 20, dwelt);
        gpu.squeeze(800 << 20, dwelt);
        assert_eq!(
            (
                gpu.level(),
                gpu.restorations(),
                gpu.churn(),
                gpu.dwell_multiplier()
            ),
            (2, 1, 1, 4),
            "the fixture no longer carries four distinct figures",
        );
        let line = budget_state_line(
            &budgets,
            &profile,
            None,
            POOL,
            BALLOON,
            OVER,
            &CAP,
            PROBE,
            WATCH,
            &no_readout(),
            None,
            &crate::recovery::HostRecovery::untouched(),
            &gpu,
            squallar_egui::admission::Totals::default(),
            squallar_device_profile::admit::Spare::default(),
            None,
        );
        assert!(
            line.contains(", gpu steps 2 restored 1 dwell 4x 120 s churn 1,"),
            "the gpu governor's counters are not on the line, or not in that \
             order: {line}",
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

/// The would-have-fired counter and the line that says what it counted.
///
/// These test the instrument's own arithmetic. What they do **not** test, and
/// what nothing in this workspace can, is the campaign question the counter
/// exists to settle: whether a real desktop session ever crosses the line.
/// That is a reading off a running app, which is why this landed as an
/// always-on instrument rather than as an argument.
#[cfg(test)]
mod host_heap_watch_tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    /// A reading at or past the act line is counted over; one under it is
    /// counted judged and not over. The line here is the percentage term —
    /// 87 % of 1,000 MiB is 870 — because the headroom is small enough that
    /// `allowance - headroom` is the looser of the two bounds.
    #[test]
    fn a_reading_past_the_line_counts_over_and_one_under_it_does_not() {
        let allowance = Some(1000 * MIB);
        let headroom = 10 * MIB;
        let mut watch = HostHeapWatch::default();
        watch.observe(Some(500 * MIB), allowance, headroom);
        assert_eq!(watch.counts(), (0, 1, 57), "500 of the 870 MiB line");
        watch.observe(Some(900 * MIB), allowance, headroom);
        assert_eq!(watch.counts(), (1, 2, 103));
        watch.observe(Some(870 * MIB), allowance, headroom);
        assert_eq!(
            watch.counts(),
            (2, 3, 103),
            "at the line is at or past it, and the peak is a high-water mark",
        );
    }

    /// **The headroom is the other bound, and it is the one that can bite.**
    /// A scene whose next picture batch is 400 MiB of a 1,000 MiB allowance
    /// puts the line at 600, not at 870, so a reading of 700 is over on the
    /// same allowance that left it under above. This is the term that makes
    /// pulling a lever RAISE the line, and a version of this instrument built
    /// on the percentage alone would not have it.
    #[test]
    fn the_scenes_next_batch_lowers_the_line_below_the_percentage() {
        let allowance = Some(1000 * MIB);
        let mut under = HostHeapWatch::default();
        under.observe(Some(700 * MIB), allowance, 10 * MIB);
        assert_eq!(under.counts(), (0, 1, 80), "700 of an 870 MiB line");
        let mut over = HostHeapWatch::default();
        over.observe(Some(700 * MIB), allowance, 400 * MIB);
        assert_eq!(over.counts(), (1, 1, 116), "700 of a 600 MiB line");
    }

    /// **An allowance of zero is a silence, not a wall of zero.** Without the
    /// guard `act_line(0, h)` is 0, every reading is at or past it, and a
    /// profile that carries no host figure would read as permanently
    /// pressured — the exact false positive that would make the counter's
    /// answer worthless. `linear_memory_verdict` spells a `max` of 0 `Quiet`
    /// for the same reason.
    #[test]
    fn a_zero_allowance_is_absent_rather_than_a_wall_of_zero() {
        let mut watch = HostHeapWatch::default();
        watch.observe(Some(1), Some(0), 0);
        assert_eq!(watch.counts(), (0, 0, 0), "neither over nor judged");
        assert!(host_heap_watch_line(Some(1), Some(0), 0, watch).contains("act line none"));
    }

    /// **The denominator is readings it could judge, not ticks.** A profile
    /// with no host figure and a binary with no counting allocator are both
    /// silences, and counting either as "under the line" would report a
    /// session as comfortable when it was only unmeasured.
    #[test]
    fn a_reading_missing_either_figure_is_in_neither_count() {
        let mut watch = HostHeapWatch::default();
        watch.observe(None, Some(1000 * MIB), 0);
        watch.observe(Some(500 * MIB), None, 0);
        assert_eq!(watch.counts(), (0, 0, 0));
        watch.observe(Some(500 * MIB), Some(1000 * MIB), 0);
        assert_eq!(watch.counts(), (0, 1, 57));
    }

    /// The line names both figures, the line derived from them, the headroom
    /// term that derived it, and both halves of the count.
    #[test]
    fn the_line_says_both_figures_the_line_and_both_halves_of_the_count() {
        let mut watch = HostHeapWatch::default();
        watch.observe(Some(900 * MIB), Some(1000 * MIB), 10 * MIB);
        assert_eq!(
            host_heap_watch_line(Some(900 * MIB), Some(1000 * MIB), 10 * MIB, watch),
            "host heap watch: live 900 MiB, allowance 1000 MiB, act line 870 MiB, \
             headroom 10 MiB, over 1 of 1 readings, peak 103 percent",
        );
    }

    /// A binary with no counting allocator and a profile with no host figure
    /// both print `none`, and the count still says how little it saw.
    #[test]
    fn absent_figures_print_none_rather_than_zero() {
        assert_eq!(
            host_heap_watch_line(None, None, 0, HostHeapWatch::default()),
            "host heap watch: live none, allowance none, act line none, headroom 0 MiB, \
             over 0 of 0 readings, peak 0 percent",
        );
    }
}
