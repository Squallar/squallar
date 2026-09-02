//! **What the machine told the budget system, and what the budgets are**, as
//! one telemetry sentence.
//!
//! The `loop state:` line prices what the loops hold; this one names the
//! bracket and rung the whole budget set was resolved at, and every host
//! signal the profile carries beside it, so a row can say *which machine* it
//! was measured on without a log somebody kept. Today the signals ride along
//! unread — `resolve` spends none of them, and
//! `the_new_signals_change_no_budget_yet` in the floor crate is the proof —
//! so this line is where a reader sees a device populate before anything
//! spends it.
//!
//! **Denominators, stated once, never added.** `bracket` is the compile-time
//! set (`Budgets::name`) and `rung` the promotion it was resolved at, `steps`
//! the ladder rungs surrendered. `pool` is the loop pool's bracket ceiling
//! (`Budgets::loop_pool_ceiling_bytes`) — the term `app_texture_bytes` sums,
//! never the live pool, which `loop state:` prints in bytes — and `ceiling`
//! is the whole-application texture ceiling (`app_texture_ceiling_bytes`).
//! `vram`, `ram` and `declared` come from three different sources (measured
//! VRAM, measured RAM, a browser's `deviceMemory` declaration) and are never
//! summed or read as one figure; `threads` is what the host reports, not what
//! any pool was built with. `linear` is the wasm page instance's heap over
//! the rasterization worker's — two instances, two ceilings, never one
//! figure. Every byte figure is MiB by integer division, because the rig's
//! probe reads these sentences with `(\d+)` groups.
//!
//! Product telemetry, not a campaign instrument: it rides
//! `report_frame_telemetry`'s existing 2 s tick, and no figure it prints
//! gates CI.

use crate::platform::LinearMemory;
use squallar_device_profile::budget::{Budgets, DeviceProfile, FormFactor, Promotion};

/// The `budget state:` line.
///
/// Every field is always present, so a real zero is a real zero. `vram`,
/// `ram`, `declared`, `threads` and both `linear` figures print `0` for an
/// unread signal only because 0 is not a possible measurement of any of them
/// — no machine has zero bytes of RAM or zero threads, and a wasm instance's
/// heap is at least its initial pages — while `form` spells unknown
/// explicitly as 0 against 1 (handheld) and 2 (desktop).
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
         linear {}/{} MiB",
        budgets.name,
        budgets.steps_back,
        mib(budgets.loop_pool_ceiling_bytes as u64),
        mib(budgets.app_texture_ceiling_bytes as u64),
        mib(profile.vram_bytes.unwrap_or(0)),
        mib(profile.system_ram_bytes.unwrap_or(0)),
        mib(profile.declared_ram_bytes.unwrap_or(0)),
        profile.parallelism.unwrap_or(0),
        mib(linear.map_or(0, |l| l.page_bytes)),
        mib(linear.and_then(|l| l.worker_bytes).unwrap_or(0)),
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

    /// A profile with a distinct value in every position the line prints, so
    /// a transposed pair cannot read as a correct line. The two budget
    /// figures are set directly rather than resolved, for the same reason.
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
            loop_pool_ceiling_bytes: 3 << 30,
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
    #[test]
    fn the_budget_state_line_reads_exactly_as_pinned() {
        let (budgets, profile, linear) = distinct();
        assert_eq!(
            budget_state_line(&budgets, &profile, linear),
            "budget state: bracket desktop, rung 1, steps 3, pool 3072 MiB, \
             ceiling 3840 MiB, vram 24576 MiB, ram 65536 MiB, declared 8192 MiB, \
             threads 32, form 2, linear 300/700 MiB",
        );
    }

    /// **An unread signal prints 0, and every field is still there.** The
    /// sentinel is argued in the formatter's doc: 0 is not a possible
    /// measurement of RAM, VRAM, threads or a live heap. `form` carries its
    /// own explicit unknown.
    #[test]
    fn an_unread_signal_prints_zero_and_no_field_is_dropped() {
        let profile = DeviceProfile::for_target();
        let budgets = Budgets {
            loop_pool_ceiling_bytes: 3 << 30,
            app_texture_ceiling_bytes: 3840 << 20,
            ..resolve(&profile)
        };
        let line = budget_state_line(&budgets, &profile, None);
        let (_, tail) = line
            .split_once(", vram ")
            .expect("the line carries a vram field");
        assert_eq!(
            tail,
            "0 MiB, ram 0 MiB, declared 0 MiB, threads 0, form 0, linear 0/0 MiB",
        );
        assert_eq!(
            line.matches(", ").count(),
            10,
            "eleven fields, ten separators: a field was dropped or gained",
        );
    }

    /// **The rig reads the budget line the app actually writes.** An extra
    /// space here turns the rig's whole budget reading into `null`, which a
    /// reader would take as "a bundle older than the line".
    #[test]
    fn the_rig_reads_the_budget_line_the_app_actually_writes() {
        let (budgets, profile, linear) = distinct();
        assert_eq!(
            budget_state_line(&budgets, &profile, linear),
            rendered(
                &pattern("budget_state_re"),
                &[
                    "desktop", "1", "3", "3072", "3840", "24576", "65536", "8192", "32", "2",
                    "300", "700",
                ],
            ),
            "the `budget state:` line and the rig's probe have drifted",
        );
    }

    /// The floor under the seam test above: `rendered` really can disagree.
    #[test]
    fn a_budget_line_that_drifted_by_one_space_is_not_accepted() {
        let (budgets, profile, linear) = distinct();
        let good = rendered(
            &pattern("budget_state_re"),
            &[
                "desktop", "1", "3", "3072", "3840", "24576", "65536", "8192", "32", "2", "300",
                "700",
            ],
        );
        assert_eq!(budget_state_line(&budgets, &profile, linear), good);
        let drifted = good.replacen(" rung", "  rung", 1);
        assert_ne!(drifted, good, "the perturbation perturbed nothing");
        assert_ne!(
            budget_state_line(&budgets, &profile, linear),
            drifted,
            "a line with one extra space compared equal to the real one, so the \
             seam test above cannot fail",
        );
    }
}
