use crate::config_store::{ConfigStore, MemoryConfigStore, UI_CONFIG_KEY};

/// Settings the user changed must come back after a save/load cycle.
///
/// Every asserted field is first checked to *differ* from what a fresh
/// `Gui` starts with. Without that guard this test would still pass if
/// `load_ui_config` did nothing at all, since the default would supply the
/// expected value on its own.
#[test]
fn changed_settings_survive_a_save_and_load() {
    use crate::pane::{OrbitDelta, PaneKind};

    let store = MemoryConfigStore::default();

    let baseline = crate::Gui::new();
    assert_ne!(baseline.loop_lookback_secs, 7200);
    assert_ne!(baseline.loop_speed_fps, 12.5);
    assert!(
        baseline.pane(0).unwrap().viewport_link && baseline.pane(0).unwrap().layer_link,
        "default is linked; test flips both off"
    );
    assert_eq!(
        baseline.pane(0).unwrap().kind(),
        PaneKind::Map,
        "default is a map; test converts it"
    );

    let mut gui = crate::Gui::new();
    gui.loop_lookback_secs = 7200;
    gui.loop_speed_fps = 12.5;
    gui.pane_mut(0).unwrap().viewport_link = false;
    gui.pane_mut(0).unwrap().layer_link = false;
    // A 3D pane whose camera has been moved off its default, so the assertion
    // below is about the saved value rather than about two defaults agreeing.
    gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
    let nudged = {
        let volume = gui.pane_mut(0).unwrap().volume_mut().expect("converted");
        volume.camera.nudge(OrbitDelta {
            yaw_deg: -47.5,
            pitch_deg: 12.25,
            zoom_factor: 1.5,
            // Panned and stretched too, so the round trip below covers every
            // field the camera persists rather than only the three angles.
            pan: [0.2, -0.35, 0.1],
        });
        volume.camera.set_vertical_exaggeration(5.5);
        volume.camera
    };
    assert_ne!(
        nudged,
        crate::pane::OrbitCamera::default(),
        "precondition: the camera must differ from the default"
    );
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.loop_lookback_secs, 7200);
    assert_eq!(restored.loop_speed_fps, 12.5);
    assert!(
        !restored.pane(0).unwrap().viewport_link && !restored.pane(0).unwrap().layer_link,
        "the per-pane links must survive the round trip"
    );
    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Volume);
    assert_eq!(
        restored.pane(0).unwrap().volume().map(|v| v.camera),
        Some(nudged),
        "the pane came back as a 3D view aimed somewhere else"
    );
}

/// M11-3. **An old config's `viewport_sync: false` loads as every restored
/// pane viewport-unlinked, and `sync_layers: false` as every pane layer- and
/// time-unlinked — the retired globals fold into the per-pane links once,
/// on load.**
///
/// `sync_layers` seeds `time_link` too because under the old model that one
/// global gated the whole shared-time fan-out: a pane's stored `time_link`
/// was inert while it was off, and a migrated config must keep behaving as
/// it observably did — no fan-out.
#[test]
fn a_legacy_global_off_seeds_every_restored_panes_links_off() {
    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX","viewport_sync":false,
                    "panes":[{"site":"KMPX"},{"site":"KOUN","time_link":true}]}"#,
        )
        .unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    for idx in 0..2 {
        let pane = restored.pane(idx).unwrap();
        assert!(
            !pane.viewport_link,
            "pane {idx}: the legacy viewport_sync=false must seed the link off"
        );
        assert!(
            pane.layer_link && pane.time_link,
            "pane {idx}: the other dimensions' links are not viewport_sync's \
                 to seed"
        );
    }

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX","sync_layers":false,
                    "panes":[{"site":"KMPX","time_link":true}]}"#,
        )
        .unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    // Pane 1 has no PaneConfig at all — the fold must reach it too.
    for idx in 0..2 {
        let pane = restored.pane(idx).unwrap();
        assert!(
            !pane.layer_link && !pane.time_link,
            "pane {idx}: the legacy sync_layers=false must seed the layer \
                 and time links off"
        );
        assert!(
            pane.viewport_link,
            "pane {idx}: the viewport link is not sync_layers' to seed"
        );
    }
}

