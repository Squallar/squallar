//! Where `decode_messages` believes one message ends and the next begins.
//!
//! Every message header declares its own size, and for a Type 31 the parser
//! also walks the message's data blocks and arrives at an end of its own. On
//! WSR-88D data the two always agree, so which one the reader trusts never
//! mattered. TDWR pads each Type 31 body out to an eight-byte boundary, and
//! there the two disagree by the width of the pad on every single radial.
//!
//! These tests fix both answers in place: the padded stream has to stay framed,
//! and the unpadded stream has to keep the exact offsets and sizes it has
//! always had.
//!
//! The same declared size also says how much of a fixed-length segment is
//! payload, and the tests at the foot of this file pin that: all of what the
//! segment declares, and never more than its frame holds.

use super::digital_radar_data::raw as radar_raw;
use super::{decode_messages, Message, MessageContents};

/// Bytes of RPG-supplied prefix at the head of every message header. The
/// `segment_size` field counts halfwords from the end of it, so a message
/// occupies `12 + message_size_bytes()` bytes on the wire.
const CTM_PREFIX_LEN: usize = 12;

/// The message header less its CTM prefix — the part `segment_size` counts.
const HEADER_LEN: usize = 16;

/// One fixed-length message frame: CTM prefix, header block, then body.
const SEGMENT_FRAME_LEN: usize = 2432;

/// The body of one such frame — all the payload it can physically hold.
const SEGMENT_BODY_LEN: usize = SEGMENT_FRAME_LEN - CTM_PREFIX_LEN - HEADER_LEN;

/// Payload bytes a full segment carries in real RDA output.
///
/// Every one of the 855 multi-segment frames measured across 171 WSR-88D
/// volumes declares 1,208 halfwords — the sixteen-byte header block and 2,400
/// bytes of payload — which leaves four bytes of the frame unused. None
/// declares more than the frame body holds.
const RDA_SEGMENT_PAYLOAD_LEN: usize = 2400;

/// `digital_radar_data::raw::Header`, the first thing a Type 31 body carries.
const RADIAL_HEADER_LEN: usize = 32;

/// One data block pointer: a big-endian `u32`, one per declared block, in a
/// table immediately after the radial header.
const POINTER_LEN: usize = 4;

/// A generic data block's fixed prefix — its four-byte id, then the generic
/// header. The moment's gate bytes follow it.
const MOMENT_BLOCK_HEADER_LEN: usize =
    size_of::<radar_raw::DataBlockId>() + size_of::<radar_raw::GenericDataBlockHeader>();

/// Gates in the reflectivity block the padded fixtures carry. Any count does;
/// what matters is its parity, which decides whether an odd pad is expressible
/// at all.
const MOMENT_GATES: u16 = 16;

/// Bytes TDWR pads a Type 31 body with to reach an eight-byte boundary. Four
/// is what a body of `16 + 32` needs, and what real TPIT radials carry most
/// often; the range observed is 4 to 7.
const TDWR_PAD: usize = 4;

/// The widest trailing pad `decode_messages` will accept.
///
/// Written out rather than read from `MAX_TRAILING_PAD` on purpose: a test
/// phrased in terms of the bound moves with the bound and pins nothing, which
/// is how a widening back to the failure the bound exists to prevent would pass
/// unnoticed.
///
/// Seven is not a corner case. Measured over 48,600 real TDWR radials, the
/// trailing pad is 4 on 30,285 of them, 5 on 1,440, 6 on 10,395 and **7 on
/// 6,480** — 13.3% of every radial the site emits sits exactly on this
/// inclusive endpoint, and losing it desyncs all of them.
const WIDEST_PAD: usize = 7;

/// Filler for pad bytes. Deliberately not zero: a reader that desyncs into
/// padding should be reading obvious garbage, not a plausible empty header.
const PAD_BYTE: u8 = 0xAA;

