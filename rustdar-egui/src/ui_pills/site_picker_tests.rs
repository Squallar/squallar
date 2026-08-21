//! The site picker's shortcut sections and its ranked search — W12 to W17.
//!
//! A dedicated file rather than another block in `input_harness/tests.rs`:
//! everything here is about one body, `pills::site_list_ui`, and the probes it
//! fills.

use super::{
    FAVORITES_HEADING, IN_USE_HEADING, MatchRank, NEARBY_HEADING, SiteSection, match_rank,
};
use crate::input_harness::InputHarness;

/// A screen wide enough for the Expanded chrome, where the inspector is a
/// floating panel rather than a sheet page.
const WIDE: egui::Vec2 = egui::vec2(1400.0, 900.0);

/// The rows the inspector's site list painted, sections included.
fn rows(h: &InputHarness) -> Vec<super::SiteRowProbe> {
    h.inspector().site_section_rows
}

/// The identifiers drawn in `section`, in draw order.
fn in_section(h: &InputHarness, section: SiteSection) -> Vec<String> {
    rows(h)
        .into_iter()
        .filter(|row| row.section == section)
        .map(|row| row.site)
        .collect()
}

/// Every identifier drawn in one of the three shortcut sections.
fn shortcut_sites(h: &InputHarness) -> Vec<String> {
    rows(h)
        .into_iter()
        .filter(|row| !matches!(row.section, SiteSection::Network(_)))
        .map(|row| row.site)
        .collect()
}

/// One row, by identifier, from the network groups — where every radar the
/// filter kept is drawn exactly once.
fn group_row(h: &InputHarness, site: &str) -> super::SiteRowProbe {
    rows(h)
        .into_iter()
        .find(|row| row.site == site && matches!(row.section, SiteSection::Network(_)))
        .unwrap_or_else(|| panic!("{site} was not drawn in a network group"))
}

/// Type `query` into the inspector's site search, replacing whatever is there.
fn search_for(h: &mut InputHarness, query: &str) {
    let field = h.inspector().site_search;
    h.mouse_click(field.center());
    h.key_press(egui::Key::A);
    h.type_text(query);
    h.warm_up();
}

// -- W12: the sections exist, and are absent when they have nothing ---------

/// **An empty section is furniture, not an expressed option.**
///
/// The ordinary harness has one pane, nothing starred and no location fix, so
/// all three shortcut sections have nothing to say — and say nothing, heading
/// included.
#[test]
fn a_shortcut_section_with_nothing_in_it_is_absent_rather_than_empty() {
    let mut h = InputHarness::with_screen(WIDE);
    h.open_pane_props();

    assert!(
        shortcut_sites(&h).is_empty(),
        "precondition: nothing starred, one pane, no fix — drew {:?}",
        shortcut_sites(&h),
    );
    let painted = h.painted_text_strings();
    for heading in [FAVORITES_HEADING, IN_USE_HEADING, NEARBY_HEADING] {
        assert!(
            !painted.iter().any(|text| text == heading),
            "{heading:?} was drawn over nothing; painted {painted:?}",
        );
    }
    // Non-triviality: the list itself did draw, so the absence above is about
    // the sections rather than about a body that never ran.
    assert!(
        !rows(&h).is_empty(),
        "the network groups must still have drawn"
    );
}

/// **A shortcut is a shortcut, not a partition**: a site in a section is still
/// in its network group, and both rows pick it.
#[test]
fn a_site_in_a_section_is_still_in_its_network_group() {
    let mut h = InputHarness::with_screen(WIDE);
    h.gui_mut().favorite_sites = vec!["KMKX".to_owned()];
    h.open_pane_props();

    assert_eq!(in_section(&h, SiteSection::Favorites), vec!["KMKX"]);
    let group = group_row(&h, "KMKX");
    assert!(
        group.rect.top() > rows(&h)[0].rect.top(),
        "the favourite is drawn above the group copy",
    );

    // The inventory counts radars, not rows: `site_rows` still has one entry
    // per radar even though `KMKX` was painted twice.
    let inventory = h.inspector().site_rows;
    assert_eq!(
        inventory.iter().filter(|(site, ..)| site == "KMKX").count(),
        1,
        "the inventory must not double-count a repeated row",
    );
    assert_eq!(
        inventory.len(),
        rustdar_radar::sites::radars().len() + rustdar_radar::sites::unplaced().len(),
        "and it must still be the whole table",
    );
}

