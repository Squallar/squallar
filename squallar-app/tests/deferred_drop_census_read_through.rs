//! The heap census's `deferred drops` family is wired to the discard ledger.
//!
//! The family is **read through**, not published: `heap_census::census()`
//! calls `squallar_device_profile::discard_ledger::in_flight_bytes()` at the
//! instant of the read. Two reasons, and the second is why this file exists.
//!
//! A discard lives for one burst and is gone — 27.9 ms for 604 MiB natively,
//! about 0.67 s on wasm — so a 2 s telemetry tick would catch one almost
//! never, and the allocation-error hook reads `census()` after the allocator
//! has already refused, when no publisher is going to run at all.
//!
//! And the family used to read the **deferred queue alone**, which on native
//! is the route that never runs: `offload::discard` hands a native payload to
//! the job pool's `rd-free` lane and leaves the queue empty. So the family
//! reported 0 while the lane held the bytes. A family that is structurally
//! zero on a whole target does not read as broken — it reads as *healthy*,
//! nothing in flight, nothing to worry about — and the family's own
//! instruction to wait on it before calling a fall settled turns that into an
//! immediate all-clear at exactly the moment the reader most needs the truth.
//! A confident wrong answer, where a missing family would at least have shown
//! up in the residual.
//!
//! **Why an integration test.** The ledger and the census are process-global,
//! and inside `squallar-app`'s lib test binary other tests discard payloads of
//! their own. An integration test is its own process, and this is the only
//! test in it, so every figure below is one this file put there — which is
//! what lets the zero arms below be absolute. The precedent and the reasoning
//! are `render_pool_census_read_through.rs`'s.
//!
//! Native only: the lane is the route under test, and on wasm a discarded
//! payload is not dropped until a frame drains the queue. The queue route's
//! own half of this wiring is asserted in `squallar-worker`'s discard tests,
//! which run on the host.
#![cfg(not(target_arch = "wasm32"))]

use squallar_egui::heap_census::census;
use squallar_worker::offload::{Priced, discard};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// MiB-scale, so no sentinel or struct-size floor anywhere else in this
/// process could be mistaken for it.
const PRICE: u64 = 64 << 20;

/// Stops inside `Drop` so the test can read the census at the one instant the
/// payload is genuinely in flight: handed over by the frame thread, not yet
/// freed by the lane.
struct HeldOpen {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl Drop for HeldOpen {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        // `Err` once the test drops its end, which is the release.
        let _ = self.release.recv();
    }
}

#[test]
fn a_reader_of_live_bytes_cannot_get_an_all_clear_while_a_discard_is_in_flight() {
    // The green arm, and the one that would be tempting to skip: a census
    // taken when nothing is in flight must read zero. Without it, a family
    // wired to a constant zero would pass every assertion below that only
    // checks the loaded state.
    assert_eq!(
        census().deferred_drop_bytes,
        0,
        "premise: this process discarded something before its first discard"
    );

    let (entered, has_entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    discard(
        "test-census-in-flight",
        Priced::new(
            PRICE,
            HeldOpen {
                entered,
                release: released,
            },
        ),
    );

    has_entered
        .recv_timeout(Duration::from_secs(10))
        .expect("the free lane must reach the drop");

    // The red arm, and the property the family exists for. Nothing publishes
    // in this process — no telemetry tick runs and there is no setter — so a
    // family that were still published rather than read through would read 0
    // here, which is precisely the all-clear this test refuses.
    let in_flight = census().deferred_drop_bytes;
    assert!(
        in_flight >= PRICE,
        "the census reads {in_flight} B while the lane is holding a {PRICE} B payload; \
         a reader of live bytes would call this fall settled with the bytes still resident"
    );

    // And it follows the ledger back down once the drop lands, so a fall the
    // reader was told to wait for is one it can actually see arrive.
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(10);
    while census().deferred_drop_bytes > 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        census().deferred_drop_bytes,
        0,
        "the census still reads {} B after the lane finished the drop",
        census().deferred_drop_bytes
    );
}
