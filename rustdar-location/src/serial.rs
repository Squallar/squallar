//! The serial GPS provider, adapted to the fix model: NMEA parsing and the
//! port transport live in `rustdar-nmea-serial`, which deliberately does not
//! know this crate's [`Fix`] — this module is where its parsed sentences
//! become fixes (WO-RL-3 flipped the RL-1 edge; the facade stands on the
//! provider, never the reverse).

use crate::{Fix, FixQuality};
use rustdar_nmea_serial::{ParsedFix, ParsedQuality, SerialConfig, SerialGpsReader};
use std::sync::mpsc;

/// What the parser said, as the app's fix model.
///
/// The position rides through [`Fix::from_lat_lon`]; the quality named in the
/// literal overrides the `Gps` that constructor stamps. `accuracy_m` is `None`
/// because NMEA has no accuracy field — GGA and GSA give HDOP, which is a
/// geometry factor and not metres, and turning one into the other needs the
/// receiver's UERE, which it does not report. `None` is the honest answer and
/// every reader treats it as passing.
fn fix_from(parsed: ParsedFix) -> Fix {
    Fix {
        altitude_m: parsed.altitude_m,
        speed_mps: parsed.speed_mps,
        heading_deg: parsed.heading_deg,
        satellites: parsed.satellites,
        fix_quality: quality_from(parsed.quality),
        hdop: parsed.hdop,
        accuracy_m: None,
        timestamp: parsed.timestamp,
        ..Fix::from_lat_lon(parsed.lat, parsed.lon)
    }
}

/// The nine GGA quality codes, one to one. Never [`FixQuality::Device`] — that
/// variant exists for platform location services precisely because no NMEA
/// receiver can produce it.
fn quality_from(quality: ParsedQuality) -> FixQuality {
    match quality {
        ParsedQuality::None => FixQuality::None,
        ParsedQuality::Gps => FixQuality::Gps,
        ParsedQuality::Dgps => FixQuality::Dgps,
        ParsedQuality::Pps => FixQuality::Pps,
        ParsedQuality::Rtk => FixQuality::Rtk,
        ParsedQuality::FloatRtk => FixQuality::FloatRtk,
        ParsedQuality::Estimated => FixQuality::Estimated,
        ParsedQuality::Manual => FixQuality::Manual,
        ParsedQuality::Simulation => FixQuality::Simulation,
    }
}

/// Hand one translated fix to the consumer, and say whether the reader may
/// keep going.
///
/// Split out of the reader callback so the pairing can be tested without a
/// serial port: the send and the wake are one step, and a wake that gets
/// separated from its send is a fix that sits in the channel until something
/// else draws a frame — the exact failure the `wake` parameter exists for.
/// `false` means the consumer is gone and the reader thread should stop.
fn deliver(fix: Fix, fix_sender: &mpsc::Sender<Fix>, wake: &impl Fn()) -> bool {
    if fix_sender.send(fix).is_err() {
        return false;
    }
    wake();
    true
}

/// A running serial GPS source delivering [`Fix`]es. Dropping it stops the
/// reader thread (which holds the port open exclusively).
pub struct SerialFixReader {
    _reader: SerialGpsReader,
}