/// **Both routes draw the same sections.** The pill popover and the inspector
/// body render one body; nothing about the sections may differ between them.
#[test]
fn the_pill_popover_and_the_inspector_draw_the_same_sections() {
    let mut h = InputHarness::with_screen(WIDE);
    h.gui_mut().favorite_sites = vec!["KMKX".to_owned(), "KINX".to_owned()];

    h.open_pane_props();
    let through_inspector: Vec<(SiteSection, String)> = rows(&h)
        .into_iter()
        .map(|row| (row.section, row.site))
        .collect();
    h.close_inspector();

    h.close_layers();
    let pill = h
        .pill(0, crate::ui::PillKind::Site)
        .expect("pane 0 draws a site pill")
        .1;
    h.mouse_click(pill.center());
    h.frame();
    h.warm_up();
    let popover = h.pill_popover().expect("the site popover is open");
    let through_pill: Vec<(SiteSection, String)> = popover
        .site_rows
        .into_iter()
        .map(|row| (row.section, row.site))
        .collect();

    assert!(
        !through_pill.is_empty(),
        "precondition: the popover drew rows",
    );
    assert_eq!(
        through_pill, through_inspector,
        "the two routes drew different lists",
    );
}

// -- W13: favorites ---------------------------------------------------------

/// **The star is the second meaning on the row**, and it does not pick.
#[test]
fn the_star_toggles_a_favorite_without_switching_the_site() {
    let mut h = InputHarness::with_screen(WIDE);
    h.open_pane_props();

    let row = group_row(&h, "KMKX");
    assert!(!row.starred, "precondition: nothing is starred yet");
    h.mouse_click(row.star.center());
    h.warm_up();

    assert_eq!(
        h.gui().favorite_sites,
        vec!["KMKX".to_owned()],
        "the star must record the bare ICAO, as a pick persists",
    );
    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::SwitchRadarSite { .. })),
        "clicking the star must not also pick the row it sits in",
    );
    assert_eq!(in_section(&h, SiteSection::Favorites), vec!["KMKX"]);

    // And back off again, from the Favorites row this time.
    let favorite = rows(&h)
        .into_iter()
        .find(|row| row.section == SiteSection::Favorites)
        .expect("the favourite is drawn");
    assert!(favorite.starred, "the section's own row reads as starred");
    h.mouse_click(favorite.star.center());
    h.warm_up();
    assert!(h.gui().favorite_sites.is_empty(), "the star unstars");
    assert!(shortcut_sites(&h).is_empty(), "and the section goes away");
}

/// **The row highlight reads `contains_pointer`, not `hovered`.**
///
/// The star sits on top of the row rect and takes `hovered` with it, so a
/// `hovered` read blinks the highlight off as the pointer crosses the star.
/// The regression is invisible in state and visible only in paint.
#[test]
fn the_row_stays_highlighted_while_the_pointer_is_on_its_star() {
    let mut h = InputHarness::with_screen(WIDE);
    h.open_pane_props();

    // A row that is NOT the current site, so any fill under it is the hover
    // and not the selection.
    let row = group_row(&h, "KMKX");
    assert!(!row.is_current, "precondition: KMKX is not the pane's site");

    // Non-triviality: with the pointer away from the row, nothing paints.
    h.mouse_move(h.screen_rect().left_bottom());
    h.warm_up();
    let cold = h.painted_fills_within(row.rect, 1.0);

    h.mouse_move(row.rect.center());
    h.warm_up();
    let over_row = h.painted_fills_within(group_row(&h, "KMKX").rect, 1.0);
    assert!(
        over_row.len() > cold.len(),
        "precondition: hovering the row paints a highlight; cold {cold:?}, \
         hovered {over_row:?}",
    );

    h.mouse_move(group_row(&h, "KMKX").star.center());
    h.warm_up();
    let over_star = h.painted_fills_within(group_row(&h, "KMKX").rect, 1.0);
    assert!(
        over_star.len() > cold.len(),
        "the highlight blinked off as the pointer reached the star: cold \
         {cold:?}, over the star {over_star:?}",
    );
}

// -- W14: in use ------------------------------------------------------------

