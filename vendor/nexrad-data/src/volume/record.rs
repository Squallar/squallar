use std::fmt::Debug;

/// The largest an LDM record may decompress to before [`Record::decompress`]
/// treats it as malformed rather than as data.
///
/// A bzip2 stream does not declare its decompressed size, so decompressing one
/// is unbounded by construction: the only way to learn how big a record is, is
/// to finish expanding it. Every record this crate decompresses arrived over a
/// network, and expansion ratios in real NEXRAD data reach 1,363:1 — a record
/// of mostly-empty TDWR gates — so "the compressed record is small" implies
/// nothing about the memory the decompression will take. Without a ceiling a
/// corrupt or hostile record is an out-of-memory abort, and on the parallel
/// path it is one per worker at once.
///
/// **16 MiB**, and it rests on measured headroom over real data. There is no
/// structural ceiling to appeal to — see below — so this is an empirical bound
/// and is documented as one.
///
/// *Real records.* Measured over 10,063 compressed records in 176 volumes,
/// WSR-88D and TDWR, the two formats with different record structures. Zero
/// rejections:
///
/// | | largest record | ceiling is |
/// | --- | --- | --- |
/// | any site | 1,424,736 B | **11.8×** larger |
/// | TDWR | 325,888 B | 51× larger |
///
/// *Why there is no structural leg.* An earlier version of this comment
/// claimed one, and it was wrong; it is written down so nobody reconstructs
/// it. The reasoning was that Archive II packs at most 120 messages per
/// record, so 120 × 131,082 (the largest a `u16` halfword count plus the
/// 12-byte CTM prefix can express) = 15,729,840 < 16,777,216 would put the
/// ceiling above anything the format can say.
///
/// Both halves fail. "120" is the radial count, not the message count —
/// decoding every record in the corpus gives **78–127** messages per record on
/// WSR-88D and **120–134** on TDWR, because the metadata messages sit in the
/// same records. At 134 messages the same arithmetic gives 17,564,988, which
/// is *above* the ceiling. And 131,082 is unreachable in practice anyway: the
/// largest real message measured is **12,160 bytes** on WSR-88D and 2,432 on
/// TDWR, so a 134-message record of real messages is about 1.6 MB.
///
/// So the format does not bound this and the ceiling does not pretend it does.
/// What it has is an order of magnitude of headroom over every record ever
/// measured, which is what
/// `decompress_bound_tests::the_ceiling_keeps_real_headroom_over_the_largest_record_measured`
/// pins.
///
/// *Worth having.* 16 MiB caps a decompression bomb at 16 MiB per worker
/// instead of at the machine's memory. At 32 threads that is a 512 MB worst
/// case that ends in an `Err`, against an abort.
///
/// Exceeding it is [`Error::RecordTooLarge`](crate::result::Error::RecordTooLarge),
/// which is an error and not a panic: one bad record in a volume of 50-130 is
/// a decode that reports a bad record, not a process that dies.
pub const MAX_DECOMPRESSED_RECORD_BYTES: usize = 16 * 1024 * 1024;

/// The capacity [`Record::decompress`] starts its output buffer at.
///
/// `Vec::new()` starts at nothing and `read_to_end` doubles from there, so a
/// 1.4 MB record was reached through about fifteen reallocations, each copying
/// everything accumulated so far. Across one WSR-88D volume that is 237 MB of
/// `memcpy` to produce 75 MB of output.
///
/// A bzip2 stream carries no decompressed-size hint and the four-byte LDM
/// prefix is the *compressed* length, so there is nothing exact to size from —
/// and nothing approximate either, since real expansion ratios inside a single
/// volume run from 2.2:1 to 1,363:1. What is left is a fixed starting capacity
/// chosen from the distribution of real records. Measured over the same 693
/// records as [`MAX_DECOMPRESSED_RECORD_BYTES`]:
///
/// | | smallest | p25 | median | p75 | largest |
/// | --- | --- | --- | --- | --- | --- |
/// | all 693 | 89,760 | 191,520 | 245,280 | 737,760 | 1,416,480 |
///
/// 256 KiB is the median record rounded up to a power of two: half the corpus
/// is decompressed in a single allocation with no growth at all, and the half
/// that grows starts five doublings up.
///
/// It is also the measured optimum rather than the reasoned one. Three
/// candidates, same corpus, same method — amplification is
/// `(allocated + reallocated) / output`, RSS is a 32-thread parallel decode:
///
/// | capacity | KFTG amplification | TDWR amplification | peak RSS @32 |
/// | --- | --- | --- | --- |
/// | `Vec::new()` | 7.57× | 23.73× | 246.0 MB |
/// | 128 KiB | 7.41× | 22.97× | 233.1 MB |
/// | **256 KiB** | **7.26×** | **22.77×** | **230.9 MB** |
/// | 512 KiB | 6.94× | 24.23× | 244.2 MB |
///
/// 512 KiB is the instructive one. It keeps improving the WSR-88D volumes,
/// whose records are large, and simultaneously pushes TDWR *worse than doing
/// nothing at all* — every one of the 364 TDWR records in the corpus is under
/// 326 KB, so a 512 KiB floor over-allocates all of them — and gives the whole
/// RSS saving back. Tuning this on WSR-88D alone would have picked it.
const INITIAL_DECOMPRESSED_CAPACITY: usize = 256 * 1024;

