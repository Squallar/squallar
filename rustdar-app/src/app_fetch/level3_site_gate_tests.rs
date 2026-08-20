use super::*;

/// Which sites a Level II load spends four S3 requests on, and which it does
/// not.
#[test]
fn only_a_site_with_an_rpg_behind_it_fetches_level3_objects() {
    crate::test_sites::install();
    for (site, offers, why) in [
        (
            "KTLX",
            true,
            "the WSR-88D every Level III path was built on",
        ),
        (
            "KPBZ",
            true,
            "Pittsburgh's WSR-88D — the TDWR below is not it",
        ),
        (
            "TPIT",
            false,
            "a TDWR: its SPG generates none of N0K/EET/DVL/DPR",
        ),
        ("TJUA", true, "San Juan's WSR-88D, despite the T"),
        (
            "ZZZZ",
            true,
            "in no row of the site table: fetch, as before this gate existed",
        ),
    ] {
        assert_eq!(site_offers_level3(site), offers, "{site} — {why}");
    }
}

/// The gate skips the four object fetches and **not** the sounding above them.
#[test]
fn the_gate_skips_the_objects_and_leaves_the_sounding_alone() {
    let source = include_str!("../app_fetch.rs");
    let start = source
        .find("pub(super) fn spawn_level3_fetches(")
        .expect("spawn_level3_fetches is gone");
    let body = &source[start..];
    let body = &body[..body
        .find("pub(super) fn local_to_utc(")
        .expect("spawn_level3_fetches no longer ends where it did")];

    let sounding = body
        .find("self.channels.sounding_sender")
        .expect("the sounding spawn left the function");
    let gate = body.find("if !site_offers_level3(site)").expect(
        "the Level III fetch gate is gone, so a TDWR fetches four \
             objects that do not exist on every scan load and every poll",
    );
    let objects = body
        .find("for code in RadarProduct::level3_codes_for(")
        .expect("the object loop left the function");
    let melting_layer = body
        .find("self.channels.melting_layer_sender")
        .expect("the melting-layer spawn left the function");
    let storm_motion = body
        .find("self.channels.storm_motion_sender")
        .expect("the storm-motion spawn left the function");

    assert!(
        sounding < gate,
        "the sounding spawn moved behind the Level III gate, so a TDWR pane \
             gets no environmental heights and PoSH and MEHS go quiet",
    );
    assert!(
        gate < objects,
        "the object loop runs before the gate, which is the same as no gate",
    );
    assert!(
        gate < melting_layer,
        "the melting-layer fetch runs before the RPG gate, so a TDWR asks S3 \
             for an N0M its SPG never generates",
    );
    assert!(
        gate < storm_motion,
        "the storm-motion fetch runs before the RPG gate, so a TDWR asks S3 \
             for an N0S its SPG never generates",
    );
}
