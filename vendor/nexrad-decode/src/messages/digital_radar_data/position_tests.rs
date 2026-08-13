//! What scale the Volume Data Block's position is read at.
//!
//! An in-source module rather than a `tests/` target, so it travels with the
//! change it pins when this vendored delta is re-applied onto a later upstream
//! release — the same arrangement, for the same reason, as
//! `crate::messages::framing_tests`.
//!
//! The three blocks quoted below are verbatim: 40 or 48 bytes lifted out of a
//! real archive, starting at the byte after the block's `RVOL` identifier. Each
//! names the volume it came from, so a reader can pull the same file and get
//! the same bytes.

use super::raw::{VolumeDataBlock as RawModern, VolumeDataBlockLegacy as RawLegacy};
use super::VolumeDataBlock;
use crate::messages::raw::primitive_aliases::Real4;
use zerocopy::FromBytes;

/// `s3://unidata-nexrad-level2/2020/08/10/TORD/TORD20200810_203830_V08`, the
/// first Message 31's VOL block. `lrtup` 44, version 1.0, and a position in
/// thousandths of a degree: `47 23 45 00` is the `f32` 41797.0.
const TORD_2020: [u8; 40] = [
    0x00, 0x2c, 0x01, 0x00, 0x47, 0x23, 0x45, 0x00, 0xc7, 0xab, 0x99, 0x00, 0x00, 0xe2, 0x00, 0xe2,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x50, 0x00, 0x00,
];

/// `s3://unidata-nexrad-level2/2026/08/12/TORD/TORD20260812_000527_V08`, the
/// same site and the same block after the producer was corrected: `42 27 30 21`
/// is the `f32` 41.797.
const TORD_2026: [u8; 40] = [
    0x00, 0x2c, 0x01, 0x00, 0x42, 0x27, 0x30, 0x21, 0xc2, 0xaf, 0xb7, 0x4c, 0x00, 0xe2, 0x00, 0xe2,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x5a, 0x00, 0x00,
];

