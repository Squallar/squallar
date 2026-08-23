use super::*;
use rustdar_radar::fields as radar_fields;

/// Splitting to fewer panes leaves the extra `PaneState`s in the vector so a
/// re-split can restore them. They are not drawn and not updated, so the
/// "every pane" slice must stop at the layout's count — otherwise a polled
/// scan appends loop frames to panes nobody is looking at.
#[test]
fn the_pane_slices_stop_at_the_visible_count() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    for (idx, pane) in gui.panes_mut().iter_mut().enumerate() {
        pane.set_site(format!("PANE{idx}"));
    }

    gui.set_pane_count_for_test(2);

    assert_eq!(gui.panes().len(), 2);
    assert_eq!(gui.panes_mut().len(), 2);
    assert_eq!(
        gui.panes().iter().map(|p| p.site()).collect::<Vec<_>>(),
        ["PANE0", "PANE1"],
    );
    assert_eq!(
        gui.pane(3).map(|p| p.site()),
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
    gui.claim_pane_count_for_test(4);

    assert_eq!(gui.panes().len(), 1);
    assert_eq!(gui.panes_mut().len(), 1);
}

/// **The pin under the pane-loop conversions in `rustdar-app`'s `app_render.rs`.**
///
/// Those loops walked `0..gui.pane_count()` and dropped every index whose
/// `pane`/`pane_mut` answered `None`; they now walk `panes()`/`panes_mut()`
/// straight. The two forms are interchangeable only if they visit the **same
/// panes in the same order**, and that is a property of `visible_pane_count`'s
/// clamp rather than of either accessor alone — so it is asserted here instead
/// of argued in a comment. Nothing else in the tree checks it: the neighbours
/// above pin slice *lengths* and no-panic behaviour, which both traversals
/// satisfy for reasons that have nothing to do with agreeing with each other.
///
/// Both skew directions are covered. Production only ever produces the second
/// one — `set_pane_count` and `load_ui_config` grow the vector before they move
/// the layout — and the first is the case the clamp exists for.
#[test]
fn the_index_walk_and_the_slice_walk_visit_the_same_panes() {
    /// Both traversals, over all four accessors, identifying panes by a field
    /// that survives the walk rather than by address.
    fn walks(gui: &mut Gui) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
        let by_index: Vec<String> = (0..gui.pane_count())
            .filter_map(|idx| gui.pane(idx))
            .map(|pane| pane.site().to_string())
            .collect();
        let by_slice: Vec<String> = gui
            .panes()
            .iter()
            .map(|pane| pane.site().to_string())
            .collect();

        let mut by_index_mut: Vec<String> = Vec::new();
        for idx in 0..gui.pane_count() {
            if let Some(pane) = gui.pane_mut(idx) {
                by_index_mut.push(pane.site().to_string());
            }
        }
        let by_slice_mut: Vec<String> = gui
            .panes_mut()
            .iter_mut()
            .map(|pane| pane.site().to_string())
            .collect();

        (by_index, by_slice, by_index_mut, by_slice_mut)
    }

    // Direction 1: the layout claims more panes than the vector holds. This is
    // the case `visible_pane_count`'s `.min` exists for.
    let mut claimed = Gui::new();
    claimed.set_pane_count_for_test(2);
    for (idx, pane) in claimed.panes_mut().iter_mut().enumerate() {
        pane.set_site(format!("CLAIM{idx}"));
    }
    claimed.claim_pane_count_for_test(4);
    assert_eq!(
        claimed.pane_count(),
        4,
        "precondition: the layout claims four panes"
    );
    assert_eq!(
        claimed.panes().len(),
        2,
        "precondition: the vector holds two, so the claim really does outrun it"
    );

    let (index, slice, index_mut, slice_mut) = walks(&mut claimed);
    assert_eq!(
        index, slice,
        "the index walk and the slice walk visit different panes when the \
         layout outruns the vector"
    );
    assert_eq!(
        index_mut, slice_mut,
        "`pane_mut` by index and `panes_mut` visit different panes when the \
         layout outruns the vector"
    );
    assert_eq!(
        index, index_mut,
        "the shared and mutable index walks disagree"
    );
    assert_eq!(index, ["CLAIM0", "CLAIM1"]);
    let claimed_len = index.len();

    // Direction 2: the vector holds more panes than the layout shows — what a
    // split-down leaves behind, and the direction production actually produces.
    let mut shrunk = Gui::new();
    shrunk.set_pane_count_for_test(4);
    for (idx, pane) in shrunk.panes_mut().iter_mut().enumerate() {
        pane.set_site(format!("SHRINK{idx}"));
    }
    shrunk.set_pane_count_for_test(3);
    assert_eq!(
        shrunk.pane_count(),
        3,
        "precondition: the layout shows three panes"
    );
    assert_eq!(
        shrunk.pane(3).map(|pane| pane.site()),
        Some("SHRINK3"),
        "precondition: the fourth pane is still in the vector, reachable by index"
    );

    let (index, slice, index_mut, slice_mut) = walks(&mut shrunk);
    assert_eq!(
        index, slice,
        "the index walk and the slice walk visit different panes when the \
         vector outruns the layout"
    );
    assert_eq!(
        index_mut, slice_mut,
        "`pane_mut` by index and `panes_mut` visit different panes when the \
         vector outruns the layout"
    );
    assert_eq!(
        index, index_mut,
        "the shared and mutable index walks disagree"
    );
    assert_eq!(index, ["SHRINK0", "SHRINK1", "SHRINK2"]);
    let shrunk_len = index.len();

    // Non-triviality. A `Gui` holding no panes satisfies every assertion above
    // vacuously, and two directions of equal length would let one hard-coded
    // bound stand in for the clamp being correct in either.
    assert!(
        claimed_len > 0 && shrunk_len > 0,
        "both directions must visit at least one pane, or the equality above \
         is between two empty vectors: {claimed_len}, {shrunk_len}"
    );
    assert_ne!(
        claimed_len, shrunk_len,
        "the two skew directions must land on different lengths, or a single \
         fixed bound satisfies both and the clamp is never exercised"
    );
}

