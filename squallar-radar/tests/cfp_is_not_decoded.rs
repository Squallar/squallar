//! **The clutter-filter-power moment is not decoded, and the six that are read
//! survive.**
//!
//! Its own binary because the ledger it reads is a process-global running
//! total ([`squallar_radar::moment_drop`]) and this crate's unit tests are one
//! binary: a delta read there would be a race against every other suite that
//! decodes. Here nothing else runs, so the totals are absolute.
//!
//! The fixtures are the two committed WSR-88D first-messages — a real archive
//! cut to its first Message 31, header verbatim. **They are not the same case
//! and that is why both are here.** `KTLX20260811` carries a 1,832-byte CFP
//! block, so the drop assertion is checked against a message that HAS the
//! moment to drop and is not vacuous. `KAMX20200810` carries none — read off
//! the fixtures, not assumed — so it also pins the no-op arm: a radial with
//! nothing to drop must move neither counter, or the ledger would read as a
//! saving on volumes that never carried the moment.

use nexrad_model::data::DataMoment;

/// `(name, bytes, carries a CFP block)`.
const FIXTURES: [(&str, &[u8], bool); 2] = [
    (
        "KTLX20260811_000049_V06",
        include_bytes!("../testdata/KTLX20260811_000049_V06.first-message"),
        true,
    ),
    (
        "KAMX20200810_000424_V06",
        include_bytes!("../testdata/KAMX20200810_000424_V06.first-message"),
        false,
    ),
];

/// What the ICD gives a WSR-88D surveillance cut's CFP block, and what both
/// fixtures carry: 1,832 gates at one byte each.
const CFP_ENCODED_BYTES: u64 = 1832;

#[test]
fn a_decoded_radial_carries_no_clutter_filter_power_and_keeps_every_other_moment() {
    let mut dropping_radials = 0u64;
    let mut carried_cfp = false;
    for (name, bytes, carries_cfp) in FIXTURES {
        carried_cfp |= carries_cfp;
        let contents = squallar_radar::chunks::decode_chunk(name, bytes)
            .unwrap_or_else(|e| panic!("decoding {name}: {e}"));
        assert!(!contents.radials.is_empty(), "{name} decoded no radials");
        for radial in &contents.radials {
            if carries_cfp {
                dropping_radials += 1;
            }
            assert!(
                radial.clutter_filter_power().is_none(),
                "{name} kept its clutter filter power"
            );
            // **The tamper the drop has to survive.** A decode that returned
            // every moment empty would pass the assertion above and be a
            // catastrophe; these say the drop is one slot and not a blanket.
            // Presence AND a non-empty buffer, because
            // `nexrad_model::MomentData::without_values` produces a moment
            // that is present with nothing in it and that is exactly the
            // silent-blank hazard `squallar_radar::skeleton` documents.
            for (moment, label) in [
                (radial.reflectivity(), "reflectivity"),
                (radial.differential_reflectivity(), "ZDR"),
                (radial.differential_phase(), "PHI"),
                (radial.correlation_coefficient(), "RHO"),
            ] {
                let moment =
                    moment.unwrap_or_else(|| panic!("{name} lost its {label} along with the CFP"));
                assert!(
                    !moment.raw_values().is_empty(),
                    "{name}'s {label} came back with an empty buffer"
                );
            }
        }
    }

    // The ledger saw exactly those radials, TWO blocks apiece — a decoded
    // moment's gates live in a `Vec<u8>` behind a `GateBuffer` `Arc` — at the
    // block size the fixtures carry plus this crate's own per-allocation
    // charge on each, so the figure a leg quotes is the same bytes
    // `scan_size` would have priced had they been allocated.
    assert!(
        carried_cfp && dropping_radials > 0,
        "no fixture carried a CFP block, so the drop was never exercised"
    );
    assert_eq!(
        squallar_radar::moment_drop::dropped(),
        dropping_radials,
        "one drop per radial that carried the moment, and none for the rest"
    );
    assert_eq!(
        squallar_radar::moment_drop::blocks(),
        dropping_radials * 2,
        "two blocks per drop: the gate `Vec` and the `Arc` that would have \
         shared it"
    );
    assert_eq!(
        squallar_radar::moment_drop::bytes(),
        dropping_radials
            * (CFP_ENCODED_BYTES
                + squallar_radar::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64
                + squallar_radar::scan_size::GATE_BUFFER_SHARE_BYTES as u64
                + squallar_radar::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64),
        "the gate bytes and the blocks holding them"
    );
    // Zero by construction: nothing reads a moment this drops, so nothing can
    // ask for it back. A non-zero reading here means the premise is false.
    assert_eq!(
        squallar_radar::moment_drop::redecodes(),
        0,
        "a decode was re-run to recover a dropped moment"
    );
}