/// Modified Julian date, and milliseconds past midnight. Both nonzero so the
/// headers these tests build carry a real timestamp rather than the epoch.
const DATE: u16 = 20_000;
const TIME: u32 = 43_200_000;

/// A message header: twelve zero bytes of CTM prefix, then the sixteen bytes
/// `segment_size` counts.
fn message_header(
    segment_size: u16,
    message_type: u8,
    sequence_number: u16,
    segment_count: u16,
    segment_number: u16,
) -> Vec<u8> {
    let mut header = vec![0u8; CTM_PREFIX_LEN];
    header.extend_from_slice(&segment_size.to_be_bytes());
    header.push(0); // redundant channel: legacy single channel
    header.push(message_type);
    header.extend_from_slice(&sequence_number.to_be_bytes());
    header.extend_from_slice(&DATE.to_be_bytes());
    header.extend_from_slice(&TIME.to_be_bytes());
    header.extend_from_slice(&segment_count.to_be_bytes());
    header.extend_from_slice(&segment_number.to_be_bytes());
    assert_eq!(header.len(), CTM_PREFIX_LEN + HEADER_LEN);
    header
}

/// The radial header at the head of a Type 31 body.
fn radial_header(azimuth_number: u16, data_block_count: u16) -> Vec<u8> {
    let mut header = Vec::with_capacity(RADIAL_HEADER_LEN);
    header.extend_from_slice(b"TPIT");
    header.extend_from_slice(&TIME.to_be_bytes());
    header.extend_from_slice(&DATE.to_be_bytes());
    header.extend_from_slice(&azimuth_number.to_be_bytes());
    header.extend_from_slice(&f32::from(azimuth_number).to_be_bytes());
    header.push(0); // uncompressed
    header.push(0); // spare, for halfword alignment
    header.extend_from_slice(&(RADIAL_HEADER_LEN as u16).to_be_bytes()); // radial length
    header.push(2); // azimuth resolution spacing: 1.0 degrees, as TDWR reports
    header.push(1); // radial status: intermediate
    header.push(1); // elevation number
    header.push(0); // cut sector number
    header.extend_from_slice(&0.5f32.to_be_bytes()); // elevation angle
    header.push(0); // spot blanking: none
    header.push(0); // azimuth indexing mode: no indexing
    header.extend_from_slice(&data_block_count.to_be_bytes());
    assert_eq!(header.len(), RADIAL_HEADER_LEN);
    header
}

/// A reflectivity block: its id, its generic header, then one byte per gate.
///
/// Eight-bit moment data, which is what makes an odd block length — and so an
/// odd trailing pad — reachable at all.
fn reflectivity_block(gates: u16) -> Vec<u8> {
    let mut block = Vec::with_capacity(MOMENT_BLOCK_HEADER_LEN + gates as usize);
    block.push(b'D'); // data block type
    block.extend_from_slice(b"REF");
    block.extend_from_slice(&0u32.to_be_bytes()); // reserved
    block.extend_from_slice(&gates.to_be_bytes());
    block.extend_from_slice(&2_125u16.to_be_bytes()); // range to first gate
    block.extend_from_slice(&150u16.to_be_bytes()); // gate interval
    block.extend_from_slice(&0u16.to_be_bytes()); // tover
    block.extend_from_slice(&16i16.to_be_bytes()); // SNR threshold
    block.push(0); // control flags: none
    block.push(8); // data word size: eight bits per gate
    block.extend_from_slice(&2.0f32.to_be_bytes()); // scale
    block.extend_from_slice(&66.0f32.to_be_bytes()); // offset
    assert_eq!(block.len(), MOMENT_BLOCK_HEADER_LEN);
    for gate in 0..gates {
        block.push((gate % 250) as u8 + 2);
    }
    block
}

