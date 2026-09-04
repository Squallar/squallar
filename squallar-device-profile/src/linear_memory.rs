//! The wasm heap watermark: whether one reading of a linear memory against its
//! ceiling is quiet, worth a line, or pressure.
//!
//! Pure integer policy over two figures the caller reads. wasm linear memory
//! only ever grows — a page once acquired is the instance's for life, whatever
//! the allocator later frees inside it — so a reading is a high-water mark by
//! nature, and the policy is shaped for one: a second action needs the mark to
//! have **risen** past the last action by [`LINEAR_MEMORY_REFIRE_STEP_BYTES`],
//! not merely to still stand above the line. Without that a heap that acted
//! once would act on every tick for the rest of the session.
//!
//! The ceiling itself is [`crate::constants::WASM_LINEAR_MEMORY_MAX_BYTES`], a
//! build constant; the reading is the bridge's (`memory().buffer().byteLength`
//! on the page, the worker's own figure on its envelopes), and the two module
//! instances are judged by the fuller of the two, never by their sum. A native
//! bridge reads no heap, so nothing here is reached natively — that is the
//! caller's job, held in `squallar-app` by
//! `a_native_profile_with_no_heap_reading_is_never_pressured_by_the_tick`.

/// Percent of the ceiling at which a reading is worth one line in the log:
/// 768 MiB of the 1 GiB build.
pub const LINEAR_MEMORY_WARN_PERCENT: u64 = 75;

/// Percent of the ceiling **past which a reading is pressure whatever the
/// scene**: 891 MiB of the 1 GiB build (the first whole MiB at or past the
/// line, which falls at 890.88 MiB). The ceiling of the action line, not the
/// line itself — see [`act_line`].
///
/// The measured Tier-2 `firefox.huge` trap of 2026-08-31 was an MRMS decode's
/// 98 MB request failing **at** the ceiling
/// (`squallar-overlays/src/mrms/staging.rs`), and on wasm32 an allocation the
/// engine cannot serve aborts without unwinding. Acting 133 MiB short of the
/// wall left room for that one allocation in flight. It did not leave room
/// for the page's own next batch: the same leg on 2026-09-02, with thirteen
/// overlay pictures of 41.7 MB re-rasterising together, climbed 11, 522, 939,
/// 1019 MiB across four two-second ticks, acted at 939 with nothing left that
/// the levers could free, and trapped in `rust_oom` inside the frame.
pub const LINEAR_MEMORY_ACT_PERCENT: u64 = 87;

/// How far the high-water mark has to rise past the last action before the
/// watermark acts again.
pub const LINEAR_MEMORY_REFIRE_STEP_BYTES: u64 = 32 << 20;

/// **Where a reading of a heap of `max` bytes becomes pressure, given what
/// the scene is about to allocate.** The lower of [`LINEAR_MEMORY_ACT_PERCENT`]
/// of the ceiling and `max - headroom`, where `headroom` is the caller's
/// figure for the largest single allocation plus the next batch of them —
/// on the page, one overlay picture plus every shown picture at the budget's
/// oversampling, which the need model already prices
/// (`crate::fit::NeedTerms::pictures_host` and `picture_arrival_host`).
///
/// The percentage alone was the defect: a line 133 MiB short of a 1 GiB wall
/// is short of one 41.7 MB picture and long past a batch of thirteen, so the
/// heap could stand under it on one tick and trap before the next. Deriving
/// the line from the batch says what is true — a scene whose next batch is
/// 581 MB is pressure from 443 MiB up, and the levers (the oversampling
/// rung first among them) shrink the batch and so raise the line, until at
/// 1x it stands at 758 MiB for the same scene. A batch larger than the heap
/// puts the line at zero: every reading is pressure, which is the truth of a
/// scene that cannot be held, and the re-fire step bounds how often that is
/// acted on.
pub fn act_line(max: u64, headroom: u64) -> u64 {
    let percent = (u128::from(max) * u128::from(LINEAR_MEMORY_ACT_PERCENT) / 100) as u64;
    percent.min(max.saturating_sub(headroom))
}

/// What one reading of the heap asks of the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearMemoryVerdict {
    /// Under the warning line, or the ceiling is unknown.
    Quiet,
    /// At or past the warning line and not an action: worth a line, once.
    Warn,
    /// At or past the action line, and the mark has grown enough since the
    /// last action for another to be worth taking.
    Act,
}

