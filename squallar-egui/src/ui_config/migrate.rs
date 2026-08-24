//! Stepwise config-format migrations, applied to the raw JSON tree before
//! `UiConfig` ever sees it.

pub(crate) const CONFIG_VERSION: u32 = 5;

/// The version a file with no `config_version` key speaks: every config
/// written before the field existed. A constant fact about history — this
/// never moves when [`CONFIG_VERSION`] does.
pub(crate) fn first_version() -> u32 {
    1
}

/// One in-place format rewrite: the version it reads, and the edit that
/// makes the tree speak the next one.
type Migration = (u32, fn(&mut serde_json::Value));

/// One step per entry: a file at exactly the named version is rewritten in
/// place to speak the next one, in order, so a v1 file walks every rung.
const MIGRATIONS: &[Migration] = &[
    (1, split_gps_config),
    (2, panes_take_layer_slots),
    (3, radar_takes_its_settings),
    (4, panes_take_the_root_site),
];

/// v1 → v2: the `gps_config` container split — the serial half (`port_path`,
/// `baud_rate`) keeps the container under the new name `serial_config`, and
/// `heading_source` becomes its own top-level key.
fn split_gps_config(value: &mut serde_json::Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    if !root.get("gps_config").is_some_and(|v| v.is_object()) {
        return;
    }
    let mut blob = root.remove("gps_config").expect("presence checked above");
    let heading = blob
        .as_object_mut()
        .expect("shape checked above")
        .remove("heading_source");
    root.insert("serial_config".to_string(), blob);
    if let Some(heading) = heading {
        root.insert("heading_source".to_string(), heading);
    }
}

