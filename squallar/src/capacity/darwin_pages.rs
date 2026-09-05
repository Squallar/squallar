//! The page arithmetic behind the Darwin available-memory reader.
//!
//! **Mounted on every target on purpose.** `apple.rs` compiles nowhere but
//! macOS and iOS, and no CI row runs an Apple test binary — `build.yaml`'s
//! Apple rows cross-*build* and stop. A test sitting beside the
//! `host_statistics64` call would therefore compile in CI and execute on no
//! arm at all, which is a gate that cannot fail. Nothing in this arithmetic
//! names a platform type, so it is mounted unconditionally and every arm's
//! `cargo test -p squallar` runs it against counts a real Mac produced.

/// **Bytes a process could take right now**, out of one `vm_statistics64`
/// snapshot: free pages **less the speculative ones**, plus inactive, at the
/// host's page size.
///
/// **`free_count` is inclusive of `speculative_count`, and that is the whole
/// reason a subtraction is here.** The struct offers the two counts side by
/// side as though they were disjoint queues and nothing in the field names
/// says otherwise; Darwin's own `vm_stat` subtracts one from the other before
/// it prints "Pages free". Measured on jacobs-mac-mini (M2, macOS 26.4.1,
/// 8 GiB unified, page size 16384) on 2026-09-04, one call read a
/// `free_count` of 102,389 against `vm_stat`'s 35,304 "Pages free", with a
/// `speculative_count` of 67,084 — and `102,389 - 67,084 = 35,305`, one page
/// of drift between two interleaved reads. What the sum is worth, and why the
/// smaller figure is the one taken, is on `apple::available_ram_bytes` — a
/// module this one is deliberately not `cfg`-gated with, so the link is spelled
/// rather than followed. This function only performs the arithmetic.
/// Pinned by `speculative_pages_are_subtracted_and_the_sum_matches_vm_stat`.
///
/// **The subtraction saturates rather than wrapping.** Inclusiveness makes
/// `speculative > free` a state the kernel should never publish, but the two
/// counts come out of one snapshot the ABI does not promise is internally
/// consistent, and a `u32` wrap would turn a few pages of skew into ~4 billion
/// pages — a 64 TiB pool on a 16 KiB page, which is over every budget rather
/// than under one. Pinned by
/// `a_speculative_count_over_the_free_count_saturates_rather_than_wrapping`.
///
/// **`None` rather than zero** for an empty pool, an unread page size or a
/// product that overflows: a pool of zero bytes is a wall every scene is over,
/// where an absent pool is the arm every budget already handles.
pub fn available_bytes_from(
    free: u32,
    speculative: u32,
    inactive: u32,
    page_bytes: u64,
) -> Option<u64> {
    let pages = u64::from(free.saturating_sub(speculative)) + u64::from(inactive);
    let bytes = pages.checked_mul(page_bytes)?;
    (bytes > 0).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::available_bytes_from;

    /// jacobs-mac-mini's page size, confirmed three ways on 2026-09-04 — it is
    /// **not** 4096, and every byte figure below is a page count times this.
    const PAGE_BYTES: u64 = 16_384;

    /// The one `free_count` captured raw from a `host_statistics64` call on
    /// that box, beside a `speculative_count` of [`SPECULATIVE`].
    const FREE: u32 = 102_389;

    /// The `speculative_count` of that same call. `vm_stat` printed 35,304
    /// "Pages free" beside it and `FREE - SPECULATIVE` is 35,305.
    const SPECULATIVE: u32 = 67_084;

    /// Three interleaved samples from jacobs-mac-mini on 2026-09-04: what the
    /// reader returned before this fix (`(free + inactive) * page`), and the
    /// same quantity computed from `vm_stat`'s output at that moment, where
    /// `vm_stat` has already taken speculative out of the free count it
    /// prints. The reader stood **+52.1 %** over `vm_stat` on all three.
    const SAMPLES: [(&str, u64, u64); 3] = [
        ("S1", 3_210_330_112, 2_111_127_552),
        ("S2", 3_211_034_624, 2_110_898_176),
        ("S3", 3_208_232_960, 2_109_095_936),
    ];

    /// The `(free, speculative, inactive)` one sample pins. Both byte figures
    /// are measured; the speculative count is the difference between them in
    /// pages, and the inactive count is what the summed figure has left over
    /// [`FREE`].
    ///
    /// **How the summed figure splits between free and inactive cannot change
    /// the answer** — the function reads `free - speculative + inactive`, in
    /// which the two are interchangeable — so pinning `free` from the one call
    /// whose raw counts were captured costs the fixture nothing. The split has
    /// to satisfy one thing only, that `free >= speculative`, and every sample
    /// clears it by ~35,000 pages.
    fn counts(summed_bytes: u64, vm_stat_bytes: u64) -> (u32, u32, u32) {
        assert_eq!(summed_bytes % PAGE_BYTES, 0, "a whole number of pages");
        assert_eq!(vm_stat_bytes % PAGE_BYTES, 0, "a whole number of pages");
        let summed_pages = summed_bytes / PAGE_BYTES;
        let speculative = summed_pages - vm_stat_bytes / PAGE_BYTES;
        let inactive = summed_pages - u64::from(FREE);
        let narrow = |pages: u64| u32::try_from(pages).expect("a page count fits a natural_t");
        (FREE, narrow(speculative), narrow(inactive))
    }

    /// **The fix, against the machine that found the defect.** On every
    /// sample the function returns what `vm_stat` agrees to, to the byte, and
    /// never the figure the reader used to return.
    #[test]
    fn speculative_pages_are_subtracted_and_the_sum_matches_vm_stat() {
        for (name, summed, vm_stat) in SAMPLES {
            let (free, speculative, inactive) = counts(summed, vm_stat);
            let bytes = available_bytes_from(free, speculative, inactive, PAGE_BYTES);
            assert_eq!(bytes, Some(vm_stat), "{name}");
            assert_ne!(bytes, Some(summed), "{name}");
        }
    }

    /// **The size of what was fixed**, and the check that these fixtures are
    /// the real machine's rather than numbers this test invented: with nothing
    /// subtracted the function reproduces each sample's pre-fix reading to the
    /// byte, and that reading stands more than half again over the corrected
    /// one — 1.099 GB of speculative pages inside a 3.21 GB pool on S1.
    #[test]
    fn without_the_subtraction_the_fixtures_reproduce_the_pre_fix_reading() {
        for (name, summed, vm_stat) in SAMPLES {
            let (free, _, inactive) = counts(summed, vm_stat);
            assert_eq!(
                available_bytes_from(free, 0, inactive, PAGE_BYTES),
                Some(summed),
                "{name}",
            );
            assert!(
                summed > vm_stat + vm_stat / 2,
                "{name}: {summed} vs {vm_stat}"
            );
        }
    }

    /// The two counts that were captured raw, on their own: subtracting is
    /// what closes a 67,084-page gap to the one page of drift between two
    /// interleaved reads, and taking `free_count` as it comes over-states that
    /// term alone by 2.9x.
    #[test]
    fn the_raw_counts_close_on_vm_stats_own_free_page_figure() {
        assert_eq!(
            available_bytes_from(FREE, SPECULATIVE, 0, PAGE_BYTES),
            Some(35_305 * PAGE_BYTES),
        );
        assert_eq!(
            available_bytes_from(FREE, 0, 0, PAGE_BYTES),
            Some(u64::from(FREE) * PAGE_BYTES),
        );
    }

    /// A snapshot whose speculative count exceeds its free count must cost the
    /// pool its free term, not hand back a `u32` wrap: at 16 KiB pages that
    /// wrap is a ~64 TiB pool, which over-commits every budget built on it.
    #[test]
    fn a_speculative_count_over_the_free_count_saturates_rather_than_wrapping() {
        for (free, speculative) in [(0, 1), (10, u32::MAX), (FREE, FREE + 1)] {
            assert_eq!(
                available_bytes_from(free, speculative, 1_000, PAGE_BYTES),
                Some(1_000 * PAGE_BYTES),
                "{free} free, {speculative} speculative",
            );
        }
    }

    /// Nothing here reads as a machine with no memory. An empty pool, a page
    /// size the kernel never filled in, and a product too large for the
    /// figure are all unknown.
    #[test]
    fn an_empty_pool_an_unread_page_size_or_an_overflow_is_unknown_not_zero() {
        assert_eq!(available_bytes_from(0, 0, 0, PAGE_BYTES), None);
        assert_eq!(available_bytes_from(u32::MAX, 0, 0, 0), None);
        assert_eq!(available_bytes_from(u32::MAX, 0, u32::MAX, u64::MAX), None);
    }
}
