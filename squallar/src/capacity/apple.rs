//! macOS and iOS: `NSProcessInfo.physicalMemory` for the total, and the Mach
//! host's own VM statistics for what is free of it.

use objc2_foundation::NSProcessInfo;

/// Total physical memory in bytes. `processInfo` is a class method that
/// always answers and `physicalMemory` a plain getter, so the only `None` is
/// a zero — which no device reports, and which would otherwise read as small
/// rather than as unread.
pub fn system_ram_bytes() -> Option<u64> {
    let bytes = NSProcessInfo::processInfo().physicalMemory();
    (bytes > 0).then_some(bytes)
}

/// **Memory this process could take right now**, in bytes: the Mach host's
/// free plus inactive pages, at the host's page size.
///
/// `None` when `host_statistics64` refuses, when the page size reads zero, or
/// when the sum is zero — a machine with no reclaimable page at all is a call
/// that went wrong, and a pool of zero bytes is a wall every scene is over
/// where an absent pool is the arm every budget already handles.
///
/// **Why free + inactive, and nothing else.** Darwin's `free_count` alone is
/// close to nothing on any machine that has been up: the kernel keeps almost
/// no page free and moves what it no longer needs to the *inactive* queue,
/// which is reclaimable on demand and is what Activity Monitor's own
/// arithmetic treats as available. `active`, `wire` and `compressor` pages
/// are memory in use, and `speculative` is read-ahead the kernel is still
/// betting on; none of them are this process's to take. `purgeable_count` is
/// a subset of the counts already summed here and adding it double-counts.
///
/// **No `mach_port_deallocate`.** `mach_host_self` returns a send right the
/// task already holds — the host name port is a task special port, not a new
/// reference per call — so there is nothing here to release.
///
/// **This figure already excludes this process**, which is why nothing may
/// take a percentage of it directly — see
/// `squallar_device_profile::scene::host_pool_bytes`.
///
/// The crate is `deny(unsafe_code)`; this function carries the scoped allow
/// because the API writes through a raw pointer into a caller-owned struct
/// whose count-in/count-out argument names its own size, and that is the
/// whole of the contract — the same shape as the Windows reader's.
///
/// **UNEXECUTED.** Written and compile-checked from a Linux box on
/// 2026-09-04; no Apple arm has run it. What a run has to confirm is the one
/// thing a compiler cannot: that the sum tracks what the machine will
/// actually hand out. The `vm_stat` command prints the same three counts.
#[allow(
    unsafe_code,
    reason = "host_statistics64 writes through a raw pointer to a struct this function owns"
)]
// `libc::mach_host_self` carries a `deprecated` pointing at `mach2`, and
// `mach2` cannot take this call: the version this workspace resolves (0.4.3)
// exposes neither `mach_host_self` nor `host_statistics64`, and it reaches the
// graph only through `io-kit-sys` behind the serial feature — which is off on
// iOS, where this reader still has to work. `host_statistics64` and
// `vm_page_size` are not deprecated; only the port getter is, and there is no
// second way to name the host port.
#[allow(
    deprecated,
    reason = "mach2 0.4.3 has no mach_host_self and is not in the iOS graph at all"
)]
pub fn available_ram_bytes() -> Option<u64> {
    // The buffer and its count are spelled over the same type, so the size
    // the call is told can never drift from the size it is given.
    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64_data_t>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `stats` is a live, zeroed `vm_statistics64` for the whole call
    // and is written only by it; `count` is the buffer's own size in
    // `integer_t` units, which is the count-in/count-out contract
    // `host_statistics64` states. `mach_host_self` hands over a right this
    // task already holds.
    let status = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if status != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: the call above returned `KERN_SUCCESS`, so it wrote the whole
    // structure. `vm_page_size` is a `static` the kernel fills in at load.
    let (stats, page_bytes) = unsafe { (stats.assume_init(), libc::vm_page_size) };
    // `vm_statistics64` is `repr(packed(8))`: each field is copied out by
    // value rather than referenced, which is what a packed field allows.
    let pages = u64::from(stats.free_count) + u64::from(stats.inactive_count);
    let bytes = pages.checked_mul(page_bytes as u64)?;
    (bytes > 0).then_some(bytes)
}