/// The rects a test clicks are the rects the frame drew, so the helper that
/// produces them takes the visible slice's bound too. With the raw count it
/// handed back a rect per *claimed* pane, and a test clicking the last of them
/// would have been driving a pane no frame ever rendered.
#[test]
fn the_pane_rects_a_test_sees_are_only_the_ones_a_frame_drew() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.probes.last_map_panel_rect =
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
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
    gui.claim_pane_count_for_test(4);

    gui.sync_viewports(&[0.0; 4], &[None; 4]);

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
/// windows — the top bar takes no pane, and the shell's stack+inspector take
/// opens only after it has dispatched — so a direct write from
/// `apply_menu_event` would pass every behavioural test in the suite, this
/// one included, if this one did not hold the pane out by hand. (The
/// inspector's kind segmented control, by contrast, dispatches from *inside*
/// that take, which is this deferral earning its keep in production.)
///
/// That makes this a test of the *mechanism* and not of user-visible
/// behaviour, which is a thing worth saying out loud: it is here because
/// WP-G's writers run inside `render_panes`' take, where the same direct write
/// is silently discarded, and a test written after that code would be a test
/// written after the bug. Driven through `apply_menu_event` rather than
/// `request_pane_view` so it covers the arm and the deferral together. The
/// end-to-end behavioural version, which passes either way, is
/// `converting_the_active_pane_from_the_dropdown_makes_it_a_volume_pane`.
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
    gui.pane_mut(1).unwrap().set_site("KDDC".to_owned());

    let held = std::mem::take(&mut gui.panes[gui.active_pane]);
    assert_eq!(
        gui.panes[1].site(),
        "KTLX",
        "precondition: the slot now holds a default PaneState, which is what \
             makes a direct write vanish"
    );

    let mut actions = Vec::new();
    gui.apply_menu_event(
        MenuEvent::Toggled(MenuToggle::VolumePane, true),
        &mut actions,
    );

    gui.panes[gui.active_pane] = held;
    gui.apply_pending_pane_view(&mut Vec::new());

    assert_eq!(
        gui.pane(1).unwrap().site(),
        "KDDC",
        "precondition: the original pane must be the one back in the slot"
    );
    assert_eq!(
        gui.pane(1).unwrap().render_view(),
        rustdar_radar::types::RenderView::Volume,
        "the conversion was written to the pane that was held out and thrown \
             away, so the menu item silently did nothing"
    );
    assert_eq!(
        gui.pending_pane_view_for_test(),
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
#[test]
fn a_pane_kind_request_for_a_pane_that_is_gone_converts_nothing() {
    use crate::pane::PaneKind;

    let mut gui = Gui::new();
    gui.request_pane_view(7, rustdar_radar::types::RenderView::Volume);
    gui.apply_pending_pane_view(&mut Vec::new());

    assert_eq!(gui.pane(0).unwrap().kind(), PaneKind::Map);
    assert_eq!(gui.pending_pane_view_for_test(), None);
}

/// A line for the target rule to place, and the pane it was drawn on.
fn drawn_line() -> crate::pane::SectionLine {
    crate::pane::SectionLine::new(
        rustdar_geo::GeoPoint {
            lat: 35.0,
            lon: -97.8,
        },
        rustdar_geo::GeoPoint {
            lat: 35.6,
            lon: -96.9,
        },
    )
    .expect("a fixture line must be finite and have two distinct ends")
}

/// A cut of the right shape and no content, so a fixture can hold a picture
/// for a retarget to throw away.
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
        vec![0],
    )
    .expect("a full-size, all-NoCoverage section is well formed")
}