/// M11-4. **A config with no legacy globals — one this build wrote, or an
/// old one that simply never mentioned them — loads with every pane linked,
/// and the legacy fields are never written again.**
///
/// The second half is what makes the fold a *migration* rather than a
/// second copy of the state: a save from the new model must not put
/// `viewport_sync`/`sync_layers` back on the wire, or a later load would
/// AND stale globals into links the user has since changed.
#[test]
fn absent_legacy_globals_mean_linked_and_are_never_rewritten() {
    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX",
                    "panes":[{"site":"KMPX"},{"site":"KOUN"}]}"#,
        )
        .unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    for idx in 0..2 {
        let pane = restored.pane(idx).unwrap();
        assert!(
            pane.viewport_link && pane.layer_link && pane.time_link,
            "pane {idx}: absent legacy fields must load as all-linked"
        );
    }

    let json = restored.ui_config_json().expect("serializable");
    assert!(
        !json.contains("\"viewport_sync\"") && !json.contains("\"sync_layers\""),
        "the retired globals must never be written again"
    );
    assert!(
        json.contains("\"viewport_link\"") && json.contains("\"layer_link\""),
        "the per-pane links are the persisted state now"
    );
}

/// A drawn Volume Alpha curve survives the round trip, per product, and an
/// untouched product comes back untouched.
///
/// The untouched half is the bit-exactness pin at the persistence layer: a
/// product with no entry must load with no entry, because "no entry" is
/// what licenses the renderer to upload the palette's own LUT unmodified.
/// A load that filled every product with a synthesised default curve would
/// round-trip every *assertion about values* here and still break that.
#[test]
fn volume_alpha_curves_survive_a_save_and_load() {
    use crate::volume_alpha::{AlphaCurve, CURVE_LEN};
    use rustdar_radar::types::RadarProduct;

    let store = MemoryConfigStore::default();
    let mut gui = crate::Gui::new();
    let mut alphas = [0u8; CURVE_LEN];
    for (i, slot) in alphas.iter_mut().enumerate() {
        *slot = (i / 2) as u8; // a curve no default produces
    }
    let curve = AlphaCurve::from_alphas(alphas);
    gui.volume_alpha
        .set(RadarProduct::Reflectivity, curve.clone());
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.volume_alpha.get(RadarProduct::Reflectivity),
        Some(curve),
        "the drawn curve must come back exactly",
    );
    assert_eq!(
        restored.volume_alpha.get(RadarProduct::Velocity),
        None,
        "a product the user never edited must come back with no curve at all",
    );
}

/// A config written before Volume Alpha existed loads with every editor
/// untouched — the field defaults to empty, and empty means bit-exact.
#[test]
fn an_old_config_without_volume_alpha_loads_with_every_editor_untouched() {
    use rustdar_radar::types::RadarProduct;

    let store = MemoryConfigStore::default();
    // A minimal pre-feature config: every field the format has ever had is
    // `#[serde(default)]`-covered, so `{}` is exactly what an old file
    // looks like to the new deserializer.
    store
        .store(UI_CONFIG_KEY, "{}")
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store), "an old config still loads");
    assert!(
        !gui.volume_alpha.is_edited(RadarProduct::Reflectivity),
        "an old config must not conjure a curve for any product",
    );
}

/// A hand-edited or version-skewed curve cannot poison the load: a wrong
/// length is dropped by name, and a curve claiming a visible no-data index
/// is re-clamped on the way in.
///
/// The re-clamp half is the config-side door of the index-0 invariant —
/// the editor and the stroke both enforce it live, and this is the one
/// writer that bypasses them.
#[test]
fn a_hostile_volume_alpha_entry_is_dropped_or_reclamped_never_trusted() {
    use rustdar_radar::types::RadarProduct;

    let store = MemoryConfigStore::default();
    // Entry one: three alphas where 256 are required. Entry two: a full
    // curve whose entry 0 claims opaque no-data.
    let mut full: Vec<String> = vec!["255".to_owned(); 256];
    full[1] = "9".to_owned();
    let json = format!(
        r#"{{"volume_alpha":[
                {{"product":"Reflectivity","alpha":[1,2,3]}},
                {{"product":"Velocity","alpha":[{}]}}
            ]}}"#,
        full.join(","),
    );
    store
        .store(UI_CONFIG_KEY, &json)
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store), "the rest of the config loads");
    assert_eq!(
        gui.volume_alpha.get(RadarProduct::Reflectivity),
        None,
        "a wrong-length curve must be dropped, not padded or truncated",
    );
    let velocity = gui
        .volume_alpha
        .get(RadarProduct::Velocity)
        .expect("a well-sized curve loads");
    assert_eq!(
        velocity.alphas()[0],
        0,
        "entry 0 is the no-data index and must be re-clamped on load",
    );
    assert_eq!(
        velocity.alphas()[1],
        9,
        "the rest of the curve is kept as saved"
    );
    assert_eq!(velocity.alphas()[255], 255);
}

