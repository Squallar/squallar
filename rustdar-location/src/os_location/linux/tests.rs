use super::*;
use crate::FixQuality;
use std::sync::{Arc, Mutex, PoisonError};

// Every test here runs with no session bus, no portal and no geoclue.

/// Speed and heading share one sentinel in the payload geoclue hands the portal.
/// Test-only, because `ashpd` strips it.
const SPEED_HEADING_UNKNOWN: f64 = -1.0;

/// The application id: the basename of the shipped `.desktop` file, the iOS
/// bundle id and the Android `applicationId`, which are one string.
const APP_ID: &str = "dev.mcswain.rustdar";

/// The sentinel is spelled `f64::MIN` here, `-f64::MAX` inside `ashpd` and
/// `-1.7976931348623157e+308` in the interface XML.
#[test]
fn the_altitude_sentinel_is_the_number_the_interface_documents() {
    assert_eq!(ALTITUDE_UNKNOWN, -1.797_693_134_862_315_7e308);
    assert_eq!(ALTITUDE_UNKNOWN, -f64::MAX);
}

/// Reaching [`decode_altitude`] with it means `ashpd` changed its mind — and the
/// answer must still be "absent".
#[test]
fn an_unknown_altitude_is_absent_rather_than_enormous() {
    assert_eq!(decode_altitude(ALTITUDE_UNKNOWN), None);
}

/// Altitude is the one field whose sentinel is *not* a sign test.
#[test]
fn an_altitude_below_sea_level_is_a_reading_and_not_a_sentinel() {
    assert_eq!(decode_altitude(-430.0), Some(-430.0));
}

#[test]
fn an_ordinary_altitude_survives() {
    assert_eq!(decode_altitude(357.0), Some(357.0));
}

/// A sign flip on either side would turn "1 m/s" into "unknown" on every fix.
#[test]
fn the_speed_and_heading_sentinel_is_the_number_the_interface_documents() {
    assert_eq!(SPEED_HEADING_UNKNOWN, -1.0);
    assert_eq!(decode_speed(SPEED_HEADING_UNKNOWN), None);
    assert_eq!(decode_heading(SPEED_HEADING_UNKNOWN), None);
}

#[test]
fn a_reversing_speed_is_not_a_speed() {
    assert_eq!(decode_speed(-0.5), None);
}

/// `HeadingSource::Auto` reads exactly this field to decide whether to trust a
/// bearing.
#[test]
fn a_stationary_reading_is_no_speed_rather_than_a_speed_of_zero() {
    assert_eq!(decode_speed(0.0), None);
}

#[test]
fn a_real_speed_survives() {
    assert_eq!(decode_speed(3.5), Some(3.5));
}

/// Due north is 0 degrees, so a truthiness test would delete the one bearing
/// users notice.
#[test]
fn a_heading_of_due_north_is_kept() {
    assert_eq!(decode_heading(0.0), Some(0.0));
}

#[test]
fn a_heading_outside_the_compass_is_rejected() {
    assert_eq!(decode_heading(360.5), None);
}

#[test]
fn a_negative_accuracy_radius_is_not_a_radius() {
    assert_eq!(decode_accuracy(-1.0), None);
}

/// The measured value on the development machine, which must pass.
#[test]
fn the_twenty_five_kilometre_fix_this_machine_reports_is_kept() {
    assert_eq!(decode_accuracy(25_000.0), Some(25_000.0));
}

#[test]
fn an_infinite_accuracy_radius_is_rejected() {
    assert_eq!(decode_accuracy(f64::INFINITY), None);
}

#[test]
fn the_epoch_converts_to_the_epoch() {
    let t = timestamp_from_epoch(0).expect("the epoch is representable");
    assert_eq!(t.and_utc().timestamp(), 0);
}

#[test]
fn a_real_portal_timestamp_survives_the_conversion() {
    let t = timestamp_from_epoch(1_786_203_902).expect("representable");
    assert_eq!(t.and_utc().timestamp(), 1_786_203_902);
    assert_eq!(t.and_utc().timestamp_subsec_nanos(), 0);
}

#[test]
fn a_timestamp_beyond_the_representable_range_is_absent_rather_than_wrapped() {
    assert_eq!(timestamp_from_epoch(u64::MAX), None);
}

/// The reading this machine actually answers with, measured through the portal:
/// a real position, a 25 km circle, and a sentinel everywhere else.
fn measured() -> Reading {
    Reading {
        latitude: 35.4689,
        longitude: -97.5195,
        accuracy_m: 25_000.0,
        altitude_m: None,
        speed_mps: None,
        heading_deg: None,
        timestamp_s: 1_786_203_902,
    }
}

