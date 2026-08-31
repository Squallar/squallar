//! **The time contract, walked over every layer this build registers.**
//!
//! Four user-reported bugs in a row were one defect: the app reconstructed a
//! time fact the source should have been made to answer. `SourceHandler` and
//! [`FrameSource`] now carry that contract — but a contract nothing enumerates
//! is a contract the *next* datasource can join without satisfying. This module
//! is that enumeration, and it is the reason the contract exists.
//!
//! **It walks the composed fifteen**, through
//! `OverlayRegistry::with_handlers(super::all())` and never through
//! `OverlayRegistry::default()`: `default()` is the overlays crate's own
//! fourteen and cannot see radar, which is the layer three of the four laws
//! below are hardest on.
//!
//! **Every walk carries a count floor against
//! [`REGISTERED_LAYER_COUNT`](super::REGISTERED_LAYER_COUNT)** — the hand-kept
//! second spelling, never a second read of the registry. `parity_walk`'s own
//! floor records why: filtering one id out of the walked vector **was tampered
//! and came back green** before that assertion existed, and a conformance walk
//! is exactly the shape of test that passes while doing nothing.
//!
//! **The fixture drives the real handlers.** No fake source: the one that
//! existed was deleted at `c617ccf7` because its job-codec row broke two
//! literal-table pins in `squallar-worker`, and a double cannot catch a layer
//! that files its frames wrongly. Each framed layer is hydrated through its
//! own public listing scope (`RadarListing`, `GmgsiListing`, `MrmsListing`) or
//! its own `deserialize_pane_state` — the doors the arrival path uses.
//!
//! [`FrameSource`]: squallar_source::time::FrameSource

use std::any::Any;

use chrono::{Duration, NaiveDateTime};
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::handler::{FetchConfig, FetchPayload, PaneRef};
use squallar_source::id::{LayerId, known};
use squallar_source::time::{FrameListing, FrameStamp, TimeAxis};

use super::all;

/// **The four layers that come in stamped frames**, spelled here so this
/// module's walks can floor on the set rather than on whatever the registry
/// happens to hand them.
///
/// **Not a second authority.** The ruling on which layers may be in this set —
/// and what a fifth joining would move — lives in
/// `sources::registry_identity_tests::radar_takes_the_clock_wherever_it_is_drawn`,
/// with the GMGSI (WB-11) and MRMS (WB-10) precedents written out. A fifth
/// framed layer is that pin's ruling to make; this literal is what stops the
/// walks below from quietly walking three.
const FRAMED_LAYERS: [&str; 4] = ["Gmgsi", "ModelData", "Mrms", "Radar"];

/// **The layers that ignore the depicted instant entirely**, and so for which
/// `Residency::none()` is the right answer rather than an inherited silence.
///
/// Spelled out for one reason: `every_layer_that_reads_the_clock_asks_for_residency`
/// is a claim about *non*-`Live` layers, and a build in which every layer were
/// non-`Live` would satisfy it without distinguishing anything. The arm each
/// layer declares is pinned by name in
/// `sources::registry_identity_tests::every_layer_declares_what_it_does_with_the_clock`;
/// this is the half of that map the residency law needs as a floor.
const LIVE_LAYERS: [&str; 8] = [
    "BasemapTiles",
    "CityLabels",
    "ColorScale",
    "Metar",
    // Fixed installations. The list changes on the scale of decommissionings,
    // not of anything a pane's timeline reaches — the same reason `RadarSites`
    // beside it is `Live`.
    "RadarCoverage",
    "RadarSites",
    "Terrain",
    "UserLocation",
];

/// The instant every hand-filed listing in this module is anchored on. A fixed
/// date, so the three observed layers' fixtures mean the same thing whenever
/// they run; the model's own stamps are wall-clock anchored and derived
/// separately, because a run choice is a *relative* one by construction.
fn base() -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
        .expect("a real date")
        .and_hms_opt(12, 0, 0)
        .expect("a real time")
}

/// **Wider than any layer's frames, in both directions.** `list_frames` is a
/// windowed question and this module wants the unwindowed answer, so the
/// window is spelled as two instants no fixture can reach rather than as an
/// offset from one.
fn everything() -> (NaiveDateTime, NaiveDateTime) {
    let at = |y| {
        chrono::NaiveDate::from_ymd_opt(y, 1, 1)
            .expect("a real date")
            .and_hms_opt(0, 0, 0)
            .expect("a real time")
    };
    (at(2000), at(2100))
}

