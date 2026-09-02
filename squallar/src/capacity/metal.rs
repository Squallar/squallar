//! macOS and iOS: Metal's own working-set figure.

use objc2::runtime::NSObjectProtocol;
use objc2_metal::MTLDevice;
use squallar_app::platform::GpuCapacitySource;

/// `recommendedMaxWorkingSetSize`, for every device class: on Apple silicon
/// the GPU has no memory of its own and wgpu's `Integrated` says nothing
/// about how much of the shared pool it may hold, while Metal states that
/// figure directly. `None` when the device is not Metal's, when the OS
/// predates the selector (it arrived in iOS 16; the app ships to 14), or when
/// it answers zero.
///
/// The crate is `deny(unsafe_code)`; the scoped allow is for `as_hal`, which
/// hands out the backend object wgpu wraps.
#[allow(
    unsafe_code,
    reason = "Device::as_hal is an unsafe fn; the Metal device it exposes is only read"
)]
pub fn gpu_capacity(
    _adapter: &wgpu::Adapter,
    device: &wgpu::Device,
) -> Option<(u64, GpuCapacitySource)> {
    // SAFETY: the guard borrows `device`, which outlives this function, and
    // the `MTLDevice` behind it is neither destroyed nor written -- two
    // getters are called on it.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
    let raw = hal.raw_device();
    if !raw.respondsToSelector(objc2::sel!(recommendedMaxWorkingSetSize)) {
        return None;
    }
    let bytes = raw.recommendedMaxWorkingSetSize();
    (bytes > 0).then_some((bytes, GpuCapacitySource::Measured))
}
