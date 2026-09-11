//! **That building a render input does not duplicate the volume's gate
//! arrays**, watched at the allocator.
//!
//! `MomentPayload::from_moment_data` used to call `raw_values().to_vec()`: a
//! fresh allocation and a memcpy of a whole gate array per (radial, moment),
//! out of a `nexrad_model::data::GateBuffer` that has been an `Arc<Vec<u8>>`
//! since the sweep clone was made a refcount bump. `to_moment_data` copied the
//! same array back on the way out. The copies were a property of the
//! signatures — `&[u8]` in, `Vec<u8>` out, neither able to express sharing —
//! and not of any work that needed doing.
//!
//! Measured on 12 volumes of the real Archive II corpus, one per site, four
//! extracts apiece (2026-09-10, release build): **1,558–2,294 allocations in
//! the gate band per volume before, 86–134 after**, and 2,333,792–3,344,192 B
//! before against 160,832–256,832 B after. The residue is not gate copies —
//! there are 1,440–2,160 payloads per volume and 86 blocks left — it is the
//! per-radial containers beside them.
//!
//! **`live_bytes` is the wrong instrument here and this suite deliberately does
//! not use it.** The copy is a *transient*: on the arms that drop a render
//! input after encoding it, the array is taken and freed inside the round, so a
//! level sampled either side reads the same number whether the copy happened or
//! not. What can see it is the allocation itself, so this binary installs a
//! `#[global_allocator]` that counts grants of the gate array's own size within
//! an explicit window — the shape `decode_archive_not_copied.rs` uses, for the
//! same reason.
//!
//! **The size is watched exactly, not as a band.** `GATES` is a number no other
//! allocation in the extract path asks for, so a single grant of it is a copied
//! gate array and nothing else. A band would have to argue about the containers.
//!
//! **One `#[test]`**, because the counter and its window are process-global and
//! libtest runs a binary's tests on several threads, so a second test here
//! would be allocating inside this one's window.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

use nexrad_model::data::{
    ChannelConfiguration, DataMoment, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus,
    Scan, Sweep, VolumeCoveragePattern, WaveformType,
};
use squallar_radar::render_input::RenderInput;
use squallar_radar::types::RadarProduct;

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
/// A real VCP-212 surveillance cut's shape, so the population under test is the
/// one a volume actually carries rather than a picture of four gates.
const RADIALS: usize = 720;
/// **The watched size.** A full-range 250 m surveillance cut, and a number
/// nothing else in the extract path allocates, which is what lets a single
/// grant of it be read as a copied gate array.
const GATES: usize = 1832;
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

static GATE_SIZED: AtomicUsize = AtomicUsize::new(0);
/// **The positive control.** Every grant the window saw, whatever its size.
///
/// Without it the subject assertion is vacuous in the one way that matters: a
/// window that never opened reads zero gate arrays for exactly the same reason
/// a working one does. Reading the ledger around the extract does NOT close
/// that hole — the counters fire whether the window is open or not, so the
/// snapshot counts the work either way, which was **measured** by closing the
/// window early and watching this suite stay green. Only a figure the allocator
/// itself produces inside the window can tell a live window from a dead one.
static ALL_GRANTS: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);

struct GateArrays;

