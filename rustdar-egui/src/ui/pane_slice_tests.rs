use super::*;

/// Splitting to fewer panes leaves the extra `PaneState`s in the vector so a
/// re-split can restore them. They are not drawn and not updated, so the
/// "every pane" slice must stop at the layout's count — otherwise a polled
/// scan appends loop frames to panes nobody is looking at.
#[test]
fn the_pane_slices_stop_at_the_visible_count() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    for (idx, pane) in gui.panes_mut().iter_mut().enumerate() {
        pane.site = format!("PANE{idx}");
    }

    // Split back down: panes 2 and 3 are remembered but no longer shown.
    gui.set_pane_count_for_test(2);

    assert_eq!(gui.panes().len(), 2);
    assert_eq!(gui.panes_mut().len(), 2);
    assert_eq!(
        gui.panes()
            .iter()
            .map(|p| p.site.as_str())
            .collect::<Vec<_>>(),
        ["PANE0", "PANE1"],
    );
    assert_eq!(
        gui.pane(3).map(|p| p.site.as_str()),
        Some("PANE3"),
        "precondition: the hidden pane is still there to be reached by index"
    );
}

/// The count and the vector are kept in step by every path that changes the
/// layout, but slicing past the end would panic, and no pane update is worth
/// a crash.
#[test]
fn the_pane_slices_never_outrun_the_vector() {
    let mut gui = Gui::new();
    assert_eq!(gui.panes().len(), 1, "a fresh Gui has one pane");
    // A layout claiming more panes than the vector holds, as a config whose
    // pane_count ran ahead of its pane list would leave it.
    gui.claim_pane_count_for_test(4);

    assert_eq!(gui.panes().len(), 1);
    assert_eq!(gui.panes_mut().len(), 1);
}

/// The rects a test clicks are the rects the frame drew, so the helper that
/// produces them takes the visible slice's bound too. With the raw count it
/// handed back a rect per *claimed* pane, and a test clicking the last of them
/// would have been driving a pane no frame ever rendered.
#[test]
fn the_pane_rects_a_test_sees_are_only_the_ones_a_frame_drew() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.last_map_panel_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    assert_eq!(
        gui.pane_rects_for_test().len(),
        2,
        "precondition: two real panes give two rects"
    );

    gui.claim_pane_count_for_test(4);

    assert_eq!(gui.pane_rects_for_test().len(), 2);
}

/// `sync_viewports` reads and writes panes by raw index, so it takes its
/// bound from the visible slice rather than the layout's claim — with the
/// raw count, the same ran-ahead layout as above panicked mid-frame.
#[test]
fn viewport_sync_never_outruns_the_pane_vector() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.viewport_sync = true;
    gui.claim_pane_count_for_test(4);

    // Snapshots sized to the layout's claim, exactly as `render_panes` would
    // have taken them had it trusted the raw count too. All-zero zooms
    // make every pane look interacted, so the source scan runs as deep as
    // its bound allows.
    gui.sync_viewports(&[0.0; 4], &[None; 4]);

    // The panes that are really there still synced to a common zoom.
    assert_eq!(
        gui.pane(0).unwrap().map_memory.zoom(),
        gui.pane(1).unwrap().map_memory.zoom(),
    );
}