#[test]
fn the_reading_this_machine_sends_becomes_a_device_fix_with_its_accuracy() {
    let fix = fix_from_reading(&measured()).expect("a position");

    assert_eq!(fix.point.lat, 35.4689);
    assert_eq!(fix.point.lon, -97.5195);
    assert_eq!(fix.accuracy_m, Some(25_000.0));
    assert_eq!(fix.fix_quality, FixQuality::Device);
    assert_eq!(fix.altitude_m, None);
    assert_eq!(fix.speed_mps, None);
    assert_eq!(fix.heading_deg, None);
    assert_eq!(
        fix.timestamp.map(|t| t.and_utc().timestamp()),
        Some(1_786_203_902)
    );
}

/// Accuracy says how tight the circle is and nothing about whether a satellite
/// was involved. The variant also has to be one the site upgrade acts on.
#[test]
fn even_a_five_metre_fix_is_reported_as_a_device_fix_and_not_as_gps() {
    let fix = fix_from_reading(&Reading {
        accuracy_m: 5.0,
        ..measured()
    })
    .expect("a position");
    assert_eq!(fix.fix_quality, FixQuality::Device);
    assert!(fix.fix_quality.can_relocate());
}

/// A fix that dropped movement would lose a desktop's only heading source.
#[test]
fn a_moving_fix_keeps_its_speed_and_heading() {
    let fix = fix_from_reading(&Reading {
        altitude_m: Some(357.0),
        speed_mps: Some(13.4),
        heading_deg: Some(271.0),
        accuracy_m: 12.0,
        ..measured()
    })
    .expect("a position");

    assert_eq!(fix.altitude_m, Some(357.0));
    assert_eq!(fix.speed_mps, Some(13.4));
    assert_eq!(fix.heading_deg, Some(271.0));
}

/// The portal's dictionary always has both keys, so the shape that reaches here
/// is a NaN rather than a missing entry — and a NaN on a map is a dot at nowhere.
#[test]
fn a_reading_with_no_real_coordinates_is_not_a_fix() {
    assert!(
        fix_from_reading(&Reading {
            latitude: f64::NAN,
            ..measured()
        })
        .is_none()
    );
    assert!(
        fix_from_reading(&Reading {
            longitude: f64::INFINITY,
            ..measured()
        })
        .is_none()
    );
}

/// A `LocationUpdated` body, built and encoded the way the portal sends one,
/// then decoded by `ashpd`. The only test that touches D-Bus types: it pins the
/// seven-field mapping in [`Reading::from`] and `ashpd`'s sentinel handling.
fn decode_wire(latitude: f64, longitude: f64, altitude: f64, speed: f64, heading: f64) -> Location {
    use ashpd::zvariant::{self, Endian, ObjectPath, OwnedValue, Value};

    let dict: std::collections::HashMap<&str, OwnedValue> = [
        ("Latitude", latitude),
        ("Longitude", longitude),
        ("Accuracy", 25_000.0),
        ("Altitude", altitude),
        ("Speed", speed),
        ("Heading", heading),
    ]
    .into_iter()
    .map(|(k, v)| (k, OwnedValue::from(v)))
    .chain([
        (
            "Description",
            OwnedValue::try_from(Value::from("GeoIP (ichnaea)")).expect("a string"),
        ),
        (
            "Timestamp",
            OwnedValue::try_from(Value::from(zvariant::Structure::from((
                1_786_203_902u64,
                0u64,
            ))))
            .expect("a (tt)"),
        ),
    ])
    .collect();

    let body = (
        ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_1/rustdar")
            .expect("an object path"),
        dict,
    );
    let ctxt = zvariant::serialized::Context::new_dbus(Endian::native(), 0);
    let encoded = zvariant::to_bytes(ctxt, &body).expect("an encodable body");
    encoded
        .deserialize::<Location>()
        .expect("the portal's own payload shape")
        .0
}

#[test]
fn the_payload_the_portal_sends_decodes_into_the_fix_this_machine_reported() {
    let location = decode_wire(
        35.4689,
        -97.5195,
        ALTITUDE_UNKNOWN,
        SPEED_HEADING_UNKNOWN,
        SPEED_HEADING_UNKNOWN,
    );

    assert_eq!(Reading::from(&location), measured());

    let fix = fix_from_location(&location).expect("a position");
    assert_eq!(fix.point.lat, 35.4689);
    assert_eq!(fix.point.lon, -97.5195);
    assert_eq!(fix.accuracy_m, Some(25_000.0));
    assert_eq!(fix.fix_quality, FixQuality::Device);
}