#[derive(Clone, PartialEq, Eq, Hash)]
enum RecordData<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl Debug for RecordData<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordData::Borrowed(data) => write!(f, "RecordData::Borrowed({} bytes)", data.len()),
            RecordData::Owned(data) => write!(f, "RecordData::Owned({} bytes)", data.len()),
        }
    }
}

/// Represents a single LDM record with its data which may be compressed.
///
/// The Unidata Local Data Manager (LDM) is a data distribution system used by the NWS to distribute
/// NEXRAD archival radar data. A NEXRAD "Archive II" file starts with an
/// [crate::volume::Header] followed by a series of compressed LDM records, each
/// containing messages with radar data.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Record<'a>(RecordData<'a>);

impl<'a> Record<'a> {
    /// Creates a new LDM record with the provided data.
    pub fn new(data: Vec<u8>) -> Self {
        Record(RecordData::Owned(data))
    }

    /// Creates a new LDM record with the provided data slice.
    pub fn from_slice(data: &'a [u8]) -> Self {
        Record(RecordData::Borrowed(data))
    }

    /// The data contained in this LDM record.
    pub fn data(&self) -> &[u8] {
        match &self.0 {
            RecordData::Borrowed(data) => data,
            RecordData::Owned(data) => data,
        }
    }

    /// Whether this LDM record's data is compressed.
    pub fn compressed(&self) -> bool {
        self.data().len() >= 6 && self.data()[4..6].as_ref() == b"BZ"
    }

    /// Decompresses this LDM record's data.
    ///
    /// Records larger than [`MAX_DECOMPRESSED_RECORD_BYTES`] are rejected with
    /// [`Error::RecordTooLarge`](crate::result::Error::RecordTooLarge) rather
    /// than decompressed.
    pub fn decompress<'b>(&self) -> crate::result::Result<Record<'b>> {
        use crate::result::Error;
        use bzip2::read::BzDecoder;
        use std::io::Read;

        if !self.compressed() {
            return Err(Error::UncompressedData);
        }

        // Skip the four-byte record size prefix
        let data = self.data().split_at(4).1;

        // Read one byte past the ceiling, so that a record which is exactly at
        // it still decompresses and anything beyond it is detected without
        // being decompressed any further. `Take` bounds only how much is read
        // per iteration and how much is kept; it does not reserve its limit, so
        // this costs nothing for an ordinary record.
        let mut decompressed_data = Vec::with_capacity(INITIAL_DECOMPRESSED_CAPACITY);
        BzDecoder::new(data)
            .take(MAX_DECOMPRESSED_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut decompressed_data)?;

        if decompressed_data.len() > MAX_DECOMPRESSED_RECORD_BYTES {
            return Err(Error::RecordTooLarge {
                limit: MAX_DECOMPRESSED_RECORD_BYTES,
                compressed: data.len(),
            });
        }

        Ok(Record::new(decompressed_data))
    }

    /// Decodes the NEXRAD level II messages contained in this LDM record.
    pub fn messages(&self) -> crate::result::Result<Vec<nexrad_decode::messages::Message<'_>>> {
        use crate::result::Error;
        use nexrad_decode::messages::decode_messages;

        if self.compressed() {
            return Err(Error::CompressedData);
        }

        Ok(decode_messages(self.data())?)
    }

    /// Decodes the radar radials contained in this LDM record.
    ///
    /// This extracts all digital radar data messages (both modern type 31 and legacy type 1)
    /// from the record and converts them into [`Radial`](nexrad_model::data::Radial) objects.
    /// Non-radial messages are skipped.
    ///
    /// The record must be decompressed before calling this method.
    #[cfg(feature = "nexrad-model")]
    pub fn radials(&self) -> crate::result::Result<Vec<nexrad_model::data::Radial>> {
        use nexrad_decode::messages::MessageContents;

        let mut radials = Vec::new();
        for message in self.messages()? {
            match message.into_contents() {
                MessageContents::DigitalRadarData(m) => radials.push(m.into_radial()?),
                MessageContents::DigitalRadarDataLegacy(m) => radials.push(m.into_radial()?),
                _ => {}
            }
        }
        Ok(radials)
    }
}

