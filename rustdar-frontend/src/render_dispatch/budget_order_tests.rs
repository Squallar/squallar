//! The render budget is read **before** the volume is walked, not after.
//!
//! `RenderInput::extract` copies the gates of every sweep the render reads — a
//! whole-volume product's payload is every velocity tilt in the volume — and it
//! used to run unconditionally: a pane that then found `MAX_CONCURRENT_RENDERS`
//! full had already spent that walk on the frame thread and threw the payload
//! away.
//!
//! Nothing about that refusal is recorded — no slot taken, no in-flight mark —
//! so the pane asked again on the very next frame, and paid again, for as long
//! as the budget stayed full. Measured over four archived volumes at 0.12–1.01
//! ms per starved frame for one pane and 0.44–2.41 ms for a four-pane split,
//! against 0.00 ms with the gate in place. It matters most on wasm, where the
//! budget is 1 and so any second render at all is a starved frame for as long
//! as the first one runs.

/// Read off the source, because there is nothing else to read it off.
///
/// Both orderings dispatch exactly the same jobs and leave exactly the same
/// state; the only difference is how much work the frame thread did on its way
/// to doing nothing, and no test can observe a payload that was built and
/// dropped. `app::tests::the_frame_re_arm_holds_only_work_that_finishes` reads
/// source for the same reason.
///
/// The two markers are the load-bearing calls themselves rather than line
/// numbers, so moving either within the function keeps the test true and
/// deleting either fails it by name.
///
/// # It asserts source position, not semantics
///
/// Read as a guard and not as a proof: the whole cost this was written for can
/// come back underneath it. A body shaped
///
/// ```text
/// let free = self.render_slot_free();
/// … RenderInput::extract(…) …
/// if !free { return; }
/// ```
///
/// satisfies `gate < walk` while paying for every discarded extraction exactly
/// as before, and this test would pass. It cannot be strengthened here, which
/// is why it is kept anyway: with the budget full the two orderings differ in
/// no counter, no flag, no key and no posted job, so ordering is the only fact
/// about them a test can hold on to.
#[test]
fn the_budget_is_read_before_the_volume_is_walked() {
    let src = include_str!("../render_dispatch.rs");
    let (_, body) = src
        .split_once("pub fn spawn_level2_render(")
        .expect("spawn_level2_render is no longer a method here");
    let body = body
        .split_once("\n    }")
        .map(|(body, _)| body)
        .expect("spawn_level2_render has no recognisable body");

    let gate = body
        .find("render_slot_free()")
        .expect("spawn_level2_render no longer asks whether a slot is free");
    let walk = body
        .find("RenderInput::extract(")
        .expect("spawn_level2_render no longer extracts a payload");
    assert!(
        gate < walk,
        "the volume is walked before the budget is read, so a refused dispatch \
         pays the whole extraction and discards it — every frame, for as long \
         as the budget is full",
    );
}
