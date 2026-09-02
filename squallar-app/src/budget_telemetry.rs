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
//! Every byte figure is MiB by integer division, because the rig's probe
//! reads these sentences with `(\d+)` groups.
//!
//! Product telemetry, not a campaign instrument: it rides
//! `report_frame_telemetry`'s existing 2 s tick, and no figure it prints
//! gates CI.

use crate::platform::LinearMemory;
use squallar_device_profile::budget::{Budgets, DeviceProfile, FormFactor, Promotion};
use squallar_device_profile::scene::{Capacity, CapacitySource};

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
/// says which arm it is ([`capacity_source_code`]).
///
/// A free function returning a `String` for the reason every other telemetry
/// sentence in this tree is one: `.github/browser-rig/drive.py` scrapes it
/// with a regex in another language in another directory, and
/// `the_rig_reads_the_budget_line_the_app_actually_writes` holds the two ends
/// together.
pub(crate) fn budget_state_line(
    budgets: &Budgets,
    profile: &DeviceProfile,
    linear: Option<LinearMemory>,
    pool_bytes: usize,
    cap: &Capacity,
) -> String {
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
    format!(
        "budget state: bracket {}, rung {rung}, steps {}, pool {} MiB, ceiling {} MiB, \
         vram {} MiB, ram {} MiB, declared {} MiB, threads {}, form {form}, \
         linear {}/{} MiB, cap {} {}",
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_device_profile::budget::{
        AdapterCeilings, BudgetLimits, BudgetMemo, Platform, resolve,
    };
    use squallar_device_profile::quality::DeviceClass;

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
            worker_bytes: Some(700 << 20),
        });
        (budgets, profile, linear)
    }

    /// The literal pin: every figure once, in the documented order, in MiB.
    /// `pool` is the live pool handed in, not the bracket ceiling the line
    /// once printed: a scene's loops are what it holds. `cap` is the capacity
    /// handed in and its source code, not a field of the profile.
    #[test]
    fn the_budget_state_line_reads_exactly_as_pinned() {
        let (budgets, profile, linear) = distinct();
        assert_eq!(
            budget_state_line(&budgets, &profile, linear, POOL, &CAP),
            "budget state: bracket desktop, rung 1, steps 3, pool 3072 MiB, \
             ceiling 3840 MiB, vram 24576 MiB, ram 65536 MiB, declared 8192 MiB, \
             threads 32, form 2, linear 300/700 MiB, cap 5120 2",
        );
        // The figure follows the pool it is handed, not a field of the budgets.
        assert!(
            budget_state_line(&budgets, &profile, linear, 576 << 20, &CAP)
                .contains(", pool 576 MiB,"),
        );
        // And the capacity follows what it is handed: this profile's own
        // measured 24 GiB reads `24576 1`, a session presumption lowered to
        // 3456 MiB reads `3456 0`.
        assert!(
            budget_state_line(&budgets, &profile, linear, POOL, &profile.capacity())
                .ends_with(", cap 24576 1"),
        );
        let lowered = Capacity::presumed(&BudgetLimits::DESKTOP).held_to(Some(3456 << 20));
        assert!(
            budget_state_line(&budgets, &profile, linear, POOL, &lowered).ends_with(", cap 3456 0"),
        );
        assert_eq!(capacity_source_code(CapacitySource::Presumed), 0);
        assert_eq!(capacity_source_code(CapacitySource::Measured), 1);
        assert_eq!(capacity_source_code(CapacitySource::Probed), 2);
        assert_eq!(capacity_source_word(CapacitySource::Measured), "measured");
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
        let line = budget_state_line(&budgets, &profile, None, POOL, &cap);
        let (_, tail) = line
            .split_once(", vram ")
            .expect("the line carries a vram field");
        assert_eq!(
            tail,
            "0 MiB, ram 0 MiB, declared 0 MiB, threads 0, form 0, linear 0/0 MiB, \
             cap 3840 0",
        );
        assert_eq!(
            line.matches(", ").count(),
            11,
            "twelve comma-separated groups, eleven separators: a field was dropped or \
             gained",
        );
    }

    /// The values the distinct line carries, in the rig's group order: the
    /// bracket word and the thirteen integers after it.
    const DISTINCT_GROUPS: [&str; 14] = [
        "desktop", "1", "3", "3072", "3840", "24576", "65536", "8192", "32", "2", "300", "700",
        "5120", "2",
    ];

    /// **The rig reads the budget line the app actually writes.** An extra
    /// space here turns the rig's whole budget reading into `null`, which a
    /// reader would take as "a bundle older than the line".
    #[test]
    fn the_rig_reads_the_budget_line_the_app_actually_writes() {
        let (budgets, profile, linear) = distinct();
        assert_eq!(
            budget_state_line(&budgets, &profile, linear, POOL, &CAP),
            rendered(&pattern("budget_state_re"), &DISTINCT_GROUPS),
            "the `budget state:` line and the rig's probe have drifted",
        );
    }

    /// The floor under the seam test above: `rendered` really can disagree.
    #[test]
    fn a_budget_line_that_drifted_by_one_space_is_not_accepted() {
        let (budgets, profile, linear) = distinct();
        let good = rendered(&pattern("budget_state_re"), &DISTINCT_GROUPS);
        assert_eq!(
            budget_state_line(&budgets, &profile, linear, POOL, &CAP),
            good
        );
        let drifted = good.replacen(" rung", "  rung", 1);
        assert_ne!(drifted, good, "the perturbation perturbed nothing");
        assert_ne!(
            budget_state_line(&budgets, &profile, linear, POOL, &CAP),
            drifted,
            "a line with one extra space compared equal to the real one, so the \
             seam test above cannot fail",
        );
    }
}
