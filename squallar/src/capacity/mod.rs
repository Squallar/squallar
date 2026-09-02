//! What this machine can hold, read where an API exists.
//!
//! One RAM reader per OS and one GPU reader per backend, selected by which
//! module is mounted: a `cfg` here picks a module and never forks a body.
//! Every reader answers `None` rather than a guess when its API does not — a
//! signal a platform cannot read is absent, and the device profile treats
//! absent as the majority arm rather than as small.

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub use linux::system_ram_bytes;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::system_ram_bytes;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::system_ram_bytes;

/// An OS this crate has no reader for. Unknown, not zero.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
)))]
mod unknown {
    pub fn system_ram_bytes() -> Option<u64> {
        None
    }
}
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
)))]
pub use unknown::system_ram_bytes;

// ── GPU readers, one per backend ──

#[cfg(any(target_os = "linux", target_os = "android", target_os = "windows"))]
mod vulkan;
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "windows")))]
use unread as vulkan;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod metal;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
use unread as metal;

#[cfg(target_os = "windows")]
mod dx12;
#[cfg(not(target_os = "windows"))]
use unread as dx12;

/// The reader for a backend this platform never builds. Unknown, not zero.
mod unread {
    use squallar_app::platform::GpuCapacitySource;

    pub fn gpu_capacity(
        _adapter: &wgpu::Adapter,
        _device: &wgpu::Device,
    ) -> Option<(u64, GpuCapacitySource)> {
        None
    }
}

use squallar_app::platform::{FormFactor, GpuCapacitySource, HostSignals};

/// Whether a driver's "local" memory figure is memory the GPU has. Only a
/// discrete card has memory of its own; on a UMA part the heaps Vulkan flags
/// device-local and the segment group DXGI calls local are system RAM under
/// another name, and the figure lies in both directions — this box's llvmpipe
/// reports 93.9 GiB of "device-local" memory, and an APU reports a 256 MiB
/// carve-out for a GPU that can use the whole pool. Metal is not asked this
/// question: its working set is Apple's own figure for the shared pool.
pub fn trust_local_heaps(device_type: wgpu::DeviceType) -> bool {
    matches!(device_type, wgpu::DeviceType::DiscreteGpu)
}

/// The GPU's capacity in bytes and how it was read, or `None` where no reader
/// exists for this adapter: GL and both browser backends state nothing, and
/// the two heap-listing backends are believed for a discrete card only.
///
/// The `match` is on what the adapter reports at run time, not on a `cfg`: a
/// platform with more than one native backend (Windows has three) picks the
/// reader by the backend wgpu actually opened, and a backend this build never
/// compiles is mounted as `unread`.
pub fn gpu_capacity(
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
) -> Option<(u64, GpuCapacitySource)> {
    let info = adapter.get_info();
    match info.backend {
        wgpu::Backend::Vulkan | wgpu::Backend::Dx12 if !trust_local_heaps(info.device_type) => None,
        wgpu::Backend::Vulkan => vulkan::gpu_capacity(adapter, device),
        wgpu::Backend::Dx12 => dx12::gpu_capacity(adapter, device),
        wgpu::Backend::Metal => metal::gpu_capacity(adapter, device),
        wgpu::Backend::Gl | wgpu::Backend::BrowserWebGpu | wgpu::Backend::Noop => None,
    }
}

/// Threads the OS says this process may run at once, or `None` when it will
/// not say. The machine's own figure — not the size of any pool built on it.
pub fn parallelism() -> Option<usize> {
    std::thread::available_parallelism()
        .ok()
        .map(std::num::NonZeroUsize::get)
}

/// Every host signal a native bridge hands over. `form_factor` is the one
/// term the bridge knows and this module does not: a build fact, spelled by
/// whichever bridge was compiled. Declared RAM is a browser's notion and is
/// `None` on every native target.
pub fn host_signals(form_factor: FormFactor) -> HostSignals {
    HostSignals {
        system_ram_bytes: system_ram_bytes(),
        declared_ram_bytes: None,
        parallelism: parallelism(),
        form_factor: Some(form_factor),
    }
}

#[cfg(test)]
mod tests {
    use super::trust_local_heaps;
    use wgpu::DeviceType;

    /// This box on 2026-09-02, per `vulkaninfo`: the RTX 3090 lists 24 GiB and
    /// 246 MiB device-local; llvmpipe beside it lists 93.9 GiB device-local,
    /// which is the machine's RAM. Same flag, one true and one a lie, and the
    /// device type is the only thing that tells them apart.
    #[test]
    fn only_a_discrete_card_is_believed_about_its_local_heaps() {
        assert!(trust_local_heaps(DeviceType::DiscreteGpu));
        for lying in [
            DeviceType::IntegratedGpu,
            DeviceType::Cpu,
            DeviceType::VirtualGpu,
            DeviceType::Other,
        ] {
            assert!(!trust_local_heaps(lying), "{lying:?}");
        }
    }
}