/// A 3D pane's view mode and the per-product isosurface thresholds
/// survive the round trip; an untouched product comes back untouched.
///
/// The untouched half is the exceptions-store pin, exactly as the alpha
/// curves': absence means the argued default, and a load that filled
/// every product with an entry would survive the value assertions and
/// still break "re-arguing a default reaches everyone who never moved
/// it".
#[test]
fn the_isosurface_mode_and_thresholds_survive_a_save_and_load() {
    use crate::pane::{PaneKind, VolumeViewMode};
    use rustdar_radar::types::RadarProduct;

    let store = MemoryConfigStore::default();
    let mut gui = crate::Gui::new();
    gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
    gui.pane_mut(0).unwrap().volume_mut().unwrap().view_mode = VolumeViewMode::Isosurface;
    gui.volume_iso.set(RadarProduct::Velocity, 35.0);
    assert_ne!(
        rustdar_radar::voxel::default_iso_threshold(RadarProduct::Velocity),
        35.0,
        "precondition: the saved threshold must differ from the default",
    );
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).unwrap().volume().unwrap().view_mode,
        VolumeViewMode::Isosurface,
        "a pane set to isosurface must come back one",
    );
    assert_eq!(restored.volume_iso.get(RadarProduct::Velocity), 35.0);
    assert!(
        !restored.volume_iso.is_edited(RadarProduct::Reflectivity),
        "an untouched product must come back at the argued default",
    );
}

/// A 3D pane that turned its map floor off comes back with it off.
///
/// The rule this serves is the codebase's standing one: reopening the app is
/// 1:1 visually with how it was closed, live data excepted. The floor is not
/// live data — it is a choice the user made with a checkbox — so a pane that
/// closed without a floor and opened with one is a visible difference on
/// launch, which is exactly what the rule forbids. `hide_floor` was
/// hardcoded to `false` on load and commented as session state; this is the
/// pin on that no longer being true.
///
/// Both directions are asserted, because a field written but never read and a
/// field read but never written fail in opposite halves of the round trip and
/// a one-sided test sees only one of them.
#[test]
fn a_hidden_map_floor_survives_a_save_and_load() {
    use crate::pane::PaneKind;

    let store = MemoryConfigStore::default();
    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::Volume);
    assert!(
        !gui.pane(0).unwrap().volume().unwrap().hide_floor,
        "precondition: a fresh 3D pane shows its floor",
    );
    gui.pane_mut(0).unwrap().volume_mut().unwrap().hide_floor = true;
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert!(
        restored.pane(0).unwrap().volume().unwrap().hide_floor,
        "a pane that turned the floor off must come back with it off",
    );
    assert!(
        !restored.pane(1).unwrap().volume().unwrap().hide_floor,
        "and the toggle is per pane: its neighbour keeps its floor",
    );
}

/// A config written before `hide_floor` existed comes back with the floor
/// **showing**.
///
/// This is what the wire form's inversion buys, and it is the reason the
/// persisted field is `hide_floor` rather than a positive `show_floor`:
/// `#[serde(default)]` supplies `false` for the missing key, and `false` is
/// the floor showing. Stored the other way round, every config already on
/// every user's disk would restore with the floor off — a silent, global
/// regression on the first launch after the upgrade, and one no round-trip
/// test would catch because a round trip never sees an absent key.
#[test]
fn a_config_written_before_the_floor_toggle_keeps_its_floor() {
    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "KDMX",
                    "panes": [{"kind": "Volume", "site": "KDMX", "volume": {}}]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store));
    assert!(
        !gui.pane(0).unwrap().volume().unwrap().hide_floor,
        "an absent key must mean the shipped default: the floor shows",
    );
}