/// A pane conversion asked for during the UI pass lands on the **real** pane,
/// not on the placeholder standing in for it.
///
/// This pins the write half of the `mem::take` hazard: the thing the type
/// system cannot help with. Two production paths hold a `PaneState` out of the
/// vector for a whole pass — `render_layers_panel` takes the active pane,
/// `render_panes` takes each pane in turn — leaving a default `PaneState` in
/// the slot. Inside either window the obvious implementation of the toggle's
/// arm,
///
/// ```ignore
/// self.panes[self.active_pane].set_kind(kind);
/// ```
///
/// writes the *placeholder*, and the line that puts the real pane back discards
/// it: no panic, no warning, and a control that will not stay set.
///
/// # This test builds the window itself, because no caller currently provides
/// one
///
/// Read the `std::mem::take` below as the load-bearing part of the fixture
/// rather than as scene-setting. Today's menu dispatch is **outside** both
/// windows — `render_layers_panel` restores the pane at `ui_chrome.rs:425` and
/// dispatches at `:438`, and `render_menu_bar_panel` takes no pane — so a
/// direct write from `apply_menu_event` would pass every behavioural test in
/// the suite, this one included, if this one did not hold the pane out by hand.
///
/// That makes this a test of the *mechanism* and not of user-visible
/// behaviour, which is a thing worth saying out loud: it is here because
/// WP-G's writers run inside `render_panes`' take, where the same direct write
/// is silently discarded, and a test written after that code would be a test
/// written after the bug. Driven through `apply_menu_event` rather than
/// `request_pane_kind` so it covers the arm and the deferral together. The
/// end-to-end behavioural version, which passes either way, is
/// `converting_the_active_pane_from_the_drawer_makes_it_a_volume_pane`.
#[test]
fn a_pane_kind_request_survives_the_pane_being_held_out_of_the_vector() {
    use super::ui_menu::{MenuEvent, MenuToggle};
    use crate::pane::PaneKind;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.active_pane = 1;
    assert_eq!(
        gui.pane(1).unwrap().kind(),
        PaneKind::Map,
        "precondition: the pane starts as a map"
    );
    // Something on the real pane that the placeholder does not have, so the
    // restore below can be shown to have really put the original back rather
    // than to have left a default in place.
    gui.pane_mut(1).unwrap().site = "KDDC".to_owned();

    let held = std::mem::take(&mut gui.panes[gui.active_pane]);
    assert_eq!(
        gui.panes[1].site, "KTLX",
        "precondition: the slot now holds a default PaneState, which is what \
             makes a direct write vanish"
    );

    let mut actions = Vec::new();
    gui.apply_menu_event(
        MenuEvent::Toggled(MenuToggle::VolumePane, true),
        &mut actions,
    );

    // The restore, which throws the placeholder away.
    gui.panes[gui.active_pane] = held;
    gui.apply_pending_pane_kind(&mut Vec::new());

    assert_eq!(
        gui.pane(1).unwrap().site,
        "KDDC",
        "precondition: the original pane must be the one back in the slot"
    );
    assert_eq!(
        gui.pane(1).unwrap().kind(),
        PaneKind::Volume,
        "the conversion was written to the pane that was held out and thrown \
             away, so the menu item silently did nothing"
    );
    assert_eq!(
        gui.pending_pane_kind_for_test(),
        None,
        "the request must be consumed, or every later frame re-converts the \
             pane and any per-kind state it gathers is discarded each time"
    );
    assert_eq!(
        gui.pane(0).unwrap().kind(),
        PaneKind::Map,
        "the request converted a pane other than the one it named"
    );
}

/// A request naming a pane the layout no longer has is dropped, not clamped.
///
/// Reachable in one frame: the pane picker can shrink the layout after the
/// menu event was recorded. Converting whichever pane happens to be at a
/// nearby index would convert one the user never pointed at.
#[test]
fn a_pane_kind_request_for_a_pane_that_is_gone_converts_nothing() {
    use crate::pane::PaneKind;

    let mut gui = Gui::new();
    gui.request_pane_kind(7, PaneKind::Volume);
    gui.apply_pending_pane_kind(&mut Vec::new());

    assert_eq!(gui.pane(0).unwrap().kind(), PaneKind::Map);
    assert_eq!(gui.pending_pane_kind_for_test(), None);
}

/// A line for the target rule to place, and the pane it was drawn on.
fn drawn_line() -> crate::pane::SectionLine {
    crate::pane::SectionLine::new(
        crate::pane::GeoPoint {
            lat: 35.0,
            lon: -97.8,
        },
        crate::pane::GeoPoint {
            lat: 35.6,
            lon: -96.9,
        },
    )
    .expect("a fixture line must be finite and have two distinct ends")
}

/// A cut of the right shape and no content, so a fixture can hold a picture
/// for a retarget to throw away.
///
/// Full size — `from_parts` refuses anything else, because a mis-shaped
/// section reaches `ColorImage::from_rgba_unmultiplied`'s `assert_eq!` on
/// the main thread. `NoCoverage` everywhere, which is what an empty volume
/// really renders as.
fn blank_section() -> rustdar_radar::xsect::CrossSection {
    use rustdar_radar::sampler::SampleStatus;
    use rustdar_radar::xsect::{SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};
    let pixels = SECTION_WIDTH * SECTION_HEIGHT;
    rustdar_radar::xsect::CrossSection::from_parts(
        vec![0u8; pixels * 4],
        vec![f32::NAN; pixels],
        vec![SampleStatus::NoCoverage.wire_code(); pixels],
        SectionAxes {
            length_km: 100.0,
            base_km_msl: 0.4,
            top_km_msl: 20.4,
            near_ground_range_km: 10.0,
            far_ground_range_km: 110.0,
            coverage_ground_range_km: 0.0,
            cone_of_silence_km: 0.0,
            tilt_count: 1,
            widest_tilt_gap_deg: 0.0,
            top_tilt_deg: 0.5,
            top_declared_cut_deg: 19.5,
        },
        vec![0.5],
    )
    .expect("a full-size, all-NoCoverage section is well formed")
}

