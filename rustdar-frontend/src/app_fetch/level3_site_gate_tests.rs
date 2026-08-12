use super::*;

/// Which sites a Level II load spends four S3 requests on, and which it does
/// not.
///
/// The rows that carry the decision are `TPIT` and the two that bracket it.
/// `TJUA` is the one site a naive `T` prefix gets wrong — San Juan's WSR-88D,
/// which has an RPG like any other — and an id in no row of the resolved table
/// keeps fetching, because an unrecognised four-letter id is far likelier to be
/// a WSR-88D this build's table predates than a TDWR.
#[test]
fn only_a_site_with_an_rpg_behind_it_fetches_level3_objects() {
    // The rows the gate reads. There is no compiled-in table under them —
    // see `rustdar_radar::sites::SiteTable` — so a test that did not place
    // them would find `ZZZZ`'s "not in the table" answer for every row.
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
///
/// A source probe for the same reason the completed-volume probe in
/// `app_chunks` is one: `spawn_level3_fetches` reaches the network through
/// `spawn_async_task` and leaves nothing on `App` to assert against, so the only
/// thing that can be checked without a live bucket is where the `return` sits.
///
/// Its position is the whole correctness of the change. The two spawns want
/// opposite answers for a TDWR: the objects are not generated for it, but the
/// 0 °C / −20 °C environmental heights are what PoSH and MEHS integrate the
/// local reflectivity volume against, and those two are among the eight
/// products a TDWR *does* offer. A gate placed one block earlier would mute
/// them at every TDWR — silently, since the hail pair returns `None` on missing
/// heights exactly as it does on a volume with no echo.
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

    assert!(
        sounding < gate,
        "the sounding spawn moved behind the Level III gate, so a TDWR pane \
             gets no environmental heights and PoSH and MEHS go quiet",
    );
    assert!(
        gate < objects,
        "the object loop runs before the gate, which is the same as no gate",
    );
}