/// Reading them the wrong way round produces a position that is well-formed and
/// on the other side of the planet.
#[test]
fn latitude_and_longitude_are_not_read_the_wrong_way_round() {
    let location = decode_wire(1.0, 2.0, ALTITUDE_UNKNOWN, -1.0, -1.0);
    assert_eq!(Reading::from(&location).latitude, 1.0);
    assert_eq!(Reading::from(&location).longitude, 2.0);
}

/// `ashpd` owns the three sentinels now, so this notices if it stops.
#[test]
fn ashpd_still_strips_the_sentinels_this_file_no_longer_checks() {
    let location = decode_wire(
        35.4689,
        -97.5195,
        ALTITUDE_UNKNOWN,
        SPEED_HEADING_UNKNOWN,
        SPEED_HEADING_UNKNOWN,
    );
    assert_eq!(location.altitude(), None);
    assert_eq!(location.speed(), None);
    assert_eq!(location.heading(), None);

    let moving = decode_wire(35.4689, -97.5195, 357.0, 13.4, 271.0);
    assert_eq!(moving.altitude(), Some(357.0));
    assert_eq!(moving.speed(), Some(13.4));
    assert_eq!(moving.heading(), Some(271.0));
}

/// The refusal this machine gives by default, verbatim from the wire:
/// `org.freedesktop.portal.Error.NotAllowed: Location services disabled`.
fn lockdown() -> ashpd::Error {
    ashpd::Error::Portal(ashpd::PortalError::NotAllowed(
        "Location services disabled".to_owned(),
    ))
}

/// A preference somebody set, so `Denied` and not `Unavailable`, which would
/// hide the sentence that fixes the problem.
#[test]
fn the_lockdown_switch_reads_as_a_denial_and_not_as_a_missing_service() {
    assert_eq!(classify(&lockdown()), LocationPermission::Denied);
}

/// The advice has to be actionable on a desktop with no page for this, which is
/// every desktop except GNOME.
#[test]
fn the_lockdown_message_names_the_setting_that_would_turn_it_on() {
    let message = explain(&lockdown());
    assert!(
        message.contains("org.gnome.system.location enabled"),
        "{message}"
    );
    assert!(message.contains("gsettings set"), "{message}");
}

/// Response code 1: the portal's dialog answered "Deny", or a stored refusal.
#[test]
fn a_refused_request_reads_as_a_denial() {
    assert_eq!(
        classify(&ashpd::Error::Response(ResponseError::Cancelled)),
        LocationPermission::Denied
    );
}

/// Response code 2, what this machine returns when the portal's own GeoClue
/// client will not start. `Denied` and `Unavailable` are both terminal, so
/// either would end location over something a retry fixes.
#[test]
fn a_portal_that_could_not_carry_the_request_out_leaves_the_user_able_to_retry() {
    assert_eq!(
        classify(&ashpd::Error::Response(ResponseError::Other)),
        LocationPermission::Prompt
    );
    assert!(explain(&ashpd::Error::Response(ResponseError::Other)).contains("again"));
}

/// A machine with no portals frontend has no switch to turn on.
#[test]
fn a_missing_portal_reads_as_unavailable_and_not_as_a_denial() {
    let missing = ashpd::Error::PortalNotFound(
        zbus::names::OwnedInterfaceName::try_from("org.freedesktop.portal.Location")
            .expect("a well-formed interface name"),
    );
    assert_eq!(classify(&missing), LocationPermission::Unavailable);
    assert!(explain(&missing).contains("xdg-desktop-portal"));
}

fn method_error(name: &str) -> ashpd::Error {
    ashpd::Error::Zbus(zbus::Error::MethodError(
        name.try_into().expect("a well-formed error name"),
        None,
        zbus::message::Message::method_call("/", "Whatever")
            .expect("a path and a member")
            .build(&())
            .expect("an empty body"),
    ))
}

/// `ashpd` reports "nobody owns `org.freedesktop.portal.Desktop`" as an ordinary
/// method error rather than as `PortalNotFound`, so this arm stands between a
/// machine with no portal and a settings pane offering a dead button.
#[test]
fn a_bus_with_no_portal_on_it_reads_as_unavailable() {
    assert_eq!(
        classify(&method_error("org.freedesktop.DBus.Error.ServiceUnknown")),
        LocationPermission::Unavailable
    );
    assert_eq!(
        classify(&method_error("org.freedesktop.DBus.Error.NameHasNoOwner")),
        LocationPermission::Unavailable
    );
}