/// A Type 31 message carrying no data blocks, followed by `pad` bytes of
/// trailing slack that the declared size accounts for and the data block walk
/// never reaches.
fn m31(azimuth_number: u16, pad: usize) -> Vec<u8> {
    assert_eq!(
        pad % 2,
        0,
        "an odd pad truncates when halved into segment_size, \
         leaving a fixture that disagrees with itself"
    );

    let body = HEADER_LEN + RADIAL_HEADER_LEN + pad;
    let mut message = message_header((body / 2) as u16, 31, azimuth_number, 0, 0);
    message.extend_from_slice(&radial_header(azimuth_number, 0));
    message.resize(message.len() + pad, PAD_BYTE);
    assert_eq!(message.len(), CTM_PREFIX_LEN + body);
    message
}

/// The same, but carrying one reflectivity block, so the walk can stop on an
/// odd byte and the pad can be odd with it.
fn m31_with_moment(azimuth_number: u16, gates: u16, pad: usize) -> Vec<u8> {
    let body = HEADER_LEN
        + RADIAL_HEADER_LEN
        + POINTER_LEN
        + MOMENT_BLOCK_HEADER_LEN
        + gates as usize
        + pad;
    assert_eq!(
        body % 2,
        0,
        "segment_size counts halfwords, so a fixture whose framed body is odd \
         cannot declare its own length"
    );

    let mut message = message_header((body / 2) as u16, 31, azimuth_number, 0, 0);
    message.extend_from_slice(&radial_header(azimuth_number, 1));
    // The block sits immediately after the pointer table, and the pointer is
    // measured from the start of the body.
    message.extend_from_slice(&((RADIAL_HEADER_LEN + POINTER_LEN) as u32).to_be_bytes());
    message.extend_from_slice(&reflectivity_block(gates));
    message.resize(message.len() + pad, PAD_BYTE);
    assert_eq!(message.len(), CTM_PREFIX_LEN + body);
    message
}

/// Where the data block walk stops in a message built by [`m31_with_moment`],
/// measured from the start of the message.
fn walked_length(gates: u16) -> usize {
    CTM_PREFIX_LEN
        + HEADER_LEN
        + RADIAL_HEADER_LEN
        + POINTER_LEN
        + MOMENT_BLOCK_HEADER_LEN
        + gates as usize
}