/// `s3://unidata-nexrad-level2/2026/08/11/KTLX/KTLX20260811_000049_V06`, a
/// WSR-88D's VOL block: `lrtup` 52, version 3.0, degrees.
const KTLX_2026: [u8; 48] = [
    0x00, 0x34, 0x03, 0x00, 0x42, 0x0d, 0x55, 0x5d, 0xc2, 0xc2, 0x8e, 0x37, 0x01, 0x72, 0x00, 0x13,
    0xc2, 0x2b, 0x0a, 0x41, 0x43, 0x2d, 0xd7, 0xe7, 0x43, 0x24, 0x78, 0x76, 0xbf, 0xc4, 0x3e, 0xc6,
    0x42, 0x70, 0x00, 0x00, 0x00, 0xd4, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// The `f32` pair sitting in a quoted block's latitude and longitude, read
/// straight out of the bytes rather than through the accessors under test.
fn stated(bytes: &[u8]) -> (f32, f32) {
    let lat = f32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let lon = f32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    (lat, lon)
}

/// A legacy block carrying `lat`/`lon` and nothing else that matters here.
fn legacy_at(lat: f32, lon: f32) -> RawLegacy {
    let mut raw = RawLegacy::read_from_bytes(&TORD_2026[..]).expect("40 bytes is the legacy block");
    raw.latitude = Real4::new(lat);
    raw.longitude = Real4::new(lon);
    raw
}

/// The position a legacy block carrying `lat`/`lon` decodes to.
fn read_back(lat: f32, lon: f32) -> (f32, f32) {
    let raw = legacy_at(lat, lon);
    let block = VolumeDataBlock::new_legacy(&raw);
    (block.latitude_raw(), block.longitude_raw())
}

/// The pre-2021-09-15 TDWR producer's thousandths are read as the degrees they
/// mean. Chicago O'Hare's terminal radar is at 41.797 °N, 87.858 °W.
#[test]
fn a_tdwr_position_in_thousandths_is_read_in_degrees() {
    let raw = RawLegacy::read_from_bytes(&TORD_2020[..]).expect("40 bytes is the legacy block");
    assert_eq!(
        stated(&TORD_2020),
        (41797.0, -87858.0),
        "the block states thousandths; if this moved, the fixture did"
    );

    let block = VolumeDataBlock::new_legacy(&raw);
    assert!(block.is_legacy(), "lrtup 44 is the 40-byte block");
    assert_eq!(block.latitude_raw(), 41.797);
    assert_eq!(block.longitude_raw(), -87.858);
}

/// The corrected producer's degrees are the same position, and are handed back
/// exactly as stated rather than passing through any arithmetic.
#[test]
fn a_tdwr_position_in_degrees_is_the_bytes_untouched() {
    let raw = RawLegacy::read_from_bytes(&TORD_2026[..]).expect("40 bytes is the legacy block");
    let block = VolumeDataBlock::new_legacy(&raw);

    let (lat, lon) = stated(&TORD_2026);
    assert_eq!(block.latitude_raw().to_bits(), lat.to_bits());
    assert_eq!(block.longitude_raw().to_bits(), lon.to_bits());
    assert_eq!(
        (block.latitude_raw(), block.longitude_raw()),
        (41.797, -87.858)
    );
}

/// A WSR-88D states degrees and gets its own bits back — bit-identity, not
/// agreement to some tolerance, because a `/ 1000.0` that fired and a `* 1000.0`
/// that undid it would agree to any tolerance and still be a different number.
#[test]
fn a_wsr88d_position_is_the_bytes_untouched() {
    let raw = RawModern::read_from_bytes(&KTLX_2026[..]).expect("48 bytes is the modern block");
    let block = VolumeDataBlock::new(&raw);
    assert!(!block.is_legacy(), "lrtup 52 is the 48-byte block");

    let (lat, lon) = stated(&KTLX_2026);
    assert_eq!(block.latitude_raw().to_bits(), lat.to_bits());
    assert_eq!(block.longitude_raw().to_bits(), lon.to_bits());
}

/// Every position the ICD's range admits is returned as stated, whichever
/// hemisphere it is in and whether or not it happens to be a whole number of
/// degrees.
///
/// The whole-degree rows are the ones that matter: 45.0 and -93.0 are exact
/// integers, which is half of what the thousandths reading looks for, and a
/// rule that only checked integrality would move a radar in Minnesota to within
/// 5 km of Null Island.
#[test]
fn a_position_on_earth_is_never_rescaled() {
    for (lat, lon) in [
        (41.797_f32, -87.858_f32),
        (35.3334, -97.2778),
        (45.0, -93.0),
        (0.0, 0.0),
        (-35.0, 145.0),
        (18.474, -66.179),
        (90.0, 180.0),
        (-90.0, -180.0),
        (13.456, 144.811),
    ] {
        let (out_lat, out_lon) = read_back(lat, lon);
        assert_eq!(out_lat.to_bits(), lat.to_bits(), "latitude {lat} moved");
        assert_eq!(out_lon.to_bits(), lon.to_bits(), "longitude {lon} moved");
    }
}

/// Thousandths are read the same either side of the equator and either side of
/// the prime meridian.
///
/// Every radar in the archive is north and west, so a sign error in this
/// conversion would hide behind the sample. Dividing carries the sign, and
/// these rows are what says so.
#[test]
fn thousandths_are_read_in_both_hemispheres() {
    for (stated_pair, expected) in [
        ((41797.0_f32, -87858.0_f32), (41.797_f32, -87.858_f32)),
        ((-35_000.0, 145_000.0), (-35.0, 145.0)),
        ((-33_868.0, 151_209.0), (-33.868, 151.209)),
        ((13_456.0, 144_811.0), (13.456, 144.811)),
        ((-1_000.0, -1_000.0), (-1.0, -1.0)),
    ] {
        assert_eq!(read_back(stated_pair.0, stated_pair.1), expected);
    }
}

/// The scale is decided for the pair, not for each coordinate on its own.
///
/// A radar within 0.09° of the equator states a thousandths latitude that is
/// also a legal degrees latitude, so a per-coordinate rule would leave the
/// latitude at 40° and divide the longitude, putting the radar 4,400 km from
/// either reading. Deciding once cannot produce a position neither scale names.
#[test]
fn the_scale_is_decided_for_the_pair() {
    assert_eq!(read_back(40.0, -87_858.0), (0.04, -87.858));
}

/// A position no scale rescues is left where it is, out of range, for the
/// caller to refuse.
///
/// Rescaling is not a repair: it recognises one encoding, and everything it
/// does not recognise has to still be recognisably wrong. Each row here would
/// become a plausible-looking coordinate if the reading were loosened —
/// dropping the integrality condition takes the first two, and rescaling
/// whatever is out of range takes the rest.
#[test]
fn a_position_no_scale_rescues_stays_out_of_range() {
    for (lat, lon) in [
        (41_797.5_f32, -87_858.5_f32),
        (41_797.0, -87_858.4),
        (200_000.0, -93_000.0),
        (45_000.0, -593_000.0),
        (1e30, -1e30),
        (f32::INFINITY, f32::NEG_INFINITY),
        (f32::NAN, f32::NAN),
    ] {
        let (out_lat, out_lon) = read_back(lat, lon);
        assert!(
            !(-90.0..=90.0).contains(&out_lat) || !(-180.0..=180.0).contains(&out_lon),
            "({lat}, {lon}) decoded to ({out_lat}, {out_lon}), which reads as a place"
        );
    }
}

/// Nothing but the two coordinates moves. The heights in particular are whole
/// metres in both producers — `TORD` states 226 either side of the change — so
/// a fix that reached them would be inventing a defect.
#[test]
fn the_rest_of_the_block_is_untouched() {
    let raw = RawLegacy::read_from_bytes(&TORD_2020[..]).expect("40 bytes is the legacy block");
    let block = VolumeDataBlock::new_legacy(&raw);
    assert_eq!(block.lrtup_raw(), 44);
    assert_eq!(block.major_version_number(), 1);
    assert_eq!(block.minor_version_number(), 0);
    assert_eq!(block.site_height_raw(), 226);
    assert_eq!(block.tower_height_raw(), 226);
    assert_eq!(block.volume_coverage_pattern_number(), 80);
}
