//! Bytes of storage, exactly.
//!
//! Deliberately **not** a [`Quantity`](crate::Quantity) variant:
//! `Quantity::convert` and `Measured.value` are `f32`, which is exact on
//! integers only to 2²⁴ = 16,777,216 — a gigabyte figure routed through them
//! quantises to ~128-byte steps, so an "exact" size would become inexact by
//! construction at every size this type is for. And not a
//! [`UserPreferences`](crate::UserPreferences) entry: there is no imperial
//! megabyte, so there is no preference to hold and no float to answer for its
//! `Eq`.

use serde::{Deserialize, Serialize};

/// A count of bytes. Everything here is `u64` arithmetic — nothing touches a
/// float, which is what keeps a figure above 2²⁴ exact, label included.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DataSize(u64);

impl DataSize {
    pub const ZERO: DataSize = DataSize(0);

    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> u64 {
        self.0
    }

    /// Decimal MB/GB (10⁶ / 10⁹) — what `navigator.storage.estimate()` reports
    /// and what a phone's storage screen says. The denominator is named once in
    /// the header of whatever screen lists these, never re-derived per row.
    ///
    /// One decimal while it still informs — below 10 MB, and below 100 GB
    /// where a whole gigabyte is still >1% of the figure — whole numbers
    /// elsewhere: "3.9 MB", "47 MB", "310 MB", "1.8 GB", "125 GB".
    /// Round-half-up in integer tenths, so the label is a pure function of the
    /// exact byte count.
    pub fn label(self) -> String {
        let tenths_of_mb = (self.0 + 50_000) / 100_000;
        if tenths_of_mb < 100 {
            return format!("{}.{} MB", tenths_of_mb / 10, tenths_of_mb % 10);
        }
        let whole_mb = (self.0 + 500_000) / 1_000_000;
        if whole_mb < 1_000 {
            return format!("{whole_mb} MB");
        }
        let tenths_of_gb = (self.0 + 50_000_000) / 100_000_000;
        if tenths_of_gb < 1_000 {
            return format!("{}.{} GB", tenths_of_gb / 10, tenths_of_gb % 10);
        }
        format!("{} GB", (self.0 + 500_000_000) / 1_000_000_000)
    }
}

impl std::ops::Add for DataSize {
    type Output = DataSize;
    fn add(self, rhs: DataSize) -> DataSize {
        DataSize(self.0 + rhs.0)
    }
}

impl std::ops::AddAssign for DataSize {
    fn add_assign(&mut self, rhs: DataSize) {
        self.0 += rhs.0;
    }
}

impl std::iter::Sum for DataSize {
    fn sum<I: Iterator<Item = DataSize>>(iter: I) -> DataSize {
        iter.fold(DataSize::ZERO, std::ops::Add::add)
    }
}

impl<'a> std::iter::Sum<&'a DataSize> for DataSize {
    fn sum<I: Iterator<Item = &'a DataSize>>(iter: I) -> DataSize {
        iter.copied().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_read_as_the_design_vocabulary() {
        // The shapes the offline-area detail list is specified in.
        assert_eq!(DataSize::from_bytes(12_000_000).label(), "12 MB");
        assert_eq!(DataSize::from_bytes(47_000_000).label(), "47 MB");
        assert_eq!(DataSize::from_bytes(310_000_000).label(), "310 MB");
        assert_eq!(DataSize::from_bytes(1_800_000_000).label(), "1.8 GB");
        // Small values keep a decimal rather than rounding to a lie of "4 MB".
        assert_eq!(DataSize::from_bytes(3_865_522).label(), "3.9 MB");
        assert_eq!(DataSize::ZERO.label(), "0.0 MB");
        // Scale flips exactly where rounding would print "1000 MB".
        assert_eq!(DataSize::from_bytes(999_499_999).label(), "999 MB");
        assert_eq!(DataSize::from_bytes(999_500_000).label(), "1.0 GB");
        // And decimals stop where they stop informing.
        assert_eq!(DataSize::from_bytes(9_949_999).label(), "9.9 MB");
        assert_eq!(DataSize::from_bytes(9_950_000).label(), "10 MB");
        assert_eq!(DataSize::from_bytes(99_940_000_000).label(), "99.9 GB");
        assert_eq!(DataSize::from_bytes(125_000_000_000).label(), "125 GB");
    }

    /// The argument that kept this out of `Quantity`, pinned: above 2²⁴, `f32`
    /// cannot separate byte counts that this type must — including at the
    /// label's own rounding boundary.
    #[test]
    fn a_figure_above_two_to_the_24_stays_exact_where_f32_collapses() {
        let a = DataSize::from_bytes(16_777_216); // 2^24
        let b = DataSize::from_bytes(16_777_217);
        assert_ne!(a, b);
        assert_eq!(b.bytes(), 16_777_217);
        #[allow(clippy::cast_precision_loss, reason = "the loss is the subject")]
        {
            assert_eq!(
                16_777_216_u64 as f32, 16_777_217_u64 as f32,
                "if f32 ever separates these, the DataSize-over-Quantity \
                 argument needs re-litigating",
            );
        }

        // One byte apart at GB scale: the labels differ, and f32 — whose step
        // here is 128 bytes — maps both to the same value, so a float-routed
        // figure could not place this boundary at all.
        let below = DataSize::from_bytes(1_849_999_999);
        let at = DataSize::from_bytes(1_850_000_000);
        assert_eq!(below.label(), "1.8 GB");
        assert_eq!(at.label(), "1.9 GB");
        #[allow(clippy::cast_precision_loss, reason = "the loss is the subject")]
        {
            assert_eq!(1_849_999_999_u64 as f32, 1_850_000_000_u64 as f32);
        }
    }

    /// Segment sizes sum without loss — the whole reason the inner type is an
    /// integer. The chosen addends each lose their low bits in `f32`.
    #[test]
    fn summing_is_lossless() {
        let segments = [
            DataSize::from_bytes(16_777_215),
            DataSize::from_bytes(3),
            DataSize::from_bytes(1_000_000_007),
        ];
        let total: DataSize = segments.iter().sum();
        assert_eq!(total.bytes(), 1_016_777_225);
        assert_eq!(total, segments.into_iter().sum::<DataSize>());

        let mut running = DataSize::ZERO;
        running += DataSize::from_bytes(16_777_215);
        running += DataSize::from_bytes(3);
        assert_eq!(running, DataSize::from_bytes(16_777_218));
    }

    /// It has to work as a map key and sort by size — `Copy + Eq + Ord + Hash`
    /// exercised rather than assumed from the derive list.
    #[test]
    fn orders_hashes_and_serializes_as_a_bare_number() {
        let mut sizes = [
            DataSize::from_bytes(310_000_000),
            DataSize::from_bytes(12_000_000),
            DataSize::from_bytes(47_000_000),
        ];
        sizes.sort();
        assert_eq!(sizes[0].bytes(), 12_000_000);
        assert_eq!(sizes[2].bytes(), 310_000_000);

        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(DataSize::from_bytes(1)));
        assert!(!seen.insert(DataSize::from_bytes(1)));

        // `#[serde(transparent)]`: persisted as the number, not `{"0":n}`, so
        // the eventual config field reads as a size and not a tuple struct.
        let json = serde_json::to_string(&DataSize::from_bytes(3_865_522)).expect("serialize");
        assert_eq!(json, "3865522");
        let back: DataSize = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, DataSize::from_bytes(3_865_522));
    }
}