/// The azimuth number of every radial that decoded, in order.
fn azimuths(messages: &[Message<'_>]) -> Vec<u16> {
    messages
        .iter()
        .filter_map(|message| match message.contents() {
            MessageContents::DigitalRadarData(data) => Some(data.header().azimuth_number()),
            _ => None,
        })
        .collect()
}

/// Where each message starts, and how long it claims to be.
fn frames(messages: &[Message<'_>]) -> Vec<(usize, usize)> {
    messages.iter().map(|m| (m.offset(), m.size())).collect()
}

/// The bug this file exists for. Three TDWR-shaped radials, each with four
/// bytes of pad the data block walk never reaches: framing from the walk reads
/// the second header out of the first radial's padding and the stream is lost
/// from there.
#[test]
fn tdwr_padded_type31_stream_stays_framed() {
    let mut input = Vec::new();
    for azimuth in 1..=3u16 {
        input.extend_from_slice(&m31(azimuth, TDWR_PAD));
    }

    let messages = decode_messages(&input).expect("decodes");

    assert_eq!(messages.len(), 3, "every radial decodes");
    assert_eq!(azimuths(&messages), vec![1, 2, 3]);
    assert_eq!(frames(&messages), vec![(0, 64), (64, 64), (128, 64)]);

    let last = &messages[2];
    assert_eq!(
        last.offset() + last.size(),
        input.len(),
        "the last radial ends where the input does — nothing is left over"
    );
}

/// Every pad width the accepted range admits, one at a time, including the
/// inclusive endpoint that 13.3% of real TDWR radials sit on. Closing the range
/// one byte early — `walk..walk + MAX_TRAILING_PAD` instead of `..=` — is a
/// one-character edit that desyncs every one of them, and it is this that
/// notices.
#[test]
fn every_pad_width_within_the_bound_stays_framed() {
    for pad in 0..=WIDEST_PAD {
        // A framed body is always an even number of bytes, because
        // `segment_size` counts halfwords. So an odd pad only exists behind an
        // odd moment length — which is exactly how real TDWR radials come by
        // theirs: eight-bit moment data, an odd gate count.
        let gates = MOMENT_GATES + (pad % 2) as u16;
        let framed = walked_length(gates) + pad;

        let mut input = Vec::new();
        for azimuth in 1..=3u16 {
            input.extend_from_slice(&m31_with_moment(azimuth, gates, pad));
        }

        let messages = decode_messages(&input).expect("decodes");

        assert_eq!(azimuths(&messages), vec![1, 2, 3], "pad of {pad} bytes");
        assert_eq!(
            frames(&messages),
            vec![(0, framed), (framed, framed), (2 * framed, framed)],
            "pad of {pad} bytes"
        );
    }
}

/// One byte past the bound, and the other half of the pin.
///
/// Eight bytes of slack is more than padding to an eight-byte boundary can
/// produce, so the declaration is not a pad — it is a disagreement, and the
/// framing stays with the parse. Together with the test above, the accepted
/// range is closed on both sides by fixtures rather than by a constant, so
/// widening `MAX_TRAILING_PAD` back towards trusting a corrupt `segment_size`
/// fails here.
#[test]
fn a_pad_one_byte_past_the_bound_is_not_trusted() {
    const PAD: usize = WIDEST_PAD + 1;

    let input = m31_with_moment(1, MOMENT_GATES, PAD);
    let walked = walked_length(MOMENT_GATES);

    let messages = decode_messages(&input).expect("decodes");

    assert_eq!(azimuths(&messages), vec![1]);
    assert_eq!(
        frames(&messages),
        vec![(0, walked)],
        "framed from the parse, not from a declaration no pad can explain"
    );
    assert_eq!(
        input.len() - walked,
        PAD,
        "and the slack it declared is left unread"
    );
}

/// The WSR-88D pin. With no pad the declared end and the walk are the same
/// byte, and every offset and size is the one this decoder has always
/// reported.
#[test]
fn unpadded_type31_stream_is_byte_identical_to_before() {
    let mut input = Vec::new();
    for azimuth in 1..=3u16 {
        input.extend_from_slice(&m31(azimuth, 0));
    }

    let messages = decode_messages(&input).expect("decodes");

    assert_eq!(azimuths(&messages), vec![1, 2, 3]);
    assert_eq!(frames(&messages), vec![(0, 60), (60, 60), (120, 60)]);
}

/// The same pin against real WSR-88D bytes rather than a builder: the packaged
/// radial declares exactly the length of the file, so skipping to the declared
/// end moves the reader nowhere.
#[test]
fn wsr88d_fixture_carries_zero_pad_so_the_skip_is_a_noop() {
    const FIXTURE: &[u8] = include_bytes!("../../tests/data/messages/digital_radar_data_full.bin");

    let messages = decode_messages(FIXTURE).expect("decodes");
    assert_eq!(messages.len(), 1);

    let declared = CTM_PREFIX_LEN + 2 * messages[0].header().segment_size.get() as usize;
    assert_eq!(
        declared,
        FIXTURE.len(),
        "the fixture carries no trailing pad"
    );
    assert_eq!(messages[0].size(), declared);
}

/// A radial whose first data block pointer points behind the reader fails with
/// `InvalidDataBlockPointer` — an error, but not the end of the input, so the
/// decoder skips to the message's declared end and carries on. Skipping to
/// `offset + size` instead lands twelve bytes short, in the padding.
#[test]
fn a_failed_type31_parse_recovers_to_the_next_message() {
    // One data block, whose pointer is zero: by the time the walk reads it,
    // the reader is 36 bytes into the body and cannot go back.
    const PAD: usize = 20;

    let body = HEADER_LEN + RADIAL_HEADER_LEN + POINTER_LEN + PAD;
    let mut input = message_header((body / 2) as u16, 31, 1, 0, 0);
    input.extend_from_slice(&radial_header(1, 1));
    input.extend_from_slice(&0u32.to_be_bytes());
    input.resize(input.len() + PAD, PAD_BYTE);
    assert_eq!(input.len(), CTM_PREFIX_LEN + body);

    input.extend_from_slice(&m31(2, 0));

    let messages = decode_messages(&input).expect("decodes");

    assert_eq!(
        azimuths(&messages),
        vec![2],
        "the broken radial is dropped and the next one is found"
    );
    assert_eq!(frames(&messages), vec![(84, 60)]);
}

/// An unrecognised variable-length message has no parser to walk it, so its
/// declared end is the only framing there is.
///
/// Note what this pins: the ICD says the 0xFFFF sentinel repurposes the
/// segment count and number fields as a 32-bit byte count, but not what that
/// count is measured from. Twelve bytes past the message start is this
/// decoder's own inference, consistent with the halfword case; no real
/// sentinel message exists in any fixture here to check it against.
#[test]
fn an_unknown_variable_length_message_is_skipped_to_its_declared_end() {
    const DECLARED: u32 = 52;

    let mut input = message_header(
        0xFFFF,
        200,
        0,
        (DECLARED >> 16) as u16,
        (DECLARED & 0xFFFF) as u16,
    );
    input.resize(CTM_PREFIX_LEN + DECLARED as usize, PAD_BYTE);

    input.extend_from_slice(&m31(7, 0));

    let messages = decode_messages(&input).expect("decodes");

    assert_eq!(messages.len(), 2);
    assert_eq!(frames(&messages), vec![(0, 64), (64, 60)]);
    assert_eq!(azimuths(&messages), vec![7]);
}

/// The declared end is trusted, but only within a pad's width of where the
/// parse stopped. A radial that parsed cleanly and then claims to be 64
/// kilobytes long is claiming something no parse agrees with, and the framing
/// stays with the parse — which is what kept a corrupt `segment_size` from
/// desyncing a WSR-88D record before any of this.
#[test]
fn a_corrupt_declared_size_does_not_desync_a_clean_parse() {
    let mut input = m31(1, 0);
    input[CTM_PREFIX_LEN..CTM_PREFIX_LEN + 2].copy_from_slice(&0x7FFFu16.to_be_bytes());
    input.extend_from_slice(&m31(2, 0));

    let messages = decode_messages(&input).expect("decodes");

    assert_eq!(azimuths(&messages), vec![1, 2]);
    assert_eq!(
        frames(&messages),
        vec![(0, 60), (60, 60)],
        "both radials framed from the parse, the lie ignored"
    );
}

/// Message type 15, the Clutter Filter Map — the fixed-segment message these
/// last two tests build, chosen because every byte of its payload is reachable
/// through the public API.
const CLUTTER_FILTER_MAP_TYPE: u8 = 15;

/// Azimuth segments in one elevation of a clutter filter map, one per degree.
const AZIMUTHS_PER_ELEVATION: u16 = 360;

/// Message type 13, the Clutter Filter Bypass Map. Its range bins are read
/// across segment boundaries rather than structure by structure, which is what
/// lets a segment's content run to the very last byte of its frame.
const CLUTTER_FILTER_BYPASS_MAP_TYPE: u8 = 13;

/// Range bin bytes one elevation of a bypass map carries: 360 radials of 32
/// halfwords.
const BYPASS_MAP_RANGE_BIN_LEN: usize = 360 * 32 * 2;

/// Where a segment's declared halfword count is measured from.
#[derive(Clone, Copy)]
enum DeclaredFrom {
    /// The end of the CTM prefix, which is what the ICD says and what every
    /// real segment measured here does.
    HeaderBlock,
    /// The start of the frame, which is what this crate's own
    /// `generate_synthetic_fixtures` writes — twelve bytes more than the frame
    /// body holds.
    FrameStart,
}

/// A clutter filter map body of `elevations` elevation segments, each carrying
/// the full 360 azimuth segments of one range zone that the ICD calls for.
///
/// Every zone's end range is its own index across the whole map, so a payload
/// that arrives short or shifted shows up as a wrong value and not only as a
/// wrong count.
fn clutter_filter_map_body(elevations: u16) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&DATE.to_be_bytes()); // map generation date
    body.extend_from_slice(&720u16.to_be_bytes()); // map generation time, minutes
    body.extend_from_slice(&elevations.to_be_bytes());

    for zone in 0..elevations * AZIMUTHS_PER_ELEVATION {
        body.extend_from_slice(&1u16.to_be_bytes()); // one range zone
        body.extend_from_slice(&1u16.to_be_bytes()); // op code: bypass filter
        body.extend_from_slice(&zone.to_be_bytes()); // end range, km
    }
    body
}

