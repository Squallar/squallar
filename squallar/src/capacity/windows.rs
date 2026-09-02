//! Windows: `GlobalMemoryStatusEx`.

use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// Total physical memory in bytes, or `None` when the call fails.
///
/// The crate is `deny(unsafe_code)`; this function carries the scoped allow
/// because the API writes through a raw pointer into a caller-owned struct
/// whose `dwLength` names its own size, and that is the whole of the
/// contract.
#[allow(
    unsafe_code,
    reason = "GlobalMemoryStatusEx writes through a raw pointer to a struct this function owns"
)]
pub fn system_ram_bytes() -> Option<u64> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a live, correctly sized `MEMORYSTATUSEX` for the
    // whole call, and `dwLength` names its size as the API requires.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    (status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}