/// Judge `used` bytes of a linear memory whose ceiling is `max` bytes, given
/// the mark the watermark last acted at and the `headroom` the scene's next
/// allocations need ([`act_line`]).
///
/// `Act` when `used` is at or past [`act_line`]`(max, headroom)` and either
/// nothing has acted yet or the mark has risen by
/// [`LINEAR_MEMORY_REFIRE_STEP_BYTES`] since; otherwise `Warn` when at or past
/// [`LINEAR_MEMORY_WARN_PERCENT`]; otherwise `Quiet`. Integer arithmetic that
/// cannot overflow — the products are taken in `u128`, because saturating them
/// in `u64` would make a half-full heap read as full near the top of the range
/// — and `max == 0`, no ceiling to judge against, is `Quiet`. A `headroom` of
/// zero is the percentage line alone, which is what a caller with no scene
/// to price — the worker instance — judges by.
pub fn linear_memory_verdict(
    used: u64,
    max: u64,
    last_acted_at: Option<u64>,
    headroom: u64,
) -> LinearMemoryVerdict {
    if max == 0 {
        return LinearMemoryVerdict::Quiet;
    }
    let at_or_past = |percent: u64| u128::from(used) * 100 >= u128::from(max) * u128::from(percent);
    let grown_enough = last_acted_at
        .is_none_or(|last| used.saturating_sub(last) >= LINEAR_MEMORY_REFIRE_STEP_BYTES);
    let act = at_or_past(LINEAR_MEMORY_ACT_PERCENT) || used >= act_line(max, headroom);
    if act && grown_enough {
        LinearMemoryVerdict::Act
    } else if at_or_past(LINEAR_MEMORY_WARN_PERCENT) || act {
        LinearMemoryVerdict::Warn
    } else {
        LinearMemoryVerdict::Quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LinearMemoryVerdict::{Act, Quiet, Warn};

    const MIB: u64 = 1 << 20;
    const MAX: u64 = crate::constants::WASM_LINEAR_MEMORY_MAX_BYTES;

    /// The lines fall where the percentages say against the shipped ceiling,
    /// and a reading is judged at, below and above each. With no headroom
    /// asked for, the action line is the percentage alone.
    #[test]
    fn the_warn_and_act_lines_fall_at_768_and_891_mib_of_the_build() {
        assert_eq!(MAX, 1024 * MIB, "the ceiling is 1 GiB");
        // 75 % of 1024 MiB is exactly 768 MiB.
        assert_eq!(linear_memory_verdict(768 * MIB - 1, MAX, None, 0), Quiet);
        assert_eq!(linear_memory_verdict(768 * MIB, MAX, None, 0), Warn);
        assert_eq!(linear_memory_verdict(768 * MIB + 1, MAX, None, 0), Warn);
        // 87 % of 1024 MiB is 890.88 MiB: 890 MiB is under it, 891 is past.
        assert_eq!(linear_memory_verdict(890 * MIB, MAX, None, 0), Warn);
        assert_eq!(linear_memory_verdict(891 * MIB, MAX, None, 0), Act);
        assert_eq!(linear_memory_verdict(MAX, MAX, None, 0), Act);
        assert_eq!(linear_memory_verdict(0, MAX, None, 0), Quiet);
        assert_eq!(
            act_line(MAX, 0),
            934_155_386,
            "890.88 MiB, floored to the byte"
        );
    }

    /// **The action line is the lower of the percentage and the wall less
    /// the scene's headroom**, pinned at the `huge` leg's own figures: the
    /// leg's PANE is 2878 x 1611 — the window less its forty-point top bar,
    /// forty physical pixels at device pixel ratio 1 — and with thirteen
    /// pictures shown, a batch plus one arrival is 14 pictures at the rung's
    /// oversampling. 584,072,832 B at 1.5x (the trap's own scene: the line
    /// falls to 466 MiB, 425 MiB short of the percentage, which is why 87 %
    /// acted too late), 405,482,616 B at 1.25x (637 MiB), 259,641,648 B at
    /// 1x (776 MiB). A headroom past the wall puts the line at zero and
    /// every reading past it; the percentage stays the ceiling of the line.
    ///
    /// The per-picture figure is not modelled: both legs reported
    /// `overlay pictures: ... px=4317x2416`, and Firefox's allocation
    /// failures asked for exactly `4317 * 2416 * 4` = 41,719,488 B twelve
    /// times over.
    #[test]
    fn the_act_line_is_the_lower_of_the_percentage_and_the_wall_less_the_batch() {
        let picture = |percent: u64| (2878 * percent / 100) * (1611 * percent / 100) * 4;
        let batch = |percent: u64| 14 * picture(percent);
        assert_eq!(picture(150), 41_719_488, "the leg's own reported picture");
        assert_eq!(batch(150), 584_072_832);
        assert_eq!(batch(125), 405_482_616);
        assert_eq!(batch(100), 259_641_648);
        assert_eq!(act_line(MAX, batch(150)), MAX - 584_072_832);
        assert_eq!(act_line(MAX, batch(150)) / MIB, 466);
        assert_eq!(act_line(MAX, batch(125)) / MIB, 637);
        assert_eq!(act_line(MAX, batch(100)) / MIB, 776);
        // At the trap's second tick — 522 MiB — the scene line is pressure
        // where the percentage line was quiet.
        assert_eq!(linear_memory_verdict(522 * MIB, MAX, None, batch(150)), Act);
        assert_eq!(linear_memory_verdict(522 * MIB, MAX, None, 0), Quiet);
        assert_eq!(
            linear_memory_verdict(452 * MIB, MAX, None, batch(150)),
            Quiet
        );
        // The percentage is a ceiling on the line: a scene with room to
        // spare is still pressure at 891 MiB.
        assert_eq!(act_line(MAX, 1), act_line(MAX, 0));
        assert_eq!(linear_memory_verdict(891 * MIB, MAX, None, 1), Act);
        // A batch the heap cannot hold at all: the line is zero, and a
        // reading past it that has not grown since the last action is a
        // warning, not a second action.
        assert_eq!(act_line(MAX, MAX), 0);
        assert_eq!(act_line(MAX, u64::MAX), 0);
        assert_eq!(linear_memory_verdict(1, MAX, None, MAX), Act);
        assert_eq!(linear_memory_verdict(1, MAX, Some(1), MAX), Warn);
    }

    /// A second action needs the mark to have risen by the refire step; a mark
    /// standing still above the line, or short of the step, is a warning at
    /// most.
    #[test]
    fn a_second_action_needs_the_mark_to_have_grown_by_the_step() {
        let acted = Some(891 * MIB);
        assert_eq!(linear_memory_verdict(891 * MIB, MAX, acted, 0), Warn);
        assert_eq!(
            linear_memory_verdict(
                891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES - 1,
                MAX,
                acted,
                0
            ),
            Warn,
        );
        assert_eq!(
            linear_memory_verdict(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES, MAX, acted, 0),
            Act,
        );
        // A reading below the last action is not growth, whatever the line.
        assert_eq!(linear_memory_verdict(891 * MIB - 1, MAX, acted, 0), Warn);
    }

    /// No ceiling, no judgement: a zero `max` is quiet even for a reading that
    /// would be pressure against any real ceiling.
    #[test]
    fn a_zero_ceiling_is_quiet_whatever_the_reading() {
        assert_eq!(linear_memory_verdict(891 * MIB, 0, None, 0), Quiet);
        assert_eq!(linear_memory_verdict(u64::MAX, 0, None, 0), Quiet);
        assert_eq!(linear_memory_verdict(u64::MAX, 0, None, u64::MAX), Quiet);
    }

    /// The arithmetic neither overflows nor saturates: a reading and ceiling
    /// at the top of the range are judged exactly, so a half-full heap there
    /// is still quiet.
    #[test]
    fn the_arithmetic_is_exact_at_the_top_of_the_range() {
        assert_eq!(linear_memory_verdict(u64::MAX, u64::MAX, None, 0), Act);
        assert_eq!(
            linear_memory_verdict(u64::MAX, u64::MAX, Some(u64::MAX), 0),
            Warn,
            "a mark that cannot have grown re-fired",
        );
        assert_eq!(
            linear_memory_verdict(u64::MAX / 2, u64::MAX, None, 0),
            Quiet,
            "a half-full heap at the top of the range read as pressure",
        );
        // Exactly three quarters of the range; `MAX / 4 * 3` floors a hair
        // under the line, and the exact arithmetic says so.
        assert_eq!(
            linear_memory_verdict(u64::MAX - u64::MAX / 4, u64::MAX, None, 0),
            Warn
        );
        assert_eq!(
            linear_memory_verdict(u64::MAX / 4 * 3, u64::MAX, None, 0),
            Quiet
        );
        assert_eq!(
            act_line(u64::MAX, 0),
            u64::MAX / 100 * 87 + (u64::MAX % 100) * 87 / 100
        );
    }
}
