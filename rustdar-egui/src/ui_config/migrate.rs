//! Stepwise config-format migrations, applied to the raw JSON tree before
//! `UiConfig` ever sees it.
//!
//! The version names the **format**, not the build: it moves only when a key
//! changes meaning in a way field-level tolerance cannot absorb — a renamed
//! field, a restructured container. Additive changes never bump it, because
//! `#[serde(default)]` already reads their absence correctly, and the whole
//! point of the armor around this module is that *unknown* content passes
//! through untouched rather than needing a version to explain it.

/// The config format this build writes.
pub(crate) const CONFIG_VERSION: u32 = 2;

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
/// place to speak the next one. Applied in order by [`migrate_to_current`],
/// so a v1 file walks every rung to reach the current format.
///
/// The table (empty from M2 until WO-RL-1) exists so the *mechanism* was
/// proven before the first real migration rather than alongside it.
const MIGRATIONS: &[Migration] = &[(1, split_gps_config)];

/// v1 → v2: the `gps_config` container split with its crate (WO-RL-1) — the
/// serial half (`port_path`, `baud_rate`) keeps the container under the new
/// name `serial_config`, and `heading_source` becomes its own top-level key,
/// because heading choice matters on every platform and the serial port on
/// almost none.
///
/// A pure `Value` edit, deliberately: members this build cannot name ride the
/// rename verbatim (parsing into the new types here would silently shed them
/// — the tolerant loader does the parsing *after* the walk, at field
/// granularity). A `gps_config` that is absent or not an object is left
/// untouched: the tolerant load defaults both new fields, and whatever the
/// old key held rides on as unknown content.
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

/// Walk `value` up from whatever version it speaks to [`CONFIG_VERSION`].
///
/// A version **greater** than this build's is deliberately not an error and
/// not migrated: the tolerant load reads what it can name, the unknown-field
/// and unknown-kind preservation carries the rest verbatim, and the next
/// save writes this build's version — which is the honest description of
/// the file it just wrote. That is what makes running an older build against
/// a newer file safe (the downgrade case), and it is why refusing here would
/// be strictly worse than proceeding.
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