/// A second line, distinguishable from [`drawn_line`], for a section that
/// belongs to another map and must be left alone.
fn other_line() -> crate::pane::SectionLine {
    crate::pane::SectionLine::new(
        crate::pane::GeoPoint {
            lat: 40.0,
            lon: -100.0,
        },
        crate::pane::GeoPoint {
            lat: 41.0,
            lon: -99.0,
        },
    )
    .expect("a fixture line must be finite and have two distinct ends")
}

fn wide(count: usize) -> Gui {
    let mut gui = Gui::new();
    gui.layout.width = crate::ui_layout::WidthClass::Expanded;
    gui.set_pane_count_for_test(count);
    gui
}

/// Step 1: a second line on the same map re-aims the section already cut
/// from it, rather than filling the screen with panes nobody asked for.
#[test]
fn a_second_line_on_one_map_re_aims_the_section_it_already_feeds() {
    let mut gui = wide(2);
    gui.panes[1].set_kind(crate::pane::PaneKind::CrossSection);
    gui.panes[1].cross_section_mut().unwrap().source_pane = Some(0);
    let before = gui.pane_count();

    gui.pending_section_line = Some((0, drawn_line()));
    gui.apply_pending_section_line();

    assert_eq!(gui.pane_count(), before, "the layout grew for a re-aim");
    assert_eq!(
        gui.pane(1).unwrap().cross_section().unwrap().line,
        Some(drawn_line())
    );
}

/// Step 2: with no section fed by *this* map, the layout grows — even when
/// another map's section is sitting right there.
///
/// The pane count is the load-bearing assertion, and the second half of the
/// fixture is what makes it one: a section pane exists, but it belongs to
/// pane 1, and stealing it would silently re-aim a picture the user is
/// still using. Only once the layout cannot grow (the test below) is that
/// the right answer.
#[test]
fn a_line_with_nowhere_to_go_grows_the_layout_rather_than_taking_a_map() {
    let mut gui = wide(1);

    gui.pending_section_line = Some((0, drawn_line()));
    gui.apply_pending_section_line();

    assert_eq!(gui.pane_count(), 2, "the layout did not grow");
    assert_eq!(
        gui.pane(0).unwrap().kind(),
        crate::pane::PaneKind::Map,
        "the map survived"
    );
    assert_eq!(
        gui.pane(1).unwrap().kind(),
        crate::pane::PaneKind::CrossSection
    );
    assert_eq!(
        gui.pane(1).unwrap().cross_section().unwrap().source_pane,
        Some(0),
        "the section must remember its map, or the next line converts \
             another pane instead of re-aiming this one"
    );
    assert_eq!(
        gui.active_pane, 1,
        "the pane the user just asked for is not the one they are looking at"
    );

    // The same, with another map's section already on screen and room still
    // to grow. Growing must still win: re-aiming pane 2 would throw away a
    // picture pane 1 is still using, silently.
    let mut gui = wide(3);
    gui.panes[2].set_kind(crate::pane::PaneKind::CrossSection);
    gui.panes[2].cross_section_mut().unwrap().source_pane = Some(1);
    gui.panes[2].cross_section_mut().unwrap().line = Some(other_line());
    gui.pending_section_line = Some((0, drawn_line()));
    gui.apply_pending_section_line();

    assert_eq!(
        gui.pane_count(),
        4,
        "the layout had room and did not use it: another map's section was \
             taken instead"
    );
    assert_eq!(
        gui.pane(2).unwrap().cross_section().unwrap().line,
        Some(other_line()),
        "pane 1's section was re-aimed at a line drawn on pane 0"
    );
    assert_eq!(
        gui.pane(3).unwrap().cross_section().unwrap().line,
        Some(drawn_line())
    );
}

