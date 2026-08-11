pub mod clutter_censor_zones;
pub mod clutter_filter_bypass_map;
pub mod clutter_filter_map;
pub mod console_message;
pub mod digital_radar_data;
pub mod digital_radar_data_legacy;
pub mod loopback_test;
pub mod performance_maintenance_data;
pub mod rda_adaptation_data;
pub mod rda_control_commands;
pub mod rda_log_data;
pub mod rda_prf_data;
pub mod rda_status_data;
pub mod request_for_data;
pub mod volume_coverage_pattern;

mod raw;
pub(crate) use raw::primitive_aliases;
pub use raw::{MessageHeader, MessageType, RedundantChannel};

mod message;
pub use message::{Message, MessageHeaders};

mod message_contents;
pub use message_contents::MessageContents;

#[cfg(test)]
mod framing_tests;

use crate::result::{Error, Result};
use crate::segmented_slice_reader::SegmentedSliceReader;
use crate::slice_reader::SliceReader;
use log::{trace, warn};

/// Size of a single fixed-length message frame (header + content + padding).
const SEGMENT_FRAME_SIZE: usize = 2432;

/// Size of the RPG-supplied prefix at the head of every message header.
///
/// `MessageHeader::message_size_bytes` counts from the end of this prefix, not
/// from the start of the message, so a message occupies
/// `CTM_PREFIX_SIZE + message_size_bytes()` bytes on the wire.
const CTM_PREFIX_SIZE: usize = 12;

/// The largest gap tolerated between where a message's parser stopped and where
/// its header says the message ends.
///
/// TDWR pads each Type 31 body out to an eight-byte boundary, so its trailing
/// slack is 4-7 bytes; WSR-88D pads none. A declaration further out than this
/// disagrees with the parse by more than padding can explain and is not
/// trusted.
const MAX_TRAILING_PAD: usize = 7;

/// The byte position at which a message's header says the message ends.
///
/// Saturating throughout: a variable-length header can declare a size up to
/// `u32::MAX`, which overflows a 32-bit `usize`. A saturated target is past the
/// end of any input, which `try_skip_to` rejects.
fn declared_end(offset: usize, header: &MessageHeader) -> usize {
    offset
        .saturating_add(CTM_PREFIX_SIZE)
        .saturating_add(header.message_size_bytes() as usize)
}

/// Decode a series of NEXRAD Level II messages from a reader.
///
/// Segmented messages (like Clutter Filter Map) are accumulated and returned
/// as a single logical message with multiple headers. Segments are assumed to
/// appear consecutively in the input.
pub fn decode_messages<'a>(input: &'a [u8]) -> Result<Vec<Message<'a>>> {
    trace!("Decoding messages");

    let mut reader = SliceReader::new(input);
    let mut messages = Vec::new();
    let mut accumulator = SegmentAccumulator::new();

    while reader.remaining().len() >= size_of::<MessageHeader>() {
        let offset = reader.position();

        let header = match reader.take_ref::<MessageHeader>() {
            Ok(h) => h,
            Err(_) => break,
        };

        // Variable-length messages
        if !header.segmented() {
            match decode_variable_length_message(&mut reader, header, offset) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    if matches!(e, Error::UnexpectedEof) {
                        break;
                    }
                    warn!(
                        "Failed to parse variable-length message (type {:?}) at offset {}: {:?}",
                        header.message_type(),
                        offset,
                        e
                    );
                    if !reader.try_skip_to(declared_end(offset, header)) {
                        break;
                    }
                }
            }
            continue;
        }

        // Fixed-segment messages
        let segment_num = header.segment_number().unwrap_or(1);
        let segment_count = header.segment_count().unwrap_or(1);

        // Multi-segment messages: extract only the declared payload size so that
        // SegmentedSliceReader's auto-skip logic correctly detects segment boundaries.
        // Single-segment messages: use the full frame content area as payload so
        // parsers can read structs that extend beyond the declared message_size.
        //
        // Note: this payload is CTM_PREFIX_SIZE bytes short. message_size_bytes()
        // already excludes the prefix, so subtracting the whole header excludes it
        // twice. The reader position self-corrects — the frame is padded to
        // SEGMENT_FRAME_SIZE regardless — but the tail of a multi-segment payload
        // (e.g. a Message 15 clutter filter map) is truncated. Left alone here
        // because correcting it changes what WSR-88D messages decode to, which
        // wants fixture coverage this crate does not yet have.
        let payload = if segment_count > 1 {
            let payload_size =
                (header.message_size_bytes() as usize).saturating_sub(size_of::<MessageHeader>());
            let payload = match reader.take_bytes(payload_size) {
                Ok(p) => p,
                Err(_) => break,
            };
            let consumed = size_of::<MessageHeader>() + payload_size;
            if consumed < SEGMENT_FRAME_SIZE {
                reader.advance(SEGMENT_FRAME_SIZE - consumed);
            }
            payload
        } else {
            let content_size = SEGMENT_FRAME_SIZE - size_of::<MessageHeader>();
            match reader.take_bytes(content_size) {
                Ok(p) => p,
                Err(_) => break,
            }
        };

        trace!(
            "Fixed-segment message: type={:?}, segment={}/{}",
            header.message_type(),
            segment_num,
            segment_count
        );

        // For continuation segments of a multi-segment message, verify the
        // accumulator is active. For first segments (or single-segment messages),
        // start a fresh accumulation.
        if segment_count > 1 && segment_num != 1 {
            if !accumulator.is_active() {
                warn!(
                    "Segment {} arrived without preceding segment 1, skipping",
                    segment_num
                );
                continue;
            }
        } else {
            if accumulator.is_active() {
                warn!(
                    "Discarding incomplete segmented message: got {}/{} segments",
                    accumulator.headers.len(),
                    accumulator.expected_count
                );
            }
            accumulator.start(offset, segment_count);
        }

        if accumulator.push(header, payload) {
            match build_fixed_segment_message(
                &accumulator.headers,
                &accumulator.payloads,
                accumulator.first_offset,
            ) {
                Ok(msg) => {
                    messages.push(msg);
                }
                Err(e) => {
                    warn!(
                        "Failed to parse fixed-segment message (type {:?}) at offset {}: {:?}",
                        accumulator.headers[0].message_type(),
                        accumulator.first_offset,
                        e
                    );
                }
            }
            accumulator.clear();
        }
    }

    if accumulator.is_active() {
        warn!(
            "Incomplete segmented message: got {}/{} segments",
            accumulator.headers.len(),
            accumulator.expected_count
        );
    }

    let bytes_remaining = input.len().saturating_sub(reader.position());
    if bytes_remaining > 0 {
        warn!(
            "Bytes remaining after decoding all messages: {}",
            bytes_remaining
        );
    }

    Ok(messages)
}

