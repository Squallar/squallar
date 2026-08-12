use crate::messages::clutter_filter_map::elevation_segment::ElevationSegment;
use crate::messages::clutter_filter_map::raw::Header;
use crate::result::{Error, Result};
use crate::segmented_slice_reader::SegmentedSliceReader;
use crate::util::get_datetime;
use chrono::{DateTime, Duration, Utc};
use std::borrow::Cow;
use std::fmt::Debug;

/// A clutter filter map describing elevations, azimuths, and ranges containing clutter to
/// filtered from radar products. The RDA transmits this any time the map changes.
///
/// This message's contents correspond to ICD 2620002AA section 3.2.4.15 Table XIV.
/// The message starts with a brief header followed by a loop of elevation, azimuth,
/// and finally range/gate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message<'a> {
    /// Decoded header information for this clutter filter map.
    header: Cow<'a, Header>,

    /// The elevation segments defined in this clutter filter map.
    elevation_segments: Vec<ElevationSegment<'a>>,
}

impl<'a> Message<'a> {
    /// Parse a clutter filter map message from segmented input.
    ///
    /// Clutter filter maps span multiple fixed-length segments. The data is read
    /// across all segment payloads using the SegmentedSliceReader.
    pub(crate) fn parse(reader: &mut SegmentedSliceReader<'a, '_>) -> Result<Self> {
        let header = reader.take_ref::<Header>()?;

        // The count is declared as a `u16` and `ElevationSegment` numbers
        // itself with a `u8`, so the two have to be reconciled somewhere. `as
        // u8` reconciled them by truncating: a header declaring 256 segments
        // became zero, the loop below never ran, and the caller got `Ok` on an
        // empty map whose own `elevation_segment_count()` still said 256.
        //
        // Refused rather than saturated. The ICD allows 1 to 5 elevation
        // segments (2620002AA Table XIV), so a declaration past 255 is not a
        // map this decoder can act on at all, and clamping it to 255 would be
        // acting on a number the file never stated.
        let declared_count = header.elevation_segment_count.get();
        let segment_count = u8::try_from(declared_count).map_err(|_| {
            Error::Decoding(format!(
                "clutter filter map declares {declared_count} elevation segments; \
                 a segment number is a u8 and the ICD allows 1 to 5"
            ))
        })?;

        let mut message = Message {
            header: Cow::Borrowed(header),
            elevation_segments: Vec::with_capacity(segment_count as usize),
        };

        for segment_number in 0..segment_count {
            let segment = ElevationSegment::parse(reader, segment_number)?;
            message.elevation_segments.push(segment);
        }

        Ok(message)
    }

    /// The date the clutter filter map was generated represented as a count of days since 1 January
    /// 1970 00:00 GMT. It is also referred-to as a "modified Julian date" where it is the Julian
    /// date - 2440586.5.
    pub fn map_generation_date(&self) -> u16 {
        self.header.map_generation_date.get()
    }

    /// The time the clutter filter map was generated in minutes past midnight, GMT.
    pub fn map_generation_time(&self) -> u16 {
        self.header.map_generation_time.get()
    }

    /// The number of elevation segments defined in this clutter filter map. There may be 1 to 5,
    /// though there are typically 2. They will follow this header in order of increasing elevation.
    pub fn elevation_segment_count(&self) -> u16 {
        self.header.elevation_segment_count.get()
    }

    /// The date and time the clutter filter map was generated.
    pub fn date_time(&self) -> Option<DateTime<Utc>> {
        get_datetime(
            self.header.map_generation_date.get(),
            Duration::minutes(self.header.map_generation_time.get() as i64),
        )
    }

    /// The elevation segments defined in this clutter filter map.
    pub fn elevation_segments(&self) -> &[ElevationSegment<'a>] {
        &self.elevation_segments
    }

    /// Convert this message to an owned version with `'static` lifetime.
    pub fn into_owned(self) -> Message<'static> {
        Message {
            header: Cow::Owned(self.header.into_owned()),
            elevation_segments: self
                .elevation_segments
                .into_iter()
                .map(|s| s.into_owned())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clutter filter map header, followed by one azimuth segment of one
    /// range zone — enough for the first elevation segment to have something
    /// to read.
    fn map_bytes(elevation_segment_count: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&20_000u16.to_be_bytes()); // generation date
        bytes.extend_from_slice(&720u16.to_be_bytes()); // generation time, minutes
        bytes.extend_from_slice(&elevation_segment_count.to_be_bytes());

        bytes.extend_from_slice(&1u16.to_be_bytes()); // one range zone
        bytes.extend_from_slice(&2u16.to_be_bytes()); // op code: bypass filter
        bytes.extend_from_slice(&511u16.to_be_bytes()); // end range, km
        bytes
    }

    /// A count wider than a segment number is refused, not truncated.
    ///
    /// It used to reach `ElevationSegment` through `as u8`, so a header
    /// declaring 256 segments decoded **zero** of them and still returned
    /// `Ok`: an empty clutter filter map whose own `elevation_segment_count()`
    /// went on reporting 256, with nothing anywhere noting that the two
    /// disagreed.
    #[test]
    fn an_elevation_segment_count_wider_than_a_segment_number_is_refused() {
        for declared in [256u16, 257, 512, u16::MAX] {
            let bytes = map_bytes(declared);
            let payloads = [bytes.as_slice()];
            let mut reader = SegmentedSliceReader::new(&payloads);

            let result = Message::parse(&mut reader);

            assert!(
                matches!(result, Err(Error::Decoding(_))),
                "a declared count of {declared} should be refused, got {result:?}"
            );
        }
    }

    /// And every count a segment number can hold still parses, up to and
    /// including the last one — so the guard refuses only what it has to.
    #[test]
    fn an_elevation_segment_count_a_segment_number_can_hold_is_parsed() {
        for declared in [0u16, 1, 2, 5, 255] {
            let bytes = map_bytes(declared);
            let payloads = [bytes.as_slice()];
            let mut reader = SegmentedSliceReader::new(&payloads);

            let message = Message::parse(&mut reader).expect("parses");

            assert_eq!(
                message.elevation_segments().len(),
                declared as usize,
                "a declared count of {declared}"
            );
            assert_eq!(message.elevation_segment_count(), declared);
        }
    }
}
