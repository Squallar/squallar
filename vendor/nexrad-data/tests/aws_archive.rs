#![cfg(feature = "aws")]

use chrono::{Datelike, NaiveDate, Timelike};
use nexrad_data::aws::archive::{self, Identifier};

#[test]
fn test_identifier_new() {
    let name = "KDMX20220305_232324_V06".to_string();
    let identifier = Identifier::new(name.clone());

    assert_eq!(identifier.name(), name);
}

#[test]
fn test_identifier_site() {
    let identifier = Identifier::new("KDMX20220305_232324_V06".to_string());
    assert_eq!(identifier.site(), Some("KDMX"));

    let identifier = Identifier::new("KABR20220305_120000_V06".to_string());
    assert_eq!(identifier.site(), Some("KABR"));

    // Test with name too short
    let identifier = Identifier::new("KDM".to_string());
    assert_eq!(identifier.site(), None);
}

#[test]
fn test_identifier_date_time() {
    // Valid identifier
    let identifier = Identifier::new("KDMX20220305_232324_V06".to_string());
    let date_time = identifier.date_time();

    assert!(date_time.is_some());
    let date_time = date_time.unwrap();

    assert_eq!(date_time.year(), 2022);
    assert_eq!(date_time.month(), 3);
    assert_eq!(date_time.day(), 5);
    assert_eq!(date_time.hour(), 23);
    assert_eq!(date_time.minute(), 23);
    assert_eq!(date_time.second(), 24);
}

#[test]
fn test_identifier_date_time_various_dates() {
    // Test different valid dates
    let test_cases = vec![
        ("KDMX20220101_000000_V06", 2022, 1, 1, 0, 0, 0),
        ("KDMX20221231_235959_V06", 2022, 12, 31, 23, 59, 59),
        ("KDMX20200229_120000_V06", 2020, 2, 29, 12, 0, 0), // Leap year
        ("KDMX20230615_153045_V06", 2023, 6, 15, 15, 30, 45),
    ];

    for (name, year, month, day, hour, minute, second) in test_cases {
        let identifier = Identifier::new(name.to_string());
        let date_time = identifier.date_time();

        assert!(date_time.is_some(), "Failed to parse date from: {}", name);
        let date_time = date_time.unwrap();

        assert_eq!(date_time.year(), year);
        assert_eq!(date_time.month(), month);
        assert_eq!(date_time.day(), day);
        assert_eq!(date_time.hour(), hour);
        assert_eq!(date_time.minute(), minute);
        assert_eq!(date_time.second(), second);
    }
}

#[test]
fn test_identifier_date_time_invalid() {
    // Invalid date format
    let identifier = Identifier::new("KDMX2022030_232324_V06".to_string()); // Too short
    assert_eq!(identifier.date_time(), None);

    // Invalid date values
    let identifier = Identifier::new("KDMX20221301_120000_V06".to_string()); // Month 13
    assert_eq!(identifier.date_time(), None);

    let identifier = Identifier::new("KDMX20220230_120000_V06".to_string()); // Feb 30
    assert_eq!(identifier.date_time(), None);

    // Invalid time format
    let identifier = Identifier::new("KDMX20220305_2323_V06".to_string()); // Too short
    assert_eq!(identifier.date_time(), None);

    // Invalid time values
    let identifier = Identifier::new("KDMX20220305_256000_V06".to_string()); // Hour 25
    assert_eq!(identifier.date_time(), None);

    // Name too short overall
    let identifier = Identifier::new("KDMX".to_string());
    assert_eq!(identifier.date_time(), None);
}

#[test]
fn test_identifier_ordering() {
    let id1 = Identifier::new("KDMX20220305_120000_V06".to_string());
    let id2 = Identifier::new("KDMX20220305_130000_V06".to_string());
    let id3 = Identifier::new("KDMX20220306_120000_V06".to_string());

    assert!(id1 < id2);
    assert!(id2 < id3);
    assert!(id1 < id3);
}

#[test]
fn test_identifier_equality() {
    let id1 = Identifier::new("KDMX20220305_232324_V06".to_string());
    let id2 = Identifier::new("KDMX20220305_232324_V06".to_string());
    let id3 = Identifier::new("KDMX20220305_232325_V06".to_string());

    assert!(id1 == id2);
    assert!(id1 != id3);
}

#[test]
fn test_identifier_clone() {
    let original = Identifier::new("KDMX20220305_232324_V06".to_string());
    let cloned = original.clone();

    assert!(original == cloned);
    assert_eq!(original.name(), cloned.name());
    assert_eq!(original.site(), cloned.site());
    assert_eq!(original.date_time(), cloned.date_time());
}

// The eight `#[tokio::test]` integration tests that stood here are gone: they
// list and download from the live AWS archive bucket, and a workspace suite
// that reaches the network is a suite that fails when the network does. They
// were already `#[ignore = "requires AWS access"]` upstream, so removing them
// costs no coverage -- but their `#[tokio::test]` attribute is what pulled the
// `tokio` dev-dependency, which is why they had to go rather than stay
// ignored. The eleven `Identifier` tests around them are plain, synchronous
// and fixture-free, and run here. See VENDORED.md.

#[test]
fn test_identifier_mdm_file() {
    // MDM files have a different naming convention
    let identifier = Identifier::new("KDMX20220305_232324_V06_MDM".to_string());

    assert_eq!(identifier.site(), Some("KDMX"));
    // Date/time parsing should still work
    assert!(identifier.date_time().is_some());
}

#[test]
fn test_identifier_various_sites() {
    let sites = vec!["KDMX", "KABR", "KATX", "KFTG", "KGJX"];

    for site in sites {
        let name = format!("{site}20220305_120000_V06");
        let identifier = Identifier::new(name);

        assert_eq!(identifier.site(), Some(site));
        assert!(identifier.date_time().is_some());
    }
}

#[test]
fn test_identifier_hash() {
    use std::collections::HashSet;

    let id1 = Identifier::new("KDMX20220305_120000_V06".to_string());
    let id2 = Identifier::new("KDMX20220305_120000_V06".to_string());
    let id3 = Identifier::new("KDMX20220305_130000_V06".to_string());

    let mut set = HashSet::new();
    set.insert(id1.clone());
    set.insert(id2.clone());
    set.insert(id3.clone());

    // id1 and id2 are equal, so set should have 2 elements
    assert_eq!(set.len(), 2);
}