/// Steps 3 and 4: a full layout re-aims the lowest section before it
/// converts any map, and converts the *highest* map rather than the one
/// under the line.
#[test]
fn a_full_layout_re_aims_a_section_before_it_takes_a_map() {
    let full = crate::ui_layout::WidthClass::Expanded.max_panes();

    // Step 3: a section exists somewhere, aimed from another map.
    let mut gui = wide(full);
    gui.panes[2].set_kind(crate::pane::PaneKind::CrossSection);
    gui.panes[2].cross_section_mut().unwrap().source_pane = Some(1);
    gui.pending_section_line = Some((0, drawn_line()));
    gui.apply_pending_section_line();
    assert_eq!(gui.pane_count(), full, "a full layout cannot grow");
    assert_eq!(
        gui.pane(2).unwrap().cross_section().unwrap().source_pane,
        Some(0),
        "the existing section should have been re-aimed and re-sourced"
    );
    assert!(
        (0..full)
            .filter(|&i| gui.pane(i).unwrap().kind() == crate::pane::PaneKind::Map)
            .count()
            == full - 1,
        "a map was converted while a section was there to re-aim"
    );

    // Step 4: no section anywhere. The highest-indexed pane converts, and
    // the map the line was drawn on is left alone.
    let mut gui = wide(full);
    gui.pending_section_line = Some((0, drawn_line()));
    gui.apply_pending_section_line();
    assert_eq!(
        gui.pane(0).unwrap().kind(),
        crate::pane::PaneKind::Map,
        "the map under the line was taken"
    );
    assert_eq!(
        gui.pane(full - 1).unwrap().kind(),
        crate::pane::PaneKind::CrossSection
    );
}

/// The rule is **total**: a drawn line always lands somewhere, at every
/// pane count either width class can reach.
///
/// The one that most needs saying is a compact layout already at its own
/// ceiling — a phone that has split as far as it is allowed to. There, every
/// earlier step has failed and the only answer left is to convert a map. A
/// silent no-op is the failure this is written against: a drag that produced
/// nothing, with nothing on screen to explain it, after the user had gone to
/// the menu to arm a mode.
///
/// **What is not covered, and cannot be.** The final `unwrap_or(source)` —
/// converting the pane drawn on — needs `max_panes() == 1`, and no
/// [`WidthClass`](crate::ui_layout::WidthClass) reports that: `Compact` is 4
/// and the others 6. It is unreachable today and stays because
/// `highest_pane_other_than` returning `None` must mean *something* other
/// than dropping the line.
#[test]
fn a_drawn_line_lands_somewhere_at_every_reachable_pane_count() {
    use crate::ui_layout::WidthClass;
    for width in [WidthClass::Compact, WidthClass::Expanded] {
        for count in 1..=width.max_panes() {
            let mut gui = Gui::new();
            gui.layout.width = width;
            gui.set_pane_count_for_test(count);

            gui.pending_section_line = Some((0, drawn_line()));
            gui.apply_pending_section_line();

            let sections = gui
                .panes()
                .iter()
                .filter(|p| p.kind() == crate::pane::PaneKind::CrossSection)
                .count();
            assert_eq!(
                sections, 1,
                "{width:?} with {count} panes placed {sections} sections for one line"
            );
            assert_eq!(
                gui.pane(0).unwrap().kind(),
                crate::pane::PaneKind::Map,
                "{width:?} with {count} panes took the map the line was drawn on"
            );
            // Grown while it could, and only converted once it could not.
            let expected = (count + 1).min(width.max_panes());
            assert_eq!(
                gui.pane_count(),
                expected,
                "{width:?} with {count} panes should have ended at {expected}"
            );
        }
    }
}

