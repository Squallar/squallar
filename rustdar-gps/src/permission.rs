//! Whether this app may ask the operating system where it is.
//!
//! Deliberately *not* about GPS hardware. A serial NMEA dongle is a device the
//! user plugged in and pointed us at; a platform location service is a
//! privilege the OS grants and can withdraw. Every platform rustdar runs on has
//! the second concept — Windows `AppCapability`, macOS/iOS `CLAuthorizationStatus`,
//! Android runtime permissions, the browser's Permissions API, the freedesktop
//! location portal
//! — and until this existed the app modelled none of them, so on three of five
//! platforms "denied" was indistinguishable from "no signal" and the UI could
//! never explain why the map had no blue dot.

/// What the OS currently says about this app's access to the user's location.
///
/// # Why five variants and not a `bool`
///
/// Two of the distinctions are load-bearing, and both were bugs before this
/// type existed.
///
/// **`Unknown` is not `Prompt`.** Every platform's query is briefly unavailable
/// at startup: the browser's `navigator.permissions.query` resolves a Promise,
/// Android's `checkSelfPermission` needs an `Activity` that `android_main` has
/// not stashed yet, and Windows' `AppCapability::CheckAccess` can fail
/// transiently on an RPC hiccup. A bridge that answered `Prompt` in that window
/// would be saying "nobody has been asked", and the app would go ahead and ask
/// — which is the web build's audited defect: a permission prompt on first
/// paint, before the user has seen the app do anything and without a gesture.
/// **`Unknown` does nothing**: the gate neither asks nor gives up, it just
/// looks again shortly. It is the [`Default`] so a bridge that has not been
/// wired yet is inert rather than eager.
///
/// **`Denied` is not `Unavailable`.** `Denied` is a decision the user made and
/// can unmake, so the honest advice is "turn it back on in system settings".
/// `Unavailable` is a platform with no location service to grant at all — a
/// headless container, a build with the provider compiled out — where that
/// advice sends the user hunting for a switch that does not exist. Collapsing
/// them costs nothing at the call site and costs the user the only sentence
/// that would have helped.
///
/// # Why coarse-vs-fine and while-in-use-vs-always are *not* here
///
/// Both collapse into [`Granted`](Self::Granted). Nothing downstream can use
/// the distinction — the app picks a radar site ~200 km away from the nearest
/// one, so a city-block fix and a city-wide fix choose the same site — and
/// rustdar has no background mode, so "always" would be a permission it asks
/// for and never exercises.
///
/// # The representation is chosen for Windows
///
/// This is `Copy` and carried by value out of a `&self` getter on the frame
/// path. Windows' `AppCapability::AccessChanged` callback runs on an RPC
/// thread, `TypedEventHandler` is itself `!Send`/`!Sync`, a `Cell` is not
/// `Send`, and an `mpsc::Receiver` cannot be drained from `&self` — so that
/// bridge will store its status in an `Arc<AtomicI32>` and decode it here. A
/// query that had to return a borrow, or a guard, would force a lock onto the
/// frame path; this one does not, and that is not an accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocationPermission {
    /// The platform has not answered yet. Ask nothing, conclude nothing, look
    /// again shortly.
    #[default]
    Unknown,
    /// Nobody has been asked. This is the one state in which prompting is
    /// legitimate.
    Prompt,
    /// The app may read the user's location.
    Granted,
    /// The user said no. Reversible, but only by them, in system settings.
    Denied,
    /// This platform has no location service to grant.
    Unavailable,
}

// No `label()` and no `Display`, deliberately. Each state needs a *sentence* in
// the settings pane, not a word — a denial is only useful next to where to go
// and undo it — so the copy lives at the one place that renders it. A generic
// label here would be the thing everyone reaches for and nobody is served by,
// and it would leak UI wording into log lines, where `Debug` is what is wanted.
