//! Linux and Android: `MemTotal` and `MemAvailable` out of `/proc/meminfo`.

/// Total physical memory in bytes, or `None` when `/proc/meminfo` cannot be
/// read or says nothing [`field_bytes`] accepts.
pub fn system_ram_bytes() -> Option<u64> {
    field_bytes_of_meminfo("MemTotal:")
}

/// **Memory this process could take right now** without pushing the machine
/// into reclaim, in bytes — `MemAvailable`, the kernel's own estimate: free
/// pages plus the reclaimable part of the page cache and the slabs, less each
/// zone's low watermark (`fs/proc/meminfo.c`, `si_mem_available`).
///
/// `None` where `/proc/meminfo` cannot be read or carries no such line. The
/// field has existed since Linux 3.14 and every arm this build ships to is
/// far past it, but a container mounting a doctored `/proc`, or a kernel
/// built without it, must read as unknown rather than as a machine with no
/// memory free.
///
/// **Never `MemFree`.** Free pages alone under-state what is available by the
/// whole reclaimable page cache — on a box that has been up a day that is
/// most of RAM — and a capacity taken from it would shed rungs on a machine
/// holding nothing but cache. The kernel already computes the figure that
/// answers the question; this reads it rather than re-deriving it.
///
/// **This figure already excludes this process**, which is why nothing may
/// take a percentage of it directly — see
/// `squallar_device_profile::scene::host_pool_bytes`.
pub fn available_ram_bytes() -> Option<u64> {
    field_bytes_of_meminfo("MemAvailable:")
}

/// One `/proc/meminfo` read, one field.
///
/// Read per call rather than cached: `MemAvailable` moves with every other
/// process on the machine, and a cached figure would be exactly the
/// high-water mark this reader exists to replace. The file is a few hundred
/// bytes of procfs and its callers poll it on the telemetry tick, never on
/// the frame thread.
fn field_bytes_of_meminfo(key: &str) -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .as_deref()
        .and_then(|meminfo| field_bytes(meminfo, key))
}

/// The named field of a `/proc/meminfo` text, in bytes. `key` carries its own
/// colon, so `MemTotal:` cannot match the tail of `HugeMemTotal:`.
///
/// The kernel prints the figure in `kB` and means 1024-byte units
/// (`fs/proc/meminfo.c`'s `show_val_kb` shifts pages by `PAGE_SHIFT - 10`),
/// so that is the one unit accepted. Any other unit, a missing line, an
/// unparsable figure or a zero is `None` rather than a guess: a wrong RAM
/// figure is worse than an absent one, because absent is the arm every
/// budget already handles.
pub(super) fn field_bytes(meminfo: &str, key: &str) -> Option<u64> {
    let line = meminfo.lines().find_map(|line| line.strip_prefix(key))?;
    let mut parts = line.split_whitespace();
    let kib: u64 = parts.next()?.parse().ok()?;
    match parts.next() {
        Some("kB") => kib.checked_mul(1024).filter(|bytes| *bytes > 0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{available_ram_bytes, field_bytes, system_ram_bytes};

    fn mem_total_bytes(meminfo: &str) -> Option<u64> {
        field_bytes(meminfo, "MemTotal:")
    }

    fn mem_available_bytes(meminfo: &str) -> Option<u64> {
        field_bytes(meminfo, "MemAvailable:")
    }

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

    /// **The available figure is its own line, and it is not the free one.**
    /// The fixture's `MemFree` is half its `MemAvailable` on that box; a
    /// reader that took the first `Mem…` line it found, or fell back to
    /// `MemFree` when `MemAvailable` was absent, would under-state what this
    /// process can take by 23 GiB.
    #[test]
    fn mem_available_is_read_in_kib_and_is_never_mem_free() {
        assert_eq!(mem_available_bytes(FIXTURE), Some(47_241_536 * 1024));
        assert_ne!(mem_available_bytes(FIXTURE), Some(23_345_628 * 1024));
    }

    #[test]
    fn a_meminfo_without_a_total_is_unknown_not_zero() {
        assert_eq!(mem_total_bytes("MemFree:        23345628 kB\n"), None);
        assert_eq!(mem_total_bytes(""), None);
    }

    /// A kernel too old for `MemAvailable`, or a `/proc` that hides it, reads
    /// as unknown. The one thing it must not read as is zero: a pool of zero
    /// bytes is a wall every scene is over, where an absent pool is the arm
    /// every budget already handles.
    #[test]
    fn a_meminfo_without_an_available_line_is_unknown_not_zero() {
        assert_eq!(
            mem_available_bytes("MemTotal:       98459412 kB\nMemFree:  23345628 kB\n"),
            None,
        );
        assert_eq!(mem_available_bytes(""), None);
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
        for key in ["MemTotal:", "MemAvailable:"] {
            let junk = |tail: &str| field_bytes(&format!("{key}       {tail}\n"), key);
            assert_eq!(junk("98459412 MB"), None, "{key}");
            assert_eq!(junk("98459412"), None, "{key}");
            assert_eq!(junk("lots kB"), None, "{key}");
            assert_eq!(junk("0 kB"), None, "{key}");
        }
    }

    /// **The readers executed against the live file**, not a fixture: the
    /// parser above is worth only what the real `/proc/meminfo` gives it.
    ///
    /// The premise is stated rather than assumed — a machine that cannot read
    /// the file at all (a locked-down container, a `/proc`-less chroot) is a
    /// legitimate `None` on both readings, and this test asserts nothing
    /// there, which is the fail-soft contract. Where the file *is* readable
    /// both fields must answer, and available can never exceed total: a pool
    /// larger than the machine is a reader bug every fixture above would pass.
    #[test]
    fn the_live_meminfo_answers_both_fields_and_available_never_exceeds_total() {
        if std::fs::read_to_string("/proc/meminfo").is_err() {
            return;
        }
        let total = system_ram_bytes().expect("a readable /proc/meminfo states MemTotal");
        let available =
            available_ram_bytes().expect("a readable /proc/meminfo states MemAvailable");
        assert!(
            available <= total,
            "available {available} B over total {total} B",
        );
        // Captured unless `--nocapture`, and there so the figures can be read
        // beside `free -m`'s: an assertion that two numbers are ordered is
        // not evidence that either is the right number, and the only check
        // available for that is an independent reader of the same file.
        const MIB: u64 = 1024 * 1024;
        println!(
            "capacity::linux: total {} MiB, available {} MiB",
            total / MIB,
            available / MIB,
        );
    }
}