/// v2 → v3: **a pane's three parallel layer containers become one ordered
/// list of slots.** `draw_order`, `enabled_overlays` and `overlay_configs`
/// were keyed on the same layer ids and had to be kept in step by hand; a
/// slot carries all three facts about one layer together, and the list's
/// order is the draw order.
///
/// The radar layer gets a slot like any other, and its config is where this
/// pane's own selection now lives — `site`, `product`, `elevation`, moved out
/// of the flat pane fields — plus `live_chunks`, **fanned out from the one
/// global** so a per-pane answer becomes expressible. The global stays at the
/// root: the settings UI still writes it, and an older build still reads it.
///
/// Everything the step does not consume is left exactly as it was, and every
/// value it moves is moved **verbatim**: an id no build here serves keeps its
/// slot, a product spelling this build cannot read is still handed to the
/// tolerant reader that has always fielded it, and a `draw_order` element
/// that is not a string at all rides along as a non-slot entry.
fn panes_take_layer_slots(value: &mut serde_json::Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    // The global's value at the moment of the move — the switch every pane
    // was answering to before per-pane answers existed.
    let global_live_chunks = root
        .get("live_chunks")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let Some(panes) = root
        .get_mut("panes")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for pane in panes {
        let Some(pane) = pane.as_object_mut() else {
            continue;
        };
        // Consumed either way: a pane that already carries slots is one a
        // newer build wrote and an older one handed back, and its flat fields
        // are the older build's stale copy, not the truth.
        let draw_order = pane.remove("draw_order");
        let enabled_overlays = pane.remove("enabled_overlays");
        let overlay_configs = pane.remove("overlay_configs");
        let site = pane.remove("site");
        let product = pane.remove("selected_product");
        let elevation = pane.remove("selected_elevation");
        if pane.contains_key("layer_slots") {
            continue;
        }

        let member = |value: &Option<serde_json::Value>, key: &str| -> Option<serde_json::Value> {
            value.as_ref()?.as_object()?.get(key).cloned()
        };
        let keys = |value: &Option<serde_json::Value>| -> Vec<String> {
            value
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default()
        };

        // The order, exactly as the list gave it. An absent list is not "the
        // default order" here: the default is the build's weight order, which
        // this step cannot know and the load reconstructs anyway.
        let mut ids: Vec<String> = Vec::new();
        let mut non_ids: Vec<serde_json::Value> = Vec::new();
        if let Some(serde_json::Value::Array(list)) = &draw_order {
            for entry in list {
                match entry {
                    serde_json::Value::String(name) if !ids.contains(name) => {
                        ids.push(name.clone());
                    }
                    serde_json::Value::String(_) => {}
                    other => non_ids.push(other.clone()),
                }
            }
        }
        // Ids the maps name but the order did not, appended in a fixed order
        // so the same file always migrates to the same stack. The radar id is
        // always one of them: its slot is where the pane's selection goes,
        // so it has to exist whether or not the file listed it.
        let mut extras: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for key in keys(&enabled_overlays)
            .into_iter()
            .chain(keys(&overlay_configs))
        {
            extras.insert(key);
        }
        extras.insert(RADAR_ID.to_string());
        for id in ids.iter() {
            extras.remove(id);
        }
        ids.extend(extras);

        let mut slots: Vec<serde_json::Value> = Vec::with_capacity(ids.len() + non_ids.len());
        for id in ids {
            let mut slot = serde_json::Map::new();
            slot.insert("id".to_string(), serde_json::Value::String(id.clone()));
            // A layer the map states nothing about states nothing here
            // either: the load asks the handler, exactly as it always did for
            // a missing map entry.
            if let Some(on) = member(&enabled_overlays, &id).and_then(|v| v.as_bool()) {
                slot.insert("enabled".to_string(), serde_json::Value::Bool(on));
            }
            let mut config = match member(&overlay_configs, &id) {
                Some(serde_json::Value::Object(map)) => map,
                _ => serde_json::Map::new(),
            };
            if id == RADAR_ID {
                if let Some(site) = site.clone() {
                    config.insert("site".to_string(), site);
                }
                if let Some(product) = product.clone() {
                    config.insert("product".to_string(), product);
                }
                if let Some(elevation) = elevation.clone() {
                    config.insert("elevation".to_string(), elevation);
                }
                config.insert(
                    "live_chunks".to_string(),
                    serde_json::Value::Bool(global_live_chunks),
                );
            }
            if !config.is_empty() {
                slot.insert("config".to_string(), serde_json::Value::Object(config));
            }
            slots.push(serde_json::Value::Object(slot));
        }
        slots.extend(non_ids);
        pane.insert("layer_slots".to_string(), serde_json::Value::Array(slots));
    }
}

/// The radar layer's id, spelled here rather than imported: a migration is a
/// fact about a file that was already written, and it must not move when the
/// constant a live build uses moves.
const RADAR_ID: &str = "Radar";

