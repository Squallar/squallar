//! The building geometry ceiling is one number stated twice, and this crate is
//! where the two statements can be compared.
//!
//! `squallar_buildings::DEFAULT_PRISM_VRAM_BYTES` is the worker side's own
//! default for the VRAM row its prism ladder is fitted inside, and
//! `squallar_device_profile::budget::Budgets::prism_vram_bytes` is the same
//! row resolved as a budget. Neither crate may name the other: the buildings
//! crate links inside the offload worker and its charter forbids the device
//! floor, and the device floor's charter declares `squallar-radar` and nothing
//! else. `squallar-worker` declares both, so the agreement lives here.
//!
//! # Where the resolved figure is to be threaded
//!
//! `squallar_buildings::BuildingMeshJob` is registered and its wire row is
//! pinned, but no production code constructs one yet -- every constructor is
//! in a test. When a dispatch site lands, it fills the job's ceilings from the
//! budgets it already holds rather than from the worker's default:
//!
//! ```text
//! ceilings.vram_bytes = budgets.prism_vram_bytes as u64
//! ```
//!
//! Until then the two figures agree by this test and nothing spends either.

use squallar_buildings::{DEFAULT_PRISM_VRAM_BYTES, PrismCeilings};
use squallar_device_profile::budget::{BudgetLimits, DeviceProfile, Platform, Promotion, resolve};

/// A profile for one shipped bracket, with every runtime field at the reading
/// this build has before it has met an adapter.
fn shipped_profile(limits: BudgetLimits) -> DeviceProfile {
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

/// The worker's default and the resolved budget are the same bytes on every
/// shipped arm, at every rung the arm can earn.
#[test]
fn the_workers_default_prism_row_is_the_resolved_budget_on_every_arm() {
    assert_eq!(
        PrismCeilings::DEFAULT.vram_bytes,
        DEFAULT_PRISM_VRAM_BYTES,
        "the worker's default ceilings no longer carry the default row"
    );
    for limits in BudgetLimits::SHIPPED {
        let budgets = resolve(&shipped_profile(limits));
        assert_eq!(
            budgets.prism_vram_bytes as u64, DEFAULT_PRISM_VRAM_BYTES,
            "{}: the device floor resolves a building geometry row the worker's \
             own default disagrees with; a job dispatched from one and fitted \
             against the other would be budgeted twice, differently",
            limits.name,
        );
        for promotion in [Promotion::Floor, Promotion::Step, Promotion::Ceiling] {
            assert_eq!(
                limits.prism_geometry_bytes.at(promotion) as u64,
                DEFAULT_PRISM_VRAM_BYTES,
                "{}: the row moves with {promotion:?}, and it is pinned because \
                 it was measured on one machine",
                limits.name,
            );
        }
    }
}
