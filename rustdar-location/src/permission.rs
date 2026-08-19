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

/// [`LocationPermission`] as one byte, for a platform bridge's atomic.
///
/// A provider thread that learns the answer off the frame path needs to hand
/// it to a `&self` getter *on* the frame path, and an `AtomicU8` is the
/// lock-free cell that does it — see the representation note on
/// [`LocationPermission`] for why a lock there is not acceptable.
///
/// Hand-written rather than derived, and the discriminants are pinned by the
/// round-trip tests below: the enum is not `repr(u8)` and nothing here
/// promises its variants keep their order, so an `as u8` cast at a bridge
/// would be a silent miscommunication the first time someone inserts a
/// variant.
pub fn encode_permission(permission: LocationPermission) -> u8 {
    use LocationPermission as P;
    match permission {
        P::Unknown => 0,
        P::Prompt => 1,
        P::Granted => 2,
        P::Denied => 3,
        P::Unavailable => 4,
    }
}

/// The inverse of [`encode_permission`], with anything unrecognised read as
/// `Unknown` — the one state that neither asks nor concludes.
pub fn decode_permission(raw: u8) -> LocationPermission {
    use LocationPermission as P;
    match raw {
        1 => P::Prompt,
        2 => P::Granted,
        3 => P::Denied,
        4 => P::Unavailable,
        _ => P::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LocationPermission as P;

    const ALL: &[P] = &[P::Unknown, P::Prompt, P::Granted, P::Denied, P::Unavailable];

    /// The provider thread writes this byte and the frame path reads it, so a
    /// mapping that is not a bijection is a permission silently turning into a
    /// different one — most damagingly `Denied` arriving as `Granted`.
    #[test]
    fn every_permission_survives_the_trip_through_the_atomic() {
        for &permission in ALL {
            assert_eq!(decode_permission(encode_permission(permission)), permission);
        }
    }

    /// Distinct codes, checked separately from the round trip: a collision
    /// where two variants share a byte would still round-trip for one of them
    /// and quietly rewrite the other.
    #[test]
    fn no_two_permissions_share_a_code() {
        let mut codes: Vec<u8> = ALL.iter().map(|&p| encode_permission(p)).collect();
        codes.sort_unstable();
        let count = codes.len();
        codes.dedup();
        assert_eq!(
            codes.len(),
            count,
            "two permissions encode to the same byte"
        );
    }

    /// The atomic starts at zero, and a `AtomicU8::new(0)` that meant anything
    /// else would have the bridge claiming an answer before one exists.
    /// `Unknown` is the state that neither asks nor concludes, which is the
    /// only safe thing for a value nobody has written yet to mean.
    #[test]
    fn an_unwritten_atomic_reads_as_unknown() {
        assert_eq!(decode_permission(0), P::Unknown);
        assert_eq!(encode_permission(P::Unknown), 0);
    }

    /// Nothing writes a byte outside the mapping today, but the decode is on
    /// the frame path and a garbage value must not become a *grant*.
    #[test]
    fn an_unrecognised_code_reads_as_unknown_rather_than_as_a_grant() {
        assert_eq!(decode_permission(200), P::Unknown);
        assert_eq!(decode_permission(u8::MAX), P::Unknown);
    }
}