/// v3 → v4: **the archive poll and the chunk feed's three switches move from
/// the config root into the radar layer's own state blob.**
///
/// `auto_poll`, `live_chunks`, `chunk_notifications` and `notifier_endpoint`
/// were four root keys read by `Gui` fields. The fields are now
/// `RadarSource`'s, so the keys travel with the layer, under
/// `overlay_states["Radar"]` — the same place every other handler's settings
/// already live, written and read by `serialize_state`/`deserialize_state`.
///
/// **A pure key move.** Each value is carried across **verbatim**, by
/// `serde_json::Value` and not by type: a `live_chunks` some other build wrote
/// as a string is handed to the handler's own tolerant reader exactly as it
/// arrived, rather than being coerced or dropped here. A key the file does not
/// have is not invented — the handler's default stands, which is what
/// `#[serde(default)]` did for these keys at the root.
///
/// **It runs after [`panes_take_layer_slots`]**, which reads the root
/// `live_chunks` for its per-pane fan-out. The chain guarantees the order: a
/// v2 file walks rung 2 before rung 3, so the fan-out reads the key while it
/// is still at the root.
///
/// An existing `overlay_states["Radar"]` entry is **not** replaced — its
/// members are merged under, so a key the blob already carries wins over the
/// root's copy. A file holding both is one a newer build wrote and an older
/// one handed back, and the blob is the newer half.
fn radar_takes_its_settings(value: &mut serde_json::Value) {
    const MOVED: [&str; 4] = [
        "auto_poll",
        "live_chunks",
        "chunk_notifications",
        "notifier_endpoint",
    ];
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let carried: Vec<(String, serde_json::Value)> = MOVED
        .iter()
        .filter_map(|key| root.remove(*key).map(|v| ((*key).to_string(), v)))
        .collect();
    if carried.is_empty() {
        return;
    }
    let states = root
        .entry("overlay_states".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !states.is_object() {
        // A malformed `overlay_states` is dropped by the loader's own repair
        // step; overwriting it here would hide that from it.
        return;
    }
    let states = states.as_object_mut().expect("shape checked above");
    let radar = states
        .entry(RADAR_ID.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !radar.is_object() {
        return;
    }
    let radar = radar.as_object_mut().expect("shape checked above");
    for (key, moved) in carried {
        radar.entry(key).or_insert(moved);
    }
}

/// v4 → v5: **the root `site` reaches the panes, and the key goes.**
///
/// There was one app-wide site: the key every save wrote, and the seed a pane
/// naming none of its own was opened on. A pane owns its site now, so the
/// value has to reach the panes before the key can go — otherwise an upgrade
/// would open every such pane on a radar the user never chose.
///
/// **A pane that already names a site is not touched.** Its own answer
/// outranks the root's, which is the whole point of the key going away; the
/// seed only fills a gap. The value is carried **verbatim**, into the same
/// radar-slot member [`panes_take_layer_slots`] writes, so a spelling this
/// build could not read is handed to the tolerant reader that has always
/// fielded it rather than being coerced or dropped here.
///
/// **The key goes whether or not a pane took it**, so a build that no longer
/// reads it cannot rewrite it forever as an unknown it is preserving. A file
/// that never had one is left untouched: nothing to seed, nothing to remove.
fn panes_take_the_root_site(value: &mut serde_json::Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let Some(site) = root.remove("site") else {
        return;
    };
    // **The site must have somewhere to land.** A file naming a root site and
    // no panes at all is not hypothetical — it is the shape the Tier-2 rig
    // seeded its scene with until this land, and the shape a hand-written
    // config takes. Dropping the key without placing it would reopen that
    // session on a compiled-in default. One entry is enough: the load seeds
    // every pane the file counts but never describes from the first pane it
    // does.
    if !root.get("panes").is_some_and(serde_json::Value::is_array) {
        root.insert("panes".to_string(), serde_json::Value::Array(Vec::new()));
    }
    let Some(panes) = root
        .get_mut("panes")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    if panes.is_empty() {
        panes.push(serde_json::json!({}));
    }
    for pane in panes {
        let Some(pane) = pane.as_object_mut() else {
            continue;
        };
        let slots = pane
            .entry("layer_slots".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let Some(slots) = slots.as_array_mut() else {
            continue;
        };
        if !slots
            .iter()
            .any(|slot| slot.get("id").and_then(serde_json::Value::as_str) == Some(RADAR_ID))
        {
            slots.push(serde_json::json!({ "id": RADAR_ID }));
        }
        let Some(radar) = slots
            .iter_mut()
            .find(|slot| slot.get("id").and_then(serde_json::Value::as_str) == Some(RADAR_ID))
        else {
            continue;
        };
        let Some(radar) = radar.as_object_mut() else {
            continue;
        };
        let config = radar
            .entry("config".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let Some(config) = config.as_object_mut() else {
            continue;
        };
        config
            .entry("site".to_string())
            .or_insert_with(|| site.clone());
    }
}

/// Walk `value` up from whatever version it speaks to [`CONFIG_VERSION`].
pub(crate) fn migrate_to_current(value: &mut serde_json::Value) {
    let mut version = value
        .get("config_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or_else(first_version);
    for (from, step) in MIGRATIONS {
        if version == *from {
            step(value);
            version = from + 1;
        }
    }
}