fn ctx() -> FetchConfig {
    // A `reqwest::Client` cannot be built before the process has a rustls
    // provider, and a test that builds one is otherwise green only when some
    // EARLIER test in the same binary happened to install it.
    squallar_source::tls::init();
    FetchConfig {
        client: Default::default(),
        zone_cache_dir: None,
        sources: squallar_source::origins::DataSources::default(),
        viewport: None,
        as_of: base(),
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    }
}

/// One registered layer as this module asks it questions: the pane config and
/// per-layer state it is asked *through*, owned here so a [`PaneRef`] can
/// borrow both.
struct Slot {
    id: LayerId,
    config: serde_json::Value,
    state: Option<FetchPayload>,
}

impl Slot {
    fn pane(&self) -> PaneRef<'_> {
        PaneRef {
            pane_idx: 0,
            config: &self.config,
            state: self.state.as_ref().map(|s| &**s as &dyn Any),
            slots: &[],
            loading_site: None,
            peers: &[],
        }
    }
}

/// **The composed fifteen, with the four framed layers taught what frames
/// exist.**
///
/// Hydration is not decoration. Three of the four answer *nothing* about time
/// until a listing lands — radar's `residency_for` doc says so in as many
/// words, and `a_site_with_no_listing_asks_for_nothing_yet` pins it — so a
/// walk that skipped this step would read every law below as satisfied by
/// layers that had simply declined to answer. That is the vacuous-verification
/// shape this whole module exists to close, so it is closed on the fixture too.
struct Bench {
    registry: OverlayRegistry,
    slots: Vec<Slot>,
    /// The runs the model layer's stamps may legitimately carry — one entry
    /// unless the wall clock crossed an hour while the fixture was built.
    model_runs: Vec<NaiveDateTime>,
}

impl Bench {
    fn new() -> Self {
        let before = chrono::Utc::now().naive_utc();
        let mut registry = OverlayRegistry::with_handlers(all());

        let ids: Vec<LayerId> = registry.handlers().map(|h| h.id()).collect();
        let slots: Vec<Slot> = ids
            .iter()
            .map(|id| {
                let (config, saved) = match id.as_str() {
                    // Radar reads its site off the slot config and nothing
                    // else; `PaneRef::across`'s null config is exactly the
                    // state in which it answers nothing, which is why this
                    // module never asks it through one.
                    "Radar" => (serde_json::json!({ "site": "KTLX" }), None),
                    "Gmgsi" => (
                        serde_json::Value::Null,
                        Some(serde_json::json!({
                            "enabled": true,
                            "channel": squallar_overlays::gmgsi::GmgsiChannel::LongwaveIr.as_str(),
                        })),
                    ),
                    "Mrms" => (
                        serde_json::Value::Null,
                        Some(serde_json::json!({
                            "enabled": true,
                            "product": squallar_overlays::mrms::MrmsProduct::ReflectivityComposite
                                .as_str(),
                        })),
                    ),
                    // The model needs no listing at all on the forecast axis:
                    // the set is a closed form of the run, so naming the run
                    // IS the hydration. `latest` is a relative choice by
                    // construction — a saved absolute instant is refused —
                    // which is why this fixture's model instants come off the
                    // wall clock and the other three's do not.
                    "ModelData" => (
                        serde_json::Value::Null,
                        Some(serde_json::json!({
                            "enabled": true,
                            "axis": "forecast",
                            "run": "latest",
                            "forecast_hour": 0,
                        })),
                    ),
                    _ => (serde_json::Value::Null, None),
                };
                let state = saved.and_then(|value| {
                    registry
                        .get_handler(id)
                        .expect("an id the registry just handed out")
                        .deserialize_pane_state(value, true)
                });
                Slot {
                    id: id.clone(),
                    config,
                    state,
                }
            })
            .collect();

        // The arrival path's own door: a listing is filed under the scope its
        // dispatch captured, and the `PaneRef` it arrives on is the union
        // across panes whose config is null by construction.
        let across = PaneRef::across(&[]);
        let radar_range = (base(), base() + Duration::minutes(10));
        registry.apply_frames(
            &known::RADAR,
            FrameListing {
                range: radar_range,
                frames: radar_stamps().iter().map(observed).collect(),
                complete: true,
            },
            Box::new(squallar_radar::source::RadarListing {
                site: "KTLX".to_string(),
                range: radar_range,
                scans: radar_stamps()
                    .iter()
                    .map(|&valid| {
                        (
                            valid,
                            squallar_radar::archive::Identifier::new(format!(
                                "KTLX{}",
                                valid.format("%Y%m%d_%H%M%S")
                            )),
                        )
                    })
                    .collect(),
            }),
            &across,
        );

        let gmgsi_range = (base() - Duration::hours(2), base());
        registry.apply_frames(
            &known::GMGSI,
            FrameListing {
                range: gmgsi_range,
                frames: gmgsi_stamps().iter().map(observed).collect(),
                complete: true,
            },
            Box::new(squallar_overlays::gmgsi::GmgsiListing {
                channel: squallar_overlays::gmgsi::GmgsiChannel::LongwaveIr,
                range: gmgsi_range,
                keys: gmgsi_stamps()
                    .iter()
                    .map(|&valid| (valid, format!("lw/{valid}")))
                    .collect(),
                complete: true,
            }),
            &across,
        );

        let mrms_range = (base(), base() + Duration::minutes(4));
        registry.apply_frames(
            &known::MRMS,
            FrameListing {
                range: mrms_range,
                frames: mrms_stamps().iter().map(observed).collect(),
                complete: true,
            },
            Box::new(squallar_overlays::mrms::MrmsListing {
                product: squallar_overlays::mrms::MrmsProduct::ReflectivityComposite,
                range: mrms_range,
                keys: mrms_stamps()
                    .iter()
                    .map(|&valid| (valid, format!("refc/{valid}")))
                    .collect(),
                complete: true,
            }),
            &across,
        );

        let after = chrono::Utc::now().naive_utc();
        let mut model_runs: Vec<NaiveDateTime> =
            [before, after].iter().map(|&now| latest_run(now)).collect();
        model_runs.dedup();

        Self {
            registry,
            slots,
            model_runs,
        }
    }

