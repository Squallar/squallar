//! What a GPU's memory heaps add up to, once a backend has listed them.
//!
//! Pure arithmetic, so this crate stays `forbid(unsafe_code)`: the readers
//! that ask a driver for its heaps live in the `squallar` shell and hand this
//! module `(size, device_local)` pairs.

/// The bytes of every heap flagged device-local, or `None` when no heap is,
/// or those that are add up to nothing.
///
/// A heap without the flag is host memory the GPU can reach, not memory it
/// has, so it is left out whatever its size. `None` rather than `0` because a
/// zero would read downstream as a measured figure, and the majority arm of
/// `DeviceProfile::vram_bytes` is "unread".
pub fn device_local_total(heaps: &[(u64, bool)]) -> Option<u64> {
    let total = heaps
        .iter()
        .filter(|(_, device_local)| *device_local)
        .try_fold(0u64, |sum, (size, _)| sum.checked_add(*size))?;
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::device_local_total;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    /// One RTX 3090's heaps as `vulkaninfo` listed them on 2026-09-02: the
    /// card's own 24 GiB, the 70.42 GiB host heap the driver also exposes, and
    /// the 246 MiB host-visible window into the card. Both device-local heaps
    /// count; the host heap does not, whatever its size.
    #[test]
    fn a_discrete_card_sums_its_device_local_heaps_and_skips_the_host_heap() {
        let heaps = [(24 * GIB, true), (75_616_828_416, false), (246 * MIB, true)];
        assert_eq!(device_local_total(&heaps), Some(24 * GIB + 246 * MIB));
    }

    /// A UMA part with one 256 MiB carve-out flagged device-local beside the
    /// system RAM it really draws on: the arithmetic answers the carve-out.
    /// Whether that figure can be believed is the caller's question.
    #[test]
    fn a_uma_carve_out_is_summed_as_listed() {
        let heaps = [(256 * MIB, true), (16 * GIB, false)];
        assert_eq!(device_local_total(&heaps), Some(256 * MIB));
    }

    #[test]
    fn no_heaps_no_device_local_heaps_or_empty_ones_are_unknown_not_zero() {
        assert_eq!(device_local_total(&[]), None);
        assert_eq!(device_local_total(&[(16 * GIB, false)]), None);
        assert_eq!(device_local_total(&[(0, true), (0, true)]), None);
    }

    #[test]
    fn a_sum_past_u64_is_unknown_rather_than_wrapped() {
        assert_eq!(device_local_total(&[(u64::MAX, true), (1, true)]), None);
    }
}