/// A second line, distinguishable from [`drawn_line`], for a section that
/// belongs to another map and must be left alone.
fn other_line() -> crate::pane::SectionLine {
    crate::pane::SectionLine::new(
        rustdar_geo::GeoPoint {
            lat: 40.0,
            lon: -100.0,
        },
        rustdar_geo::GeoPoint {
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
#[test]
fn a_retargeted_section_takes_the_maps_site_and_drops_the_old_picture() {
    let ctx = egui::Context::default();
    let mut gui = wide(2);
    gui.panes[0].set_site("KTLX".to_owned());
    gui.panes[0].set_selected_product(radar_fields::known::VELOCITY);
    gui.panes[1].set_site("KINX".to_owned());
    gui.panes[1].set_kind(crate::pane::PaneKind::CrossSection);
    {
        let section = gui.panes[1].cross_section_mut().unwrap();
        section.source_pane = Some(0);
        section.unavailable = Some(crate::pane::SectionUnavailable::RenderFailed);
        section.rendered_for = Some(crate::pane::SectionTarget {
            volume: crate::pane::VolumeStamp {
                site: "KINX".to_owned(),
                collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                    .unwrap()
                    .and_hms_opt(18, 30, 0)
                    .unwrap(),
            },
            product: radar_fields::known::REFLECTIVITY,
            line: other_line(),
            ladder: 9,
        });
        section.section = Some(std::sync::Arc::new(blank_section()));
        section.texture = Some(ctx.load_texture(
            "retarget-fixture",
            egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        ));
    }

    gui.pending_section_line = Some((0, drawn_line()));
    gui.apply_pending_section_line();

    let pane = gui.pane(1).unwrap();
    assert_eq!(pane.site(), "KTLX");
    assert_eq!(pane.selected_product(), radar_fields::known::VELOCITY);
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

/// Escape and Android's back cancel the armed draw — last, below every
/// painted layer, because it is a mode rather than something on screen.
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
#[test]
fn converting_a_pane_tears_down_its_loop_and_nothing_else() {
    use crate::pane::LoopPhase;

    for view in [
        rustdar_radar::types::RenderView::CrossSection,
        rustdar_radar::types::RenderView::Volume,
    ] {
        let mut gui = Gui::new();
        {
            let pane = gui.pane_mut(0).unwrap();
            pane.set_site("KDDC".to_owned());
            pane.set_selected_product(radar_fields::known::VELOCITY);
            pane.set_selected_elevation(1.5);
            pane.viewing_live = false;
            pane.time.step = crate::pane::TimeStep::from_secs(1800);
            pane.time_state_mut(&known::RADAR).phase = LoopPhase::Playing;
            assert!(
                pane.time_state(&known::RADAR).is_active(),
                "precondition: the loop must be running, or there is nothing \
                     to tear down"
            );
        }

        gui.pane_mut(0).unwrap().set_view(view);

        let pane = gui.pane(0).unwrap();
        assert!(
            !pane.time_state(&known::RADAR).is_active(),
            "{view:?}: the loop survived, so it will hold every other pane's \
                 loop back and never finish"
        );
        assert_eq!(pane.site(), "KDDC", "{view:?}: the site went with the loop");
        assert_eq!(pane.selected_product(), radar_fields::known::VELOCITY);
        assert_eq!(pane.selected_elevation(), 1.5);
        assert!(!pane.viewing_live);
        assert_eq!(pane.time.step.as_secs(), 1800);

        gui.pane_mut(0)
            .unwrap()
            .set_view(rustdar_radar::types::RenderView::PlanView);
        assert!(!gui.pane(0).unwrap().time_state(&known::RADAR).is_active());
    }
}

/// Overlay auto-poll and the pane a fetch is attributed to both skip panes
/// with no **ground**, while the panes keep their layer toggles.
#[test]
fn overlay_polling_skips_panes_with_no_ground_but_keeps_their_toggles() {
    use crate::pane::PaneKind;

    let kind = rustdar_source::id::known::CITY_LABELS;
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    for idx in 0..2 {
        gui.pane_mut(idx)
            .unwrap()
            .set_overlay_enabled(kind.clone(), true);
    }
    assert!(
        gui.any_pane_has_overlay_enabled(&kind),
        "precondition: two map panes want the layer"
    );
    assert_eq!(gui.first_pane_with_overlay_enabled(&kind), Some(0));

    gui.pane_mut(0)
        .unwrap()
        .set_view(rustdar_radar::types::RenderView::Volume);
    assert_eq!(
        gui.first_pane_with_overlay_enabled(&kind),
        Some(0),
        "the fetch skipped the 3D pane whose floor draws this very layer",
    );
    assert!(gui.any_pane_has_overlay_enabled(&kind));

    gui.pane_mut(0)
        .unwrap()
        .volume_mut()
        .expect("a 3D pane has volume state")
        .hide_floor = true;
    assert_eq!(
        gui.first_pane_with_overlay_enabled(&kind),
        Some(1),
        "a fetch was attributed to a pane with no surface to draw it on"
    );
    assert!(gui.any_pane_has_overlay_enabled(&kind));

    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    assert!(
        !gui.any_pane_has_overlay_enabled(&kind),
        "no pane on screen can draw this overlay, yet its auto-poll timer is \
             still being kept alive"
    );
    assert_eq!(gui.first_pane_with_overlay_enabled(&kind), None);

    for idx in 0..2 {
        assert!(
            gui.pane(idx).unwrap().is_overlay_enabled(&kind),
            "pane {idx} lost its remembered layer choice"
        );
    }
    gui.pane_mut(0)
        .unwrap()
        .set_view(rustdar_radar::types::RenderView::PlanView);
    assert_eq!(gui.first_pane_with_overlay_enabled(&kind), Some(0));
}

/// A loop on a pane the layout no longer shows is not "active": it is
/// stranded, and saying otherwise pins the event loop at loop frame rate for
/// the life of the process.
#[test]
fn a_loop_on_a_hidden_pane_stops_holding_the_event_loop_awake() {
    use crate::pane::LoopPhase;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(1).unwrap().time_state_mut(&known::RADAR).phase = LoopPhase::Playing;
    assert!(
        gui.any_loop_active(),
        "precondition: a loop is playing on a pane that is on screen"
    );

    gui.set_pane_count_for_test(1);

    assert!(
        gui.pane(1).unwrap().time_state(&known::RADAR).is_active(),
        "precondition: the hidden pane kept its loop, which is what makes \
             this worth guarding"
    );
    assert!(
        !gui.any_loop_active(),
        "a loop on a pane no frame draws is holding the event loop at loop \
             frame rate, for an animation nothing advances"
    );
}

/// A pane with no map neither drives the shared viewport nor follows it.
#[test]
fn a_pane_with_no_map_neither_drives_nor_follows_the_shared_viewport() {
    use crate::pane::PaneKind;

    let moved_to = 6.0;
    let untouched = 4.0;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(3);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    for idx in 0..3 {
        assert_eq!(
            gui.pane(idx).unwrap().map_memory.zoom(),
            untouched,
            "precondition: every pane starts at the same zoom"
        );
    }

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
#[test]
fn a_non_map_active_pane_is_not_the_fallback_sync_source() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(1)
        .unwrap()
        .set_view(rustdar_radar::types::RenderView::Volume);
    gui.active_pane = 1;

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

/// M11-1. **The viewport group is per-pane: a move on a linked pane drives
/// the linked panes and only them; a move on an unlinked pane moves nobody
/// else.**
#[test]
fn the_viewport_group_is_per_pane_on_both_ends() {
    let untouched = 4.0;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(3);
    gui.pane_mut(1).unwrap().viewport_link = false;

    gui.pane_mut(0)
        .unwrap()
        .map_memory
        .set_zoom(6.0)
        .expect("in range");
    gui.sync_viewports(&[untouched; 3], &[None; 3]);
    assert_eq!(
        (0..3)
            .map(|idx| gui.pane(idx).unwrap().map_memory.zoom())
            .collect::<Vec<_>>(),
        vec![6.0, untouched, 6.0],
        "a linked move must reach the linked panes and skip the unlinked one"
    );

    gui.pane_mut(1)
        .unwrap()
        .map_memory
        .set_zoom(8.0)
        .expect("in range");
    gui.sync_viewports(&[6.0, untouched, 6.0], &[None; 3]);
    assert_eq!(
        (0..3)
            .map(|idx| gui.pane(idx).unwrap().map_memory.zoom())
            .collect::<Vec<_>>(),
        vec![6.0, 8.0, 6.0],
        "an unlinked move must stay local - and must not hand the frame to \
             the active-pane hold either"
    );
}

/// M11-2. **An unlinked active pane holds nobody.**
#[test]
fn an_unlinked_active_pane_is_not_the_fallback_hold_source() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.active_pane = 0;
    gui.pane_mut(0).unwrap().viewport_link = false;
    gui.pane_mut(0)
        .unwrap()
        .map_memory
        .set_zoom(9.0)
        .expect("in range");

    gui.sync_viewports(&[9.0, 4.0], &[None; 2]);

    assert_eq!(
        gui.pane(1).unwrap().map_memory.zoom(),
        4.0,
        "the active pane's viewport link is off, so it must not hold the \
             linked pane to its own viewport"
    );
}

/// Loop actions target every pane that can animate, and only those.
#[test]
fn loop_actions_skip_panes_that_draw_no_frames() {
    use crate::pane::{PaneKind, SectionLine};
    use rustdar_geo::GeoPoint;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    gui.pane_mut(2)
        .unwrap()
        .set_view(rustdar_radar::types::RenderView::Volume);

    assert_eq!(
        gui.loop_sync_targets(),
        vec![0, 2, 3],
        "an unaimed section pane is not a loop target — it has no line, so its \
         frames could never be cut and the batch would never settle — while a \
         3D pane is one from the start, having nothing to be aimed at"
    );

    gui.pane_mut(1).unwrap().cross_section_mut().unwrap().line = SectionLine::new(
        GeoPoint {
            lat: 35.0,
            lon: -98.0,
        },
        GeoPoint {
            lat: 36.0,
            lon: -97.0,
        },
    );
    assert_eq!(
        gui.loop_sync_targets(),
        vec![0, 1, 2, 3],
        "an aimed section pane was left out of the loop fan-out, so enabling \
         the loop animates every pane beside it and not this one"
    );
    assert!(gui.loop_sync_targets().contains(&2));

    for idx in [0, 1, 3] {
        gui.pane_mut(idx).unwrap().time_link = false;
    }
    gui.active_pane = 2;
    assert_eq!(gui.loop_sync_targets(), vec![2]);

    for idx in [0, 1, 3] {
        gui.pane_mut(idx).unwrap().time_link = true;
    }
    assert_eq!(gui.loop_sync_targets(), vec![0, 1, 2, 3]);
}

/// The composed fan-out rule.
#[test]
fn an_unlinked_pane_is_no_loop_target_whatever_its_kind() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    gui.pane_mut(2)
        .unwrap()
        .set_view(rustdar_radar::types::RenderView::Volume);
    gui.active_pane = 0;

    gui.pane_mut(2).unwrap().time_link = false;
    assert_eq!(
        gui.loop_sync_targets(),
        vec![0, 1, 3],
        "an unlinked volume pane was fanned out to: `can_loop` must not \
         override the pane's own time link"
    );

    gui.pane_mut(3).unwrap().time_link = false;
    assert_eq!(gui.loop_sync_targets(), vec![0, 1]);
}

/// The graphics-state reset reaches panes of every kind, including the ones
/// the layout is not currently showing.
#[test]
fn clearing_graphics_state_reaches_panes_of_every_kind() {
    use crate::pane::PaneKind;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    gui.pane_mut(2)
        .unwrap()
        .set_view(rustdar_radar::types::RenderView::Volume);
    gui.set_pane_count_for_test(2);

    let before: Vec<u64> = gui
        .panes
        .iter()
        .map(|pane| pane.radar_sites_render_gen)
        .collect();
    assert_eq!(before.len(), 4, "precondition: four panes to reach");
    assert_eq!(
        gui.panes
            .iter()
            .map(|pane| pane.render_view())
            .collect::<Vec<_>>(),
        [
            rustdar_radar::types::RenderView::PlanView,
            rustdar_radar::types::RenderView::CrossSection,
            rustdar_radar::types::RenderView::Volume,
            rustdar_radar::types::RenderView::PlanView
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

/// A 1x1 overlay raster — the picture every non-radar animating layer holds in
/// its frames, wrapping a `TextureHandle` minted by `ctx`, which is the device
/// that goes away below.
fn device_raster(ctx: &egui::Context, name: &str) -> crate::overlay_cache::OverlayTextureData {
    crate::overlay_cache::OverlayTextureData {
        texture: ctx.load_texture(
            name.to_owned(),
            egui::ColorImage::filled([1, 1], egui::Color32::RED),
            egui::TextureOptions::default(),
        ),
        placed: rustdar_geo::PlacedRaster::of(rustdar_geo::GeoBounds {
            min_lat: 34.0,
            max_lat: 36.0,
            min_lon: -98.0,
            max_lon: -96.0,
        }),
        data_generation: 0,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map: None,
    }
}

/// Radar's own frame picture, a plan-view render — the one the reset already
/// reached before WO-T3.5, and the floor it must not trade away.
fn device_plan_view(ctx: &egui::Context) -> crate::pane::RadarImageData {
    crate::pane::RadarImageData {
        texture: ctx.load_texture(
            "radar".to_owned(),
            egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        ),
        lat: 35.0,
        lon: -97.0,
        max_range_km: 100.0,
        placed: rustdar_radar::types::ImageBounds::from_radar_site(35.0, -97.0, 100.0).into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: std::sync::Arc::new(rustdar_radar::hover::HoverSource::empty()),
    }
}

/// Arm `id` on `pane` with one frame carrying `image` and one the renderer is
/// still filling.
fn arm_with_picture(
    pane: &mut crate::pane::PaneState,
    id: &rustdar_source::id::LayerId,
    image: crate::pane::LoopFrameImage,
) {
    let ls = pane.time_state_mut(id);
    ls.phase = crate::pane::LoopPhase::Playing;
    ls.frames = vec![
        crate::pane::LoopFrame {
            timestamp: loop_ts(0),
            image: Some(image),
            render_in_flight: false,
            render_failed: false,
        },
        crate::pane::LoopFrame {
            timestamp: loop_ts(5),
            image: None,
            render_in_flight: true,
            render_failed: false,
        },
    ];
}

/// **WO-T3.5 — a device loss drops every animating layer's frame textures.**
///
/// The reset walked radar's slot by name, so a
/// satellite or model loop on the same pane came out of a surface loss still
/// holding `LoopFrameImage::Overlay` handles minted by the dead device — while
/// the three lines below it already released the *live* cache of those very
/// same layers generically.
///
/// **Floor** — radar's frames are asserted dropped in the same pass, so
/// widening the walk cannot be paid for by narrowing it.
#[test]
fn a_device_loss_drops_every_animating_layers_frame_textures() {
    use rustdar_source::id::known;

    let ctx = egui::Context::default();
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(1);

    let pane = gui.pane_mut(0).unwrap();
    arm_with_picture(
        pane,
        &known::RADAR,
        crate::pane::LoopFrameImage::PlanView(device_plan_view(&ctx)),
    );
    for (id, name) in [(known::GMGSI, "gmgsi"), (known::MODEL_DATA, "model")] {
        arm_with_picture(
            pane,
            &id,
            crate::pane::LoopFrameImage::Overlay(device_raster(&ctx, name)),
        );
    }

    let armed = [known::RADAR, known::GMGSI, known::MODEL_DATA];
    for id in &armed {
        let ls = gui.pane(0).unwrap().time_state(id);
        assert!(
            ls.is_active() && ls.frames.iter().any(|f| f.image.is_some()),
            "precondition: {id:?} holds a picture minted by the device that is \
             about to go away",
        );
        assert!(
            ls.frames.iter().any(|f| f.render_in_flight),
            "precondition: {id:?} has a render in flight to abandon",
        );
    }

    gui.clear_graphics_state();

    for id in &armed {
        let ls = gui.pane(0).unwrap().time_state(id);
        assert!(
            ls.frames.iter().all(|f| f.image.is_none()),
            "{id:?} came out of a graphics-device loss still holding a texture \
             handle the dead device minted",
        );
        assert!(
            ls.frames.iter().all(|f| !f.render_in_flight),
            "{id:?} came out of a graphics-device loss still expecting a render \
             the dead device will never deliver",
        );
    }
}

/// A timestamp `n` minutes past a fixed instant, for the loop frames below.
fn loop_ts(minute: i64) -> chrono::NaiveDateTime {
    chrono::DateTime::from_timestamp(1_700_000_000 + minute * 60, 0)
        .expect("a representable instant")
        .naive_utc()
}

fn loop_frame(minute: i64) -> crate::pane::LoopFrame {
    crate::pane::LoopFrame {
        timestamp: loop_ts(minute),
        image: None,
        render_in_flight: false,
        render_failed: false,
    }
}

/// **A layer-link sync moves the stack and leaves every pane where it was on
/// the clock.** Before WO-E7a the timeline was a `PaneState` field the sync
/// could not reach, so a sync could not disturb it; now it lives on the very
/// slots `adopt_layers` replaces, and this is what says the destination pane's
/// own frames and playhead survive the copy.
#[test]
fn a_layer_link_sync_moves_the_stack_and_leaves_every_pane_on_its_own_clock() {
    use rustdar_source::id::known;

    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    // Two panes, two different loops: different lengths and different
    // playheads, so neither assertion below can pass on a coincidence.
    for (idx, pane) in gui.panes.iter_mut().enumerate().take(2) {
        pane.layer_link = true;
        let ls = pane.time_state_mut(&known::RADAR);
        ls.phase = crate::pane::LoopPhase::Playing;
        ls.frames = (0..(4 + idx as i64 * 2)).map(loop_frame).collect();
        pane.park_on_frame(&known::RADAR, 1 + idx * 4);
    }
    // The sync has work to do: the two stacks disagree about a layer.
    gui.panes[0].set_overlay_enabled(known::NWS_ALERTS, false);
    gui.panes[1].set_overlay_enabled(known::NWS_ALERTS, true);
    assert_ne!(
        gui.panes[0].time_state(&known::RADAR).current_frame(),
        gui.panes[1].time_state(&known::RADAR).current_frame(),
        "precondition: the two panes are on different frames",
    );
    assert_ne!(
        gui.panes[0].time_state(&known::RADAR).frames.len(),
        gui.panes[1].time_state(&known::RADAR).frames.len(),
        "precondition: and they hold different numbers of them",
    );

    gui.propagate_pane_sync();

    assert!(
        !gui.panes[1].is_overlay_enabled(&known::NWS_ALERTS),
        "precondition: the sync ran and moved the stack at all",
    );
    assert_eq!(
        gui.panes[1].time_state(&known::RADAR).current_frame(),
        5,
        "pane 1 was moved to the active pane's playhead by a stack copy",
    );
    assert_eq!(
        gui.panes[1].time_state(&known::RADAR).frames.len(),
        6,
        "pane 1's own frames were replaced by the active pane's",
    );
    assert_eq!(
        gui.panes[0].time_state(&known::RADAR).current_frame(),
        1,
        "and the active pane is where it was",
    );
}

/// **One number, every pane.** The timeline window and the playback rate are
/// persisted once at the root of the config file and read per pane, so a
/// setter that reached only the global — or only the sync group — would leave
/// panes animating at a rate the file does not name.
#[test]
fn the_timeline_window_and_the_playback_rate_reach_every_panes_posture() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(4);
    // Nothing starts on the values under test, so neither assertion can pass
    // on a default.
    assert!(
        gui.panes
            .iter()
            .all(|p| p.time.span_secs != 900 && p.time.speed_fps != 12.0),
        "precondition: no pane already holds the values being written",
    );

    gui.set_loop_span_secs(900);
    gui.set_loop_speed_fps(12.0);

    for (idx, pane) in gui.panes.iter().enumerate() {
        assert_eq!(pane.time.span_secs, 900, "pane {idx}'s window");
        assert_eq!(pane.time.speed_fps, 12.0, "pane {idx}'s rate");
    }
    assert_eq!(gui.loop_lookback_secs, 900, "and the persisted window");
    assert_eq!(gui.loop_speed_fps, 12.0, "and the persisted rate");
}
