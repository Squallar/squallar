//! Linux and Android: `MemTotal` out of `/proc/meminfo`.

/// Total physical memory in bytes, or `None` when `/proc/meminfo` cannot be
/// read or says nothing [`mem_total_bytes`] accepts.
pub fn system_ram_bytes() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .as_deref()
        .and_then(mem_total_bytes)
}

/// The `MemTotal:` line of a `/proc/meminfo` text, in bytes.
///
/// The kernel prints the figure in `kB` and means 1024-byte units
/// (`fs/proc/meminfo.c`'s `show_val_kb` shifts pages by `PAGE_SHIFT - 10`),
/// so that is the one unit accepted. Any other unit, a missing line, an
/// unparsable figure or a zero is `None` rather than a guess: a wrong RAM
/// figure is worse than an absent one, because absent is the arm every
/// budget already handles.
pub(super) fn mem_total_bytes(meminfo: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?;
    let mut parts = line.split_whitespace();
    let kib: u64 = parts.next()?.parse().ok()?;
    match parts.next() {
        Some("kB") => kib.checked_mul(1024).filter(|bytes| *bytes > 0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::mem_total_bytes;

    /// The first three lines of one real `/proc/meminfo`, read 2026-09-01 on
    /// a 96 GiB box, kept as text so the parser is tested against the format
    /// the kernel writes rather than one this test invented.
    const FIXTURE: &str = "MemTotal:       98459412 kB\n\
                           MemFree:        23345628 kB\n\
                           MemAvailable:   47241536 kB\n";

    #[test]
    fn mem_total_is_read_in_kib_and_scaled_to_bytes() {
        assert_eq!(mem_total_bytes(FIXTURE), Some(98_459_412 * 1024));
    }

    #[test]
    fn a_meminfo_without_a_total_is_unknown_not_zero() {
        assert_eq!(mem_total_bytes("MemFree:        23345628 kB\n"), None);
        assert_eq!(mem_total_bytes(""), None);
    }

    /// Only the key at the head of a line is the key: a field that merely
    /// contains `MemTotal` is not it.
    #[test]
    fn the_total_is_found_wherever_its_line_sits_and_nowhere_else() {
        assert_eq!(
            mem_total_bytes("MemFree:        1 kB\nMemTotal:       2 kB\n"),
            Some(2048),
        );
        assert_eq!(mem_total_bytes("HugeMemTotal:   2 kB\n"), None);
    }

    #[test]
    fn an_unrecognised_unit_figure_or_zero_is_refused_rather_than_guessed() {
        assert_eq!(mem_total_bytes("MemTotal:       98459412 MB\n"), None);
        assert_eq!(mem_total_bytes("MemTotal:       98459412\n"), None);
        assert_eq!(mem_total_bytes("MemTotal:       lots kB\n"), None);
        assert_eq!(mem_total_bytes("MemTotal:       0 kB\n"), None);
    }
}