/// A clutter filter bypass map body of one elevation segment.
///
/// The range bins run 0, 1, 2, … modulo a prime, so a payload that arrives
/// short, shifted or spliced shows up as a wrong byte and not only as a wrong
/// length.
fn clutter_filter_bypass_map_body() -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + BYPASS_MAP_RANGE_BIN_LEN);
    body.extend_from_slice(&DATE.to_be_bytes()); // map generation date
    body.extend_from_slice(&720u16.to_be_bytes()); // map generation time, minutes
    body.extend_from_slice(&1u16.to_be_bytes()); // one elevation segment
    body.extend_from_slice(&1u16.to_be_bytes()); // elevation segment number
    body.extend((0..BYPASS_MAP_RANGE_BIN_LEN).map(|bin| (bin % 251) as u8));
    body
}

/// Frames a fixed-segment message body into 2432-byte frames, `payload_len`
/// bytes of it per frame, each declaring its own size.
fn fixed_segment_frames(
    message_type: u8,
    body: &[u8],
    payload_len: usize,
    declared_from: DeclaredFrom,
) -> Vec<u8> {
    assert!(
        payload_len <= SEGMENT_BODY_LEN,
        "a segment cannot carry more payload than its frame holds"
    );

    let segment_count = body.len().div_ceil(payload_len);
    let mut frames = Vec::with_capacity(segment_count * SEGMENT_FRAME_LEN);

    for (index, payload) in body.chunks(payload_len).enumerate() {
        let declared = match declared_from {
            DeclaredFrom::HeaderBlock => HEADER_LEN + payload.len(),
            DeclaredFrom::FrameStart => CTM_PREFIX_LEN + HEADER_LEN + payload.len(),
        };
        assert_eq!(declared % 2, 0, "segment_size counts halfwords");

        let segment_number = index as u16 + 1;
        let mut frame = message_header(
            (declared / 2) as u16,
            message_type,
            segment_number,
            segment_count as u16,
            segment_number,
        );
        frame.extend_from_slice(payload);
        frame.resize(SEGMENT_FRAME_LEN, PAD_BYTE);
        frames.extend_from_slice(&frame);
    }

    frames
}