/// A view mode from a future build loads as the lit volume, and a
/// threshold for an unknown product is dropped — the same forward
/// tolerance the product enum has, pinned for the two new fields.
#[test]
fn an_unknown_view_mode_or_iso_product_does_not_poison_the_load() {
    use crate::pane::VolumeViewMode;
    use rustdar_radar::types::RadarProduct;

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "KDMX",
                    "panes": [{
                        "kind": "Volume",
                        "site": "KDMX",
                        "volume": {"view_mode": "HolographicSlices"}
                    }],
                    "volume_iso": [
                        {"product": "TornadoProbability", "threshold": 5.0},
                        {"product": "Velocity", "threshold": 30.0}
                    ]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "an unknown view mode or product name must not fail the load",
    );
    assert_eq!(
        gui.pane(0).unwrap().volume().unwrap().view_mode,
        VolumeViewMode::LitVolume,
        "an unknown mode falls back to the lit volume",
    );
    assert_eq!(
        gui.volume_iso.get(RadarProduct::Velocity),
        30.0,
        "the entry beside the unknown one still loads",
    );
    assert_eq!(
        gui.volume_iso.entries().count(),
        1,
        "the unknown product's threshold is dropped, never reassigned",
    );
}

/// A config naming a product this build does not know still loads.
///
/// The products WP multiplies the entries in saved configs, so the failure
/// this pins is a *forward*-compatibility one: a config written by a later
/// build (or the same build with a product since renamed) must not fail
/// the whole load — which would cost the user their site, layout and
/// curves permanently, because the autosave then rewrites the file from
/// defaults. The pane falls back to the default product; everything else
/// in the file survives. The fixture site is deliberately not the KTLX
/// default, so "the site survived" cannot pass by the default masking a
/// failed load.
#[test]
fn a_config_naming_a_product_from_the_future_still_loads() {
    use rustdar_radar::types::RadarProduct;

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "KDMX",
                    "loop_lookback_secs": 7200,
                    "panes": [{"selected_product": "TornadoProbability", "site": "KDMX"}]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "one unknown product name must not fail the whole config load",
    );
    assert_eq!(
        gui.pane(0).unwrap().selected_product,
        RadarProduct::Reflectivity,
        "the unknown product falls back to the default product",
    );
    assert_eq!(
        gui.loop_lookback_secs, 7200,
        "the rest of the file must survive the unknown product",
    );
}

/// An alpha curve saved for an unknown product is dropped, never
/// reassigned to a product this build knows.
///
/// Falling back to a default here — the way the pane's product picker does
/// — would be wrong in kind, not just in degree: the curve would silently
/// restyle a product the user never drew it for. The entry beside it must
/// still load, pinning that the drop is per-entry rather than the whole
/// list.
#[test]
fn an_alpha_curve_for_an_unknown_product_is_dropped_not_reassigned() {
    use rustdar_radar::types::RadarProduct;

    let store = MemoryConfigStore::default();
    let full: Vec<String> = vec!["128".to_owned(); 256];
    let json = format!(
        r#"{{"volume_alpha":[
                {{"product":"TornadoProbability","alpha":[{alphas}]}},
                {{"product":"Velocity","alpha":[{alphas}]}}
            ]}}"#,
        alphas = full.join(","),
    );
    store
        .store(UI_CONFIG_KEY, &json)
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store), "the rest of the config loads");
    assert_eq!(
        gui.volume_alpha.entries().count(),
        1,
        "exactly the known product's curve loads — the unknown one is \
             dropped, not remapped onto some default product",
    );
    assert!(
        gui.volume_alpha.get(RadarProduct::Velocity).is_some(),
        "the entry beside the unknown one still loads",
    );
}

