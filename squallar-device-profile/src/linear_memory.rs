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

/// Percent of the ceiling at which a reading is pressure: 891 MiB of the 1 GiB
/// build (the first whole MiB at or past the line, which falls at 890.88 MiB).
///
/// The measured Tier-2 `firefox.huge` trap was an MRMS decode's 98 MB request
/// failing **at** the ceiling (`squallar-overlays/src/mrms/staging.rs`), and
/// on wasm32 an allocation the engine cannot serve aborts without unwinding.
/// Acting 133 MiB short of the wall is what leaves room for the allocation in
/// flight while economy is being given back.
pub const LINEAR_MEMORY_ACT_PERCENT: u64 = 87;

/// How far the high-water mark has to rise past the last action before the
/// watermark acts again.
pub const LINEAR_MEMORY_REFIRE_STEP_BYTES: u64 = 32 << 20;

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
/// the mark the watermark last acted at.
///
/// `Act` when `used` is at or past [`LINEAR_MEMORY_ACT_PERCENT`] of `max` and
/// either nothing has acted yet or the mark has risen by
/// [`LINEAR_MEMORY_REFIRE_STEP_BYTES`] since; otherwise `Warn` when at or past
/// [`LINEAR_MEMORY_WARN_PERCENT`]; otherwise `Quiet`. Integer arithmetic that
/// cannot overflow — the products are taken in `u128`, because saturating them
/// in `u64` would make a half-full heap read as full near the top of the range
/// — and `max == 0`, no ceiling to judge against, is `Quiet`.
pub fn linear_memory_verdict(
    used: u64,
    max: u64,
    last_acted_at: Option<u64>,
) -> LinearMemoryVerdict {
    if max == 0 {
        return LinearMemoryVerdict::Quiet;
    }
    let at_or_past = |percent: u64| u128::from(used) * 100 >= u128::from(max) * u128::from(percent);
    let grown_enough = last_acted_at
        .is_none_or(|last| used.saturating_sub(last) >= LINEAR_MEMORY_REFIRE_STEP_BYTES);
    if at_or_past(LINEAR_MEMORY_ACT_PERCENT) && grown_enough {
        LinearMemoryVerdict::Act
    } else if at_or_past(LINEAR_MEMORY_WARN_PERCENT) {
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
    /// and a reading is judged at, below and above each.
    #[test]
    fn the_warn_and_act_lines_fall_at_768_and_891_mib_of_the_build() {
        assert_eq!(MAX, 1024 * MIB, "the ceiling is 1 GiB");
        // 75 % of 1024 MiB is exactly 768 MiB.
        assert_eq!(linear_memory_verdict(768 * MIB - 1, MAX, None), Quiet);
        assert_eq!(linear_memory_verdict(768 * MIB, MAX, None), Warn);
        assert_eq!(linear_memory_verdict(768 * MIB + 1, MAX, None), Warn);
        // 87 % of 1024 MiB is 890.88 MiB: 890 MiB is under it, 891 is past.
        assert_eq!(linear_memory_verdict(890 * MIB, MAX, None), Warn);
        assert_eq!(linear_memory_verdict(891 * MIB, MAX, None), Act);
        assert_eq!(linear_memory_verdict(MAX, MAX, None), Act);
        assert_eq!(linear_memory_verdict(0, MAX, None), Quiet);
    }

    /// A second action needs the mark to have risen by the refire step; a mark
    /// standing still above the line, or short of the step, is a warning at
    /// most.
    #[test]
    fn a_second_action_needs_the_mark_to_have_grown_by_the_step() {
        let acted = Some(891 * MIB);
        assert_eq!(linear_memory_verdict(891 * MIB, MAX, acted), Warn);
        assert_eq!(
            linear_memory_verdict(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES - 1, MAX, acted),
            Warn,
        );
        assert_eq!(
            linear_memory_verdict(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES, MAX, acted),
            Act,
        );
        // A reading below the last action is not growth, whatever the line.
        assert_eq!(linear_memory_verdict(891 * MIB - 1, MAX, acted), Warn);
    }

    /// No ceiling, no judgement: a zero `max` is quiet even for a reading that
    /// would be pressure against any real ceiling.
    #[test]
    fn a_zero_ceiling_is_quiet_whatever_the_reading() {
        assert_eq!(linear_memory_verdict(891 * MIB, 0, None), Quiet);
        assert_eq!(linear_memory_verdict(u64::MAX, 0, None), Quiet);
    }

    /// The arithmetic neither overflows nor saturates: a reading and ceiling
    /// at the top of the range are judged exactly, so a half-full heap there
    /// is still quiet.
    #[test]
    fn the_arithmetic_is_exact_at_the_top_of_the_range() {
        assert_eq!(linear_memory_verdict(u64::MAX, u64::MAX, None), Act);
        assert_eq!(
            linear_memory_verdict(u64::MAX, u64::MAX, Some(u64::MAX)),
            Warn,
            "a mark that cannot have grown re-fired",
        );
        assert_eq!(
            linear_memory_verdict(u64::MAX / 2, u64::MAX, None),
            Quiet,
            "a half-full heap at the top of the range read as pressure",
        );
        // Exactly three quarters of the range; `MAX / 4 * 3` floors a hair
        // under the line, and the exact arithmetic says so.
        assert_eq!(
            linear_memory_verdict(u64::MAX - u64::MAX / 4, u64::MAX, None),
            Warn
        );
        assert_eq!(
            linear_memory_verdict(u64::MAX / 4 * 3, u64::MAX, None),
            Quiet
        );
    }
}
