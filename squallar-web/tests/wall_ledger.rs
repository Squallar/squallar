//! **The web memory wall ledger, held to the evidence behind each row.**
//!
//! `.github/browser-rig/wall-ceilings.tsv` records, per (device class, browser,
//! scene), the linear-memory ceilings the app ran under on a wall leg and the
//! wall that device's calibrate leg measured. `run_wall_arm.sh` prints the
//! rows; a person appends them. Columns:
//!
//! * `ceiling_page_mib` / `ceiling_worker_mib` — the `heap max a/b` figures off
//!   the app's own `budget state:` line on that leg: the ceilings `heap.js`
//!   handed the page and the rasterization worker on that device.
//! * `wall_mib` — MiB of incompressible, touched linear memory the calibrate
//!   leg on the same device and browser survived, to death, refusal or the
//!   4 GiB policy cap (the evidence file says which).
//! * `leg_id` — names `wall-evidence/<leg_id>.json`, the committed record of
//!   the leg: its row, its readings and its provenance.
//!
//! **Ceilings may only fall.** Per key, in file order, neither ceiling may rise:
//! a lower wall found later forces a lower ceiling, and a raise is a
//! measurement someone makes on a device and then writes down deliberately --
//! the rule `linear_memory_ceiling.rs` states for the link flag itself.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const LEDGER: &str = include_str!("../../.github/browser-rig/wall-ceilings.tsv");

const HEADER: &str = "device_class\tbrowser\tscene\tceiling_page_mib\tceiling_worker_mib\twall_mib\tleg_id\tcommit\tdate";

fn evidence_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github/browser-rig/wall-evidence")
}

