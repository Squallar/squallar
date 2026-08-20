//! Digital radial data array types for Level III products.

/// A decoded Digital Radial Data Array (packet code 16 / 0xAF1F) — the format
#[derive(Debug, Clone)]
pub struct RadialPacket {
    /// Index of the first range bin (distance from radar).
    pub first_range_bin: i16,
    /// Number of range bins (gates) per radial.
    pub num_range_bins: u16,
    /// I-coordinate of the center of sweep (usually 0).
    pub i_center: i16,
    /// J-coordinate of the center of sweep (usually 0).
    pub j_center: i16,
    /// Scale factor (number of pixels per range bin).
    pub scale_factor: f32,
    /// Legacy (0xAF1F) RLE format: 4-bit gate values (0–15) that go through the
    pub is_legacy: bool,
    /// From the packet-28 XDR per-radial attributes. When present,
    pub xdr_data_scale: Option<f32>,
    /// See [`xdr_data_scale`](Self::xdr_data_scale).
    pub xdr_data_offset: Option<f32>,
    /// The individual radials.
    pub radials: Vec<RadialRun>,
}

/// A single radial within a Digital Radial Data Array.
#[derive(Debug, Clone)]
pub struct RadialRun {
    /// Starting azimuth angle in degrees (0 = north, clockwise).
    pub start_angle: f32,
    /// Azimuth angular width in degrees.
    pub angle_delta: f32,
    /// Raw gate values: 0–255 for 1-byte products, 0–65535 for 2-byte ones
    pub gate_values: Vec<u16>,
}

impl RadialPacket {
    /// Range in km per gate bin: `1.0 / scale_factor`, since the packet header
    pub fn gate_interval_km(&self) -> f64 {
        if self.scale_factor > 0.01 {
            1.0 / self.scale_factor as f64
        } else {
            1.0
        }
    }

    /// Range in km to the **centre** of gate `j`, at a gate spacing of
    pub fn gate_range_km(&self, j: usize, gate_km: f64) -> f64 {
        (self.first_range_bin as f64 + j as f64 + 0.5) * gate_km
    }

    /// Range in km to the centre of the first gate, at the packet's own gate
    pub fn first_gate_range_km(&self) -> f64 {
        self.gate_range_km(0, self.gate_interval_km())
    }

    /// Range in km to the **outer edge** of the last of `gate_count` gates —
    pub fn reach_km(&self, gate_count: usize, gate_km: f64) -> f64 {
        (self.first_range_bin as f64 + gate_count as f64) * gate_km
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(first_range_bin: i16) -> RadialPacket {
        RadialPacket {
            first_range_bin,
            num_range_bins: 0,
            i_center: 0,
            j_center: 0,
            scale_factor: 0.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: Vec::new(),
        }
    }

    /// The externally-fixed number, not one of ours: ICD 2620001AD Figure E-3
    #[test]
    fn bin_zero_of_a_250_m_product_is_centred_where_the_rpg_says_it_is() {
        const RPG_DPR_FIRST_RANGE_M: f64 = 125.0;
        const DPR_BIN_WIDTH_M: f64 = 250.0;

        let centre_m = packet(0).gate_range_km(0, DPR_BIN_WIDTH_M / 1000.0) * 1000.0;
        assert!(
            (centre_m - RPG_DPR_FIRST_RANGE_M).abs() < 1e-9,
            "bin 0 of a {DPR_BIN_WIDTH_M} m product must be centred at the \
             {RPG_DPR_FIRST_RANGE_M} m the RPG declares, got {centre_m} m",
        );
    }

    /// The half gate is an offset on the whole radial, not a fudge on gate 0:
    #[test]
    fn gate_centres_step_by_one_gate_from_whatever_bin_the_index_names() {
        for gate_km in [0.25, 1.0] {
            for first_bin in [0i16, 1, 2, 230] {
                let p = packet(first_bin);
                for j in 0..4 {
                    let want = (f64::from(first_bin) + j as f64 + 0.5) * gate_km;
                    assert!(
                        (p.gate_range_km(j, gate_km) - want).abs() < 1e-12,
                        "gate {j} at bin {first_bin}, {gate_km} km gates",
                    );
                }
                let step = p.gate_range_km(1, gate_km) - p.gate_range_km(0, gate_km);
                assert!((step - gate_km).abs() < 1e-12, "step {step} != {gate_km}");
            }
        }
    }

    /// A reach is an edge, a placement is a centre. The last gate's outer
    #[test]
    fn the_reach_is_the_outer_edge_half_a_gate_past_the_last_centre() {
        for gate_km in [0.25, 1.0] {
            for first_bin in [0i16, 3] {
                let p = packet(first_bin);
                let n = 1200usize;
                let last_centre = p.gate_range_km(n - 1, gate_km);
                let reach = p.reach_km(n, gate_km);
                assert!(
                    (reach - last_centre - gate_km / 2.0).abs() < 1e-9,
                    "reach {reach} vs last centre {last_centre} at {gate_km} km",
                );
                let near_edge = p.gate_range_km(0, gate_km) - gate_km / 2.0;
                assert!(
                    (near_edge - f64::from(first_bin) * gate_km).abs() < 1e-9,
                    "near edge {near_edge} at bin {first_bin}",
                );
            }
        }
    }

    /// The convention has to put a sweep exactly on its nominal range, and
    #[test]
    fn a_full_radial_spans_exactly_its_nominal_range_from_zero() {
        for (n, gate_km, nominal, who) in [
            (
                1200usize,
                0.25f64,
                300.0f64,
                "163/165/159/154/99 at 0.5-2.4 deg",
            ),
            (920, 0.25, 230.0, "177 HHC, 176 DPR"),
            (230, 1.0, 230.0, "56 N0S"),
            (460, 1.0, 460.0, "134 DVL"),
            (1840, 0.25, 460.0, "153 N0B"),
        ] {
            let p = packet(0);
            let near_edge = p.gate_range_km(0, gate_km) - gate_km / 2.0;
            assert!(
                near_edge >= 0.0,
                "{who}: bin 0 starts at {near_edge} km, in front of the antenna",
            );
            assert!(
                (near_edge - 0.0).abs() < 1e-9,
                "{who}: bin 0 must start at the antenna, starts at {near_edge} km",
            );
            let far_edge = p.reach_km(n, gate_km);
            assert!(
                (far_edge - nominal).abs() < 1e-9,
                "{who}: {n} x {gate_km} km must reach {nominal} km, reaches {far_edge}",
            );
        }
    }

    /// `first_gate_range_km` is the packet-spacing spelling of
    #[test]
    fn the_first_gate_helper_agrees_with_the_general_one() {
        let mut p = packet(2);
        p.scale_factor = 4.0; // 0.25 km gates
        assert!((p.gate_interval_km() - 0.25).abs() < 1e-12);
        assert!((p.first_gate_range_km() - p.gate_range_km(0, 0.25)).abs() < 1e-12);
        assert!((p.first_gate_range_km() - 0.625).abs() < 1e-12);
    }
}
