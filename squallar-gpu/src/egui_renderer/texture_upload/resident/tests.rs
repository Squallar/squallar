//! The resident level's arithmetic. Host tests: no device, no browser.
//!
//! What is checked here is the ledger, not the routing. The routing — which
//! site fires for which delta — is `tests/raster_upload_gpu.rs`, on a real
//! adapter, driving `apply` and `free` themselves.

use super::*;

fn managed(n: u64) -> TextureId {
    TextureId::Managed(n)
}

/// One named mutation of the ledger, for the drift run in
/// `the_maintained_total_never_parts_from_a_walk_of_the_maps`. The name is
/// what the failure quotes, so a drift says which site produced it.
type Step = (&'static str, fn(&mut ResidentTextures));

/// A texture's charge is its texels at four bytes, and the arithmetic
/// saturates rather than wrapping on a size no device would accept.
#[test]
fn a_charge_is_the_texture_and_not_the_delta() {
    assert_eq!(texture_bytes([1, 1]), 4);
    assert_eq!(texture_bytes([1024, 1024]), 4 << 20);
    // A WSR-88D surveillance cut at this box's ceiling.
    assert_eq!(texture_bytes([7362, 7362]), 216_796_176);
    assert_eq!(texture_bytes([0, 4096]), 0);
    assert_eq!(texture_bytes([usize::MAX, usize::MAX]), u64::MAX);
}

/// **The level falls on a free.** The half that matters: a figure that only
/// rises is the cumulative counter `UploadTotals` already is.
#[test]
fn the_level_falls_when_a_texture_is_freed() {
    let mut resident = ResidentTextures::default();
    assert_eq!(resident.bytes(), 0);

    resident.egui_allocated(managed(1), [256, 256]);
    resident.owned_allocated(managed(2), [1024, 1024]);
    let peak = resident.bytes();
    assert_eq!(peak, (256 * 256 * 4) + (1024 * 1024 * 4));

    resident.freed(managed(2));
    assert_eq!(
        resident.bytes(),
        256 * 256 * 4,
        "the level did not give back the freed texture's bytes",
    );
    resident.freed(managed(1));
    assert_eq!(
        resident.bytes(),
        0,
        "every texture was freed and the level did not come back to zero",
    );
    assert_eq!(resident.charges(), (0, 0), "a freed id kept a charge");
}

/// A replace is a set, not an add: the same id uploaded twice holds one
/// texture, and holds the second one's bytes.
#[test]
fn a_replaced_texture_costs_the_new_size_and_not_both() {
    let mut resident = ResidentTextures::default();
    let id = managed(9);

    resident.egui_allocated(id, [512, 512]);
    assert_eq!(resident.bytes(), 512 * 512 * 4);
    resident.egui_allocated(id, [64, 64]);
    assert_eq!(
        resident.bytes(),
        64 * 64 * 4,
        "a replaced texture's old bytes stayed on the level, so the figure \
         climbs with the session the way a running total does",
    );
    assert_eq!(resident.charges(), (1, 0), "one id held two charges");
    assert_eq!(resident.walked_bytes(), resident.bytes());
}

/// **The two sides are a sum and not an upper bound.** One id can hold a 1x1
/// stand-in of egui's and this module's own big raster at the same time, and
/// both are really on the device until the id is freed.
#[test]
fn an_id_holding_a_stand_in_and_a_raster_is_charged_for_both() {
    let mut resident = ResidentTextures::default();
    let id = managed(3);

    // `seed` puts the stand-in there through egui's own path.
    resident.egui_allocated(id, [1, 1]);
    // The drain allocates the raster on a later frame.
    resident.owned_allocated(id, [2048, 2048]);
    assert_eq!(resident.bytes(), 4 + (2048 * 2048 * 4));
    assert_eq!(resident.charges(), (1, 1));

    // A second full delta for the same raster: the owned texture is superseded
    // at file time and the new one arrives when the drain allocates it. The
    // stand-in is egui's and does not move.
    resident.owned_dropped(id);
    assert_eq!(
        resident.bytes(),
        4,
        "dropping the owned texture took the stand-in with it",
    );
    resident.owned_allocated(id, [1024, 1024]);
    assert_eq!(resident.bytes(), 4 + (1024 * 1024 * 4));

    resident.freed(id);
    assert_eq!(
        resident.bytes(),
        0,
        "a free left one of the two sides of the id charged",
    );
}

/// Discharging an id that was never charged is a no-op, not a subtraction —
/// `free` is handed every id egui retires, including ones this module's own
/// side never allocated for.
#[test]
fn freeing_an_id_that_was_never_charged_moves_nothing() {
    let mut resident = ResidentTextures::default();
    resident.egui_allocated(managed(1), [8, 8]);
    let held = resident.bytes();

    resident.freed(managed(77));
    resident.owned_dropped(managed(1));
    assert_eq!(
        resident.bytes(),
        held,
        "an id with no charge on a side still moved the level",
    );
    assert_eq!(resident.walked_bytes(), resident.bytes());
}

/// **The maintained total and a walk of the maps agree**, over a run that hits
/// every mutation the ledger has. This is the drift check: a site that moves a
/// map without moving the total, or the reverse, parts them here.
#[test]
fn the_maintained_total_never_parts_from_a_walk_of_the_maps() {
    let mut resident = ResidentTextures::default();
    let steps: &[Step] = &[
        ("egui_allocated 1", |r| {
            r.egui_allocated(managed(1), [64, 32])
        }),
        ("egui_allocated 2", |r| r.egui_allocated(managed(2), [1, 1])),
        ("owned_allocated 2", |r| {
            r.owned_allocated(managed(2), [512, 512])
        }),
        ("egui_allocated 1 again", |r| {
            r.egui_allocated(managed(1), [128, 128])
        }),
        ("owned_dropped 2", |r| r.owned_dropped(managed(2))),
        ("owned_allocated 2 again", |r| {
            r.owned_allocated(managed(2), [256, 256])
        }),
        ("owned_allocated 3", |r| {
            r.owned_allocated(managed(3), [16, 16])
        }),
        ("freed 2", |r| r.freed(managed(2))),
        ("freed 1", |r| r.freed(managed(1))),
        ("freed 3", |r| r.freed(managed(3))),
    ];
    for (site, step) in steps {
        step(&mut resident);
        assert_eq!(
            resident.bytes(),
            resident.walked_bytes(),
            "the maintained total drifted from the maps at `{site}`: the total \
             says {} B and the maps hold {} B",
            resident.bytes(),
            resident.walked_bytes(),
        );
    }
    assert_eq!(resident.bytes(), 0, "the run ended holding bytes");
}

/// **The charge does not depend on the route, and so not on the backend.**
///
/// A delta takes one of two routes and which one is a device property:
/// `goes_whole` compares it against a band cap that is the staging ring's on a
/// device that has one and `BLOCKING_BAND_BYTES` on every device that does
/// not — Vulkan, Metal, GL and WebGL2 do not answer that the same way. Both
/// routes file their charge through `texture_bytes` and nothing else, so a
/// texture of a given size costs the same level whichever route carried it.
///
/// This is what stops the family being a false zero on one backend: the hook
/// is on egui's delta seam, above wgpu, not inside either upload path.
#[test]
fn the_charge_is_the_same_whichever_route_carried_the_texture() {
    let size = [1806, 1806];

    let mut whole = ResidentTextures::default();
    whole.egui_allocated(managed(1), size);

    let mut banded = ResidentTextures::default();
    // What the banded route does: a stand-in through egui's path, then this
    // module's own texture when the drain allocates it.
    banded.egui_allocated(managed(1), [1, 1]);
    banded.owned_allocated(managed(1), size);

    assert_eq!(
        banded.bytes() - whole.bytes(),
        4,
        "the two routes priced a {size:?} texture differently by more than the \
         1x1 stand-in the banded one leaves behind: whole {} B, banded {} B",
        whole.bytes(),
        banded.bytes(),
    );
    assert_eq!(whole.bytes(), texture_bytes(size));
}