impl Debug for Record<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Record");
        debug.field("data.len()", &self.data().len());
        debug.field(
            "borrowed",
            match &self.0 {
                RecordData::Borrowed(_) => &true,
                RecordData::Owned(_) => &false,
            },
        );
        debug.field("compressed", &self.compressed());
        debug.field(
            "messages.len()",
            &self.messages().map(|messages| messages.len()),
        );

        debug.finish()
    }
}

/// Splits record data into individual records.
///
/// Supports two archive formats:
/// - **Modern (LDM)**: Data is a series of size-prefixed, bzip2-compressed records.
///   Each record has a 4-byte big-endian size prefix followed by the compressed data.
/// - **Legacy (CTM)**: Data is a series of uncompressed 2432-byte CTM frames. Each
///   frame's first 12 bytes are the `rpg_unknown` field of the `MessageHeader`, so
///   the frames can be passed directly to the message decoder without stripping.
///   This format is used by older archive files (pre-~2016) and files with tape
///   filename version 01-04.
///
/// The format is auto-detected by checking whether the first 4 bytes form a valid
/// (non-zero) record size.
pub fn split_compressed_records(data: &[u8]) -> crate::result::Result<Vec<Record<'_>>> {
    if data.len() < 4 {
        // Not enough data for either format detection or a valid record.
        // Delegate to split_ldm_records which returns Ok(empty) for truly empty
        // data, or TruncatedRecord for 1-3 byte inputs.
        return split_ldm_records(data);
    }

    // Detect legacy CTM format: first 4 bytes are all zeros (no valid LDM size prefix).
    // In CTM frames, the first 12 bytes are the rpg_unknown field (zeros), whereas
    // in LDM records the first 4 bytes are a non-zero record size.
    let first_four = [data[0], data[1], data[2], data[3]];
    if first_four == [0, 0, 0, 0] {
        return split_ctm_frames(data);
    }

    split_ldm_records(data)
}

/// Splits modern LDM (Local Data Manager) size-prefixed records.
fn split_ldm_records(data: &[u8]) -> crate::result::Result<Vec<Record<'_>>> {
    use crate::result::Error;

    let mut records = Vec::new();

    let mut position = 0;
    loop {
        if position >= data.len() {
            break;
        }

        // Check bounds for reading record size
        if position + 4 > data.len() {
            return Err(Error::TruncatedRecord {
                expected: position + 4,
                actual: data.len(),
            });
        }

        let mut record_size_bytes = [0; 4];
        record_size_bytes.copy_from_slice(&data[position..position + 4]);
        let record_size = i32::from_be_bytes(record_size_bytes).unsigned_abs() as usize;

        // Validate record size is non-zero to prevent infinite loops
        if record_size == 0 {
            return Err(Error::InvalidRecordSize {
                size: record_size,
                offset: position,
            });
        }

        // Check bounds for full record
        let record_end = position + record_size + 4;
        if record_end > data.len() {
            return Err(Error::TruncatedRecord {
                expected: record_end,
                actual: data.len(),
            });
        }

        records.push(Record::from_slice(&data[position..record_end]));
        position = record_end;
    }

    Ok(records)
}

/// Returns legacy pre-LDM archive data as a single uncompressed record.
///
/// Legacy archive files (pre-~2016, tape filename versions 01-06) store messages
/// in a mixed format:
///
/// - **Overhead messages** (Types 2, 3, 5, 13, 15, 18, etc.) use fixed 2432-byte
///   frame-aligned segments, identical to modern LDM decompressed records.
/// - **Type 31 radial data** is contiguously packed at exactly `12 + seg_size * 2`
///   bytes per message, with NO frame alignment between radials.
///
/// The `decode_messages` function handles both modes natively: fixed-segment
/// messages consume exactly 2432 bytes per segment, and variable-length Type 31
/// messages consume their declared size. Empty padding frames (all zeros) between
/// message groups are harmlessly decoded as `MessageContents::Other`.
///
/// The data is NOT trimmed to 2432-byte boundaries because the contiguously-packed
/// Type 31 radials may extend past the last frame boundary.
fn split_ctm_frames(data: &[u8]) -> crate::result::Result<Vec<Record<'_>>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    Ok(vec![Record::from_slice(data)])
}

#[cfg(test)]
mod decompress_bound_tests {
    use super::*;
    use crate::result::Error;

    /// Build an LDM record the way a volume file carries one: a four-byte
    /// big-endian size prefix, then a bzip2 stream. `compressed()` looks for
    /// `BZ` at bytes 4..6, which is the start of the stream, so this is
    /// indistinguishable from a real record to everything downstream.
    fn ldm_record(payload: &[u8]) -> Vec<u8> {
        use bzip2::write::BzEncoder;
        use bzip2::Compression;
        use std::io::Write;

        let mut encoder = BzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(payload).expect("compress");
        let compressed = encoder.finish().expect("finish");

        let mut record = (compressed.len() as u32).to_be_bytes().to_vec();
        record.extend_from_slice(&compressed);
        record
    }