/// The section a line lands in adopts the drawing map's site and moment, and
/// throws away the picture it was showing.
///
/// A section is cut from a *site's* volume, so a target pane that kept its
/// own site would cut the line's ground out of the wrong radar — a picture
/// that renders perfectly and means nothing. Clearing the old raster matters
/// for the interval before the new cut lands: a section of the previous line
/// left on screen is of ground the user is no longer pointing at.
#[test]
fn a_retargeted_section_takes_the_maps_site_and_drops_the_old_picture() {
    let ctx = egui::Context::default();
    let mut gui = wide(2);
    gui.panes[0].site = "KTLX".to_owned();
    gui.panes[0].selected_product = RadarProduct::Velocity;
    gui.panes[1].site = "KINX".to_owned();
    gui.panes[1].set_kind(crate::pane::PaneKind::CrossSection);
    {
        let section = gui.panes[1].cross_section_mut().unwrap();
        section.source_pane = Some(0);
        section.unavailable = Some(crate::pane::SectionUnavailable::RenderFailed);
        // A picture and a key for the *previous* line, which is the state a
        // retarget has to clear. Without them in the fixture both fields are
        // `None` before and after, and the assertions below hold for a build
        // that clears neither — the exact shape of test that looks like it
        // is watching something and is not. (Found by mutation: dropping
        // both clears survived until this fixture had something to drop.)
        section.rendered_for = Some(crate::pane::SectionTarget {
            volume: crate::pane::VolumeStamp {
                site: "KINX".to_owned(),
                collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                    .unwrap()
                    .and_hms_opt(18, 30, 0)
                    .unwrap(),
            },
            product: RadarProduct::Reflectivity,
            line: other_line(),
            ladder: 9,
        });
        section.section = Some(std::sync::Arc::new(blank_section()));
        // And the raster, which needs a `Context` and is the reason the
        // first repair of this fixture stopped at `section`. Without it,
        // deleting `section.texture = None` from the retarget passes: the
        // pane would go on painting the *previous* line's picture, with the
        // new line's caption over it, for as long as the re-cut takes.
        section.texture = Some(ctx.load_texture(
            "retarget-fixture",
            egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        ));
    }

    gui.pending_section_line = Some((0, drawn_line()));
    gui.apply_pending_section_line();

    let pane = gui.pane(1).unwrap();
    assert_eq!(pane.site, "KTLX");
    assert_eq!(pane.selected_product, RadarProduct::Velocity);
    let section = pane.cross_section().unwrap();
    assert_eq!(section.line, Some(drawn_line()));
    assert!(
        section.section.is_none(),
        "the previous line's cut is still what a hover reads"
    );
    assert!(
        section.texture.is_none(),
        "the previous line's picture is still on screen under the new line's \
             caption"
    );
    assert_eq!(
        section.rendered_for, None,
        "a stale key would stop the dispatcher ever cutting the new line"
    );
    assert_eq!(
        section.unavailable, None,
        "a reason from the previous line outlived its cause"
    );
}

/// A minimal scan moment for `site`, distinguishable by its site name.
fn scan_info_for(site: &'static str) -> rustdar_radar::types::ScanInfo {
    rustdar_radar::types::ScanInfo {
        site: rustdar_radar::sites::RadarSite {
            name: site,
            lat: 35.33,
            lon: -97.27,
            elev: None,
        },
        timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
            .unwrap()
            .and_hms_opt(18, 30, 0)
            .unwrap(),
        vcp_number: 212,
        available_products: vec![RadarProduct::Reflectivity],
        product_elevations: std::collections::HashMap::new(),
        status: String::new(),
    }
}

/// The 3D pane a region lands in adopts the drawing map's site and moment —
/// the exact property the section applier pins two tests up.
///
/// A box is resampled from a *site's* volume, so a target pane that kept its
/// own site would resample its own radar over the box's ground. The fixture
/// is the failure that found this: a KTLX map and a sourceless KICT 3D pane,
/// with room to grow. The destination rule re-aims the sourceless pane, and
/// an applier that wrote only the region would leave it sampling **KICT's**
/// volume over a box centred on KTLX's ground ~220 km away — an empty or
/// sliver grid, captioned KICT. Writing the site is what makes the re-aim
/// mean "show me this map's ground in 3D".
#[test]
fn a_retargeted_3d_pane_takes_the_maps_site_and_moment() {
    let mut gui = wide(2);
    gui.panes[0].site = "KTLX".to_owned();
    gui.panes[0].scan_info = Some(scan_info_for("KTLX"));
    gui.panes[1].site = "KICT".to_owned();
    gui.panes[1].scan_info = Some(scan_info_for("KICT"));
    gui.panes[1].set_kind(crate::pane::PaneKind::Volume);
    // Sourceless: converted from the menu, reset, or restored with a
    // dangling source index — no map fed it.
    gui.panes[1].volume_mut().unwrap().source_pane = None;

    let region = crate::pane::VolumeRegion::new(
        crate::pane::GeoPoint {
            lat: 35.3,
            lon: -97.3,
        },
        40.0,
    )
    .expect("a fixture region must be a point on Earth");
    gui.pending_region = Some(crate::ui_region::PendingRegion {
        source_pane: 0,
        region,
    });
    gui.apply_pending_region();

    assert_eq!(
        gui.pane_count(),
        2,
        "the sourceless pane must be re-aimed, not a sibling grown"
    );
    let pane = gui.pane(1).unwrap();
    assert_eq!(
        pane.site, "KTLX",
        "the pane must follow the map the box was dragged on, or it \
             resamples its own radar over another site's ground"
    );
    assert_eq!(
        pane.scan_info.as_ref().map(|s| s.site.name),
        Some("KTLX"),
        "the moment must come across with the site, as a section's does"
    );
    let volume = pane.volume().expect("the pane is a 3D view");
    assert_eq!(volume.region, Some(region));
    assert_eq!(volume.source_pane, Some(0));
}