/// The evidence record for `leg_id`, or `None` when there is no readable one.
fn evidence_on_disk(leg_id: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(evidence_dir().join(format!("{leg_id}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Every reason `text` is not a ledger this file accepts. `evidence` answers
/// for a `leg_id`, so a control can hand in a doctored set without a disk.
fn ledger_defects(text: &str, evidence: &dyn Fn(&str) -> Option<serde_json::Value>) -> Vec<String> {
    let mut defects = Vec::new();
    let mut lines = text.lines();
    match lines.next() {
        Some(h) if h == HEADER => {}
        other => defects.push(format!("the header is {other:?}, not {HEADER:?}")),
    }
    let mut last: BTreeMap<(String, String, String), (u64, u64, usize)> = BTreeMap::new();
    let mut legs = BTreeSet::new();
    let mut rows = 0;
    for (i, line) in lines.enumerate() {
        let n = i + 2;
        if line.trim().is_empty() {
            defects.push(format!("line {n} is blank"));
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 9 {
            defects.push(format!("line {n} has {} fields, not 9: {line:?}", f.len()));
            continue;
        }
        rows += 1;
        let (class, browser, scene, page, worker, wall, leg, commit, date) =
            (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], f[8]);
        for (name, v) in [
            ("device_class", class),
            ("browser", browser),
            ("scene", scene),
            ("leg_id", leg),
        ] {
            if !is_word(v) {
                defects.push(format!("line {n}: {name} {v:?} is not a plain word"));
            }
        }
        let mib = |v: &str| v.parse::<u64>().ok().filter(|m| *m > 0);
        let (Some(page), Some(worker), Some(wall)) = (mib(page), mib(worker), mib(wall)) else {
            defects.push(format!(
                "line {n}: a MiB column is not a positive integer: {line:?}"
            ));
            continue;
        };
        if !(7..=40).contains(&commit.len()) || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
            defects.push(format!("line {n}: commit {commit:?} is not a hex sha"));
        }
        if !is_date(date) {
            defects.push(format!("line {n}: date {date:?} is not YYYY-MM-DD"));
        }
        if !legs.insert(leg.to_string()) {
            defects.push(format!("line {n}: leg_id {leg} appears twice"));
        }
        let key = (class.to_string(), browser.to_string(), scene.to_string());
        if let Some((prev_page, prev_worker, prev_line)) = last.get(&key)
            && (page > *prev_page || worker > *prev_worker)
        {
            defects.push(format!(
                "line {n}: {class}/{browser}/{scene} ceilings rose to {page}/{worker} MiB \
                 from {prev_page}/{prev_worker} on line {prev_line}; ceilings may only fall"
            ));
        }
        last.insert(key, (page, worker, n));
        match evidence(leg) {
            None => defects.push(format!(
                "line {n}: leg_id {leg} has no readable wall-evidence/{leg}.json"
            )),
            Some(ev) => {
                let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).map(str::to_string);
                let u = |k: &str| ev.get(k).and_then(serde_json::Value::as_u64);
                for (k, want) in [
                    ("leg_id", leg),
                    ("device_class", class),
                    ("browser", browser),
                    ("scene", scene),
                    ("commit", commit),
                    ("date", date),
                ] {
                    if s(k).as_deref() != Some(want) {
                        defects.push(format!(
                            "line {n}: evidence for {leg} says {k}={:?}, the row says {want:?}",
                            s(k)
                        ));
                    }
                }
                for (k, want) in [
                    ("ceiling_page_mib", page),
                    ("ceiling_worker_mib", worker),
                    ("wall_mib", wall),
                ] {
                    if u(k) != Some(want) {
                        defects.push(format!(
                            "line {n}: evidence for {leg} says {k}={:?}, the row says {want}",
                            u(k)
                        ));
                    }
                }
            }
        }
    }
    if rows == 0 {
        defects.push("the ledger has no rows, so every rule above held over nothing".to_string());
    }
    defects
}

/// **The ledger as committed: header exact, ceilings never rising, every row
/// backed by the evidence it names.**
#[test]
fn the_wall_ledger_holds_its_rules() {
    assert_eq!(
        ledger_defects(LEDGER, &evidence_on_disk),
        Vec::<String>::new()
    );
}

/// **A doctored ledger goes red, once per rule.** Without this the test above
/// could be passing on a checker that accepts anything.
#[test]
fn a_doctored_ledger_is_named() {
    let first = LEDGER
        .lines()
        .nth(1)
        .expect("the ledger has at least one row");
    let f: Vec<&str> = first.split('\t').collect();

    // A later row for the same key with a HIGHER page ceiling, backed by an
    // evidence record that agrees with it, so the only thing wrong is the rise.
    let risen_page = f[3].parse::<u64>().expect("a MiB column") + 512;
    let mut risen = f.clone();
    let risen_page_text = risen_page.to_string();
    risen[3] = &risen_page_text;
    risen[6] = "doctored-rise";
    let doctored = format!("{LEDGER}{}\n", risen.join("\t"));
    let with_rise = |leg: &str| -> Option<serde_json::Value> {
        if leg == "doctored-rise" {
            let mut ev = evidence_on_disk(f[6])?;
            ev["leg_id"] = "doctored-rise".into();
            ev["ceiling_page_mib"] = risen_page.into();
            Some(ev)
        } else {
            evidence_on_disk(leg)
        }
    };
    let red = ledger_defects(&doctored, &with_rise);
    assert!(
        red.len() == 1 && red[0].contains("ceilings may only fall"),
        "a risen ceiling was not the one defect named: {red:?}"
    );

    // A row naming a leg with no evidence.
    let mut orphan = f.clone();
    orphan[6] = "no-such-leg";
    let doctored = format!("{LEDGER}{}\n", orphan.join("\t"));
    let red = ledger_defects(&doctored, &evidence_on_disk);
    assert!(
        red.iter()
            .any(|d| d.contains("no readable wall-evidence/no-such-leg.json")),
        "a row with no evidence passed: {red:?}"
    );

    // A row whose ceiling disagrees with its own evidence.
    let mut lying = f.clone();
    lying[4] = "1";
    let body: String = LEDGER
        .lines()
        .skip(1)
        .map(|l| {
            if l == first {
                lying.join("\t")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let doctored = format!("{HEADER}\n{body}\n");
    let red = ledger_defects(&doctored, &evidence_on_disk);
    assert!(
        red.iter().any(|d| d.contains("ceiling_worker_mib")),
        "a row disagreeing with its evidence passed: {red:?}"
    );

    // The header respelled.
    let doctored = LEDGER.replacen("wall_mib", "wall_MiB", 1);
    let red = ledger_defects(&doctored, &evidence_on_disk);
    assert!(
        red.iter().any(|d| d.contains("the header is")),
        "a respelled header passed: {red:?}"
    );

    // And no rows at all.
    let red = ledger_defects(&format!("{HEADER}\n"), &evidence_on_disk);
    assert!(red.iter().any(|d| d.contains("no rows")), "{red:?}");
}
