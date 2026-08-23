//! **The loading state** (WI-7): what a pane shows while a loop's data is on
//! its way — a quantity on the glass instead of a silent blank.
//!
//! Two states, one plate: a frame listing in flight says how long it has been
//! out; a playhead parked on a frame whose picture has not landed says which
//! frame of how many is owed. Both are drawn by `render_map_pane`'s deferred
//! notice slot and read back here off the painted output, never off the
//! helper — a surface that stops reaching the glass must fail these tests.
//!
//! The correctness half is WI-6's and is re-asserted as a control: while
//! loading, the map paints NOTHING for the layer. The loading state is a
//! caption, never a picture.

use super::InputHarness;
use super::loop_overlay_draw_tests::{LAYER, model_loop, painted, raster, scrub_to, ts};
use crate::pane::{LoopFrame, LoopFrameImage, LoopPhase};
use rustdar_source::id::known;

/// The lowercase loading captions pane 0's glass shows this frame. The two
/// notice spellings are lowercase by design; the transport's own lines
/// ("Loading scan list...", "Rendering n/m...") are not, so they cannot leak
/// into this reading.
fn loading_texts(h: &InputHarness) -> Vec<String> {
    h.painted_text_strings_in(h.pane_rects()[0])
        .into_iter()
        .filter(|t| t.starts_with("loading frames") || t.contains("of") && t.ends_with("loading"))
        .collect()
}

/// **A listing in flight reads as loading, not as empty.**
///
/// A refill after a deep scrub re-enters `FetchingScanList` with
/// `listing_since` restamped and no frames to show — the user asked for an
/// instant and the app is getting it, and the glass says so with the one
/// quantity the phase owns: how long the listing has been out.
///
/// **Floor: delete the loading-notice draw block in `render_map_pane` and
/// this fails** — the helper alone cannot keep it green, because the reading
/// is of the painted text.
#[test]
fn a_listing_in_flight_reads_as_loading_not_as_empty() {
    let mut h = InputHarness::new();
    let (live, frames) = model_loop(&mut h);

    {
        let ls = h.gui_mut().panes_mut()[0].time_state_mut(&LAYER);
        ls.phase = LoopPhase::FetchingScanList;
        ls.listing_since = Some(web_time::Instant::now());
        ls.frames.clear();
    }
    h.warm_up();

    let texts = loading_texts(&h);
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("loading frames - ") && t.ends_with('s')),
        "a listing in flight must put the wait quantity on the glass; painted {texts:?}"
    );

    // Control (WI-6's floor C, re-asserted here so this item cannot weaken
    // it): loading is a caption, never a picture.
    let drawn = painted(&h);
    assert!(
        !drawn.contains(&live),
        "a loading pane papered itself over with the live raster"
    );
    for (i, id) in frames.iter().enumerate() {
        assert!(
            !drawn.contains(id),
            "old frame {i} is on the glass while the listing is in flight"
        );
    }
}

/// **A playhead parked on a frame whose picture has not landed names the
/// quantity** — "frame 2 of 3 loading", never an apology and never a stand-in
/// picture.
#[test]
fn the_playheads_unrendered_frame_names_its_quantity() {
    let mut h = InputHarness::new();
    let (live, frames) = model_loop(&mut h);

    {
        let ls = h.gui_mut().panes_mut()[0].time_state_mut(&LAYER);
        ls.frames[1].image = None;
        ls.phase = LoopPhase::Rendering;
    }
    scrub_to(&mut h, ts(15));

    let texts = h.painted_text_strings_in(h.pane_rects()[0]);
    assert!(
        texts.iter().any(|t| t == "frame 2 of 3 loading"),
        "the owed frame's quantity must be on the glass; painted {texts:?}"
    );

    // Control: the quantity is the whole statement — nothing is painted for
    // the layer while its frame is owed.
    let drawn = painted(&h);
    assert!(
        !drawn.contains(&live),
        "the live raster stood in for a loading frame"
    );
    for (i, id) in frames.iter().enumerate() {
        assert!(
            !drawn.contains(id),
            "frame {i}'s picture is on the glass while the playhead's frame is owed"
        );
    }
}

