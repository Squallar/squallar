use super::*;
use chrono::{FixedOffset, LocalResult, NaiveDate};

#[derive(Clone, Copy, Debug)]
struct UsCentral2024;

const CST: i32 = -6 * 3600;
const CDT: i32 = -5 * 3600;

fn at(month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2024, month, day)
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap()
}

fn fixed(seconds: i32) -> FixedOffset {
    FixedOffset::east_opt(seconds).unwrap()
}

impl chrono::TimeZone for UsCentral2024 {
    type Offset = FixedOffset;

    fn from_offset(_: &FixedOffset) -> Self {
        UsCentral2024
    }

    fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<FixedOffset> {
        self.offset_from_local_datetime(&local.and_hms_opt(0, 0, 0).unwrap())
    }

    fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<FixedOffset> {
        if *local < at(3, 10, 2, 0) {
            LocalResult::Single(fixed(CST))
        } else if *local < at(3, 10, 3, 0) {
            LocalResult::None
        } else if *local < at(11, 3, 1, 0) {
            LocalResult::Single(fixed(CDT))
        } else if *local < at(11, 3, 2, 0) {
            LocalResult::Ambiguous(fixed(CDT), fixed(CST))
        } else {
            LocalResult::Single(fixed(CST))
        }
    }

    fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
        self.offset_from_utc_datetime(&utc.and_hms_opt(0, 0, 0).unwrap())
    }

    fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
        if *utc >= at(3, 10, 8, 0) && *utc < at(11, 3, 7, 0) {
            fixed(CDT)
        } else {
            fixed(CST)
        }
    }
}

#[test]
fn an_ordinary_local_time_converts_by_the_offset_in_force() {
    assert_eq!(
        local_to_utc_in(&UsCentral2024, at(1, 15, 9, 30)),
        at(1, 15, 15, 30),
        "winter is UTC-6"
    );
    assert_eq!(
        local_to_utc_in(&UsCentral2024, at(7, 15, 9, 30)),
        at(7, 15, 14, 30),
        "summer is UTC-5"
    );
}

#[test]
fn a_time_the_clocks_skipped_lands_next_to_itself_not_at_now() {
    let asked = at(3, 10, 2, 30);
    assert!(
        matches!(
            chrono::TimeZone::offset_from_local_datetime(&UsCentral2024, &asked),
            LocalResult::None
        ),
        "precondition: the fixture really skips this hour"
    );

    assert_eq!(
        local_to_utc_in(&UsCentral2024, asked),
        at(3, 10, 8, 30),
        "the instant the clock jumped to — which is also where 02:30 would \
             have fallen had it happened",
    );
}

#[test]
fn the_whole_skipped_hour_resolves_within_an_hour_of_itself() {
    for minute in [0, 1, 30, 59] {
        let asked = at(3, 10, 2, minute);
        let resolved = local_to_utc_in(&UsCentral2024, asked);
        assert_eq!(
            resolved,
            at(3, 10, 8, minute),
            "02:{minute:02} resolved somewhere else entirely"
        );
    }
    assert_eq!(
        local_to_utc_in(&UsCentral2024, at(3, 10, 1, 59)),
        at(3, 10, 7, 59),
        "the minute before the gap is still CST"
    );
    assert_eq!(
        local_to_utc_in(&UsCentral2024, at(3, 10, 3, 0)),
        at(3, 10, 8, 0),
        "and the first minute after it is CDT"
    );
}

#[test]
fn an_hour_the_clocks_repeated_resolves_to_the_second_pass() {
    assert_eq!(
        local_to_utc_in(&UsCentral2024, at(11, 3, 1, 30)),
        at(11, 3, 7, 30),
        "the first 01:30 is 06:30 UTC, the second 07:30"
    );
}
