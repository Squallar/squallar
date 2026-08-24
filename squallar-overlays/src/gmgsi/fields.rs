//! The GMGSI channels projected into the substrate's read contract:
//! [`squallar_source::product::ProductSpec`].
//!
//! # The ramps are stated in counts, 0 to 255
//!
//! This is the one consequential decision in this source, and it is a decision
//! *against* the granule's own `units` attribute.
//!
//! `data:units` says `"K"`. It is wrong. `data:long_name` says
//! `"0-255 Brightness Temperature"`, and that is what the bytes are: over all
//! 15,000,000 samples of the reference LW granule, every value is an integer in
//! `0..=255`, none fractional, none outside. The measured equator readings —
//! `(row 1499, column 2500)`, `lat 0.0000`, `lon 0.0220`, 2025-06-01 12:00 UTC —
//! are LW **82**, SW **65**, VIS **118**, WV **166**. No terrestrial brightness
//! temperature is 82 K.
//!
//! A ramp whose first stop were a plausible Kelvin floor (180 K) would put
//! every one of those four readings *below* its first stop, and
//! [`crate::render::gridded::color_for`] paints nothing below the first stop —
//! so the layer would render **entirely blank**, on every channel, with no
//! error anywhere. `every_channel_paints_at_its_measured_equator_reading` and
//! its Kelvin floor are what stand between this source and that.
//!
//! # Ascending greyscale, not negated
//!
//! Higher count is colder. The ramps therefore run dark at 0 to white at 255,
//! which paints cold cloud top bright — the convention every IR satellite
//! product uses. The scale is **not** negated to "correct" for the inversion;
//! doing so would render warm ground white and cloud black.

use std::sync::LazyLock;

use squallar_source::product::{FieldId, LegendScale, ProductSpec};
use squallar_units::Quantity;

use super::GmgsiChannel;

/// The group label every GMGSI channel files under.
pub const GROUP: &str = "GMGSI channels";

/// The count domain, both ends. Not a choice: it is the width of the byte the
/// product quantises to, and `long_name` states it.
pub const MIN_COUNT: f32 = 0.0;
pub const MAX_COUNT: f32 = 255.0;

/// What the colour bar and the hover tooltip print after the number. One
/// spelling, read by [`Quantity::Unitless`] below and by the handler, so the
/// legend and the tooltip cannot disagree about what the values are.
pub const UNIT_LABEL: &str = "count";

/// Ascending greyscale over the full count domain.
///
/// `is_gradient: true`: this is an image, not a set of bands, and the stops are
/// waypoints on a continuous ramp rather than thresholds a reader takes a value
/// off. The first stop sits at [`MIN_COUNT`] exactly, so every count the
/// product can carry paints and only a NaN — a `_FillValue` the CF layer marked
/// missing — comes out transparent.
fn greyscale(stops: [[u8; 3]; 6]) -> LegendScale {
    let thresholds = stops
        .into_iter()
        .enumerate()
        .map(|(i, c)| (MIN_COUNT + (MAX_COUNT - MIN_COUNT) * i as f32 / 5.0, c))
        .collect::<Vec<_>>();
    LegendScale {
        thresholds,
        is_gradient: true,
        min_value: MIN_COUNT,
        max_value: MAX_COUNT,
    }
}

/// Neutral grey, cold-bright. Shared by the two infrared window channels.
static INFRARED: LazyLock<LegendScale> = LazyLock::new(|| {
    greyscale([
        [0x0a, 0x0a, 0x0a],
        [0x3c, 0x3c, 0x3c],
        [0x6e, 0x6e, 0x6e],
        [0x9c, 0x9c, 0x9c],
        [0xcd, 0xcd, 0xcd],
        [0xff, 0xff, 0xff],
    ])
});

/// Shortwave IR, warmed a touch so a pane showing both IR channels is not two
/// identical greys.
static SHORTWAVE: LazyLock<LegendScale> = LazyLock::new(|| {
    greyscale([
        [0x10, 0x0c, 0x08],
        [0x42, 0x3a, 0x30],
        [0x74, 0x6a, 0x5c],
        [0xa2, 0x99, 0x8b],
        [0xd0, 0xca, 0xc0],
        [0xff, 0xff, 0xf8],
    ])
});

/// Visible: plain neutral grey, which is what the channel measures.
static VISIBLE: LazyLock<LegendScale> = LazyLock::new(|| {
    greyscale([
        [0x00, 0x00, 0x00],
        [0x33, 0x33, 0x33],
        [0x66, 0x66, 0x66],
        [0x99, 0x99, 0x99],
        [0xcc, 0xcc, 0xcc],
        [0xff, 0xff, 0xff],
    ])
});

/// Water vapour, blued: the channel is read for moisture structure and a cool
/// cast separates it from the two IR windows at a glance.
static WATER_VAPOR: LazyLock<LegendScale> = LazyLock::new(|| {
    greyscale([
        [0x06, 0x0c, 0x14],
        [0x1e, 0x38, 0x50],
        [0x3a, 0x66, 0x8c],
        [0x74, 0x9c, 0xba],
        [0xb6, 0xd0, 0xe2],
        [0xff, 0xff, 0xff],
    ])
});

pub fn scale(channel: GmgsiChannel) -> &'static LegendScale {
    match channel {
        GmgsiChannel::LongwaveIr => &INFRARED,
        GmgsiChannel::ShortwaveIr => &SHORTWAVE,
        GmgsiChannel::Visible => &VISIBLE,
        GmgsiChannel::WaterVapor => &WATER_VAPOR,
    }
}

/// Every GMGSI channel's registration, projected once.
static FIELDS: LazyLock<Vec<ProductSpec>> = LazyLock::new(|| {
    GmgsiChannel::all()
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let scale = scale(c);
            ProductSpec {
                id: FieldId::from_static(c.as_str()),
                name: c.display_name(),
                code: c.as_str(),
                sort_order: u8::try_from(i).expect("GmgsiChannel::all() fits in a u8"),
                group: GROUP,
                // `Unitless`: a 0-255 count is not a convertible quantity, and
                // the label says so rather than repeating the granule's wrong
                // `units = "K"`.
                quantity: Quantity::Unitless { label: UNIT_LABEL },
                scale,
                value_domain: (scale.min_value, scale.max_value),
                domain_label_ends: ("0", "255"),
                // A mosaic of cloud-top imagery is a surface field: no vertical
                // extent, no tilt, so it reaches neither the isosurface slider
                // nor the 3D stack.
                vertical: false,
                tilted: false,
            }
        })
        .collect()
});

/// Every GMGSI channel, in [`GmgsiChannel::all`]'s order.
pub fn products() -> &'static [ProductSpec] {
    &FIELDS
}

/// The registration for `channel`.
pub fn spec(channel: GmgsiChannel) -> &'static ProductSpec {
    let code = channel.as_str();
    FIELDS
        .iter()
        .find(|f| f.code == code)
        .expect("every channel is registered")
}

#[cfg(test)]
mod tests;