/// A cross-section pane's line and source survive the round trip.
///
/// Separate from the test above because a section pane is the kind nothing
/// creates yet: it is reachable only through `set_kind`, and its persistence
/// has to be right *before* WP-G's draw interaction starts producing them —
/// otherwise the first line a user ever draws is also the first one to be
/// silently lost on restart.
///
/// The endpoints are compared exactly. They are `f64` written and read as
/// decimal by `serde_json`, which round-trips every finite `f64` exactly, and
/// `SectionTarget`'s staleness comparison is bitwise — so an approximate
/// assertion here would hide the one kind of drift that matters.
#[test]
fn a_drawn_section_line_survives_a_save_and_load() {
    use crate::pane::{GeoPoint, PaneKind, SectionLine};

    let store = MemoryConfigStore::default();
    let a = GeoPoint {
        lat: 35.0,
        lon: -97.8,
    };
    let b = GeoPoint {
        lat: 35.6,
        lon: -96.9,
    };

    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    {
        let section = gui
            .pane_mut(1)
            .unwrap()
            .cross_section_mut()
            .expect("converted");
        section.line = SectionLine::new(a, b);
        section.source_pane = Some(0);
    }
    assert_eq!(
        gui.pane(0).unwrap().kind(),
        PaneKind::Map,
        "precondition: the other pane stays a map, so the kind is per pane"
    );
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Map);
    let section = restored
        .pane(1)
        .unwrap()
        .cross_section()
        .expect("pane 1 came back as something other than a section");
    assert_eq!(
        section.line.map(|line| (line.a(), line.b())),
        Some((a, b)),
        "the line came back somewhere else"
    );
    assert_eq!(section.source_pane, Some(0));
    assert_eq!(
        section.rendered_for, None,
        "the staleness key must not be persisted: it names a volume that is \
             not loaded, so a restored pane would think its image was current"
    );
}

/// Every shape a config can describe that the in-memory representation
/// cannot, and each one falls back to a map rather than failing the load.
///
/// `PaneContent` derives the kind from the content, so none of these is
/// representable in the app — they exist only on the wire, where a file can
/// say anything: hand-edited, shared between versions, or written by a later
/// version than the one reading it. `Map` is the fallback because it is the
/// kind that needs nothing, and refusing the whole config would throw away
/// the site, the layout and every layer setting over one bad number.
#[test]
fn a_pane_config_that_cannot_be_a_pane_loads_as_a_map() {
    use crate::pane::PaneKind;

    for (name, pane_json) in [
        (
            "a section with no section state at all",
            r#"{"kind":"CrossSection"}"#,
        ),
        ("a 3D view with no camera", r#"{"kind":"Volume"}"#),
        (
            "a section line off the earth, which walks a well-defined great \
                 circle over nowhere and renders as empty coverage",
            r#"{"kind":"CrossSection","cross_section":{"line":
                   {"a_lat":1e9,"a_lon":-97.8,"b_lat":35.6,"b_lon":-96.9}}}"#,
        ),
        (
            "a zero-length section line, which has no bearing to walk along",
            r#"{"kind":"CrossSection","cross_section":{"line":
                   {"a_lat":35.0,"a_lon":-97.8,"b_lat":35.0,"b_lon":-97.8}}}"#,
        ),
    ] {
        let store = MemoryConfigStore::default();
        store
            .store(
                UI_CONFIG_KEY,
                &format!(r#"{{"pane_count":1,"site":"KTLX","panes":[{pane_json}]}}"#),
            )
            .unwrap();

        let mut restored = crate::Gui::new();
        assert!(
            restored.load_ui_config(&store),
            "{name}: the config must still load — falling back is per pane, \
                 not a refusal of the file"
        );
        assert_eq!(
            restored.pane(0).unwrap().kind(),
            PaneKind::Map,
            "{name}: loaded as a pane whose kind and state disagree"
        );
        assert_eq!(
            restored.pane(0).unwrap().site,
            "KTLX",
            "{name}: the rest of the pane was lost with its kind"
        );
    }
}

