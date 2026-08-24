//! Whether this app may ask the operating system where it is.
//!
//! Deliberately *not* about GPS hardware: a serial NMEA dongle is a device the
//! user plugged in, a platform location service is a privilege the OS grants and
//! can withdraw.

/// What the OS currently says about this app's access to the user's location.
///
/// **`Unknown` is not `Prompt`.** Every platform's query is briefly unavailable
/// at startup, and a bridge that answered `Prompt` there would say "nobody has
/// been asked" — a permission prompt on first paint, without a gesture.
/// `Unknown` does nothing, and is the [`Default`].
///
/// **`Denied` is not `Unavailable`.** `Denied` is a decision the user can
/// unmake, so the advice is "turn it back on in system settings";
/// `Unavailable` is a platform with no location service at all.
///
/// Coarse-vs-fine and while-in-use-vs-always both collapse into
/// [`Granted`](Self::Granted): the app picks a radar site ~200 km away, and
/// squallar has no background mode.
///
/// `Copy` and carried by value out of a `&self` getter on the frame path, since
/// a `Cell` is not `Send` and an `mpsc::Receiver` cannot be drained from
/// `&self`. Returning a borrow or a guard would force a lock onto that path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocationPermission {
    /// The platform has not answered yet. Ask nothing, conclude nothing.
    #[default]
    Unknown,
    /// Nobody has been asked. The one state in which prompting is legitimate.
    Prompt,
    Granted,
    /// The user said no. Reversible, but only by them, in system settings.
    Denied,
    Unavailable,
}

// No `label()` and no `Display`: each state needs a *sentence* in the settings
// pane, not a word, so the copy lives where it is rendered.

/// [`LocationPermission`] as one byte, for a platform bridge's atomic.
///
/// Hand-written and pinned by the round-trip tests below: the enum is not
/// `repr(u8)`, so an `as u8` cast at a bridge would silently miscommunicate the
/// first time someone inserts a variant.
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

    /// A mapping that is not a bijection is a permission silently turning into
    /// another — most damagingly `Denied` arriving as `Granted`.
    #[test]
    fn every_permission_survives_the_trip_through_the_atomic() {
        for &permission in ALL {
            assert_eq!(decode_permission(encode_permission(permission)), permission);
        }
    }

    /// A collision where two variants share a byte would still round-trip for
    /// one of them.
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

    /// The atomic starts at zero, and `Unknown` is the only safe thing for a
    /// value nobody has written yet to mean.
    #[test]
    fn an_unwritten_atomic_reads_as_unknown() {
        assert_eq!(decode_permission(0), P::Unknown);
        assert_eq!(encode_permission(P::Unknown), 0);
    }

    /// The decode is on the frame path, and a garbage value must not become a
    /// *grant*.
    #[test]
    fn an_unrecognised_code_reads_as_unknown_rather_than_as_a_grant() {
        assert_eq!(decode_permission(200), P::Unknown);
        assert_eq!(decode_permission(u8::MAX), P::Unknown);
    }
}
