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
pub(crate) const CONFIG_VERSION: u32 = 1;

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
/// Empty at M2 — the table exists so the *mechanism* is proven before the
/// first real migration (E6 adds v1 → v2) rather than alongside it.
const MIGRATIONS: &[Migration] = &[];

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
