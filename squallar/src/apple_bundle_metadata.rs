//! The Apple bundle metadata App Store review rejects a build for, gated here
//! because no Xcode template ever ran over this project to fill it in.
//!
//! Test-only. It lives in this crate for the same reason
//! [`crate::network_security_config`] does: `squallar` is the package whose
//! staticlib and binary the iOS and macOS bundles wrap, and this walker reads
//! the packaging files that wrap them.
//!
//! # Why this gate exists
//!
//! Three of the four things Apple rejects a Rust app for are *absences*, and
//! an absence has no symptom until the upload is refused:
//!
//! * no `PrivacyInfo.xcprivacy` — ITMS-91053, "Missing API declaration";
//! * no `ITSAppUsesNonExemptEncryption` — not a rejection, but App Store
//!   Connect stops and asks the export question on every single upload;
//! * a purpose string that is missing rather than thin — on iOS the OS
//!   **kills the app** at the `requestWhenInUseAuthorization` call.
//!
//! Nothing in the tree produced any of them, because nothing in the tree is
//! Xcode. They were written by hand in September 2026 and this is what keeps
//! them.
//!
//! # The assertion that matters
//!
//! Not "the manifest exists" — that one goes green forever the moment the file
//! lands. The defect this gate is built around is **the manifest drifting
//! behind the code**: a required-reason API gains a caller, and the manifest,
//! which no compiler checks, still says what was true last year.
//!
//! So [`tests::every_reached_required_reason_api_is_declared`] scrapes the
//! workspace's own Rust for the APIs Apple requires a reason for, and asserts
//! the manifest declares every category the source reaches. Add a
//! `statvfs` call and the build goes red naming the category to add. That is
//! the coupling; the structural checks below it are the floor.
//!
//! # What this does not cover
//!
//! **The dependency graph.** The scrape reads this workspace's first-party
//! source, and two of the three declared categories are reached only from
//! outside it: `SystemBootTime` through wgpu-hal's Metal `PresentationTimer`,
//! which declares `mach_absolute_time` itself, and `UserDefaults` through
//! `dark-light`'s theme probe. Neither spelling appears anywhere in this
//! workspace. That is why those two are asserted against the manifest — one
//! unconditionally, one coupled to the dependency in `Cargo.toml` — rather
//! than against a source marker. A *new* dependency that reads disk space or
//! file timestamps would not be caught here. Re-run the dependency audit when
//! the graph changes; this gate is not a substitute for it.
//!
//! **Whether Apple accepts the reason codes.** The codes are checked for shape
//! and for belonging to their category, against Apple's published list. That
//! the reviewer agrees a given reason describes this app's use is not
//! knowable without submitting.
//!
//! **Test-only occurrences inside a non-test file.** The scrape skips `tests/`
//! directories and `*tests.rs` files, which is where this workspace puts its
//! tests. A marker inside a `#[cfg(test)] mod` in an ordinary file would be
//! counted as reached and would over-fire. That is the safe direction — it
//! asks for a declaration rather than hiding a missing one — but it is a
//! false positive, so it is written down here rather than discovered.

/// The bundle metadata files, by `include_str!` so **moving or renaming one
/// fails the build** rather than one assertion inside it.
#[cfg(test)]
const IOS_INFO_PLIST: &str = include_str!("../../packaging/ios/Info.plist");
#[cfg(test)]
const MACOS_INFO_PLIST: &str = include_str!("../../packaging/macos/Info.plist");
#[cfg(test)]
const PRIVACY_MANIFEST: &str = include_str!("../../packaging/PrivacyInfo.xcprivacy");
#[cfg(test)]
const IOS_MAKEFILE: &str = include_str!("../../packaging/ios/Makefile");
#[cfg(test)]
const MACOS_MAKEFILE: &str = include_str!("../../packaging/macos/Makefile");

/// The workspace root, for the source scrape.
#[cfg(test)]
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