/// **"Pane 3 is here", not an unexplained duplicate** — and the predicate is
/// every pane, not the live ones `Gui::live_sites` counts.
#[test]
fn the_other_panes_site_is_offered_with_the_pane_that_holds_it() {
    let mut h = InputHarness::with_screen(WIDE);
    h.set_pane_count(2);
    // Layer sync copies the active pane's site onto every linked pane, so a
    // second site only survives with the link off. That is the case this
    // section exists for.
    h.set_layer_links(false);
    h.gui_mut().panes[1].set_site("KMKX".to_owned());
    // Parked in the archive, so `live_sites` would not report it. This section
    // is about what a pane is showing, which is a different question.
    h.gui_mut().panes[1].viewing_live = false;
    h.warm_up();

    h.open_pane_props();
    assert_eq!(h.active_pane_index(), 0, "precondition: pane 1 is active");

    let in_use: Vec<super::SiteRowProbe> = rows(&h)
        .into_iter()
        .filter(|row| row.section == SiteSection::InUse)
        .collect();
    assert_eq!(
        in_use
            .iter()
            .map(|row| row.site.as_str())
            .collect::<Vec<_>>(),
        vec!["KMKX"],
    );
    assert_eq!(
        in_use[0].note.as_deref(),
        Some("pane 2"),
        "the row must say which pane is there",
    );

    // The active pane's own site is never in the section: it is the row the
    // list already highlights.
    assert!(
        !in_use.iter().any(|row| row.site == "KTLX"),
        "the pane's own site must not be offered back to it: {in_use:?}",
    );
}

/// Two panes on one site produce one row, not two.
#[test]
fn a_site_two_other_panes_share_is_offered_once() {
    let mut h = InputHarness::with_screen(WIDE);
    h.set_pane_count(3);
    h.set_layer_links(false);
    h.gui_mut().panes[1].set_site("KMKX".to_owned());
    h.gui_mut().panes[2].set_site("KMKX".to_owned());
    h.warm_up();
    h.open_pane_props();

    assert_eq!(in_section(&h, SiteSection::InUse), vec!["KMKX"]);
}

// -- W15: nearby ------------------------------------------------------------

/// A fix in the fixture's own corner of the world.
fn fix_at(lat: f64, lon: f64) -> rustdar_location::Fix {
    rustdar_location::Fix {
        point: rustdar_geo::GeoPoint { lat, lon },
        ..Default::default()
    }
}

/// **Absent without a fix, ordered with one, and the distance is in the
/// user's own unit.**
#[test]
fn nearby_appears_only_with_a_fix_and_reads_in_the_users_unit() {
    let mut h = InputHarness::with_screen(WIDE);
    h.open_pane_props();
    assert!(
        in_section(&h, SiteSection::Nearby).is_empty(),
        "no fix, no section — not an empty one and not an apology",
    );

    // Twin Lakes, where `KTLX` is: the nearest rows are the Oklahoma cluster.
    h.set_gps_fix(fix_at(35.33, -97.28));
    h.warm_up();

    let nearby: Vec<super::SiteRowProbe> = rows(&h)
        .into_iter()
        .filter(|row| row.section == SiteSection::Nearby)
        .collect();
    assert_eq!(
        nearby
            .iter()
            .map(|row| row.site.as_str())
            .collect::<Vec<_>>(),
        vec!["KTLX", "KOUN", "TOKC", "KVNX", "KINX"],
        "the section is the five closest, every network — `TOKC` is a TDWR \
         and is offered, because a person reading a list is exactly the case \
         the automatic pick's filters do not apply to",
    );
    let stated: Vec<f64> = nearby
        .iter()
        .map(|row| {
            let note = row.note.as_deref().expect("every nearby row has a note");
            let (value, unit) = note.split_once(' ').expect("`<n> <unit>`");
            assert_eq!(unit, "km", "the default preference is kilometres");
            value.parse().expect("a number")
        })
        .collect();
    assert!(
        stated.windows(2).all(|w| w[0] <= w[1]),
        "the rows must be ordered by the distance they state: {stated:?}",
    );

    // The unit is the user's, through `rustdar-units` — not a hardcoded km.
    h.gui_mut().preferences.distance = rustdar_units::DistanceUnit::Miles;
    h.warm_up();
    let miles: Vec<super::SiteRowProbe> = rows(&h)
        .into_iter()
        .filter(|row| row.section == SiteSection::Nearby)
        .collect();
    assert!(
        miles
            .iter()
            .all(|row| row.note.as_deref().is_some_and(|n| n.ends_with(" mi"))),
        "the rows must follow the preference: {miles:?}",
    );
}