/// Escape and Android's back cancel the armed draw — last, below every
/// painted layer, because it is a mode rather than something on screen.
///
/// Being in the chain at all is what stops the back button from exiting the
/// app while a mode is on, which is the reading of a back press least likely
/// to be what was meant.
#[test]
fn a_back_press_cancels_an_armed_draw_after_it_has_closed_every_layer() {
    let mut gui = Gui::new();
    gui.set_section_draw_armed(true);
    gui.drawer_open = true;

    assert!(gui.dismiss_top_layer(), "the drawer was open");
    assert!(
        gui.section_draw_armed(),
        "closing the drawer must not also disarm: one layer per press"
    );
    assert!(gui.dismiss_top_layer(), "the mode was armed");
    assert!(!gui.section_draw_armed());
    assert!(
        !gui.dismiss_top_layer(),
        "with nothing left, a back press is a request to leave the app"
    );
}

/// Converting a pane keeps everything it was looking at, and tears down the
/// one thing a non-map pane cannot have: a running animation loop.
///
/// The root fix for a family of eight consumers with one cause. A loop left
/// running on a pane nothing renders frames for is not idle: it blocks every
/// *other* pane's loop through `sync_loop_playback_start`'s all-or-nothing
/// rule, keeps `Gui::any_loop_active` true so the event loop wakes at loop
/// frame rate, reads "Rendering n/m" for ever with no transport drawn to
/// cancel it, and goes on spending the shared download budget. Enforced at the
/// transition so the state is not representable, rather than filtered at each
/// consumer. `SwitchRadarSite` resets `loop_state` for the same reason.
///
/// The counterweight matters as much: every *other* field must survive, which
/// is the promise `set_kind` exists to make.
#[test]
fn converting_a_pane_tears_down_its_loop_and_nothing_else() {
    use crate::pane::{LoopPhase, PaneKind};

    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        let mut gui = Gui::new();
        {
            let pane = gui.pane_mut(0).unwrap();
            pane.site = "KDDC".to_owned();
            pane.selected_product = RadarProduct::Velocity;
            pane.selected_elevation = 1.5;
            pane.viewing_live = false;
            pane.time_step_secs = 1800;
            pane.loop_state.phase = LoopPhase::Playing;
            assert!(
                pane.loop_state.is_active(),
                "precondition: the loop must be running, or there is nothing \
                     to tear down"
            );
        }

        gui.pane_mut(0).unwrap().set_kind(kind);

        let pane = gui.pane(0).unwrap();
        assert!(
            !pane.loop_state.is_active(),
            "{kind:?}: the loop survived, so it will hold every other pane's \
                 loop back and never finish"
        );
        assert_eq!(pane.site, "KDDC", "{kind:?}: the site went with the loop");
        assert_eq!(pane.selected_product, RadarProduct::Velocity);
        assert_eq!(pane.selected_elevation, 1.5);
        assert!(!pane.viewing_live);
        assert_eq!(pane.time_step_secs, 1800);

        // …and converting back does not resurrect it. A torn-down loop is torn
        // down; re-enabling it is the transport's job.
        gui.pane_mut(0).unwrap().set_kind(PaneKind::Map);
        assert!(!gui.pane(0).unwrap().loop_state.is_active());
    }
}