/// Strip XML comments, so a key *named in prose* is not read as a declaration.
///
/// Not optional. Both plists explain themselves at length and name the very
/// keys this module looks for — `packaging/macos/Info.plist` discusses
/// `NSLocationUsageDescription` in the comment above it, and the privacy
/// manifest's header names every category it deliberately does **not**
/// declare. Reading those as declarations would invert the gate.
#[cfg(test)]
fn without_xml_comments(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start + 4..].find("-->") {
            Some(end) => rest = &rest[start + 4 + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The value of `<key>name</key>` as raw XML: the next element after the key.
///
/// Returns the element verbatim (`<string>x</string>`, `<true/>`, `<false/>`,
/// a whole `<array>…</array>`) so a caller can tell "absent" from "present and
/// empty" from "present and false" — three states this gate has to keep apart.
///
/// **Nesting-aware, and that is not a refinement.** `NSPrivacyAccessedAPITypes`
/// is an array of dicts each containing its own `<array>` of reason codes.
/// Stopping at the first `</array>` truncates the value to the first entry, so
/// a manifest declaring three categories reads as one — which is a gate that
/// silently stops looking at exactly the thing it was written to check.
#[cfg(test)]
fn plist_value<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("<key>{name}</key>");
    let after = &xml[xml.find(&key)? + key.len()..];
    let start = after.find('<')?;
    let after = &after[start..];
    let end = after.find('>')?;
    // `<true/>`, `<false/>` and an empty `<array/>` are self-closing.
    if after[..=end].ends_with("/>") {
        return Some(&after[..=end]);
    }
    let tag = &after[1..end];
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));

    let mut depth = 1usize;
    let mut i = open.len();
    loop {
        let next_open = after[i..].find(&open).map(|p| i + p);
        let next_close = after[i..].find(&close).map(|p| i + p)?;
        match next_open {
            Some(o) if o < next_close => {
                depth += 1;
                i = o + open.len();
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return Some(&after[..next_close + close.len()]);
                }
                i = next_close + close.len();
            }
        }
    }
}

/// The text inside a `<string>` value, or `None` for any other element.
#[cfg(test)]
fn plist_string<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let value = plist_value(xml, name)?;
    value
        .strip_prefix("<string>")
        .and_then(|v| v.strip_suffix("</string>"))
}

/// Apple's five required-reason API categories, with the reason codes each one
/// accepts, transcribed from Apple's published list.
///
/// The pairing is the point: a code valid for one category is rejected for
/// another (`ITMS-91055`, "Invalid API reason declaration"), and the two the
/// app declares are easy to transpose because both are four characters, a dot
/// and a digit.
#[cfg(test)]
const REQUIRED_REASON_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "NSPrivacyAccessedAPICategoryFileTimestamp",
        &["DDA9.1", "C617.1", "3B52.1", "0A2A.1"],
    ),
    (
        "NSPrivacyAccessedAPICategorySystemBootTime",
        &["35F9.1", "8FFB.1", "3D61.1"],
    ),
    (
        "NSPrivacyAccessedAPICategoryDiskSpace",
        &["85F4.1", "E174.1", "7D9E.1", "B728.1"],
    ),
    (
        "NSPrivacyAccessedAPICategoryActiveKeyboards",
        &["3EC4.1", "54BD.1"],
    ),
    (
        "NSPrivacyAccessedAPICategoryUserDefaults",
        &["CA92.1", "1C8F.1", "C56D.1", "AC6B.1"],
    ),
];

