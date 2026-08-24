//! Shared per-arm fixtures for the budget agreement tests that ride with the
//! raymarch.

use squallar_device_profile::budget::{self, BudgetLimits, Budgets, DeviceProfile, Platform};

/// A profile for one shipped bracket, with every runtime field at its most
/// conservative reading.
pub(crate) fn shipped_profile(limits: BudgetLimits) -> DeviceProfile {
    DeviceProfile {
        limits,
        platform: if limits.name == "wasm32" {
            Platform::Web
        } else {
            Platform::Native
        },
        ..DeviceProfile::for_target()
    }
}

/// Every device class this workspace builds for, exactly once.
pub(crate) fn profiles() -> [DeviceProfile; 3] {
    BudgetLimits::SHIPPED.map(shipped_profile)
}

/// What [`profiles`] resolve to.
pub(crate) fn arms() -> [Budgets; 3] {
    profiles().map(|profile| budget::resolve(&profile))
}

/// Bytes one resident voxel grid costs on this arm.
///
/// Read from [`crate::raymarch::resident_grid_bytes`] rather than recomputed, so
/// the budget matches what the upload path allocates. Four bytes per cell:
/// `Rg16Float`, which is filterable under `Features::empty()` where `R32Float`
/// is not.
pub(crate) fn volume_bytes(arm: &Budgets) -> usize {
    crate::raymarch::resident_grid_bytes(arm.grid_cells)
        .expect("a shipped grid shape cannot overflow")
}

/// Frames — and so resident voxel grids — a 3D loop holds, per arm, in
/// [`profiles`] order.
pub(crate) const SHIPPED_VOLUME_LOOP_FRAMES: [usize; 3] = [11, 17, 14];
