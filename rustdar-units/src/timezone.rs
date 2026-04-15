use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TimezonePreference {
    #[default]
    Local,
    Utc,
}

impl TimezonePreference {
    pub const ALL: &[TimezonePreference] = &[TimezonePreference::Local, TimezonePreference::Utc];

    pub fn label(self) -> &'static str {
        match self {
            TimezonePreference::Local => "Local Time",
            TimezonePreference::Utc => "UTC",
        }
    }

    /// Format a `NaiveDateTime` that is known to be in UTC, converting to the
    /// user's preferred timezone. Appends a timezone indicator.
    pub fn format_naive_utc(self, ts: NaiveDateTime, fmt: &str) -> String {
        match self {
            TimezonePreference::Utc => {
                format!("{} UTC", ts.format(fmt))
            }
            TimezonePreference::Local => {
                let utc_dt = chrono::TimeZone::from_utc_datetime(&chrono::Utc, &ts);
                let local_dt = utc_dt.with_timezone(&chrono::Local);
                // Use %Z for timezone abbreviation (e.g. "CDT", "EST")
                let extended_fmt = format!("{} %Z", fmt);
                local_dt.format(&extended_fmt).to_string()
            }
        }
    }

    /// Format an RFC 3339 / ISO 8601 timestamp string into a human-readable
    /// form in the user's preferred timezone.
    pub fn format_rfc3339(self, iso: &str) -> String {
        let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
            return iso.to_string();
        };
        match self {
            TimezonePreference::Utc => {
                let utc_dt = dt.with_timezone(&chrono::Utc);
                utc_dt.format("%b %d %Y %H:%M UTC").to_string()
            }
            TimezonePreference::Local => {
                let local_dt = dt.with_timezone(&chrono::Local);
                local_dt.format("%b %d %Y %H:%M %Z").to_string()
            }
        }
    }

    /// Convert a naive UTC datetime to the user's display timezone (as naive).
    /// Useful for populating time-picker fields.
    pub fn utc_to_display(self, ts: NaiveDateTime) -> NaiveDateTime {
        match self {
            TimezonePreference::Utc => ts,
            TimezonePreference::Local => {
                let utc_dt = chrono::TimeZone::from_utc_datetime(&chrono::Utc, &ts);
                utc_dt.with_timezone(&chrono::Local).naive_local()
            }
        }
    }
}

impl UnitLabel for TimezonePreference {
    fn display_label(self) -> &'static str {
        TimezonePreference::label(self)
    }
}