/// Source spellings that reach a required-reason API, per category.
///
/// **`SystemBootTime` is deliberately absent**: nothing in this workspace
/// spells it. It is reached through `std::time::Instant`, which on Apple
/// targets std resolves to `clock_gettime(CLOCK_UPTIME_RAW)` — a value std's
/// own source documents as "identical to the result of `mach_absolute_time()`"
/// — and that is on Apple's boot-time list. A marker scrape would have to
/// match `Instant`, which appears in 238 places and could never be removed, so
/// the category is asserted unconditionally instead. See
/// [`tests::the_boot_time_category_is_always_declared`].
///
/// **`free_space` is deliberately not a disk-space marker.** The function of
/// that name in `squallar-egui/src/ui_download_area.rs` returns arithmetic
/// over a browser `navigator.storage.estimate()` figure fetched from the
/// service worker, not a syscall. Listing it would red-gate a wasm-only path
/// that touches no Apple API — the over-firing direction, which blocks work.
#[cfg(test)]
const REACH_MARKERS: &[(&str, &[&str])] = &[
    (
        "NSPrivacyAccessedAPICategoryFileTimestamp",
        &[".modified()", ".created()", ".accessed()", "st_mtime"],
    ),
    (
        "NSPrivacyAccessedAPICategoryDiskSpace",
        &["statvfs", "statfs", "volumeAvailableCapacity"],
    ),
    (
        "NSPrivacyAccessedAPICategoryUserDefaults",
        &["NSUserDefaults", "CFPreferences"],
    ),
    (
        "NSPrivacyAccessedAPICategoryActiveKeyboards",
        &["UITextInputMode", "activeInputModes"],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    /// The privacy manifest's `NSPrivacyAccessedAPITypes`, as category → codes.
    fn declared_api_types() -> BTreeMap<String, Vec<String>> {
        let xml = without_xml_comments(PRIVACY_MANIFEST);
        let array = plist_value(&xml, "NSPrivacyAccessedAPITypes")
            .expect("PrivacyInfo.xcprivacy has no NSPrivacyAccessedAPITypes key");

        let mut out = BTreeMap::new();
        for chunk in array.split("<key>NSPrivacyAccessedAPIType</key>").skip(1) {
            let category = chunk
                .split_once("<string>")
                .and_then(|(_, r)| r.split_once("</string>"))
                .map(|(v, _)| v.trim().to_string())
                .expect("an NSPrivacyAccessedAPIType with no <string> value");
            let reasons_block = chunk
                .split_once("<key>NSPrivacyAccessedAPITypeReasons</key>")
                .map(|(_, r)| r)
                .unwrap_or("");
            let reasons_block = reasons_block
                .split_once("</array>")
                .map(|(v, _)| v)
                .unwrap_or(reasons_block);
            let mut reasons = Vec::new();
            let mut rest = reasons_block;
            while let Some((_, after)) = rest.split_once("<string>") {
                let Some((value, tail)) = after.split_once("</string>") else {
                    break;
                };
                reasons.push(value.trim().to_string());
                rest = tail;
            }
            out.insert(category, reasons);
        }
        out
    }

    /// Every first-party Rust file the scrape reads.
    ///
    /// Skips `target/` and `vendor/` (not ours), and `tests/` directories and
    /// `*tests.rs` files (this workspace's convention for test-only code), and
    /// this module, which names every marker it looks for.
    fn first_party_rust_files() -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if path.is_dir() {
                    if matches!(
                        name.as_ref(),
                        "target" | "vendor" | ".git" | "tests" | "node_modules"
                    ) {
                        continue;
                    }
                    walk(&path, out);
                } else if name.ends_with(".rs")
                    && !name.ends_with("tests.rs")
                    && name != "apple_bundle_metadata.rs"
                {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(Path::new(ROOT), &mut out);
        assert!(
            out.len() > 300,
            "the source scrape found only {} files, which means it is not \
             walking the workspace and every assertion resting on it is \
             vacuous (419 matched at the time of writing, less this module)",
            out.len()
        );
        out
    }

    /// Strip `//` line comments, so a marker named in prose is not a reach.
    fn without_line_comments(src: &str) -> String {
        src.lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---------------------------------------------------------------------
    // The coupling: what the code reaches, the manifest declares
    // ---------------------------------------------------------------------

    /// **The assertion this module exists for.** A required-reason API gains a
    /// caller in this workspace; the manifest must gain the category.
    #[test]
    fn every_reached_required_reason_api_is_declared() {
        let declared: BTreeSet<String> = declared_api_types().keys().cloned().collect();

        let mut missing: Vec<String> = Vec::new();
        for (category, markers) in REACH_MARKERS {
            if declared.contains(*category) {
                continue;
            }
            for path in first_party_rust_files() {
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let src = without_line_comments(&src);
                if let Some(marker) = markers.iter().find(|m| src.contains(**m)) {
                    missing.push(format!(
                        "{category} — reached by {marker:?} at {}",
                        path.strip_prefix(ROOT).unwrap_or(&path).display()
                    ));
                    break;
                }
            }
        }

        assert!(
            missing.is_empty(),
            "these required-reason API categories are reached by this \
             workspace's own code but are not declared in \
             packaging/PrivacyInfo.xcprivacy, which is App Store rejection \
             ITMS-91053:\n  {}\n\nAdd the category with a reason code from \
             Apple's list for it, or gate the call out of the Apple targets.",
            missing.join("\n  ")
        );
    }

    /// A category reached only through a **dependency**, which the source
    /// scrape above cannot see.
    ///
    /// `dark-light` reads `AppleInterfaceStyle` out of `NSUserDefaults` on
    /// macOS (`platforms/macos.rs`, `standardUserDefaults` →
    /// `persistentDomainForName` → `objectForKey`), and
    /// `PlatformBridge::detect_dark_theme` calls it at startup. Those selectors
    /// land in the macOS binary, which is what Apple's scanner reads — so the
    /// category is reached even though `NSUserDefaults` is spelled nowhere in
    /// this workspace. That is exactly how the first-party scrape misses it,
    /// and why this assertion is written against the manifest instead.
    ///
    /// Coupled to the dependency, so it moves in both directions: drop
    /// `dark-light` (or replace it with winit's `ThemeChanged`, which reaches
    /// no required-reason API) and this test says to retire the declaration
    /// rather than leaving a claim behind that the binary no longer supports.
    #[test]
    fn the_user_defaults_category_tracks_the_dependency_that_reaches_it() {
        let manifest = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        )
        .expect("cannot read squallar/Cargo.toml");
        let links_dark_light = manifest
            .lines()
            .any(|line| line.trim_start().starts_with("dark-light"));
        let declared =
            declared_api_types().contains_key("NSPrivacyAccessedAPICategoryUserDefaults");

        assert_eq!(
            links_dark_light,
            declared,
            "squallar/Cargo.toml {} `dark-light` and PrivacyInfo.xcprivacy {} \
             NSPrivacyAccessedAPICategoryUserDefaults. It reads \
             AppleInterfaceStyle through NSUserDefaults on macOS, so the two \
             have to agree: declaring without the dependency is a claim the \
             binary does not support, and the dependency without the \
             declaration is App Store rejection ITMS-91053.",
            if links_dark_light {
                "links"
            } else {
                "does not link"
            },
            if declared { "declares" } else { "omits" },
        );
    }

    /// `SystemBootTime` can never stop being reached while this is a Rust app
    /// that measures anything, so it is asserted rather than scraped.
    #[test]
    fn the_boot_time_category_is_always_declared() {
        assert!(
            declared_api_types().contains_key("NSPrivacyAccessedAPICategorySystemBootTime"),
            "NSPrivacyAccessedAPICategorySystemBootTime is not declared. \
             `std::time::Instant` resolves to `clock_gettime(CLOCK_UPTIME_RAW)` \
             on Apple targets, which std documents as identical to \
             `mach_absolute_time()` — a boot-time API. Every timeout, budget \
             and frame measurement in this workspace reaches it."
        );
    }

    // ---------------------------------------------------------------------
    // The manifest is well formed and its codes are real
    // ---------------------------------------------------------------------

    #[test]
    fn the_privacy_manifest_declares_the_four_top_level_keys() {
        let xml = without_xml_comments(PRIVACY_MANIFEST);
        for key in [
            "NSPrivacyTracking",
            "NSPrivacyTrackingDomains",
            "NSPrivacyCollectedDataTypes",
            "NSPrivacyAccessedAPITypes",
        ] {
            assert!(
                plist_value(&xml, key).is_some(),
                "PrivacyInfo.xcprivacy is missing the {key} key; Apple expects \
                 all four, and an app manifest that omits one is incomplete \
                 rather than defaulted"
            );
        }
    }

    #[test]
    fn every_declared_category_is_real_and_carries_a_reason_valid_for_it() {
        let declared = declared_api_types();
        assert!(
            !declared.is_empty(),
            "PrivacyInfo.xcprivacy declares no required-reason API at all, \
             which for a Rust app is never right: see the boot-time test"
        );

        for (category, reasons) in &declared {
            let known = REQUIRED_REASON_CATEGORIES
                .iter()
                .find(|(name, _)| name == category)
                .unwrap_or_else(|| {
                    panic!(
                        "{category} is not one of Apple's five required-reason \
                         API categories; a misspelling here is rejection \
                         ITMS-91056 (invalid privacy manifest)"
                    )
                });

            assert!(
                !reasons.is_empty(),
                "{category} is declared with an empty reason array; Apple \
                 requires at least one reason code per declared category"
            );

            for reason in reasons {
                assert!(
                    known.1.contains(&reason.as_str()),
                    "{reason} is not a reason code Apple accepts for \
                     {category}. Valid codes for it are {:?}. A code that is \
                     valid for a different category is rejection ITMS-91055.",
                    known.1
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // The manifest reaches the bundle, at the place each OS looks
    // ---------------------------------------------------------------------

    /// A manifest in the repository that never reaches the `.app` is the same
    /// rejection as no manifest — and the two bundles do **not** put it in the
    /// same place, which is the part that gets assumed rather than read.
    #[test]
    fn both_makefiles_copy_the_manifest_where_that_os_looks_for_it() {
        assert!(
            IOS_MAKEFILE
                .contains(r#"cp packaging/PrivacyInfo.xcprivacy "$$APP/PrivacyInfo.xcprivacy""#),
            "packaging/ios/Makefile does not copy PrivacyInfo.xcprivacy to the \
             bundle ROOT, which is where iOS looks for it"
        );
        assert!(
            MACOS_MAKEFILE.contains(
                r#"cp packaging/PrivacyInfo.xcprivacy "$$APP/Contents/Resources/PrivacyInfo.xcprivacy""#
            ),
            "packaging/macos/Makefile does not copy PrivacyInfo.xcprivacy to \
             Contents/Resources, which is where macOS looks for it — the \
             bundle root is the iOS answer and is wrong here"
        );
    }

    // ---------------------------------------------------------------------
    // Info.plist: purpose strings and the export declaration
    // ---------------------------------------------------------------------

    /// A missing purpose string is not a review note: on iOS the OS terminates
    /// the app at `requestWhenInUseAuthorization`, and on macOS the dialog
    /// silently never appears.
    #[test]
    fn every_permission_the_app_requests_has_a_non_empty_purpose_string() {
        let ios = without_xml_comments(IOS_INFO_PLIST);
        let macos = without_xml_comments(MACOS_INFO_PLIST);

        // iOS spells it `WhenInUse`; macOS reads `NSLocationUsageDescription`
        // and is documented to fail immediately without it, so it carries both.
        for (label, xml, keys) in [
            (
                "packaging/ios/Info.plist",
                &ios,
                &["NSLocationWhenInUseUsageDescription"][..],
            ),
            (
                "packaging/macos/Info.plist",
                &macos,
                &[
                    "NSLocationUsageDescription",
                    "NSLocationWhenInUseUsageDescription",
                ][..],
            ),
        ] {
            for key in keys {
                let text = plist_string(xml, key).unwrap_or_else(|| {
                    panic!(
                        "{label} has no {key}; requesting location without it terminates the app"
                    )
                });
                assert!(
                    text.trim().len() >= 30,
                    "{label}'s {key} is {:?}, which is too thin to be a purpose \
                     string. A reviewer wants the specific, user-facing use — \
                     what the app does with location and what the person gets \
                     for it — not a category name.",
                    text
                );
                assert!(
                    !text.contains("$("),
                    "{label}'s {key} carries an unexpanded build setting, so it \
                     would ship to a user as a literal"
                );
            }
        }
    }

    /// Both bundles wrap the same binary, so the export answer cannot differ.
    #[test]
    fn both_bundles_declare_the_encryption_exemption() {
        for (label, plist) in [
            ("packaging/ios/Info.plist", IOS_INFO_PLIST),
            ("packaging/macos/Info.plist", MACOS_INFO_PLIST),
        ] {
            let xml = without_xml_comments(plist);
            let value = plist_value(&xml, "ITSAppUsesNonExemptEncryption").unwrap_or_else(|| {
                panic!(
                    "{label} does not declare ITSAppUsesNonExemptEncryption, so \
                     App Store Connect stops and asks the export-compliance \
                     question on every upload"
                )
            });
            assert_eq!(
                value, "<false/>",
                "{label} declares ITSAppUsesNonExemptEncryption as {value}, not \
                 <false/>. The exemption rests on this app using nothing but \
                 standard TLS; if that stopped being true, the fix is an \
                 export-compliance ruling, not this assertion."
            );
        }
    }

    // ---------------------------------------------------------------------
    // The parsers themselves, so a green above is not a parser that reads
    // nothing
    // ---------------------------------------------------------------------

    #[test]
    fn the_comment_stripper_removes_keys_named_in_prose() {
        let xml = "<dict><!-- mentions <key>NSPrivacyTracking</key> in prose -->\
                   <key>Real</key><string>v</string></dict>";
        let stripped = without_xml_comments(xml);
        assert!(
            plist_value(&stripped, "NSPrivacyTracking").is_none(),
            "a key named inside a comment must not read as a declaration"
        );
        assert_eq!(plist_string(&stripped, "Real"), Some("v"));
    }

    #[test]
    fn the_value_reader_tells_absent_from_empty_from_false() {
        let xml = "<key>S</key><string>x</string><key>E</key><string></string>\
                   <key>F</key><false/><key>T</key><true/>";
        assert_eq!(plist_string(xml, "S"), Some("x"));
        assert_eq!(plist_string(xml, "E"), Some(""));
        assert_eq!(plist_value(xml, "F"), Some("<false/>"));
        assert_eq!(plist_value(xml, "T"), Some("<true/>"));
        assert_eq!(plist_value(xml, "Absent"), None);
    }

    /// The parser reads *shape*, not a policy value.
    ///
    /// It asserts the manifest was actually decomposed — more than one
    /// category, each with a name and at least one reason — because a parser
    /// that silently returns one entry (or none) makes every assertion built
    /// on it vacuous, which is a bug this module has already had once: the
    /// first version stopped at the first nested `</array>`.
    ///
    /// It deliberately does **not** pin a particular category to a particular
    /// code. Which reason code is right is a judgement that can legitimately
    /// change — C617.1 to DDA9.1, say — and that judgement is already policed
    /// by `every_declared_category_is_real_and_carries_a_reason_valid_for_it`.
    /// Asserting it here too would red-gate a correct edit on a test whose
    /// subject is the parser.
    #[test]
    fn the_manifest_parser_decomposes_the_real_manifest() {
        let declared = declared_api_types();
        assert!(
            declared.len() >= 2,
            "the parser read {} categories out of the real manifest. It should \
             see every entry; one means it stopped at the first nested \
             </array>, and every assertion resting on it is then vacuous: \
             {declared:?}",
            declared.len()
        );
        for (category, reasons) in &declared {
            assert!(
                category.starts_with("NSPrivacyAccessedAPICategory"),
                "parsed a category name that is not one: {category:?}"
            );
            assert!(
                !reasons.is_empty(),
                "{category} parsed with no reason codes, so the reason arrays \
                 are not being read at all"
            );
        }
    }

    /// Every XML file this gate reads must be well-formed *as XML*, checked for
    /// the one rule the rest of this module is structurally blind to: a comment
    /// body may not contain `--`.
    ///
    /// `without_xml_comments` deletes every comment before any other assertion
    /// runs, so all of them pass exactly as happily on a file no XML parser will
    /// accept. That is not a hypothetical. The first draft of
    /// `PrivacyInfo.xcprivacy` explained itself by quoting a `cargo tree`
    /// invocation with its target flag spelled out; the double hyphen in that
    /// flag made the whole manifest unparseable, while every content assertion
    /// here stayed green.
    ///
    /// A manifest Apple cannot parse is worse than an absent one: the content is
    /// right, this gate agrees, and the submission is refused anyway — with the
    /// one file that proves the app is truthful the file that broke it.
    #[test]
    fn the_xml_files_have_parseable_comments() {
        for (path, xml) in [
            ("packaging/ios/Info.plist", IOS_INFO_PLIST),
            ("packaging/macos/Info.plist", MACOS_INFO_PLIST),
            ("packaging/PrivacyInfo.xcprivacy", PRIVACY_MANIFEST),
        ] {
            let mut rest = xml;
            let mut consumed = 0usize;
            while let Some(start) = rest.find("<!--") {
                let body_start = start + 4;
                let Some(end) = rest[body_start..].find("-->") else {
                    panic!(
                        "{path}: unterminated XML comment at byte {}. Every \
                         assertion in this file reads the stripper's output, and \
                         the stripper drops everything after an unterminated \
                         comment — so the gate would go green on a truncated file",
                        consumed + start
                    );
                };
                let body = &rest[body_start..body_start + end];
                // Point at the `--` itself, not at the comment's opening. These
                // files explain themselves in single comments thousands of bytes
                // long, so a message quoting the first 200 characters of the
                // enclosing comment names a real failure with text that has
                // nothing to do with it.
                if let Some(at) = body.find("--") {
                    let from = body[..at]
                        .char_indices()
                        .rev()
                        .nth(70)
                        .map_or(0, |(i, _)| i);
                    let to = body[at..]
                        .char_indices()
                        .nth(70)
                        .map_or(body.len(), |(i, _)| at + i);
                    panic!(
                        "{path}: an XML comment contains `--` at byte {}, which is \
                         illegal in XML, so no parser will read this file however \
                         right its content is: {:?}",
                        consumed + body_start + at,
                        &body[from..to]
                    );
                }
                let step = body_start + end + 3;
                consumed += step;
                rest = &rest[step..];
            }
        }
    }
}