    #[test]
    fn a_record_at_the_ceiling_still_decompresses() {
        // Exactly at the limit, so the ceiling is inclusive and the `+ 1` on
        // the `take` is doing what it is there for. If this ever starts
        // failing, the bound has been made exclusive by accident and every
        // record of exactly this size in the wild would be rejected.
        let payload = vec![0u8; MAX_DECOMPRESSED_RECORD_BYTES];
        let record = ldm_record(&payload);
        let decompressed = Record::from_slice(&record)
            .decompress()
            .expect("a record at the ceiling is data, not an error");
        assert_eq!(decompressed.data().len(), MAX_DECOMPRESSED_RECORD_BYTES);
    }

    #[test]
    fn a_record_past_the_ceiling_is_an_error_and_not_a_panic() {
        // One byte over. The failure has to be a value the caller can handle:
        // a volume is 50-130 records and one bad one is a reported bad record,
        // not a dead process.
        let payload = vec![0u8; MAX_DECOMPRESSED_RECORD_BYTES + 1];
        let record = ldm_record(&payload);
        match Record::from_slice(&record).decompress() {
            Err(Error::RecordTooLarge { limit, compressed }) => {
                assert_eq!(limit, MAX_DECOMPRESSED_RECORD_BYTES);
                assert_eq!(compressed, record.len() - 4);
            }
            Err(other) => panic!("expected RecordTooLarge, got {other:?}"),
            Ok(r) => panic!(
                "expected RecordTooLarge, decompressed {} bytes",
                r.data().len()
            ),
        }
    }

    #[test]
    fn a_decompression_bomb_is_refused_without_being_expanded() {
        // 256 MiB of zeros compresses to a few hundred bytes: this is the
        // shape of the attack the ceiling exists for. The assertion that
        // matters is simply that this returns rather than allocating 256 MiB.
        let payload = vec![0u8; 256 * 1024 * 1024];
        let record = ldm_record(&payload);
        assert!(
            record.len() < 4096,
            "premise: the bomb is small on the wire, got {} bytes",
            record.len()
        );
        assert!(matches!(
            Record::from_slice(&record).decompress(),
            Err(Error::RecordTooLarge { .. })
        ));
    }

    #[test]
    fn an_ordinary_record_round_trips_unchanged() {
        // The other half of the bound: normal data must be untouched by it.
        // 2432 bytes is one CTM frame, the unit a real record is built from.
        let payload: Vec<u8> = (0..2432u32).map(|i| (i % 251) as u8).collect();
        let record = ldm_record(&payload);
        let decompressed = Record::from_slice(&record)
            .decompress()
            .expect("an ordinary record decompresses");
        assert_eq!(decompressed.data(), &payload[..]);
    }

    /// The largest decompressed LDM record measured anywhere: 10,063 compressed
    /// records across 176 volumes, WSR-88D and TDWR, with zero rejections.
    ///
    /// Raise this only by measuring, and raise the ceiling with it.
    const LARGEST_MEASURED_RECORD_BYTES: usize = 1_424_736;

    #[test]
    fn the_ceiling_keeps_real_headroom_over_the_largest_record_measured() {
        // This is the *whole* justification for the constant, so it is the
        // thing to assert. An earlier version of this test asserted instead
        // that the ceiling was above the largest record Archive II could
        // express, via "at most 120 messages of at most 131,082 bytes". That
        // was false twice over -- 120 is the radial count, not the message
        // count (78-127 messages per record measured on WSR-88D, 120-134 on
        // TDWR), and at 134 messages the same arithmetic exceeds the ceiling.
        // The test passed only because the 120 in it restated the bad premise.
        // There is no structural bound; there is an order of magnitude of
        // measured headroom, and that is what this pins.
        assert!(
            MAX_DECOMPRESSED_RECORD_BYTES >= 10 * LARGEST_MEASURED_RECORD_BYTES,
            "the ceiling ({MAX_DECOMPRESSED_RECORD_BYTES}) leaves less than 10x headroom \
             over the largest record ever measured ({LARGEST_MEASURED_RECORD_BYTES})"
        );
    }

    #[test]
    fn the_largest_measured_record_is_accepted() {
        // The other direction, and the one a user would feel: a record the size
        // of the biggest one in the archive is data, not an error.
        let payload = vec![0u8; LARGEST_MEASURED_RECORD_BYTES];
        let record = ldm_record(&payload);
        let decompressed = Record::from_slice(&record)
            .decompress()
            .expect("the largest measured record decompresses");
        assert_eq!(decompressed.data().len(), LARGEST_MEASURED_RECORD_BYTES);
    }
}