    fn slot(&self, id: &str) -> &Slot {
        self.slots
            .iter()
            .find(|slot| slot.id.as_str() == id)
            .unwrap_or_else(|| panic!("{id} is not registered in this build"))
    }

    /// **What this layer says it can draw, over a window nothing can fall
    /// outside** — the same door the app builds a loop's frames from.
    fn known_frames(&self, id: &LayerId) -> Vec<FrameStamp> {
        let slot = self.slot(id.as_str());
        self.registry
            .list_frames(id, &ctx(), &slot.pane(), everything())
            .frames
    }
}

/// `squallar-overlays`' own run selection, reached through its public door
/// rather than recomputed: this is the fixture's independent statement about
/// which run the model layer's stamps must carry.
fn latest_run(now: NaiveDateTime) -> NaiveDateTime {
    let (date, hour) = squallar_overlays::hrrr::fetch::run_for(now);
    date.and_hms_opt(u32::from(hour), 0, 0)
        .expect("run_for reports a wall-clock hour")
}

fn observed(valid: &NaiveDateTime) -> FrameStamp {
    FrameStamp {
        valid: *valid,
        run: None,
    }
}

/// Three volumes on the ~5-minute cadence radar declares.
fn radar_stamps() -> Vec<NaiveDateTime> {
    (0..3).map(|k| base() + Duration::minutes(5 * k)).collect()
}

/// Three hourly granules, ending on the anchor — the cadence that produced the
/// blank leading frame, and the widest gap any layer here carries.
fn gmgsi_stamps() -> Vec<NaiveDateTime> {
    (0..3).map(|k| base() - Duration::hours(2 - k)).collect()
}

/// Three mosaics on the ~2-minute cadence MRMS declares.
fn mrms_stamps() -> Vec<NaiveDateTime> {
    (0..3).map(|k| base() + Duration::minutes(2 * k)).collect()
}

/// **The instants a walk asks a framed layer about**: one before everything it
/// holds, each stamp exactly, and one minute past each stamp.
///
/// The first is not padding — a clock standing before every frame is the state
/// `qualifying_frame_at` answers `None` in and the state a layer of mixed
/// spans is routinely left in, so a sweep without it never compares the two
/// sides' `None`s at all. The last is `FrameSeries`'s carry-forward: the
/// instant between two frames is drawn by the earlier one.
fn sweep(frames: &[FrameStamp]) -> Vec<NaiveDateTime> {
    let mut instants = vec![frames[0].valid - Duration::minutes(1)];
    for stamp in frames {
        instants.push(stamp.valid);
        instants.push(stamp.valid + Duration::minutes(1));
    }
    instants
}