/// Build a `Message` from one or more fixed-segment payloads.
///
/// Constructs a `SegmentedSliceReader` over the payloads and dispatches to
/// `decode_fixed_segment_contents`.
fn build_fixed_segment_message<'a>(
    headers: &[&'a MessageHeader],
    payloads: &[&'a [u8]],
    offset: usize,
) -> Result<Message<'a>> {
    let message_type = headers[0].message_type();

    let mut segmented_reader = SegmentedSliceReader::new(payloads);
    let contents = message::decode_fixed_segment_contents(&mut segmented_reader, message_type)?;

    let total_size = headers.len() * SEGMENT_FRAME_SIZE;

    let message_headers = if headers.len() == 1 {
        MessageHeaders::Single(headers[0])
    } else {
        MessageHeaders::Multiple(headers.to_vec())
    };

    Ok(Message::new(message_headers, contents, offset, total_size))
}

/// Parse a variable-length message (e.g. Type 31 Digital Radar Data).
///
/// Leaves the reader at the end of the message, which is the end its header
/// declares whenever that is believable — see the arms below. The returned
/// message's size is measured from wherever the reader ends up, so it includes
/// any trailing pad the message carries.
fn decode_variable_length_message<'a>(
    reader: &mut SliceReader<'a>,
    header: &'a MessageHeader,
    offset: usize,
) -> Result<Message<'a>> {
    let declared = declared_end(offset, header);

    let contents = if header.message_type() == MessageType::RDADigitalRadarDataGenericFormat {
        let radar_data_message = digital_radar_data::Message::parse(reader)?;

        // The data block walk stops at the last block it was pointed at, which
        // is the end of the message only if nothing follows it. WSR-88D pads
        // nothing, so the walk and the declared end agree. TDWR pads each body
        // out to an eight-byte boundary, and without skipping that slack the
        // next header is read out of the padding.
        //
        // The declaration is trusted only within a pad's width of the walk. Any
        // further out and it disagrees with the parse by more than padding can
        // explain, and the parse is the better answer — a corrupt segment_size
        // then cannot desync a record whose messages are parsing cleanly.
        let walk = reader.position();
        if (walk..=walk.saturating_add(MAX_TRAILING_PAD)).contains(&declared) {
            // A last radial whose padding was truncated is still a whole
            // radial, so clamp to the input rather than refusing the skip.
            let end_of_input = walk.saturating_add(reader.remaining().len());
            reader.try_skip_to(declared.min(end_of_input));
        } else {
            warn!(
                "Message type 31 at offset {} declares it ends at {} but parsed to {}, \
                 framing from the parse",
                offset, declared, walk
            );
        }

        MessageContents::DigitalRadarData(Box::new(radar_data_message))
    } else {
        // Unknown variable-length message type — nothing walked its payload, so
        // its declared end is the only framing available.
        if !reader.try_skip_to(declared) {
            return Err(Error::UnexpectedEof);
        }
        MessageContents::Other
    };

    let size = reader.position() - offset;
    Ok(Message::new(
        MessageHeaders::Single(header),
        contents,
        offset,
        size,
    ))
}

/// Accumulates segments for an in-progress multi-segment message.
struct SegmentAccumulator<'a> {
    headers: Vec<&'a MessageHeader>,
    payloads: Vec<&'a [u8]>,
    first_offset: usize,
    expected_count: u16,
}

impl<'a> SegmentAccumulator<'a> {
    fn new() -> Self {
        Self {
            headers: Vec::new(),
            payloads: Vec::new(),
            first_offset: 0,
            expected_count: 0,
        }
    }

    /// Start accumulating a new message, clearing any previous state.
    fn start(&mut self, offset: usize, expected_count: u16) {
        self.headers.clear();
        self.payloads.clear();
        self.first_offset = offset;
        self.expected_count = expected_count;
    }

    /// Add a segment. Returns `true` if the message is now complete.
    fn push(&mut self, header: &'a MessageHeader, payload: &'a [u8]) -> bool {
        self.headers.push(header);
        self.payloads.push(payload);
        self.headers.len() as u16 >= self.expected_count
    }

    /// Whether there is an in-progress accumulation.
    fn is_active(&self) -> bool {
        !self.headers.is_empty()
    }

    /// Clear accumulated state.
    fn clear(&mut self) {
        self.headers.clear();
        self.payloads.clear();
    }
}
