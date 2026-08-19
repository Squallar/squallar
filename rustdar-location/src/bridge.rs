use crate::permission::LocationPermission;

/// What the permission gate needs from a platform — exactly the six calls
/// [`LocationGate`](crate::LocationGate) makes, nothing more. Widening this is
/// widening what the gate can do to a user; the gate's one-prompt-line
/// discipline is the reason it is narrow.
///
/// `pub(crate)` since WO-RL-4: the trait collapsed to the facade-internal
/// seam between the gate and the provider arms. Its one production
/// implementor is the facade's `GateSeam` adapter (the provider answers five
/// of the six; the app-passed kv closure answers the last); the test
/// implementor is `GateDouble`. Nothing outside this crate implements it —
/// the old app-side supertrait arrangement (RL-2) died with the
/// `PlatformBridge` location verbs.
pub(crate) trait LocationBridge {
    /// What the OS currently says about this app's access to the user's
    /// location.
    ///
    /// **Required, not defaulted.** Every bridge has an answer — the ones with
    /// no location service answer [`Unavailable`] — and a default would let a
    /// new bridge silently report [`Unknown`] forever, which the gate reads as
    /// "still starting up" and waits on indefinitely.
    ///
    /// **Cheap by contract.** This is polled on the frame path, so an
    /// implementation must not block, allocate or make a round trip. A bridge
    /// whose real query is asynchronous resolves it into a cell or an atomic on
    /// whatever thread the platform hands it to and answers [`Unknown`] until
    /// that lands; the gate is built to wait.
    ///
    /// [`Unavailable`]: LocationPermission::Unavailable
    /// [`Unknown`]: LocationPermission::Unknown
    fn location_permission(&self) -> LocationPermission;

    /// Prompt if the platform needs prompting, and start delivering fixes.
    ///
    /// One method rather than two because on the web they are the same call:
    /// `watchPosition` *is* the prompt, and there is no way to ask without also
    /// subscribing.
    ///
    /// # The `bool` is a hint, not a fact
    ///
    /// It answers "did the ask reach the OS", and **only Android can actually
    /// tell**: `Activity.requestPermissions` either goes through or throws, and
    /// the miss cases (no `Activity` stashed yet, JNI attach failure) are real
    /// and recoverable. Everywhere else it is fabricated to some degree —
    ///
    /// * the browser returns `true` because `watchPosition` reports nothing
    ///   synchronously and a refusal arrives later on the error callback;
    /// * Windows can only say "we spawned an MTA worker", since
    ///   `RequestAccessAsync` completes on another thread;
    /// * `requestWhenInUseAuthorization()` on Apple platforms returns `void`
    ///   and has a documented *silent* failure mode when the Info.plist usage
    ///   key is missing.
    ///
    /// **So nothing durable may hang off it.** It is worth exactly one thing:
    /// not re-firing an ask this session that the OS has already accepted.
    /// Anything that has to survive a restart — how many times the user has
    /// been asked, whether they turned it off — comes from the persisted memo
    /// in [`LocationGate`](crate::LocationGate) instead. Persisting a decision
    /// off a value three of five bridges have to invent is the bug this note
    /// exists to prevent; it has been written once already.
    fn request_location(&mut self) -> bool;

    /// Stop delivering fixes.
    ///
    /// Cannot revoke the permission — no platform offers an app a way to give
    /// one back — so this is an off switch for the *stream*, and the settings
    /// pane says so.
    fn stop_location(&mut self);

    /// Whether the platform is currently delivering location fixes.
    ///
    /// Mirrors the serial reader's `gps_active`, and exists for the same
    /// reason the UI reads that rather than a local flag: "granted" and
    /// "delivering" are different states, a bridge can stop delivering without
    /// anything app-side asking it to (iOS backgrounds, a portal session
    /// closes), and a gate-local bool would go on claiming otherwise.
    ///
    /// Defaulted `false`: a bridge with no location service is never
    /// delivering, and neither is one whose provider has not been written yet.
    fn location_active(&self) -> bool {
        false
    }

    /// Tell the bridge how many times this install has already asked.
    ///
    /// Android only, and it exists because that platform's own permission state
    /// is a *tri*-state its API does not name: never asked
    /// (`shouldShowRequestPermissionRationale` false), denied once (rationale
    /// true — **and the dialog will still show**), and permanently denied
    /// (rationale false again). The bridge cannot tell the first from the third
    /// without knowing whether an ask has ever happened, and the only thing
    /// that knows is the persisted memo on the gate's side.
    ///
    /// Replaces a planned `set_location_previously_asked(bool)`. A bool maps
    /// "denied once" onto `Denied`, which the settings pane renders with no
    /// button — a regression against what Android does today, where that user
    /// can still be asked and can still say yes.
    fn set_location_attempts(&mut self, _attempts: u8) {}

    /// Where this platform persists small blobs — the gate's memo among them —
    /// or `None` if the platform has not been told where yet (Android learns
    /// its data path only after startup).
    ///
    /// Returns a store rather than a directory so the trait carries no
    /// filesystem assumption: a web bridge hands back a `localStorage` backend,
    /// which has no path to return.
    fn kv(&self) -> Option<Box<dyn rustdar_kv::KvStore>>;
}
