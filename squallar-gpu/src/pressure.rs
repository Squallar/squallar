//! The out-of-memory count the uncaptured-error sink raises and the frame
//! loop drains.
//!
//! wgpu reports an allocation failure through `Device::on_uncaptured_error`,
//! on whichever thread the failing call ran, with nothing to hand back but the
//! error itself. The one handler a device may have is installed by
//! `squallar_volumetric`; the application that answers pressure runs on the
//! frame thread, a frame later. This counter is the wire between them: the
//! sink notes, the frame takes, and however many errors one frame produced
//! they are one pressure event.

use core::sync::atomic::{AtomicU32, Ordering};

/// Out-of-memory errors noted since the last take.
static OUT_OF_MEMORY: AtomicU32 = AtomicU32::new(0);

/// Record one `wgpu::Error::OutOfMemory`, whatever resource it was for.
pub fn note_out_of_memory() {
    // Saturating rather than wrapping: a count that came back round to zero
    // would read as no pressure at all.
    let _ = OUT_OF_MEMORY.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(1))
    });
}

/// How many were noted since the last call, resetting the count to zero.
pub fn take_out_of_memory() -> u32 {
    OUT_OF_MEMORY.swap(0, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every note is counted, one take drains them all, and the next take
    /// reads zero. The counter is process-global, so this is the only test in
    /// this binary that touches it.
    #[test]
    fn notes_accumulate_until_taken_and_a_take_resets_to_zero() {
        assert_eq!(take_out_of_memory(), 0, "the counter starts drained");
        note_out_of_memory();
        note_out_of_memory();
        note_out_of_memory();
        assert_eq!(take_out_of_memory(), 3);
        assert_eq!(take_out_of_memory(), 0, "a take did not reset the count");
        note_out_of_memory();
        assert_eq!(take_out_of_memory(), 1);
    }
}