/// A section pane converted but not yet aimed is an ordinary state, not a
/// corrupt one.
///
/// It is what every section pane looks like between being created and having
/// a line drawn on a map, so a loader that treated a missing line as
/// unrecoverable would convert it back to a map on every restart.
#[test]
fn a_section_pane_with_no_line_yet_comes_back_as_a_section() {
    use crate::pane::PaneKind;

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX",
                    "panes":[{"kind":"CrossSection","cross_section":{}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    let section = restored
        .pane(0)
        .unwrap()
        .cross_section()
        .expect("an unaimed section is a section");
    assert!(section.line.is_none());
    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::CrossSection);
}

/// A source-pane index the restored layout does not have is forgotten, and the
/// pane stays a section.
///
/// This is a six-pane desktop config opened on a phone: the clamp narrows the
/// layout, and an index saved against the wider one now names a different pane
/// or none at all. Dropped rather than clamped, because retargeting a section
/// onto whichever map happens to sit nearby is worse than treating it as never
/// having been aimed from anywhere — and the kind is kept, because the line
/// itself is still a perfectly good line.
#[test]
fn a_section_sourced_from_a_pane_that_is_gone_forgets_where_it_came_from() {
    use crate::pane::PaneKind;

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"CrossSection","cross_section":{
                        "line":{"a_lat":35.0,"a_lon":-97.8,"b_lat":35.6,"b_lon":-96.9},
                        "source_pane":5}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(
        restored.pane_count(),
        1,
        "precondition: one pane, so 5 is out"
    );
    let section = restored
        .pane(0)
        .unwrap()
        .cross_section()
        .expect("the kind survives a stale source index");
    assert_eq!(section.source_pane, None);
    assert!(
        section.line.is_some(),
        "the line is still a line; only where it was drawn was lost"
    );
    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::CrossSection);
}

/// A config written before pane kinds existed loads as a screen full of maps.
///
/// The container carries `#[serde(default)]`, so no per-field attribute is
/// needed — the same mechanism `live_chunks` and `notifier_endpoint` rely on.
/// This is the shape every already-installed copy has on disk.
#[test]
fn a_config_predating_pane_kinds_loads_as_maps() {
    use crate::pane::PaneKind;

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX",
                    "panes":[{"site":"KMPX","zoom":7.0},{"site":"KOUN"}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));

    assert_eq!(
        (0..2)
            .map(|i| restored.pane(i).unwrap().kind())
            .collect::<Vec<_>>(),
        vec![PaneKind::Map, PaneKind::Map],
    );
    assert_eq!(restored.pane(0).unwrap().map_memory.zoom(), 7.0);
    assert_eq!(restored.pane(1).unwrap().site, "KOUN");
}

/// A restored non-map pane arrives with the same invariants as a converted
/// one: no running loop.
///
/// The loader goes through `PaneState::set_content` rather than writing
/// `content`, so that the teardown a kind change implies has exactly one
/// description — see `PaneState::set_kind` for what a loop left running on a
/// pane nothing renders frames for actually does, which includes stopping
/// every *other* pane's loop from ever starting.
///
/// The pane is given a live loop first, which the startup path cannot
/// currently produce (the config is loaded into a fresh `Gui`). That is the
/// point: the invariant belongs to the setter rather than to the sequence its
/// callers happen to run in today.
#[test]
fn a_restored_non_map_pane_has_no_running_loop() {
    use crate::pane::{LoopPhase, PaneKind};

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"Volume","volume":
                        {"yaw_deg":225.0,"pitch_deg":25.0,"eye_distance":2.5}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.pane_mut(0).unwrap().loop_state.phase = LoopPhase::Playing;
    assert!(
        restored.pane(0).unwrap().loop_state.is_active(),
        "precondition: the loop must be running before the load"
    );

    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Volume);
    assert!(
        !restored.pane(0).unwrap().loop_state.is_active(),
        "a restored 3D pane came back with a loop nothing will ever render \
             frames for, which holds every other pane's loop back too"
    );
}

/// A finite camera outside the documented range is clamped, not discarded.
///
/// The distinction the loader draws: a value that is *unusable* — non-finite,
/// off the earth, a line with no bearing — costs the pane its kind, and one
/// that is merely *out of range* is brought inside it. Only a hand-edited or
/// version-skewed config can produce the second, and `restore_viewport`
/// reasons the same way about a saved zoom: there is nothing to propagate, and
/// the nearest legal camera beats discarding the pane over a number.
#[test]
fn a_saved_camera_out_of_range_is_clamped_rather_than_dropped() {
    use crate::pane::PaneKind;

    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"Volume","volume":
                        {"yaw_deg":-30.0,"pitch_deg":1000.0,"eye_distance":0.001}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Volume);
    let camera = restored
        .pane(0)
        .unwrap()
        .volume()
        .expect("a 3D pane")
        .camera;
    assert_eq!(camera.yaw_deg(), 330.0, "yaw wraps rather than clamping");
    assert!(
        camera.pitch_deg().abs() < 90.0,
        "pitch {}",
        camera.pitch_deg()
    );
    assert_eq!(
        camera.eye_distance(),
        0.05,
        "an under-range saved distance must clamp to the zoom's near stop \
             (0.05 half-diagonals — inside the box is a supported camera), not \
             be discarded",
    );
}

