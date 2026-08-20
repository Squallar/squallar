//! Stepwise config-format migrations, applied to the raw JSON tree before
//! `UiConfig` ever sees it.

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
/// place to speak the next one, in order, so a v1 file walks every rung.
const MIGRATIONS: &[Migration] = &[(1, split_gps_config)];

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
