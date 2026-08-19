//! Shared per-arm fixtures for the app-side budget agreement tests.
//!
//! WO-RD moved the budget/constants cascades down into rustdar-device-profile,
//! and with them most of their tests — but the proofs that *bridge upward*
//! (to the raymarch arithmetic, the loop pool, the mirror plan) stayed beside
//! the modules they read, because the policy floor must not call up into any
//! of them. Those relocated tests share these three fixtures; the floor
//! crate's own test modules keep their private twins.

use rustdar_device_profile::budget::{self, BudgetLimits, Budgets, DeviceProfile, Platform};

/// A profile for one shipped bracket, with every runtime field at its most
/// conservative reading. The frontend copy of the floor crate's test helper of
/// the same name, restated here because a test helper does not cross a crate
/// boundary.
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

/// Every device class this workspace builds for, exactly once — as the
/// **profiles** they are.
pub(crate) fn profiles() -> [DeviceProfile; 3] {
    BudgetLimits::SHIPPED.map(shipped_profile)
}

/// What [`profiles`] resolve to. The loop variable at every use is still
/// called `arm` because that is what one row of this is: one device class's
/// share of every cascade in the floor crate.
pub(crate) fn arms() -> [Budgets; 3] {
    profiles().map(|profile| budget::resolve(&profile))
}

/// Bytes one resident voxel grid costs on this arm.
///
/// Read from `volume::raymarch::resident_grid_bytes` rather than recomputed, so
/// the budget is checked against the arithmetic the upload path allocates by —
/// every mip level the device lays the texture out with, the colour table's own
/// texture, and the jitter tile created beside it. The earlier hand-written
/// product left the coarse level out of the budget entirely, and the version
/// after that charged the two levels the descriptor names rather than the whole
/// pyramid the driver reserves.
///
/// Spelled against the raymarch directly rather than through a `Budgets`
/// method: `Budgets::volume_bytes` was deleted at WO-RD because the resolver
/// lives below the raymarch and must not call up into it — this is the same
/// shape `loop_pool`'s production read takes.
///
/// Four bytes per cell is not an assumption to be tidied away: the format is
/// `Rg16Float` because the march reconstructs `R̄ / Ḡ` from a
/// coverage-premultiplied index and a coverage channel — which needs a filter
/// error that scales with the sample rather than with the format, or the
/// quotient is wrong by the whole palette at an echo edge — and because
/// `Rg16Float` is *filterable* under `Features::empty()` where `R32Float` is
/// not.
pub(crate) fn volume_bytes(arm: &Budgets) -> usize {
    crate::volume::raymarch::resident_grid_bytes(arm.grid_cells)
        .expect("a shipped grid shape cannot overflow")
}

/// Frames — and so resident voxel grids — a 3D loop holds, per arm, in
/// [`profiles`] order.
///
/// **Literals.** They used to be `MAX_LOOP_VOLUME_FRAMES`, a `cfg` cascade with
/// no runtime consumer that restated what `LoopPool::plan` already computes.
/// The constant is retired; the count is the planner's answer,
/// `loop_pool::tests::the_pool_reproduces_the_shipped_3d_frame_count` binds the
/// planner to these same figures, and
/// `the_3d_loop_holds_exactly_what_it_marches` binds them to the budget
/// arithmetic they came out of.
pub(crate) const SHIPPED_VOLUME_LOOP_FRAMES: [usize; 3] = [11, 17, 14];