/// What the write-side finiteness filter actually prevents — and it is worse
/// than "the config fails to serialize".
///
/// `serde_json` does **not** refuse a non-finite float. It writes `null`. So
/// the write succeeds silently, the file on disk looks fine, and it is the
/// *next load* that fails: `null` will not deserialize into an `f32`, so
/// `from_str::<UiConfig>` errors, `load_ui_config` logs one warning and
/// returns `false`, and every setting in the file is gone. The user's only
/// symptom is the app forgetting everything — one run after the mistake, with
/// nothing at the time to connect the two, and permanently, because the next
/// autosave rewrites the file from defaults.
///
/// That is why the guard is on the *write* side for every float, including the
/// ones whose in-memory writers already promise finiteness
/// (`OrbitCamera::nudge`, `SectionLine::new`): a filter costs one pane its
/// kind, a missing filter costs the user their whole configuration.
#[test]
fn a_non_finite_float_would_poison_the_config_file_permanently() {
    use crate::pane::PaneKind;

    assert_eq!(
        serde_json::to_string(&f32::NAN).expect("serde_json writes it happily"),
        "null",
        "if this ever starts erroring instead, these guards become about a \
             failed save rather than about a file that can never be read again"
    );
    assert!(
        serde_json::from_str::<f32>("null").is_err(),
        "and this is the half that makes it permanent"
    );

    // The property the filter protects: a `Gui` with a non-map pane writes a
    // config that loads back, rather than one that reads as corrupt.
    let mut gui = crate::Gui::new();
    gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
    let json = gui
        .ui_config_json()
        .expect("a 3D pane stopped the config from being written at all");
    // Checked per field rather than by looking for `null` anywhere: the file
    // legitimately contains several, because an absent `Option` is written that
    // way and reads back as `None`. It is the **non-**`Option` numbers that
    // cannot survive one.
    let written: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    for field in ["yaw_deg", "pitch_deg", "eye_distance"] {
        let value = &written["panes"][0]["volume"][field];
        assert!(
            value.is_f64(),
            "{field} was written as {value}, which will fail every future load"
        );
    }

    let store = MemoryConfigStore::default();
    store.store(UI_CONFIG_KEY, &json).unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Volume);
}

/// Zoom and pan are what "come back to where I left off" actually means, and
/// neither was persisted before.
#[test]
fn a_panned_and_zoomed_map_comes_back_where_it_was_left() {
    let store = MemoryConfigStore::default();

    let baseline = crate::Gui::new();
    let default_zoom = baseline.pane(0).unwrap().map_memory.zoom();
    assert_ne!(
        default_zoom, 9.0,
        "the test zoom must differ from the default"
    );
    assert!(
        baseline.pane(0).unwrap().map_memory.detached().is_none(),
        "a fresh pane follows its site; the test then pans it away"
    );

    let mut gui = crate::Gui::new();
    {
        let pane = gui.pane_mut(0).unwrap();
        pane.map_memory.set_zoom(9.0).unwrap();
        pane.map_memory
            .center_at(walkers::lat_lon(44.9778, -93.2650));
    }
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    let pane = restored.pane(0).unwrap();
    assert_eq!(pane.map_memory.zoom(), 9.0);
    let center = pane.map_memory.detached().expect("the pan was persisted");
    // `Position` is (x, y) = (lon, lat). A transposition here is silently a
    // valid coordinate, just the wrong hemisphere.
    assert!((center.y() - 44.9778).abs() < 1e-9, "lat {}", center.y());
    assert!((center.x() + 93.2650).abs() < 1e-9, "lon {}", center.x());
}

/// Following the site and being centred on the site's coordinates look the
/// same until the pane changes site, at which point one moves and the other
/// does not. A round trip must not silently convert the first into the second.
#[test]
fn a_map_following_its_site_does_not_come_back_pinned() {
    let store = MemoryConfigStore::default();

    let mut gui = crate::Gui::new();
    gui.pane_mut(0).unwrap().map_memory.set_zoom(7.0).unwrap();
    assert!(gui.pane(0).unwrap().map_memory.detached().is_none());
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().map_memory.zoom(), 7.0);
    assert!(
        restored.pane(0).unwrap().map_memory.detached().is_none(),
        "an un-panned map was restored as pinned to a fixed centre"
    );
}