impl SerialFixReader {
    /// Start the serial transport and deliver every parsed sentence as a
    /// [`Fix`] on `fix_sender`. `None` when no reader could be started.
    ///
    /// # `wake`
    ///
    /// Called after every fix that reaches `fix_sender`, and it is what makes
    /// the fix *visible*. The frontend runs its event loop on
    /// `ControlFlow::Wait` and drains this channel only while rendering a
    /// frame, so a fix pushed from the reader thread while the app is idle
    /// waits for some unrelated event to produce one — with auto-refresh off,
    /// that can be the next mouse move, or never.
    ///
    /// A bare `impl Fn()` rather than the frontend's `RedrawWaker`: this crate
    /// is a *dependency* of the frontend and cannot name its types. The
    /// desktop bridge passes one through; the shape matches
    /// `ChunkNotifier::sync_sites`, which takes a `wake` for the same reason.
    pub fn start(
        config: &SerialConfig,
        fix_sender: mpsc::Sender<Fix>,
        wake: impl Fn() + Send + 'static,
    ) -> Option<Self> {
        let reader = SerialGpsReader::start(config, move |parsed| {
            deliver(fix_from(parsed), &fix_sender, &wake)
        })?;
        Some(Self { _reader: reader })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counting wake, and the count.
    fn counted() -> (
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        impl Fn() + Send,
    ) {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe = std::sync::Arc::clone(&count);
        (count, move || {
            probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }

    fn woke(count: &std::sync::atomic::AtomicUsize) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The bug this parameter exists for. The consumer drains this channel only
    /// while drawing a frame, and its loop runs on `ControlFlow::Wait`: a fix
    /// that lands with nothing else happening sits there until some unrelated
    /// event draws one.
    #[test]
    fn a_fix_arriving_while_the_app_is_idle_asks_for_the_frame_that_shows_it() {
        let (tx, rx) = mpsc::channel();
        let (woken, wake) = counted();

        assert!(deliver(Fix::from_lat_lon(35.25, -97.5), &tx, &wake));

        assert_eq!(rx.try_recv().map(|f| f.point.lat), Ok(35.25));
        assert_eq!(
            woke(&woken),
            1,
            "the fix reached the channel and nothing asked for the frame that \
             would read it"
        );
    }

    /// The reader stops when the app is gone, and must not wake something that
    /// no longer exists on the way out.
    #[test]
    fn a_fix_with_no_consumer_left_stops_the_reader_without_waking() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let (woken, wake) = counted();

        assert!(
            !deliver(Fix::from_lat_lon(35.25, -97.5), &tx, &wake),
            "a closed channel must stop the reader"
        );
        assert_eq!(woke(&woken), 0, "woke the loop for a fix nothing received");
    }

    /// Every field the parser reports survives the translation, and the two
    /// this module *adds* say what they must: `accuracy_m` is `None` (NMEA has
    /// no accuracy field) and the position rides `from_lat_lon`.
    #[test]
    fn the_translation_carries_every_parsed_field() {
        let parsed = ParsedFix {
            lat: 35.25,
            lon: -97.5,
            altitude_m: Some(360.0),
            speed_mps: Some(3.2),
            heading_deg: Some(271.5),
            satellites: Some(9),
            quality: ParsedQuality::Dgps,
            hdop: Some(0.9),
            timestamp: chrono::DateTime::from_timestamp(1_700_000_000, 0).map(|t| t.naive_utc()),
        };
        let expected_timestamp = parsed.timestamp;

        let fix = fix_from(parsed);

        assert_eq!(fix.point.lat, 35.25);
        assert_eq!(fix.point.lon, -97.5);
        assert_eq!(fix.altitude_m, Some(360.0));
        assert_eq!(fix.speed_mps, Some(3.2));
        assert_eq!(fix.heading_deg, Some(271.5));
        assert_eq!(fix.satellites, Some(9));
        assert_eq!(fix.fix_quality, FixQuality::Dgps);
        assert_eq!(fix.hdop, Some(0.9));
        assert_eq!(
            fix.accuracy_m, None,
            "NMEA has no accuracy field; inventing one here would teach the \
             site upgrade to trust a number nothing measured"
        );
        assert_eq!(fix.timestamp, expected_timestamp);
    }

    /// The quality mapping is one to one onto the nine GGA codes and never
    /// invents `Device` — a serial receiver that could claim the platform-
    /// service variant would sidestep `can_relocate`'s reasoning about it.
    #[test]
    fn the_quality_mapping_is_the_gga_table_and_never_device() {
        let all = [
            (ParsedQuality::None, FixQuality::None),
            (ParsedQuality::Gps, FixQuality::Gps),
            (ParsedQuality::Dgps, FixQuality::Dgps),
            (ParsedQuality::Pps, FixQuality::Pps),
            (ParsedQuality::Rtk, FixQuality::Rtk),
            (ParsedQuality::FloatRtk, FixQuality::FloatRtk),
            (ParsedQuality::Estimated, FixQuality::Estimated),
            (ParsedQuality::Manual, FixQuality::Manual),
            (ParsedQuality::Simulation, FixQuality::Simulation),
        ];
        for (parsed, expected) in all {
            let got = quality_from(parsed);
            assert_eq!(got, expected, "{parsed:?} translated to {got:?}");
            assert_ne!(
                got,
                FixQuality::Device,
                "no NMEA quality code may claim the platform-service variant"
            );
        }
    }
}
