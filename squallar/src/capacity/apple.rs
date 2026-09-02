//! macOS and iOS: `NSProcessInfo.physicalMemory`.

use objc2_foundation::NSProcessInfo;

/// Total physical memory in bytes. `processInfo` is a class method that
/// always answers and `physicalMemory` a plain getter, so the only `None` is
/// a zero — which no device reports, and which would otherwise read as small
/// rather than as unread.
pub fn system_ram_bytes() -> Option<u64> {
    let bytes = NSProcessInfo::processInfo().physicalMemory();
    (bytes > 0).then_some(bytes)
}
