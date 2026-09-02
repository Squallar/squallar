//! Linux, Android and Windows over Vulkan: the heaps flagged `DEVICE_LOCAL`.

use ash::vk;
use squallar_app::platform::GpuCapacitySource;

/// The sum of the adapter's device-local heaps, or `None` when the adapter is
/// not Vulkan's or lists none.
///
/// Whether the heaps can be believed is settled before this is called: on a
/// UMA part the driver flags system RAM device-local (this box's llvmpipe
/// lists 93.9 GiB of it), so [`super::gpu_capacity`] asks only for a discrete
/// card. The crate is `deny(unsafe_code)`; the scoped allow is for `as_hal`,
/// which hands out the backend object wgpu wraps, and for the Vulkan query
/// itself.
#[allow(
    unsafe_code,
    reason = "Adapter::as_hal and vkGetPhysicalDeviceMemoryProperties are unsafe fns; both only read"
)]
pub fn gpu_capacity(
    adapter: &wgpu::Adapter,
    _device: &wgpu::Device,
) -> Option<(u64, GpuCapacitySource)> {
    // SAFETY: the guard borrows `adapter`, which outlives this function, and
    // nothing here destroys or writes through the hal adapter -- it is read
    // for its physical-device handle and its instance.
    let hal = unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() }?;
    // SAFETY: the physical device belongs to the instance the same adapter
    // holds, both are alive for the call, and the query writes only into the
    // struct it returns.
    let properties = unsafe {
        hal.shared_instance()
            .raw_instance()
            .get_physical_device_memory_properties(hal.raw_physical_device())
    };
    let listed = (properties.memory_heap_count as usize).min(properties.memory_heaps.len());
    let heaps: Vec<(u64, bool)> = properties.memory_heaps[..listed]
        .iter()
        .map(|heap| {
            (
                heap.size,
                heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL),
            )
        })
        .collect();
    squallar_gpu::capacity::device_local_total(&heaps)
        .map(|bytes| (bytes, GpuCapacitySource::Measured))
}
