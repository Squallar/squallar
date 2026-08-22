//! The counts-not-Kelvin decision, held by its consequence.

use super::*;
use crate::render::gridded::color_for;

/// The equator reading of each channel, at `(row 1499, column 2500)` —
/// `lat 0.0000`, `lon 0.0220` — in the 2025-06-01 **12:00 UTC** granules named
/// in [`crate::gmgsi`]'s provenance table.
///
/// 12 UTC and not 00 UTC on purpose: at 00 UTC the prime meridian is at local
/// midnight and `Visible` reads a perfectly legitimate **0**, which would make
/// this test turn on whether the bottom stop is opaque rather than on whether
/// the domain is counts or Kelvin.
const MEASURED_EQUATOR_READINGS: [(GmgsiChannel, f32); 4] = [
    (GmgsiChannel::LongwaveIr, 82.0),
    (GmgsiChannel::ShortwaveIr, 65.0),
    (GmgsiChannel::Visible, 118.0),
    (GmgsiChannel::WaterVapor, 166.0),
];

/// C.3. Every channel paints at the number the sky actually put there.
#[test]
fn every_channel_paints_at_its_measured_equator_reading() {
    for (channel, reading) in MEASURED_EQUATOR_READINGS {
        let scale = scale(channel);
        assert_eq!(
            (scale.min_value, scale.max_value),
            (MIN_COUNT, MAX_COUNT),
            "{} does not span the count domain",
            channel.display_name()
        );
        let painted = color_for(scale, reading);
        assert_ne!(
            painted[3],
            0,
            "{} paints nothing at its measured equator reading {reading}",
            channel.display_name()
        );
    }
}

/// C.3's floor, and the whole reason the decision needed a test: a ramp stated
/// in Kelvin puts every one of those four readings below its first stop, and
/// `color_for` paints nothing below the first stop. The layer would come up
/// blank on every channel with no error raised anywhere.
#[test]
fn a_kelvin_scaled_ramp_would_paint_none_of_them() {
    let kelvin = LegendScale {
        thresholds: vec![
            (180.0f32, [0xffu8, 0xff, 0xff]),
            (220.0, [0x99, 0x99, 0xff]),
            (260.0, [0x33, 0x66, 0x33]),
            (300.0, [0x33, 0x33, 0x33]),
        ],
        is_gradient: true,
        min_value: 180.0,
        max_value: 300.0,
    };
    for (channel, reading) in MEASURED_EQUATOR_READINGS {
        let painted = color_for(&kelvin, reading);
        assert_eq!(
            painted[3],
            0,
            "{} at {reading} painted through a Kelvin ramp, so this floor \
             cannot detect the mistake it exists to detect",
            channel.display_name()
        );
    }
}

/// The shipped ramps must actually cover the byte, both ends.
#[test]
fn every_channel_paints_across_the_whole_count_domain() {
    for &channel in GmgsiChannel::all() {
        let scale = scale(channel);
        for count in [0.0f32, 1.0, 128.0, 254.0, 255.0] {
            let painted = color_for(scale, count);
            assert_ne!(
                painted[3],
                0,
                "{} paints nothing at count {count}",
                channel.display_name()
            );
        }
        // Above the domain the ramp clamps rather than disappearing.
        assert_ne!(color_for(scale, 300.0)[3], 0);
        // A missing point is transparent -- this is what carries `_FillValue`.
        assert_eq!(color_for(scale, f32::NAN)[3], 0);
    }
}

/// Ascending, not negated: a higher count is colder and must come out brighter.
#[test]
fn the_greyscale_ascends_so_cold_cloud_paints_bright() {
    for &channel in GmgsiChannel::all() {
        let scale = scale(channel);
        let luma = |v: f32| {
            let c = color_for(scale, v);
            c[0] as u32 + c[1] as u32 + c[2] as u32
        };
        assert!(
            luma(255.0) > luma(0.0),
            "{} runs dark at the cold end",
            channel.display_name()
        );
        assert!(
            luma(200.0) > luma(50.0),
            "{} is not monotonic across its middle",
            channel.display_name()
        );
    }
}

#[test]
fn every_channel_is_registered_exactly_once_under_the_one_group() {
    assert_eq!(products().len(), 4);
    for &channel in GmgsiChannel::all() {
        let spec = spec(channel);
        assert_eq!(spec.code, channel.as_str());
        assert_eq!(spec.group, GROUP);
        assert_eq!(spec.value_domain, (MIN_COUNT, MAX_COUNT));
        assert!(!spec.vertical && !spec.tilted);
    }
    let mut codes: Vec<&str> = products().iter().map(|p| p.code).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), 4);
}

/// The persisted spellings are a config-file and wire contract.
#[test]
fn the_channel_codes_round_trip() {
    for &channel in GmgsiChannel::all() {
        assert_eq!(channel.as_str().parse(), Ok(channel));
    }
    assert_eq!("GmgsiSsr".parse::<GmgsiChannel>(), Err(()));
}
