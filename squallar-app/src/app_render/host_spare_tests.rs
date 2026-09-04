//! **Host spare is the model's figure bounded by the heap's**, on both arms.
//!
//! These run on the host with no allocator installed, which is exactly why the
//! arithmetic is a free function: `squallar-app`'s test binary declares no
//! `#[global_allocator]`, so `squallar_alloc::live_bytes()` is `None` in every
//! test in this crate and the live-bytes arms are unreachable through
//! `compose_budget_readout`. A test that drove the readout would pass without
//! executing the branch it is named for — the arms are fed here instead.

use super::{HostSpareInputs, host_spare_bytes};
use squallar_device_profile::linear_memory::act_line;

const MIB: u64 = 1024 * 1024;

/// A 1 GiB page whose heap has grown to `used`.
fn page(used: u64) -> HostSpareInputs {
    HostSpareInputs {
        wall: Some((1024 * MIB, used)),
        live_bytes: None,
        headroom_bytes: 0,
    }
}

/// **Nothing to say leaves the model's figure alone.** The state every build
/// before the counter was in, and every native binary that installs none: no
/// wall, no live reading, so no bound applies and spare is the model's.
#[test]
fn a_reading_less_instance_falls_back_to_the_models_own_spare() {
    let bare = HostSpareInputs::default();
    assert_eq!(host_spare_bytes(700 * MIB, 768 * MIB, bare), 700 * MIB);
    assert_eq!(host_spare_bytes(0, 768 * MIB, bare), 0);
    // And the default really is the no-information state, so the assertions
    // above are not reading a populated struct.
    assert_eq!(bare.wall, None);
    assert_eq!(bare.live_bytes, None);
}

/// **A wall bounds the model even with no counter** — the behaviour the
/// pricing lane landed, kept exactly.
#[test]
fn a_wall_bounds_the_model_without_any_live_reading() {
    // 1000 of 1024 MiB used: 24 MiB of wall room against 700 MiB of model.
    assert_eq!(
        host_spare_bytes(700 * MIB, 768 * MIB, page(1000 * MIB)),
        24 * MIB
    );
    // A model tighter than the wall room stands: the bound never widens.
    assert_eq!(
        host_spare_bytes(8 * MIB, 768 * MIB, page(1000 * MIB)),
        8 * MIB
    );
}

/// **The live bound is the act line, not the wall.** A page whose heap has
/// grown but whose allocator is holding little has real room, and the figure
/// that says so is live bytes — but only up to the line the watermark acts
/// on, because spare past it is spare the governor takes back.
#[test]
fn a_walled_instance_is_bounded_by_the_act_line_less_live_bytes() {
    // A page at 850 of 1024 MiB whose allocator holds 800: the act line is
    // 890 MiB, so the live term is 90 MiB where the wall's own room is 174 —
    // the live term binds, which is what this test is about. Live bytes are
    // never above `byteLength`, so a fixture must keep `live <= used` or it
    // is asserting about a state no page can be in.
    let max = 1024 * MIB;
    let heap = HostSpareInputs {
        wall: Some((max, 850 * MIB)),
        live_bytes: Some(800 * MIB),
        headroom_bytes: 0,
    };
    let expected = act_line(max, 0).saturating_sub(800 * MIB);
    let wall_room = max - 850 * MIB;
    assert!(
        expected < wall_room,
        "fixture: the live term ({expected}) must bind against the wall's \
         {wall_room}, or this test passes on the wall's arm",
    );
    assert_eq!(host_spare_bytes(700 * MIB, 768 * MIB, heap), expected);

    // The same wall with the heap grown to 1000 MiB and the allocator holding
    // less: now the wall's own room is the tighter bound and the live term
    // steps aside. Both arms of the min, on one page.
    let grown = HostSpareInputs {
        wall: Some((max, 1000 * MIB)),
        live_bytes: Some(300 * MIB),
        headroom_bytes: 0,
    };
    assert_eq!(host_spare_bytes(700 * MIB, 768 * MIB, grown), 24 * MIB);
}