// -- W17: place names, and the ranking ---------------------------------------

/// The four rungs, on the one function that is the sort key.
#[test]
fn the_rank_puts_the_identifier_ahead_of_the_place() {
    assert_eq!(match_rank("KMKX", "KMKX"), Some(MatchRank::ExactId));
    assert_eq!(match_rank("KMKX", "KM"), Some(MatchRank::IdPrefix));
    // The three-letter short form is why the substring rung survives.
    assert_eq!(match_rank("KMKX", "MKX"), Some(MatchRank::IdSubstring));
    assert_eq!(match_rank("KMKX", "MILWAUKEE"), Some(MatchRank::Place));
    assert_eq!(match_rank("KMKX", "PORTLAND"), None);
    // An empty query is not a filter, and every row lands on one rung so the
    // sort below it is the identity.
    assert_eq!(match_rank("KMKX", ""), match_rank("KTLX", ""));

    // The rungs are ordered best-first, which is what `sort_by_key` reads.
    let mut rungs = [
        MatchRank::Place,
        MatchRank::IdSubstring,
        MatchRank::ExactId,
        MatchRank::IdPrefix,
    ];
    rungs.sort();
    assert_eq!(
        rungs,
        [
            MatchRank::ExactId,
            MatchRank::IdPrefix,
            MatchRank::IdSubstring,
            MatchRank::Place,
        ],
    );
}

/// **"Milwaukee" finds `KMKX`.** The station record publishes one free-text
/// name per station and it is matched whole.
#[test]
fn a_place_name_finds_its_radar_and_the_row_says_why() {
    let mut h = InputHarness::with_screen(WIDE);
    h.open_pane_props();
    search_for(&mut h, "milwaukee");

    let inventory: Vec<String> = h
        .inspector()
        .site_rows
        .into_iter()
        .map(|(site, ..)| site)
        .collect();
    assert_eq!(
        inventory,
        vec!["KMKX".to_owned()],
        "the place name must reach the radar and nothing else",
    );
    assert_eq!(
        group_row(&h, "KMKX").label,
        "KMKX - Milwaukee",
        "and the row must say what matched, or the result is inexplicable",
    );
}

/// **The list is a ranking now, not a filter.**
///
/// `IN` reaches `KINX` through its identifier and `KTLX` (Twin Lakes) and
/// `KMPX` (Minneapolis) through their places. The identifier match must sort
/// above both — and `KTLX` is the *first* row of the fixture table, so an
/// unranked filter would put it first.
#[test]
fn an_identifier_match_outranks_a_place_match_that_comes_earlier_in_the_table() {
    let mut h = InputHarness::with_screen(WIDE);
    h.open_pane_props();
    search_for(&mut h, "in");

    let shown: Vec<String> = h
        .inspector()
        .site_rows
        .into_iter()
        .map(|(site, ..)| site)
        .collect();
    assert!(
        shown.contains(&"KINX".to_owned())
            && shown.contains(&"KTLX".to_owned())
            && shown.contains(&"KMPX".to_owned()),
        "precondition: the query must mix an id match with two place matches; \
         drew {shown:?}",
    );
    assert_eq!(
        shown.first().map(String::as_str),
        Some("KINX"),
        "the identifier match must lead; drew {shown:?}",
    );
}

/// **The caption names its denominator.** Both halves are radars, because the
/// sections repeat rows and a row count would disagree with the ratio.
#[test]
fn the_caption_counts_radars_rather_than_rows() {
    let mut h = InputHarness::with_screen(WIDE);
    h.gui_mut().favorite_sites = vec!["KMKX".to_owned()];
    h.open_pane_props();

    let total = rustdar_radar::sites::radars().len() + rustdar_radar::sites::unplaced().len();
    let caption = h.inspector().site_caption;
    assert!(
        caption.starts_with(&format!("{total} of {total} radars")),
        "drew {caption:?}",
    );
    assert!(
        rows(&h).len() > total,
        "precondition: the sections did repeat a row, so the count and the \
         paint really do differ — {} rows against {total} radars",
        rows(&h).len(),
    );
}
