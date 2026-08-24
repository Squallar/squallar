//! **What a layer is doing right now, as the shell sees it — and nothing
//! about what that means.**
//!
//! The presentation asks a layer for its own status text, its own freshness
//! chip, its own stamps. Those answers are the layer's vocabulary: a chunk
//! feed's tilt freshness means nothing to a lightning layer, and a generic
//! seam that named either would grow one field per layer forever.
//!
//! So the seam carries a [`LayerId`] and an opaque payload, on exactly the
//! terms `SourceEvent::Frames`'s scope and `FrameReady`'s data already use:
//! the layer's own frontend interprets it, and the path between them does
//! not. Nothing in this crate, `squallar-overlays` or `squallar-egui`'s generic
//! half ever looks inside.

use std::any::Any;
use std::sync::Arc;

use crate::id::LayerId;

/// One layer's live status, filed under the layer it is about.
#[derive(Clone)]
pub struct SourceLiveness {
    /// Whose status this is — the only thing about it the generic path reads.
    pub id: LayerId,
    /// The layer's own answer, in the layer's own type. `Arc` because the
    /// shell re-states this every frame and the payload is built when it
    /// **changes**, not when it is read.
    pub payload: Arc<dyn Any + Send + Sync>,
}

impl SourceLiveness {
    /// File `payload` under `id`.
    pub fn new(id: LayerId, payload: impl Any + Send + Sync) -> Self {
        Self {
            id,
            payload: Arc::new(payload),
        }
    }

    /// `id`'s payload as `T`, or `None` when this layer published nothing of
    /// that shape. Both halves are checked: the id, so one layer's answer is
    /// never read as another's, and the type, so a payload whose shape moved
    /// answers `None` instead of anything at all.
    pub fn find<'a, T: Any>(entries: &'a [Self], id: &LayerId) -> Option<&'a T> {
        entries
            .iter()
            .find(|entry| entry.id == *id)?
            .payload
            .downcast_ref::<T>()
    }
}

impl std::fmt::Debug for SourceLiveness {
    /// The id and nothing else: the payload is opaque by construction, and a
    /// `Debug` that pretended otherwise would be the first place a generic
    /// caller learned what a layer publishes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceLiveness")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::LayerId;

    #[derive(Debug, PartialEq)]
    struct Alpha(u32);
    #[derive(Debug, PartialEq)]
    struct Beta(u32);

    fn id(name: &'static str) -> LayerId {
        LayerId::from_static(name)
    }

    /// **Both halves of the lookup are load-bearing.** A payload is found only
    /// under its own id and only as its own type: the id so one layer's answer
    /// is never served as another's, and the type so a layer whose payload
    /// shape moved answers "nothing" rather than something.
    #[test]
    fn a_payload_is_found_only_under_its_own_id_and_only_as_its_own_type() {
        let entries = vec![
            SourceLiveness::new(id("Alpha"), Alpha(7)),
            SourceLiveness::new(id("Beta"), Beta(9)),
        ];
        assert_eq!(
            SourceLiveness::find::<Alpha>(&entries, &id("Alpha")),
            Some(&Alpha(7)),
            "premise: the entry that IS there is found, or the refusals below \
             are refusals about an empty list",
        );
        assert_eq!(
            SourceLiveness::find::<Alpha>(&entries, &id("Beta")),
            None,
            "Beta's entry was read as Alpha's — the id is not being checked",
        );
        assert_eq!(
            SourceLiveness::find::<Beta>(&entries, &id("Alpha")),
            None,
            "Alpha's payload answered a Beta question — the type is not being \
             checked, so a payload whose shape moved would be read as the old one",
        );
        assert_eq!(
            SourceLiveness::find::<Alpha>(&entries, &id("Gamma")),
            None,
            "a layer that published nothing answered anyway",
        );
    }

    /// The debug form carries the id and never the payload — the seam's whole
    /// point is that the path between the layer and its frontend does not know
    /// what is inside, and a `Debug` that printed it would be the first place
    /// a generic caller learned.
    #[test]
    fn the_debug_form_names_the_layer_and_not_its_answer() {
        let text = format!("{:?}", SourceLiveness::new(id("Alpha"), Alpha(7)));
        assert!(text.contains("Alpha"), "the id must be in it: {text}");
        assert!(
            !text.contains('7'),
            "the payload leaked into the generic path's own printout: {text}",
        );
    }
}