// ── WO-T2.1 ───────────────────────────────────────────────────────────────

/// **`time_axis() == FrameSeries` if and only if `frames().is_some()`, over
/// the composed fifteen.**
///
/// Before the supply moved behind `SourceHandler::frames` a layer could
/// declare `TimeAxis::FrameSeries` and inherit a silent, empty body for all
/// nine supply methods — which is how a listing could be fetched and paid for
/// by a layer that never filed it. The declaration and the supply are two
/// halves of one statement now, and this walks **both directions** of it.
///
/// The pairing landed at `00500089` among the registry's other identity pins
/// and moved into this module with the rest of the conformance walk, so there
/// is one place a reader has to know about rather than two. It enumerates
/// through the registry the app actually builds rather than through the bare
/// `all()` vector, which is the only thing about it that changed.
///
/// **Floors.** (a) The walk covers all fifteen registrations, so a layer
/// dropping out of the enumeration cannot make the pairing hold vacuously.
/// (b) The supplying set is exactly the four
/// `radar_takes_the_clock_wherever_it_is_drawn` already rules on — a fifth is
/// that pin's ruling to make, not this one's. (c) At least one registered
/// layer answers `frames() == None`, named, so "every layer has frames" cannot
/// pass this. (d) `frames_mut` agrees with `frames` on every layer — they are
/// two borrows of one object, not two opinions about whether this layer comes
/// in frames.
#[test]
fn every_frame_series_layer_supplies_frames() {
    let mut registry = OverlayRegistry::with_handlers(all());
    let walked: Vec<LayerId> = registry.handlers().map(|h| h.id()).collect();

    // Floor (a).
    assert_eq!(
        walked.len(),
        super::REGISTERED_LAYER_COUNT,
        "the pairing below is only a claim about this build if it walks every \
         layer this build registers; walked {walked:?}",
    );

    let mut supplying: Vec<String> = Vec::new();
    let mut silent: Vec<String> = Vec::new();
    for id in &walked {
        let handler = registry
            .handler_by_id_mut(id)
            .expect("an id the registry just handed out");
        let name = id.as_str().to_owned();
        let declared = matches!(handler.time_axis(), TimeAxis::FrameSeries { .. });
        let supplies = handler.frames().is_some();

        assert_eq!(
            declared,
            supplies,
            "{name} declares its axis as {} and {} a frame supply. The two are \
             one statement: a `FrameSeries` layer with no supply can name no \
             frame it draws, and a supply on a layer that never declared the \
             axis is nine methods nothing will ever call.",
            if declared {
                "FrameSeries"
            } else {
                "not FrameSeries"
            },
            if supplies {
                "offers one"
            } else {
                "offers none"
            },
        );

        // Floor (d): the two accessors cannot disagree.
        assert_eq!(
            handler.frames_mut().is_some(),
            supplies,
            "{name}'s `frames_mut` disagrees with its `frames`; they are two \
             borrows of one object, and an arrival taking the mutable route \
             would be silently dropped",
        );

        if supplies {
            &mut supplying
        } else {
            &mut silent
        }
        .push(name);
    }

    // Floor (b).
    supplying.sort();
    assert_eq!(
        supplying, FRAMED_LAYERS,
        "exactly the four layers `radar_takes_the_clock_wherever_it_is_drawn` \
         rules on supply frames. A fifth is that pin's ruling to make — extend \
         it there rather than adding a second authority here.",
    );

    // Floor (c): a non-framed layer is named, so the pairing above is a
    // distinguishable claim rather than a tautology over a uniform set.
    assert!(
        silent.iter().any(|id| id == "CityLabels"),
        "the city labels layer draws whatever it last fetched and has no \
         frames; without a named layer on this side, \"every layer supplies \
         frames\" would satisfy the walk. Registered but not supplying: \
         {silent:?}",
    );
}

// ── WO-T2.2 ───────────────────────────────────────────────────────────────