/// Overlay auto-poll and the pane a fetch is attributed to both skip panes
/// with no map, while the panes keep their layer toggles.
///
/// Both questions are "is this overlay being *drawn* anywhere?", and every
/// overlay is a layer over map tiles positioned against a projector a non-map
/// pane does not have — so a converted pane must not keep an auto-poll timer
/// alive or be handed a `FetchOverlay`.
///
/// `enabled_overlays` is deliberately *not* cleared, which is the second half
/// here: it is the user's remembered answer to "which layers do I want", it
/// becomes meaningful again the moment the pane converts back, and it is the
/// same choice `set_kind` makes about the viewport and the tilt.
#[test]
fn overlay_polling_skips_panes_with_no_map_but_keeps_their_toggles() {
    use crate::pane::PaneKind;

    let kind = OverlayKind::CityLabels;
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    for idx in 0..2 {
        gui.pane_mut(idx)
            .unwrap()
            .enabled_overlays
            .insert(kind, true);
    }
    assert!(
        gui.any_pane_has_overlay_enabled(kind),
        "precondition: two map panes want the layer"
    );
    assert_eq!(gui.first_pane_with_overlay_enabled(kind), Some(0));

    gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
    assert_eq!(
        gui.first_pane_with_overlay_enabled(kind),
        Some(1),
        "a fetch was attributed to a pane that cannot draw the overlay"
    );
    assert!(gui.any_pane_has_overlay_enabled(kind));

    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    assert!(
        !gui.any_pane_has_overlay_enabled(kind),
        "no pane on screen can draw this overlay, yet its auto-poll timer is \
             still being kept alive"
    );
    assert_eq!(gui.first_pane_with_overlay_enabled(kind), None);

    // The toggles themselves are untouched, so converting back restores the
    // layer rather than losing the user's choice.
    for idx in 0..2 {
        assert!(
            gui.pane(idx).unwrap().is_overlay_enabled(kind),
            "pane {idx} lost its remembered layer choice"
        );
    }
    gui.pane_mut(0).unwrap().set_kind(PaneKind::Map);
    assert_eq!(gui.first_pane_with_overlay_enabled(kind), Some(0));
}

/// A pane with no map neither drives the shared viewport nor follows it.
///
/// This is the all-panes site that goes live the instant a non-map pane can
/// exist, and it fails in the direction that looks like a bug in the *other*
/// panes. `render_panes` hands the active pane's `map_memory` to
/// `InteractionState::resolve_active` whatever kind the pane is, and on the
/// touch path `TouchGestures::update` writes a zoom into it — so a
/// double-tap-drag on a section pane moves a viewport nothing draws.
/// Unfiltered, `sync_viewports` then reads that pane as the **source**,
/// because it is the first whose zoom moved, and re-centres and re-zooms
/// every map pane on screen. `viewport_sync` defaults *on*, so this is the
/// shipped default rather than something a user opts into.
///
/// Both directions are asserted, and each one fails on its own: the source
/// scan skipping non-map panes, and the write loop skipping them. The second
/// matters because a converted pane's viewport is what it comes back to —
/// `a_converted_pane_keeps_its_site_and_viewport` is the promise — and it is
/// persisted per pane.
#[test]
fn a_pane_with_no_map_neither_drives_nor_follows_the_shared_viewport() {
    use crate::pane::PaneKind;

    // Zoom 4.0 is `DEFAULT_PANE_ZOOM`; 4.0 +/- 2.0 is well inside walkers'
    // accepted range, so `set_zoom` below cannot silently clamp and turn a
    // real move into no move at all.
    let moved_to = 6.0;
    let untouched = 4.0;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(3);
    gui.viewport_sync = true;
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    for idx in 0..3 {
        assert_eq!(
            gui.pane(idx).unwrap().map_memory.zoom(),
            untouched,
            "precondition: every pane starts at the same zoom"
        );
    }

    // The gesture: the *section* pane's viewport moved and nobody else's,
    // exactly as a double-tap-drag on it leaves things.
    gui.pane_mut(1)
        .unwrap()
        .map_memory
        .set_zoom(moved_to)
        .expect("precondition: the test zoom must be in range");
    assert_eq!(
        gui.pane(1).unwrap().map_memory.zoom(),
        moved_to,
        "precondition: walkers clamped the test zoom, so nothing moved"
    );

    gui.sync_viewports(&[untouched; 3], &[None; 3]);

    assert_eq!(
        (0..3)
            .map(|idx| gui.pane(idx).unwrap().map_memory.zoom())
            .collect::<Vec<_>>(),
        vec![untouched, moved_to, untouched],
        "a gesture on a pane with no map re-zoomed the map panes to it"
    );

    // The same pane as the *target*: now a map pane moves, and the section
    // pane must not be dragged along with the other map.
    gui.pane_mut(0)
        .unwrap()
        .map_memory
        .set_zoom(7.0)
        .expect("in range");
    gui.sync_viewports(&[untouched, moved_to, untouched], &[None; 3]);
    assert_eq!(
        (0..3)
            .map(|idx| gui.pane(idx).unwrap().map_memory.zoom())
            .collect::<Vec<_>>(),
        vec![7.0, moved_to, 7.0],
        "the section pane's own viewport was overwritten by the sync"
    );
}