#[test]
fn a_bus_policy_refusal_reads_as_a_denial() {
    assert_eq!(
        classify(&method_error("org.freedesktop.DBus.Error.AccessDenied")),
        LocationPermission::Denied
    );
}

#[test]
fn an_unrecognised_fault_leaves_the_user_able_to_try_again() {
    assert_eq!(
        classify(&method_error("org.freedesktop.DBus.Error.NoReply")),
        LocationPermission::Prompt
    );
    assert_eq!(
        classify(&ashpd::Error::NoResponse),
        LocationPermission::Prompt
    );
}

/// The push half: the gate stops asking once the answer is `Granted` and
/// delivery is live.
#[test]
fn a_session_the_portal_closed_is_reported_and_stays_retryable() {
    assert_eq!(
        Ending::Closed.report(),
        Some(LocationPermission::Prompt),
        "the portal closes sessions for reasons nobody decided; `Denied` \
             is terminal for the gate and would send the user hunting for a \
             decision to undo"
    );
}

/// The gate answers `Denied` by calling `stop_location`, so a report here would
/// overwrite the state that caused it.
#[test]
fn a_deliberate_stop_reports_nothing() {
    assert_eq!(Ending::Stopped.report(), None);
    assert_eq!(Ending::ConsumerGone.report(), None);
}

/// Bringing the provider up calls nothing, delivers nothing, and reports
/// [`LocationPermission::Prompt`] before it returns.
///
/// `Prompt` and not `Unknown`: this provider does not answer until it is
/// started, so `Unknown` would leave the pane on "Checking…" for the life of the
/// process. Synchronously, because the first frame decides whether the pane
/// offers a button.
#[test]
fn bringing_the_provider_up_says_prompt_and_starts_no_session() {
    let (fixes, _receiver) = std::sync::mpsc::channel();
    let reported = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&reported);

    let reader = OsLocationReader::start(OsLocationSink {
        fixes,
        wake: Arc::new(|| unreachable!("nothing has been delivered to wake for")),
        report: Arc::new(move |permission| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(permission);
        }),
    })
    .expect("this arm always has a provider; it cannot know otherwise");

    assert_eq!(
        *reported.lock().unwrap_or_else(PoisonError::into_inner),
        vec![LocationPermission::Prompt],
        "the initial report is what the gate's first frame reads"
    );
    assert!(
        !reader.active(),
        "bringing the provider up must not open a session"
    );
}

/// Linux offers no settings button, and that stays a deliberate `false`: the
/// only page that exists is GNOME's and the desktop whose default causes the
/// refusal is KDE.
#[test]
fn linux_offers_no_location_settings_page() {
    assert!(!OsLocationReader::settings_available());
}

/// A session whose thread has exited must not leave the provider claiming to be
/// live — `request` would see `active` and refuse to start a second one.
#[test]
fn a_session_whose_thread_has_gone_is_no_longer_active() {
    let (fixes, _receiver) = std::sync::mpsc::channel();
    let (stop, stopped) = oneshot::channel::<()>();
    let reader = OsLocationReader {
        sink: OsLocationSink {
            fixes,
            wake: Arc::new(|| unreachable!("`active` never wakes anything")),
            report: Arc::new(|_| unreachable!("`active` never reports")),
        },
        session: Some(Session { stop }),
    };
    assert!(
        reader.active(),
        "while the thread holds the receiver, the session is live"
    );

    drop(stopped);

    assert!(
        !reader.active(),
        "a thread that returned dropped the receiver, and `active` has to \
             see that"
    );
}

/// The entry the packaging installs, named by the path this file compiles it in
/// from, which pins the **basename** — the application id. `include_str!` is the
/// check.
const DESKTOP_ENTRY: &str = include_str!("../../../../packaging/linux/dev.mcswain.rustdar.desktop");

#[test]
fn the_packaged_entry_is_named_for_the_application_id() {
    assert!(
        DESKTOP_ENTRY.starts_with("[Desktop Entry]"),
        "{DESKTOP_ENTRY}"
    );
    assert_eq!(APP_ID, "dev.mcswain.rustdar");
}

#[test]
fn the_desktop_entry_carries_the_name_and_icon_a_launcher_will_show() {
    assert!(DESKTOP_ENTRY.contains("\nName=Rustdar"), "{DESKTOP_ENTRY}");
    // A bare identifier, not a path: the icon theme spec resolves this against
    // the installed hicolor sizes.
    assert!(
        DESKTOP_ENTRY.contains(&format!("\nIcon={APP_ID}\n")),
        "{DESKTOP_ENTRY}"
    );
}