/// **No layer ever names a frame that depicts an instant the clock has not
/// reached**, over a swept `t` and every layer that supplies frames.
///
/// `TimeAxis::FrameSeries`'s stated rule is `valid <= t`, and a frame valid
/// *after* the depicted instant is a fabrication rather than a fallback. This
/// is the law; each layer's own suite carries the behaviour.
///
/// **Floors.** (a) The walk reaches all four framed layers, named, at every
/// instant of each layer's own sweep — a skipped layer reads as agreement.
/// (b) **Every framed layer answers `Some` somewhere**, so no layer passes by
/// declining to answer. This is the floor the version of this walk that landed
/// at `00500089` could not carry: it ran against a fresh registry where every
/// answer was `None`, which satisfies `valid <= t` without ever evaluating it.
/// The fixture hydrates each layer through its own arrival door instead.
#[test]
fn latest_at_never_answers_ahead_of_the_instant() {
    let bench = Bench::new();

    let mut asked: Vec<(String, usize, usize)> = Vec::new();
    for slot in &bench.slots {
        let Some(frames) = bench
            .registry
            .get_handler(&slot.id)
            .expect("a registered id")
            .frames()
        else {
            continue;
        };
        let known = bench.known_frames(&slot.id);
        assert!(
            !known.is_empty(),
            "{} was hydrated by the fixture and still names no frame — every \
             assertion below it would hold by declining to answer",
            slot.id.as_str(),
        );

        let pane = slot.pane();
        let mut answered = 0usize;
        let instants = sweep(&known);
        for &t in &instants {
            if let Some(stamp) = frames.latest_at(&pane, t) {
                assert!(
                    stamp.valid <= t,
                    "{} answered {t} with a frame valid at {}, which depicts \
                     an instant the clock has not reached",
                    slot.id.as_str(),
                    stamp.valid,
                );
                answered += 1;
            }
        }

        // Floor (b), per layer rather than once for the walk: one layer
        // answering is not evidence about the other three.
        assert!(
            answered > 0,
            "{} answered `None` at every one of {} instants swept across its \
             own frames — the `valid <= t` assertion above never evaluated a \
             stamp for this layer",
            slot.id.as_str(),
            instants.len(),
        );
        asked.push((slot.id.as_str().to_owned(), instants.len(), answered));
    }

    // Floor (a).
    let mut reached: Vec<&str> = asked.iter().map(|(id, _, _)| id.as_str()).collect();
    reached.sort_unstable();
    assert_eq!(
        reached, FRAMED_LAYERS,
        "the sweep must reach every framed layer; a layer it does not ask is \
         a layer this law says nothing about. Asked: {asked:?}",
    );
}

