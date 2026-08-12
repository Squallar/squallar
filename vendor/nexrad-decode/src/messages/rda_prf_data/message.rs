use crate::messages::primitive_aliases::{Code2, Integer4};
use crate::messages::rda_prf_data::raw::Header;
use crate::messages::rda_prf_data::waveform_prf_data::WaveformPrfData;
use crate::result::Result;
use crate::segmented_slice_reader::SegmentedSliceReader;
use std::borrow::Cow;
use std::fmt::Debug;

/// An RDA PRF data message (type 32) containing pulse repetition frequency data for each waveform
/// type used by the radar.
///
/// This message's contents correspond to ICD 2620002AA section 3.2.4.32 Table XVIII.
/// The message starts with a header indicating the number of waveform sections, followed by
/// a variable-length series of waveform type and PRF value entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message<'a> {
    /// Decoded header information for this PRF data message.
    header: Cow<'a, Header>,

    /// PRF data for each waveform type included in this message.
    waveform_prf_data: Vec<WaveformPrfData>,
}

impl<'a> Message<'a> {
    /// Parse an RDA PRF data message from segmented input.
    pub(crate) fn parse(reader: &mut SegmentedSliceReader<'a, '_>) -> Result<Self> {
        let header = reader.take_ref::<Header>()?;

        let waveform_count = header.number_of_waveforms.get() as usize;

        // Sized to what the input can still supply, not to what the header
        // claims. A waveform entry is variable-length, so `take_slice` — the
        // way the rest of this crate checks a count before acting on it —
        // cannot express one; the floor on an entry's cost can, and bounding
        // by that keeps a declared count from sizing an allocation on its own.
        let smallest_entry = 2 * size_of::<Code2>();
        let mut waveform_prf_data =
            Vec::with_capacity(waveform_count.min(reader.remaining_total() / smallest_entry));

        for _ in 0..waveform_count {
            let waveform_type = reader.take_ref::<Code2>()?.get();
            let prf_count = reader.take_ref::<Code2>()?.get() as usize;

            // Taken as a slice, which fails on EOF, and only then collected —
            // the shape `volume_coverage_pattern`'s elevation cuts and the
            // clutter filter map's range zones already use.
            //
            // Narrower than the `take_ref` loop it replaces, not just
            // differently placed: `take_slice` wants the whole run inside one
            // segment, so a run that fits across the remaining segments but
            // not in the next one alone is now `UnexpectedEof` where the loop
            // would have read it in pieces. No Message 32 in any volume
            // examined here is multi-segment.
            let prf_values = reader
                .take_slice::<Integer4>(prf_count)?
                .iter()
                .map(|value| value.get())
                .collect();

            waveform_prf_data.push(WaveformPrfData::new(waveform_type, prf_values));
        }

        Ok(Message {
            header: Cow::Borrowed(header),
            waveform_prf_data,
        })
    }

    /// The number of waveform types included in this message (1-5).
    pub fn number_of_waveforms(&self) -> u16 {
        self.header.number_of_waveforms.get()
    }

    /// The PRF data for each waveform type included in this message.
    pub fn waveform_prf_data(&self) -> &[WaveformPrfData] {
        &self.waveform_prf_data
    }

    /// Convert this message to an owned version with `'static` lifetime.
    pub fn into_owned(self) -> Message<'static> {
        Message {
            header: Cow::Owned(self.header.into_owned()),
            waveform_prf_data: self.waveform_prf_data,
        }
    }
}
