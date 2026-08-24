use crate::permission::LocationPermission;

/// What the permission gate needs from a platform — exactly the six calls
/// [`LocationGate`](crate::LocationGate) makes, nothing more. Widening this is
/// widening what the gate can do to a user. Its one production implementor is
/// the facade's `GateSeam`; the test implementor is `GateDouble`.
pub(crate) trait LocationBridge {
    /// What the OS currently says about this app's access to the user's
    /// location.
    ///
    /// **Required, not defaulted:** a default would let a new bridge silently
    /// report `Unknown` forever, which the gate waits on.
    ///
    /// **Cheap by contract:** polled on the frame path, so no blocking, no
    /// allocation, no round trip. A bridge whose real query is asynchronous
    /// resolves it into a cell or atomic and answers `Unknown` until it lands.
    fn location_permission(&self) -> LocationPermission;

    /// Prompt if the platform needs prompting, and start delivering fixes. One
    /// method rather than two because on the web they are the same call.
    ///
    /// The `bool` answers "did the ask reach the OS", and **only Android can
    /// actually tell**. Elsewhere it is fabricated: the browser returns `true`
    /// because `watchPosition` reports nothing synchronously; Windows can only
    /// say it spawned an MTA worker; `requestWhenInUseAuthorization()` returns
    /// `void` and fails silently. **So nothing durable may hang off it** — it is
    /// worth only not re-firing an ask the OS has already accepted this session.
    fn request_location(&mut self) -> bool;

    /// Stop delivering fixes. Cannot revoke the permission — an off switch for
    /// the *stream*.
    fn stop_location(&mut self);

    /// Whether the platform is currently delivering location fixes. "Granted"
    /// and "delivering" are different states, and a bridge can stop delivering
    /// without anything app-side asking it to.
    fn location_active(&self) -> bool {
        false
    }

    /// Tell the bridge how many times this install has already asked.
    ///
    /// Android only: that platform's permission state is a *tri*-state its API
    /// does not name — never asked (rationale false), denied once (rationale
    /// true, **and the dialog will still show**), permanently denied (rationale
    /// false again). The bridge cannot tell the first from the third without
    /// knowing whether an ask has ever happened.
    fn set_location_attempts(&mut self, _attempts: u8) {}

    /// Where this platform persists small blobs, or `None` if it has not been
    /// told where yet (Android learns its data path only after startup). A store
    /// rather than a directory, so the trait carries no filesystem assumption.
    fn kv(&self) -> Option<Box<dyn squallar_kv::KvStore>>;
}