/// With nothing moved and a non-map pane active, there is no source at all.
///
/// The fallback used to be `source_idx.unwrap_or(self.active_pane)`, which
/// made a non-map active pane the source on *every* frame — the same failure
/// as the source scan, reached with no interaction whatsoever, and therefore
/// the more likely of the two to be seen.
#[test]
fn a_non_map_active_pane_is_not_the_fallback_sync_source() {
    use crate::pane::PaneKind;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.viewport_sync = true;
    gui.pane_mut(1).unwrap().set_kind(PaneKind::Volume);
    gui.active_pane = 1;

    // Deliberately out of step with pane 0, and deliberately *not* reported
    // as moved: `pre_zooms` says nothing changed this frame, so the only way
    // this value can escape is through the no-source fallback.
    gui.pane_mut(1)
        .unwrap()
        .map_memory
        .set_zoom(9.0)
        .expect("in range");

    gui.sync_viewports(&[4.0, 9.0], &[None; 2]);

    assert_eq!(
        gui.pane(0).unwrap().map_memory.zoom(),
        4.0,
        "the active pane has no map, so its viewport propagated to a map \
             pane that nothing had interacted with"
    );
}

/// Loop actions never target a pane that draws no plan-view frames.
///
/// A loop frame *is* a rendered plan-view tilt, and
/// `App::dispatch_loop_renders` skips panes with no plan view — so a
/// non-map pane in this list would be put into `is_active()` with a frame
/// list nothing ever fills: a loop transport stuck at "waiting", and a
/// download queue fetching volumes for a pane nobody is looking at.
///
/// The active pane is included without being asked, which the second half
/// below pins. The caller is now the floating timeline, outside every
/// `mem::take` window, but the unconditional include stands: it is the
/// pane whose own toggle was clicked, and the timeline disables that
/// toggle for a non-map active pane — see `loop_sync_targets`' own note.
#[test]
fn loop_actions_skip_panes_that_draw_no_frames() {
    use crate::pane::PaneKind;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    gui.sync_layers = true;
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    gui.pane_mut(2).unwrap().set_kind(PaneKind::Volume);

    assert_eq!(gui.loop_sync_targets(), vec![0, 3]);

    // Sync off narrows to the active pane, whatever kind it is: it is the
    // pane whose own checkbox was clicked.
    gui.sync_layers = false;
    gui.active_pane = 2;
    assert_eq!(gui.loop_sync_targets(), vec![2]);

    // And with sync back on, the active pane is still in the list even
    // though its slot says it is not a map — because the index is included
    // rather than tested.
    gui.sync_layers = true;
    assert_eq!(gui.loop_sync_targets(), vec![0, 2, 3]);
}

/// The graphics-state reset reaches panes of every kind, including the ones
/// the layout is not currently showing.
///
/// [`Gui::clear_graphics_state`] is the only place a pane-held
/// `egui::TextureHandle` is released when the egui context dies, and
/// `PaneContent::release_textures` is called from inside this same loop —
/// so if the loop skipped non-map panes, or stopped at the visible count,
/// that guard would read as covered while never running. Asserted through
/// `radar_sites_render_gen`, which the loop bumps on its way past: it is a
/// side effect of *this* loop body, so it cannot agree with a loop that
/// stopped short.
///
/// Hidden panes are included deliberately. A handle belonging to a pane the
/// user split away from is just as invalid once the context is gone, and a
/// re-split would hand it straight back to the renderer.
#[test]
fn clearing_graphics_state_reaches_panes_of_every_kind() {
    use crate::pane::PaneKind;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    gui.pane_mut(2).unwrap().set_kind(PaneKind::Volume);
    // Split back down, so panes 2 and 3 are remembered but not shown.
    gui.set_pane_count_for_test(2);

    let before: Vec<u64> = gui
        .panes
        .iter()
        .map(|pane| pane.radar_sites_render_gen)
        .collect();
    assert_eq!(before.len(), 4, "precondition: four panes to reach");
    assert_eq!(
        gui.panes.iter().map(|pane| pane.kind()).collect::<Vec<_>>(),
        [
            PaneKind::Map,
            PaneKind::CrossSection,
            PaneKind::Volume,
            PaneKind::Map
        ],
        "precondition: one pane of each kind, two of them hidden"
    );

    gui.clear_graphics_state();

    for (idx, was) in before.iter().enumerate() {
        assert_eq!(
            gui.panes[idx].radar_sites_render_gen,
            was + 1,
            "pane {idx} ({:?}) was not reached by the graphics-state reset, \
                 so nothing released whatever its kind is holding",
            gui.panes[idx].kind(),
        );
    }
}
