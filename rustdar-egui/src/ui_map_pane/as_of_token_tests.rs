//! **WO-E7c: a scrubbed pane does not draw the live pane's warnings.**
//!
//! The as-of half of the overlay cache token — what makes a pane looking at
//! 20 minutes ago rasterize the alerts that were valid *then* rather than
//! reuse the texture the live pane built for *now*.

use crate::pane::{TimeMode, as_of_bucket};
use crate::ui::Gui;
use rustdar_source::id::{LayerId, known};
use rustdar_source::time::TimeAxis;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 6, 1)
        .unwrap()
        .and_hms_opt(12, minute, 0)
        .unwrap()
}

fn token(gui: &Gui, id: &LayerId) -> u64 {
    super::overlay_cache_token(&gui.overlays, 0, &gui.panes[0], id, false)
}

/// The two layers whose picture is a function of the depicted instant, read
/// off the registry rather than listed here — so a third joining is covered.
fn event_lifetime_layers(gui: &Gui) -> Vec<LayerId> {
    gui.overlays
        .handlers()
        .filter(|h| matches!(h.time_axis(), TimeAxis::EventLifetime))
        .map(|h| h.id())
        .collect()
}

/// **THE PARITY CLAUSE, PINNED SO IT CANNOT LAPSE.** On a live pane the token
/// is byte-for-byte the token that existed before this land — every layer,
/// including the as-of-dependent ones. WO-M11 landed `as_of` dark and proved
/// parity once; this is what keeps it true permanently rather than until
/// someone reads the field.
#[test]
fn a_live_panes_cache_token_is_untouched_by_the_as_of_term() {
    let mut gui = Gui::new();
    gui.panes[0].time.mode = TimeMode::Live;

    let ids: Vec<LayerId> = gui.overlays.handlers().map(|h| h.id()).collect();
    assert_eq!(
        ids.len(),
        14 + cfg!(feature = "fake-source") as usize,
        "the walk below must cover every layer",
    );
    assert!(
        !event_lifetime_layers(&gui).is_empty(),
        "non-triviality floor: there is at least one as-of-dependent layer for \
         this to be a statement about",
    );

    for id in &ids {
        let base = gui
            .overlays
            .content_signature(id, &gui.panes[0].layer_ref(0, id));
        let base = if *id == known::RADAR_SITES {
            gui.panes[0].radar_sites_render_gen
        } else {
            base
        };
        assert_eq!(
            token(&gui, id),
            base,
            "{}'s live token gained a term - the as-of half must be exactly \
             zero while the pane is following live",
            id.as_str(),
        );
    }
}

/// Scrubbing moves the token of an as-of-dependent layer and **only** of an
/// as-of-dependent layer: a `Live` layer draws what it last fetched and a
/// `FrameSeries` layer's picture is one named frame, so neither re-rasterizes
/// because the clock moved.
#[test]
fn scrubbing_moves_the_token_of_exactly_the_as_of_dependent_layers() {
    let mut gui = Gui::new();
    let ids: Vec<LayerId> = gui.overlays.handlers().map(|h| h.id()).collect();
    let event = event_lifetime_layers(&gui);

    gui.panes[0].time.mode = TimeMode::Live;
    let live: Vec<u64> = ids.iter().map(|id| token(&gui, id)).collect();

    // Far enough back that every quantum has moved on.
    gui.panes[0].time.mode = TimeMode::AsOf(ts(20));
    let scrubbed: Vec<u64> = ids.iter().map(|id| token(&gui, id)).collect();

    let mut moved = 0;
    for (idx, id) in ids.iter().enumerate() {
        if event.contains(id) {
            assert_ne!(
                live[idx],
                scrubbed[idx],
                "{} is as-of-dependent: a scrubbed pane must not be handed the \
                 live pane's texture",
                id.as_str(),
            );
            moved += 1;
        } else {
            assert_eq!(
                live[idx],
                scrubbed[idx],
                "{} does not read the depicted instant, so scrubbing must not \
                 re-rasterize it",
                id.as_str(),
            );
        }
    }
    assert_eq!(
        moved,
        event.len(),
        "every as-of-dependent layer was checked, and there were {} of them",
        event.len(),
    );
    assert!(moved >= 2, "non-triviality floor: alerts AND lightning");
}

