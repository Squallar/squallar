//! Windows: `GlobalMemoryStatusEx`.

use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// Total physical memory in bytes, or `None` when the call fails.
pub fn system_ram_bytes() -> Option<u64> {
    non_zero(memory_status()?.ullTotalPhys)
}

/// **Memory this process could take right now**, in bytes — `ullAvailPhys`,
/// the physical memory currently available, which is what the system can hand
/// out without paging something else to disk.
///
/// `None` when the call fails, and `None` on a zero: a machine reporting no
/// physical memory available is a call that went wrong, and a pool of zero
/// bytes is a wall every scene is over, where an absent pool is the arm every
/// budget already handles.
///
/// **Not `ullAvailPageFile` and not `ullAvailVirtual`.** The first is the
/// commit limit's headroom, which the page file backs and which therefore
/// promises memory this app would pay for in disk latency; the second is
/// address space, which on 64-bit says nothing about RAM at all.
///
/// **This figure already excludes this process**, which is why nothing may
/// take a percentage of it directly — see
/// `squallar_device_profile::scene::host_pool_bytes`.
pub fn available_ram_bytes() -> Option<u64> {
    non_zero(memory_status()?.ullAvailPhys)
}

/// One `GlobalMemoryStatusEx` call, which fills every field both readers
/// above want.
///
/// Read per call rather than cached: `ullAvailPhys` moves with every other
/// process on the machine, and a cached figure would be exactly the
/// high-water mark the available reader exists to replace.
///
/// The crate is `deny(unsafe_code)`; this function carries the scoped allow
/// because the API writes through a raw pointer into a caller-owned struct
/// whose `dwLength` names its own size, and that is the whole of the
/// contract.
#[allow(
    unsafe_code,
    reason = "GlobalMemoryStatusEx writes through a raw pointer to a struct this function owns"
)]
fn memory_status() -> Option<MEMORYSTATUSEX> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a live, correctly sized `MEMORYSTATUSEX` for the
    // whole call, and `dwLength` names its size as the API requires.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status)
}

/// A zero from a call that reported success is unread, never a figure: a wall
/// of zero bytes reads as a machine with no memory, where `None` reads as the
/// unknown it is.
fn non_zero(bytes: u64) -> Option<u64> {
    (bytes > 0).then_some(bytes)
}