unsafe impl GlobalAlloc for GateArrays {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Relaxed) {
            ALL_GRANTS.fetch_add(1, Relaxed);
            if layout.size() == GATES {
                GATE_SIZED.fetch_add(1, Relaxed);
            }
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: GateArrays = GateArrays;

fn radial(index: usize) -> Radial {
    let spacing = 360.0 / RADIALS as f32;
    let refl: Vec<u8> = (0..GATES).map(|g| ((index + g) % 254 + 2) as u8).collect();
    Radial::new(
        0,
        index as u16,
        index as f32 * spacing,
        spacing,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(MomentData::from_fixed_point(
            GATES as u16,
            FIRST_GATE_M,
            GATE_M,
            8,
            2.0,
            66.0,
            refl,
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn surveillance_scan() -> Scan {
    let cut = ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        1,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    );
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![cut],
        ),
        vec![Sweep::new(1, (0..RADIALS).map(radial).collect())],
    )
}

#[test]
fn extracting_a_render_input_allocates_no_gate_arrays() {
    // The fixture allocates `RADIALS` arrays of exactly `GATES` bytes building
    // itself, which is why the window opens after it and not before.
    let scan = surveillance_scan();

    // **The ledger is read INSIDE the window's own bounds**, not around the
    // extract. A window that opened over nothing reads zero copied arrays for
    // the same reason a working one does, and a snapshot taken outside the
    // stores cannot tell the two apart — it would count the work either way.
    // Bracketed here, so the anti-vacuity assertion below fails if the window
    // ever stops covering the extract.
    ALL_GRANTS.store(0, Relaxed);
    COUNTING.store(true, Relaxed);
    let ledger_before = (
        squallar_radar::payload_share::adopted_count(),
        squallar_radar::payload_share::adopted_bytes(),
    );
    let input = RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
        .expect("a surveillance-shaped cut extracts");
    let ledger_after = (
        squallar_radar::payload_share::adopted_count(),
        squallar_radar::payload_share::adopted_bytes(),
    );
    COUNTING.store(false, Relaxed);

    let copied = GATE_SIZED.load(Relaxed);
    assert_eq!(
        copied, 0,
        "{copied} gate array(s) of {GATES} B were allocated during the extract; \
         the payload is copying gates instead of sharing the volume's buffer"
    );

    // **The window was live over work that would have tripped it**, said by
    // the allocator and not by the ledger — see `ALL_GRANTS`. An extract over
    // 720 radials cannot ask the allocator for nothing.
    // **Above zero, not above a tight floor.** MEASURED at 5 grants for this
    // 720-radial extract — the sweep vector, the radial vector and its growth,
    // and nothing per radial, because the payloads live inline in that vector
    // and their gates are now shared. That the whole extract asks the allocator
    // for five blocks is the cut restated; the guard only has to separate a live
    // window from a dead one, so it is bounded by DISTANCE from zero rather
    // than pinned near a figure that moves with an unrelated `reserve`.
    let grants = ALL_GRANTS.load(Relaxed);
    assert!(
        grants > 0,
        "the window saw no grants at all over a {RADIALS}-radial extract; it was not open"
    );

    let adopted = ledger_after.0 - ledger_before.0;
    assert_eq!(
        adopted, RADIALS as u64,
        "the extract built {adopted} payload(s), not the {RADIALS} the sweep carries — \
         the window did not cover the work it is asserting about"
    );
    assert_eq!(
        ledger_after.1 - ledger_before.1,
        RADIALS as u64
            * (GATES as u64 + squallar_radar::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64),
        "priced against what the fixture DESCRIBES: one {GATES} B array and one block per radial"
    );
    assert_eq!(
        squallar_radar::payload_share::copied(),
        0,
        "a payload fell back to copying its gates"
    );

    // ── And the way back, in the same binary ─────────────────────────────────
    //
    // `to_scan` rebuilds the volume through `to_moment_data`, which copied the
    // same arrays a second time. A separate window, and the count is reset
    // rather than read as a delta so the assertion names one leg.
    GATE_SIZED.store(0, Relaxed);
    ALL_GRANTS.store(0, Relaxed);
    COUNTING.store(true, Relaxed);
    let returned_before = squallar_radar::payload_share::returned_count();
    let rebuilt = input.to_scan();
    let returned_after = squallar_radar::payload_share::returned_count();
    COUNTING.store(false, Relaxed);

    let grants_back = ALL_GRANTS.load(Relaxed);
    assert!(
        grants_back > 0,
        "the rebuild window saw no grants at all over {RADIALS} radials; it was not open"
    );
    let copied_back = GATE_SIZED.load(Relaxed);
    assert_eq!(
        copied_back, 0,
        "{copied_back} gate array(s) were allocated rebuilding the scan;          `to_moment_data` is copying gates out of the payload"
    );
    assert_eq!(
        returned_after - returned_before,
        RADIALS as u64,
        "the rebuild did not go through the {RADIALS} payloads this window covers"
    );

    // **Pointer identity, with every owner held.** An address compared after its
    // owner dropped can match a reused block, so `scan`, `input` and `rebuilt`
    // are all alive here. The fixture makes this sharp rather than tautological:
    // each radial's gates are a separate allocation, so only `shares_with` can
    // tell the right buffer from 719 same-sized neighbours.
    for i in 0..RADIALS {
        let source = scan.sweeps()[0].radials()[i]
            .reflectivity()
            .expect("the fixture gives every radial reflectivity");
        let through = rebuilt.sweeps()[0].radials()[i]
            .reflectivity()
            .expect("and the rebuild keeps it");
        assert!(
            through.gate_buffer().shares_with(source.gate_buffer()),
            "radial {i} came back on a different allocation from the one it went in on"
        );
    }
    drop(rebuilt);
    drop(input);
    drop(scan);
}
