//! The one place this crate's loop suites spell a render identity.
//!
//! # Why a helper for a two-line constructor
//!
//! A loop frame's picture is identified by a [`RenderTarget`] — today
//! `(site, product, elevation)`. That shape is about to change: WO-E5 replaces
//! `RenderTarget`/`RenderCacheKey`/`RenderParams` with a single `RenderKey`
//! built from a `SelectKey`, quantizing the elevation once at construction
//! instead of comparing it within a tolerance at every read.
//!
//! The loop suites are the pin that protects that flip, and the port after it
//! (the loop-state move). They must go into E5 asserting exactly what they
//! assert today, so the flip is proven not to have moved any behaviour. If each
//! suite spelled the constructor itself, the flip would have to reach into
//! every one of them, and the diff that is supposed to prove nothing changed
//! would be a diff across the very files doing the proving.
//!
//! So: **every loop suite constructs render identity through this function so
//! the E5 key flip re-pins in one place.** E5 changes this body; the signature
//! and every call site stay as they are.
//!
//! # What belongs here and what does not
//!
//! Only the loop suites route through this. Files that name the type without
//! being loop pins — `melting_layer_dispatch_tests`,
//! `declared_nyquist_dispatch_tests` — stay as they are and are re-spelled by
//! E5 as ordinary call sites; pulling them in would widen the blast radius this
//! module exists to narrow.
//!
//! A suite that already has its own local `target()` helper keeps it and
//! delegates here, rather than being flattened into direct calls: those helpers
//! are named inside `assert!` bodies, and rewriting an assertion line to route
//! a constructor would edit the pin instead of the plumbing.

use rustdar_egui::pane::RenderTarget;
use rustdar_radar::types::RadarProduct;

/// The render identity of a loop frame for `site`, `product` and `elevation`.
///
/// `elevation` is the pane's **selected** elevation, not the snapped sweep
/// angle — the same thing [`RenderTarget`] has always carried. See the module
/// note for why the loop suites go through here.
pub(crate) fn key(site: impl Into<String>, product: RadarProduct, elevation: f32) -> RenderTarget {
    RenderTarget::new(site, product, elevation)
}