/// Configs written before the viewport was persisted must keep the built-in
/// default zoom rather than being read as "saved zoom 0".
#[test]
fn a_config_predating_viewport_persistence_keeps_the_default_zoom() {
    let store = MemoryConfigStore::default();
    let default_zoom = crate::Gui::new().pane(0).unwrap().map_memory.zoom();

    // A config with panes but no `zoom`/`center` keys at all — exactly the
    // shape every already-installed copy of the app has on disk right now.
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KMPX","panes":[{"site":"KMPX"}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().site, "KMPX");
    assert_eq!(
        restored.pane(0).unwrap().map_memory.zoom(),
        default_zoom,
        "an absent zoom was treated as a saved value"
    );
    assert!(restored.pane(0).unwrap().map_memory.detached().is_none());
}

/// A pane layout wider than a phone offers survives the round trip.
///
/// This is the data-loss bug the clamp exists to prevent, asserted at the
/// call site rather than on the constant. `max_panes_absolute()`'s *value*
/// was already pinned in `ui_layout`, but nothing checked that
/// `load_ui_config` used it: reverting the clamp to
/// `WidthClass::Compact.max_panes()` — the precise regression — passed the
/// whole suite. A 6-pane desktop layout opened once on a phone came back as
/// 4 and was written back as 4 on the next save.
#[test]
fn a_pane_layout_wider_than_a_phone_offers_survives_the_round_trip() {
    use crate::pane::{MAX_PANES_DESKTOP, MAX_PANES_MOBILE};
    use crate::ui_layout::WidthClass;

    assert!(
        MAX_PANES_DESKTOP > WidthClass::Compact.max_panes(),
        "precondition: the saved layout must be wider than a compact screen \
             would offer, or the clamp under test is never reached"
    );

    let store = MemoryConfigStore::default();
    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(MAX_PANES_DESKTOP);
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(
        restored.pane_count(),
        MAX_PANES_DESKTOP,
        "the config was clamped to the current device's limit, so the \
             user's layout is gone and the next save writes the truncated one"
    );

    // Saving it again must not quietly narrow it either — the round trip
    // is what turns a one-off clamp into permanent data loss.
    let second = MemoryConfigStore::default();
    restored.save_ui_config(&second);
    let mut again = crate::Gui::new();
    again.load_ui_config(&second);
    assert_eq!(again.pane_count(), MAX_PANES_DESKTOP);

    assert_ne!(
        MAX_PANES_DESKTOP, MAX_PANES_MOBILE,
        "precondition: the two limits must differ, or nothing above can \
             tell a correct clamp from the broken one"
    );
}

/// Loading from a store with nothing in it must leave the defaults alone
/// rather than zeroing them — this is every first run.
#[test]
fn an_empty_store_leaves_defaults_untouched() {
    let store = MemoryConfigStore::default();
    let mut gui = crate::Gui::new();
    let expected = gui.loop_lookback_secs;

    gui.load_ui_config(&store);

    assert_eq!(gui.loop_lookback_secs, expected);
}

/// A corrupt config must not wipe the user's session or panic.
#[test]
fn unparseable_config_is_ignored() {
    let store = MemoryConfigStore::default();
    store.store(UI_CONFIG_KEY, "{ not json").unwrap();

    let mut gui = crate::Gui::new();
    let expected = gui.loop_lookback_secs;
    gui.load_ui_config(&store);

    assert_eq!(gui.loop_lookback_secs, expected);
}

/// Saving writes under the shared key, which is what the filesystem backend
/// maps onto `ui.json`.
#[test]
fn save_writes_under_the_ui_key() {
    let store = MemoryConfigStore::default();
    assert!(store.load(UI_CONFIG_KEY).is_none());

    crate::Gui::new().save_ui_config(&store);

    let written = store.load(UI_CONFIG_KEY).expect("config should be stored");
    assert!(
        serde_json::from_str::<super::UiConfig>(&written).is_ok(),
        "stored blob should parse back as a UiConfig"
    );
}