/// **The loading state goes away when the picture lands** — and the picture
/// is there. A permanent loading caption is the mutation this floors out.
#[test]
fn the_loading_state_goes_away_when_the_picture_lands() {
    let mut h = InputHarness::new();
    let (_live, _frames) = model_loop(&mut h);

    {
        let ls = h.gui_mut().panes_mut()[0].time_state_mut(&LAYER);
        ls.frames[1].image = None;
        ls.phase = LoopPhase::Rendering;
    }
    scrub_to(&mut h, ts(15));
    assert!(
        !loading_texts(&h).is_empty(),
        "fixture: the loading state must be up before the landing is tested"
    );

    let landed = raster(&h, "landed");
    let landed_id = landed.texture.id();
    {
        let ls = h.gui_mut().panes_mut()[0].time_state_mut(&LAYER);
        ls.frames[1].image = Some(LoopFrameImage::Overlay(landed));
        ls.phase = LoopPhase::Playing;
    }
    h.warm_up();

    let texts = loading_texts(&h);
    assert!(
        texts.is_empty(),
        "the picture landed and the loading state is still up: {texts:?}"
    );
    assert!(
        painted(&h).contains(&landed_id),
        "the loading state went away but the picture is not there"
    );
}

/// **Non-triviality: a pane with nothing in flight shows no loading state** —
/// neither a bare pane with no loop, nor a loop whose every frame is rendered.
#[test]
fn a_pane_with_nothing_in_flight_shows_no_loading_state() {
    let mut h = InputHarness::new();
    h.warm_up();
    let texts = loading_texts(&h);
    assert!(texts.is_empty(), "no loop, yet the glass says {texts:?}");

    let (_live, _frames) = model_loop(&mut h);
    scrub_to(&mut h, ts(15));
    let texts = loading_texts(&h);
    assert!(
        texts.is_empty(),
        "a fully rendered loop reads as loading: {texts:?}"
    );
}

/// **Radar byte-identity: the transport's `Rendering n/m...` line is exactly
/// what it was.** A radar loop mid-render draws the same literal, and the new
/// pane-glass quantity coexists with it rather than replacing it.
#[test]
fn radars_rendering_line_is_unchanged_beside_the_new_notice() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    {
        let pane = &mut h.gui_mut().panes_mut()[0];
        let ls = pane.time_state_mut(&known::RADAR);
        ls.phase = LoopPhase::Rendering;
        ls.frames = (0..3)
            .map(|i| LoopFrame {
                timestamp: ts(-30 + i * 10),
                image: None,
                render_in_flight: true,
                render_failed: false,
            })
            .collect();
        // What the app does after every frame-list change; a hand-armed
        // fixture owes the same call.
        pane.settle_playheads();
    }
    h.warm_up();

    let row2 = h.timeline().row2.expect("the expander must open row 2");
    assert_eq!(
        row2.rendered_text, "Rendering 0/3...",
        "radar's rendering line moved"
    );
    assert!(
        h.text_painted_in(h.screen_rect(), "Rendering 0/3..."),
        "the rendering line is a probe string that never reached the glass"
    );
    assert!(
        h.text_painted_in(h.pane_rects()[0], "frame 3 of 3 loading"),
        "the pane-glass quantity must coexist with radar's own line"
    );
}

/// **The `Rendering n/m` line is the transport's, not radar's by name** —
/// address the transport at the model layer and the line counts that loop's
/// frames (the WI-7 plan entry's "verify it is, post-WI-1").
#[test]
fn the_rendering_line_counts_the_transport_layers_frames() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let (_live, _frames) = model_loop(&mut h);
    {
        let pane = &mut h.gui_mut().panes_mut()[0];
        pane.set_transport_layer(LAYER);
        let ls = pane.time_state_mut(&LAYER);
        ls.frames[2].image = None;
        ls.phase = LoopPhase::Rendering;
    }
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    let row2 = h.timeline().row2.expect("the expander must open row 2");
    assert_eq!(
        row2.rendered_text, "Rendering 2/3...",
        "the line must count the transport layer's own frames"
    );
}

/// **The transport's listing line carries the wait** — the quantity
/// `listing_wait` answers, so a listing that is taking a while reads as in
/// progress rather than stuck.
#[test]
fn the_transports_listing_line_carries_the_wait() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    {
        let ls = h.gui_mut().panes_mut()[0].time_state_mut(&known::RADAR);
        ls.phase = LoopPhase::FetchingScanList;
        ls.listing_since = Some(web_time::Instant::now());
    }
    h.warm_up();

    let texts = h.painted_text_strings_in(h.screen_rect());
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Loading scan list... ") && t.ends_with('s')),
        "the listing line must carry how long the listing has been out; painted {texts:?}"
    );
}