/// **The token keys on the QUANTIZED instant, not the raw one.** Two clocks
/// inside one bucket share a texture; the first tick of the next bucket does
/// not. This is what stops a drag minting a raster per frame.
#[test]
fn two_instants_in_one_quantum_share_a_texture_and_the_next_one_does_not() {
    let mut gui = Gui::new();
    let alerts = known::NWS_ALERTS;
    let quantum = gui
        .overlays
        .handlers()
        .find(|h| h.id() == alerts)
        .expect("alerts is registered")
        .as_of_quantum();
    assert_eq!(
        quantum,
        std::time::Duration::from_secs(60),
        "precondition: the alerts quantum is the whole minute NWS lifetimes \
         are published at",
    );

    let base = ts(10);
    let same_bucket = base + chrono::Duration::seconds(59);
    let next_bucket = base + chrono::Duration::seconds(60);
    assert_eq!(
        as_of_bucket(base, quantum),
        as_of_bucket(same_bucket, quantum),
        "precondition: 12:10:00 and 12:10:59 really are one bucket",
    );
    assert_ne!(
        as_of_bucket(base, quantum),
        as_of_bucket(next_bucket, quantum),
        "precondition: and 12:11:00 is the next one",
    );

    gui.panes[0].time.mode = TimeMode::AsOf(base);
    let at_base = token(&gui, &alerts);
    gui.panes[0].time.mode = TimeMode::AsOf(same_bucket);
    let within = token(&gui, &alerts);
    gui.panes[0].time.mode = TimeMode::AsOf(next_bucket);
    let beyond = token(&gui, &alerts);

    assert_eq!(
        at_base, within,
        "a clock that moved inside the quantum re-uses the raster - keying on \
         the raw instant would mint one per scrubber frame",
    );
    assert_ne!(
        at_base, beyond,
        "and a clock that crossed the quantum does not",
    );
}

/// **Each layer buckets the clock at its OWN quantum**, not at one number for
/// all of them. Stated as literal instants and literal expectations — never
/// by asking `as_of_bucket` what it thinks, which is the same belief the
/// token was built from and would agree with any quantum it was given.
///
/// Lightning's fade ramp is sub-minute, so one second of scrub moves its
/// texture. NWS lifetimes are published on the whole minute, so the same
/// second does not move the alerts texture — and a minute later does.
#[test]
fn each_layer_buckets_the_clock_at_its_own_quantum() {
    let mut gui = Gui::new();
    let lightning = known::LIGHTNING;
    let alerts = known::NWS_ALERTS;

    // 12:10:00, 12:10:01 and 12:11:00 — chosen so that a ONE-SECOND quantum
    // separates the first pair and a WHOLE-MINUTE quantum does not.
    let base = ts(10);
    let a_second_on = base + chrono::Duration::seconds(1);
    let a_minute_on = base + chrono::Duration::seconds(60);

    let at = |gui: &mut Gui, instant, id: &LayerId| {
        gui.panes[0].time.mode = TimeMode::AsOf(instant);
        token(gui, id)
    };

    let light_base = at(&mut gui, base, &lightning);
    let light_second = at(&mut gui, a_second_on, &lightning);
    let alert_base = at(&mut gui, base, &alerts);
    let alert_second = at(&mut gui, a_second_on, &alerts);
    let alert_minute = at(&mut gui, a_minute_on, &alerts);

    assert_ne!(
        light_base, light_second,
        "lightning fades sub-minute: one second of scrub is a different \
         picture and must be a different texture",
    );
    assert_eq!(
        alert_base, alert_second,
        "the SAME one second must NOT move the alerts texture - if it does, \
         both layers are being bucketed at one quantum instead of their own",
    );
    assert_ne!(
        alert_base, alert_minute,
        "and a whole minute does move it, so the alerts quantum is a minute \
         rather than infinite",
    );
}

/// A zero quantum is not in the contract, and a bucket is not the place to
/// discover that: it floors at a second rather than dividing by zero.
#[test]
fn a_zero_quantum_floors_instead_of_dividing_by_zero() {
    let t = ts(10);
    assert_eq!(
        as_of_bucket(t, std::time::Duration::ZERO),
        as_of_bucket(t, std::time::Duration::from_secs(1)),
    );
}