/// The azimuth segment count of each elevation of the decoded clutter filter
/// map, and the end range of every range zone in the order they arrived.
fn clutter_filter_map(messages: &[Message<'_>]) -> (Vec<usize>, Vec<u16>) {
    let map = messages
        .iter()
        .find_map(|message| match message.contents() {
            MessageContents::ClutterFilterMap(map) => Some(map),
            _ => None,
        })
        .expect("a clutter filter map decoded");

    let azimuth_counts = map
        .elevation_segments()
        .iter()
        .map(|elevation| elevation.azimuth_segments().len())
        .collect();
    let end_ranges = map
        .elevation_segments()
        .iter()
        .flat_map(|elevation| elevation.azimuth_segments())
        .flat_map(|azimuth| azimuth.range_zones())
        .map(|zone| zone.end_range())
        .collect();

    (azimuth_counts, end_ranges)
}

/// A segment's payload is everything its header declares past the header block.
///
/// `segment_size` counts halfwords from the end of the CTM prefix, so
/// subtracting the whole of `MessageHeader` takes the prefix out a second time
/// and the last twelve bytes of every segment go missing. The reader position
/// survives it — the frame is padded to 2432 either way — so nothing desyncs
/// and the loss is silent.
///
/// It is content that goes, not padding. Measured over 171 real WSR-88D
/// volumes: every clutter filter map loses ten of its 1,800 azimuth segments,
/// leaving its last elevation with 350 of 360; every adaptation data field past
/// the first segment boundary shifts, which is why the site name decodes to
/// nothing where it should read the four-letter site id; and a five-elevation
/// clutter filter bypass map comes up 588 bytes short of its own length, hits
/// the end of the input and decodes to no elevation segments at all.
#[test]
fn a_multi_segment_payload_is_everything_its_header_declares() {
    const ELEVATIONS: u16 = 2;

    let body = clutter_filter_map_body(ELEVATIONS);
    let input = fixed_segment_frames(
        CLUTTER_FILTER_MAP_TYPE,
        &body,
        RDA_SEGMENT_PAYLOAD_LEN,
        DeclaredFrom::HeaderBlock,
    );

    let messages = decode_messages(&input).expect("decodes");

    let (azimuth_counts, end_ranges) = clutter_filter_map(&messages);
    assert_eq!(
        azimuth_counts,
        vec![AZIMUTHS_PER_ELEVATION as usize; ELEVATIONS as usize],
        "every elevation is whole"
    );
    assert_eq!(
        end_ranges,
        (0..ELEVATIONS * AZIMUTHS_PER_ELEVATION).collect::<Vec<_>>(),
        "and every zone arrives in order, with none dropped at a segment boundary"
    );
}

/// A segment that declares more than its frame holds is read to the frame and
/// no further.
///
/// No real segment does this — all 855 measured here declare 2,416 bytes
/// against a 2,404-byte body — but this crate's own
/// `generate_synthetic_fixtures` measures `segment_size` from the start of the
/// frame, so `clutter_filter_bypass_map.bin` is exactly this shape and its
/// snapshot is what the clamp keeps decoding.
///
/// Without the clamp the payload runs twelve bytes into the next frame, taking
/// that frame's CTM prefix in as though it were message content and leaving the
/// reader inside the frame, where the following header is read out of the
/// middle of a segment. The frame is the bound, not the declaration.
#[test]
fn a_segment_declaring_more_than_its_frame_holds_is_read_to_the_frame() {
    let body = clutter_filter_bypass_map_body();
    let input = fixed_segment_frames(
        CLUTTER_FILTER_BYPASS_MAP_TYPE,
        &body,
        SEGMENT_BODY_LEN,
        DeclaredFrom::FrameStart,
    );

    let messages = decode_messages(&input).expect("decodes");

    assert_eq!(messages.len(), 1, "the frames assemble into one message");
    let map = messages
        .iter()
        .find_map(|message| match message.contents() {
            MessageContents::ClutterFilterBypassMap(map) => Some(map),
            _ => None,
        })
        .expect("a clutter filter bypass map decoded");

    let elevations = map.elevation_segments();
    assert_eq!(elevations.len(), 1);
    assert_eq!(elevations[0].segment_number(), 1);
    assert_eq!(
        elevations[0].range_bins(),
        &body[body.len() - BYPASS_MAP_RANGE_BIN_LEN..],
        "the range bins arrive whole, with no frame prefix spliced into them"
    );
}