/// **The bound falls and RISES with live bytes** — the whole point of the
/// counter. `byteLength` holds at the wall while the allocator gives memory
/// back, and spare follows the allocator.
#[test]
fn spare_recovers_as_live_bytes_fall_while_the_heap_reading_holds() {
    let max = 1024 * MIB;
    let at = |live: u64| {
        host_spare_bytes(
            700 * MIB,
            768 * MIB,
            HostSpareInputs {
                // The heap reading never moves: a linear memory does not shrink.
                wall: Some((max, 900 * MIB)),
                live_bytes: Some(live),
                headroom_bytes: 0,
            },
        )
    };
    let trapped = at(880 * MIB);
    let recovered = at(300 * MIB);
    assert!(
        recovered > trapped,
        "spare did not recover as live bytes fell: {trapped} then {recovered}",
    );
    // Monotone in live bytes, over the whole range, so the readout can never
    // reward the heap for growing.
    let mut previous = u64::MAX;
    for live in (0..=900).step_by(50).map(|mib| mib * MIB) {
        let spare = at(live);
        assert!(spare <= previous, "spare rose with live bytes at {live}");
        previous = spare;
    }
}

/// **An unwalled instance is bounded by the allowance less live bytes**, and
/// the wall terms do not apply to it. Native's arm.
#[test]
fn an_unwalled_instance_is_bounded_by_the_allowance_less_live_bytes() {
    let native = |live: u64| HostSpareInputs {
        wall: None,
        live_bytes: Some(live),
        headroom_bytes: 0,
    };
    // 768 MiB allowance, 700 MiB held: 68 MiB, under the model's 700.
    assert_eq!(
        host_spare_bytes(700 * MIB, 768 * MIB, native(700 * MIB)),
        68 * MIB
    );
    // Holding more than the allowance saturates at zero rather than wrapping.
    assert_eq!(host_spare_bytes(700 * MIB, 768 * MIB, native(900 * MIB)), 0);
    // And a model tighter than the live bound still stands.
    assert_eq!(
        host_spare_bytes(4 * MIB, 768 * MIB, native(100 * MIB)),
        4 * MIB
    );
}

/// **A page is never bounded twice by the same allocator figure.** The
/// allowance term is native's alone: on a walled instance the pool is the
/// linear memory, and applying `allowance - live` there would bound a browser
/// page by a figure that has nothing to do with its wall.
#[test]
fn the_allowance_term_is_natives_alone() {
    let live = 700 * MIB;
    // A tiny allowance that would dominate if it were applied to a page.
    let tiny_allowance = 8 * MIB;
    let walled = HostSpareInputs {
        wall: Some((1024 * MIB, 100 * MIB)),
        live_bytes: Some(live),
        headroom_bytes: 0,
    };
    let spare = host_spare_bytes(700 * MIB, tiny_allowance, walled);
    assert!(
        spare > tiny_allowance,
        "the allowance term was applied to a walled instance: {spare}",
    );
    // The same inputs with the wall removed: now it binds, which is what says
    // the assertion above is about the arm and not about the numbers.
    let unwalled = HostSpareInputs {
        wall: None,
        ..walled
    };
    assert_eq!(host_spare_bytes(700 * MIB, tiny_allowance, unwalled), 0);
}

/// **Headroom moves the act line, and so moves spare.** The watermark reserves
/// it under the wall; a bigger reservation is less spare, never more.
#[test]
fn reserved_headroom_lowers_the_act_line_and_the_spare_with_it() {
    let at = |headroom: u64| {
        host_spare_bytes(
            700 * MIB,
            768 * MIB,
            HostSpareInputs {
                wall: Some((1024 * MIB, 200 * MIB)),
                live_bytes: Some(100 * MIB),
                headroom_bytes: headroom,
            },
        )
    };
    assert!(
        at(256 * MIB) < at(0),
        "reserving headroom did not lower the spare: {} then {}",
        at(0),
        at(256 * MIB),
    );
}

/// Every bound saturates rather than wrapping, at the extremes a `u64` allows.
#[test]
fn no_bound_wraps_at_the_extremes() {
    let absurd = HostSpareInputs {
        wall: Some((u64::MAX, u64::MAX)),
        live_bytes: Some(u64::MAX),
        headroom_bytes: u64::MAX,
    };
    assert_eq!(host_spare_bytes(u64::MAX, u64::MAX, absurd), 0);
    let empty = HostSpareInputs {
        wall: Some((u64::MAX, 0)),
        live_bytes: Some(0),
        headroom_bytes: 0,
    };
    assert_eq!(host_spare_bytes(0, 0, empty), 0);
}