/// **The peer gate: the handler's answer and the playhead above it cannot
/// disagree.**
///
/// `FrameSource::latest_at` is deliberately a *peer* rather than the
/// authority. `LayerTimeState::qualifying_frame_at` keeps deciding from the
/// pane's own frame list, because making the handler authoritative would block
/// the whole contract on WO-M12d — radar's decoded volumes live above its
/// handler. Two implementations of one rule is a defect waiting to happen
/// unless something asserts they agree, and this is that something.
///
/// **Which list the pane side is built from is the whole question, and radar
/// is why.** The pane's list here comes from `list_frames` — the listing door,
/// which is what `build_loop_frames` fills a loop from. It is deliberately
/// **not** built from `frames_resident`: radar answers that empty by design
/// (its decoded volumes are held above this crate, WO-M12d), so a walk built
/// on residency would hand radar an empty list, get `None` from both sides at
/// every instant, and record perfect agreement about nothing. That is the one
/// layer this gate must not silently skip, so the floors below name it.
///
/// **Floors.** (a) Every framed layer is compared, named, and the skipped set
/// is asserted **empty** — an un-listed layer must never satisfy this by
/// answering nothing. (b) At least two layers agree on a `Some` at two or more
/// instants each, so the agreement is about named frames and not about a pair
/// of `None`s. (c) Radar is named explicitly: its `frames_resident` is
/// asserted empty **while** its listing-derived list is asserted non-empty, so
/// the reason this walk reads the door it reads is pinned rather than
/// remembered.
#[test]
fn latest_at_agrees_with_the_playhead_above_it() {
    use crate::pane::{LayerTimeState, LoopFrame, TimeMode};

    let bench = Bench::new();

    let mut compared: Vec<&str> = Vec::new();
    let mut skipped: Vec<&str> = Vec::new();
    // Layers on which both sides named a frame at two or more instants.
    let mut agreed_on_a_frame: Vec<&str> = Vec::new();

    for slot in &bench.slots {
        let handler = bench
            .registry
            .get_handler(&slot.id)
            .expect("a registered id");
        let Some(frames) = handler.frames() else {
            continue;
        };
        let known = bench.known_frames(&slot.id);
        if known.is_empty() {
            skipped.push(slot.id.as_str());
            continue;
        }

        // The pane's own timeline, filled the way the arrival path fills it:
        // one frame per stamp the listing named, ascending.
        let mut above = LayerTimeState::new();
        above.frames = known
            .iter()
            .map(|stamp| LoopFrame {
                timestamp: stamp.valid,
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();

        let pane = slot.pane();
        let mut both_named = 0usize;
        for t in sweep(&known) {
            let below = frames.latest_at(&pane, t).map(|stamp| stamp.valid);
            let above_answer = above
                .qualifying_frame_at(TimeMode::AsOf(t))
                .map(|index| above.frames[index].timestamp);
            assert_eq!(
                below,
                above_answer,
                "{} disagrees with the playhead above it at {t}: the handler \
                 names {below:?} and `qualifying_frame_at` names \
                 {above_answer:?}. Both claim to be `FrameSeries`'s rule, and \
                 the pane draws whichever one the caller happened to ask.",
                slot.id.as_str(),
            );
            if below.is_some() {
                both_named += 1;
            }
        }

        // The clock standing before everything held: `None` on both sides is
        // an agreement worth having, and a sweep that never produced one would
        // not have compared the `None` branch at all.
        assert!(
            frames
                .latest_at(&pane, known[0].valid - Duration::minutes(1))
                .is_none(),
            "{}'s sweep opens before its oldest frame, and a layer answering \
             there names a frame it cannot have",
            slot.id.as_str(),
        );

        compared.push(slot.id.as_str());
        if both_named >= 2 {
            agreed_on_a_frame.push(slot.id.as_str());
        }
    }

    // Floor (a): the skip list is exactly what was intended, which is nothing.
    assert!(
        skipped.is_empty(),
        "these framed layers were skipped because they named no frame: \
         {skipped:?}. A skipped layer agrees with everything; if a layer \
         genuinely cannot be hydrated, say so here by name rather than \
         letting it pass in silence.",
    );
    compared.sort_unstable();
    assert_eq!(
        compared, FRAMED_LAYERS,
        "the peer gate must compare every framed layer",
    );

    // Floor (b).
    assert!(
        agreed_on_a_frame.len() >= 2,
        "only {agreed_on_a_frame:?} agreed on a NAMED frame at two or more \
         instants; two sides that both answer `None` everywhere agree about \
         nothing",
    );

    // Floor (c): radar, by name.
    let radar = bench.slot("Radar");
    let radar_frames = bench
        .registry
        .get_handler(&known::RADAR)
        .expect("radar is registered")
        .frames()
        .expect("radar comes in stamped frames");
    assert!(
        radar_frames.frames_resident(&radar.pane()).is_empty(),
        "radar's `frames_resident` is empty BY DESIGN — its decoded volumes \
         live in the app layer above this crate (WO-M12d). If that ever \
         changes, this walk's choice of door has to be reconsidered rather \
         than inherited.",
    );
    assert!(
        !bench.known_frames(&known::RADAR).is_empty(),
        "and its listing-derived list is what this gate compares through: \
         empty here and radar would answer `None` on both sides at every \
         instant, and this test would read as agreement about nothing",
    );
    assert!(
        compared.contains(&"Radar"),
        "radar must be compared, not skipped: it is the one layer whose \
         storage sits above its handler, so it is the one a residency-built \
         walk would silently exclude",
    );
}

// ── WO-T2.3 ───────────────────────────────────────────────────────────────

/// The stops this module asks a layer to hold for, and one instant deliberately
/// left out of them.
///
/// For a framed layer the stops sit on and just past its own frames: a stop
/// before every frame is one the layer draws blank at, and asking it to hold
/// something for an instant it draws nothing at would be the wrong law. For an
/// `EventLifetime` layer any stop is answerable, so the stops are the anchor
/// and the two hours after it.
fn stops_and_an_outsider(bench: &Bench, id: &LayerId) -> (Vec<NaiveDateTime>, NaiveDateTime) {
    match bench
        .registry
        .get_handler(id)
        .expect("a registered id")
        .frames()
    {
        Some(_) => {
            let known = bench.known_frames(id);
            let stops = sweep(&known)
                .into_iter()
                .filter(|&t| t >= known[0].valid)
                .collect();
            (stops, known[0].valid - Duration::hours(1))
        }
        None => (
            (0..3).map(|k| base() + Duration::hours(k)).collect(),
            base() - Duration::days(3),
        ),
    }
}

/// **The GLM bug as a law**: every instant a pane's clock can stop on is
/// covered by what its layer asked to hold.
///
/// The bug this is: a twelve-hour satellite loop of thirteen hourly stops
/// armed its lightning layer over 43 200 s while the poll was told 3 600 s, so
/// twelve of the thirteen stops fell outside what was retained and the loop lit
/// on one frame. Two authorities on one question, and neither was wrong about
/// its own. `residency_for` is the single authority, and this is the assertion
/// that it answers for the stops rather than for the extent.
///
/// **Floor — the law must be able to fail.** For every layer walked, an instant
/// deliberately left out of the stops is asserted **not** covered. Without it
/// a `residency_for` returning one range from the beginning of time would
/// satisfy every positive assertion here, which is the exact over-reach the
/// GLM bug was.
#[test]
fn every_stop_a_pane_can_make_is_inside_what_the_layer_asked_to_hold() {
    let bench = Bench::new();

    assert_eq!(
        bench.slots.len(),
        super::REGISTERED_LAYER_COUNT,
        "the walk must cover every registered layer: {:?}",
        bench
            .slots
            .iter()
            .map(|slot| slot.id.as_str())
            .collect::<Vec<_>>(),
    );

    let mut ruled: Vec<&str> = Vec::new();
    for slot in &bench.slots {
        let handler = bench
            .registry
            .get_handler(&slot.id)
            .expect("a registered id");
        if handler.time_axis() == TimeAxis::Live {
            continue;
        }
        let name = slot.id.as_str();
        let (stops, outsider) = stops_and_an_outsider(&bench, &slot.id);
        let residency = handler.residency_for(&slot.pane(), &stops);

        for &stop in &stops {
            assert!(
                residency.covers(stop),
                "{name} can be asked to draw {stop} and did not ask to hold \
                 it: {:?}. A stop outside the residency is a stop that draws \
                 blank while the loop runs.",
                residency.ranges(),
            );
        }

        // The floor: an instant left out of the stops is left out of the ask.
        assert!(
            !residency.covers(outsider),
            "{name} asked to hold {outsider}, which is not one of the stops \
             it was asked about ({:?}). A residency wider than the stops is \
             the over-reach this law exists to catch — it is what listed \
             twelve hours of archive for thirteen pictures.",
            residency.ranges(),
        );
        ruled.push(name);
    }

    ruled.sort_unstable();
    let mut expected: Vec<&str> = bench
        .slots
        .iter()
        .map(|slot| slot.id.as_str())
        .filter(|id| !LIVE_LAYERS.contains(id))
        .collect();
    expected.sort_unstable();
    assert_eq!(
        ruled, expected,
        "every layer that reads the clock is ruled on here; a layer the walk \
         does not reach is a layer the law says nothing about",
    );
    assert_eq!(
        ruled.len(),
        super::REGISTERED_LAYER_COUNT - LIVE_LAYERS.len(),
        "ten of this build's eighteen layers read the depicted instant",
    );
}

// ── WO-T2.4 ───────────────────────────────────────────────────────────────

/// **Every layer that reads the clock asks for residency**: non-`Live` implies
/// `residency_for` answers non-empty.
///
/// `Residency::none()` is the trait's default body and it is the *correct*
/// answer for a `Live` layer — such a layer draws whatever it last fetched and
/// ignores the depicted instant, so no set of stops obliges it to hold
/// anything. It is wrong for the other nine, and this is what stops it being
/// inherited where it matters.
///
/// **Both radar handoffs are load-bearing here.** Radar's answer is empty
/// until a listing lands and is scoped to the pane's site, so a walk that
/// asked it through a bare pane, or before hydrating it, would read a false
/// red. The fixture does both; `a_site_with_no_listing_asks_for_nothing_yet`
/// is the pin that the empty is a state rather than a silence.
///
/// **Floors.** (a) The walk covers all fifteen registrations. (b) At least one
/// registered layer **is** `Live` and is asserted to answer empty, so
/// "non-`Live`" is a distinguishable claim rather than one that happens to be
/// true of everything.
#[test]
fn every_layer_that_reads_the_clock_asks_for_residency() {
    let bench = Bench::new();

    // Floor (a).
    assert_eq!(
        bench.slots.len(),
        super::REGISTERED_LAYER_COUNT,
        "the walk must cover every registered layer: {:?}",
        bench
            .slots
            .iter()
            .map(|slot| slot.id.as_str())
            .collect::<Vec<_>>(),
    );

    let mut asking: Vec<&str> = Vec::new();
    let mut silent: Vec<&str> = Vec::new();
    for slot in &bench.slots {
        let handler = bench
            .registry
            .get_handler(&slot.id)
            .expect("a registered id");
        let name = slot.id.as_str();
        let (stops, _) = stops_and_an_outsider(&bench, &slot.id);
        let residency = handler.residency_for(&slot.pane(), &stops);

        if handler.time_axis() == TimeAxis::Live {
            assert!(
                residency.is_empty(),
                "{name} ignores the depicted instant by contract and still \
                 asked to hold {:?} — either the axis is wrong or the ask is",
                residency.ranges(),
            );
            silent.push(name);
        } else {
            assert!(
                !residency.is_empty(),
                "{name} reads the depicted instant and asked for nothing over \
                 {} stops. `Residency::none()` is the trait's default body: an \
                 answer that is right for the five `Live` layers must not be \
                 inheritable by the ten it is wrong for.",
                stops.len(),
            );
            asking.push(name);
        }
    }

    // Floor (b).
    silent.sort_unstable();
    assert_eq!(
        silent, LIVE_LAYERS,
        "the `Live` set is what makes \"non-`Live`\" a distinguishable claim; \
         with none of them the law above would be satisfied by a build in \
         which every layer answered non-empty",
    );
    assert_eq!(
        asking.len() + silent.len(),
        super::REGISTERED_LAYER_COUNT,
        "every registered layer lands on one side or the other",
    );
}

// ── The fixture itself ────────────────────────────────────────────────────

/// **The hydration is real**, asserted against what the fixture filed rather
/// than against what the handlers answered.
///
/// Three of the four framed layers were taught a listing here, and this is the
/// independent statement of what that listing was: if a handler files a
/// listing under the wrong scope, or drops one, every law above would still be
/// walked — over a shorter frame set, and green. The model is the fourth and
/// is checked structurally instead, since its stamps are a closed form of a
/// run the fixture did not choose.
#[test]
fn the_fixture_taught_each_framed_layer_the_frames_it_meant_to() {
    let bench = Bench::new();

    for (id, filed) in [
        (known::RADAR, radar_stamps()),
        (known::GMGSI, gmgsi_stamps()),
        (known::MRMS, mrms_stamps()),
    ] {
        let held: Vec<NaiveDateTime> = bench
            .known_frames(&id)
            .iter()
            .map(|stamp| stamp.valid)
            .collect();
        assert_eq!(
            held,
            filed,
            "{} was filed {filed:?} and names {held:?} — every law in this \
             module would still walk it, over the wrong set",
            id.as_str(),
        );
    }

    let model = bench.known_frames(&known::MODEL_DATA);
    assert!(
        model.len() >= 13,
        "the model's forecast axis is a closed form of its run and publishes \
         at least an 18-hour horizon; {} stamps is not that",
        model.len(),
    );
    for stamp in &model {
        let run = stamp.run.expect(
            "a model stamp carries the cycle that produced it, which is what \
             tells a forecast frame from another run's frame at one instant",
        );
        assert!(
            bench.model_runs.contains(&run),
            "a model stamp names run {run}, and the fixture asked for the \
             latest ({:?})",
            bench.model_runs,
        );
    }
    let gaps: Vec<i64> = model
        .windows(2)
        .map(|pair| (pair[1].valid - pair[0].valid).num_minutes())
        .collect();
    assert!(
        gaps.iter().all(|&gap| gap == 60),
        "the forecast axis is hourly; gaps in minutes were {gaps:?}",
    );
}
