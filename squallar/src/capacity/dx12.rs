//! Windows over DX12: DXGI's local video-memory budget.

use squallar_app::platform::GpuCapacitySource;
use windows::Win32::Graphics::Dxgi::{
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
};

/// The `Budget` of the adapter's local segment group on node 0 -- what the OS
/// will let this process hold in the card's own memory right now, which is
/// the figure a budget wants rather than the card's total. `None` when the
/// adapter is not DX12's or the query fails.
///
/// Whether the figure can be believed is settled before this is called: on a
/// UMA part the local group is a share of system RAM, so
/// [`super::gpu_capacity`] asks only for a discrete card. The crate is
/// `deny(unsafe_code)`; the scoped allow is for `as_hal`, which hands out the
/// backend object wgpu wraps, and for the COM call itself.
#[allow(
    unsafe_code,
    reason = "Adapter::as_hal and IDXGIAdapter3::QueryVideoMemoryInfo are unsafe fns; the adapter is only read"
)]
pub fn gpu_capacity(
    adapter: &wgpu::Adapter,
    _device: &wgpu::Device,
) -> Option<(u64, GpuCapacitySource)> {
    // SAFETY: the guard borrows `adapter`, which outlives this function, and
    // the DXGI adapter behind it is only queried.
    let hal = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }?;
    let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
    // SAFETY: `info` is a live, correctly sized struct for the whole call, and
    // the adapter is alive behind the guard above.
    unsafe {
        hal.as_raw()
            .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
    }
    .ok()?;
    (info.Budget > 0).then_some((info.Budget, GpuCapacitySource::Measured))
}
