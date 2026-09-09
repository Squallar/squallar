//! **No value in the inspector is built by a production frame only for the probe
//! to read.**
//!
//! The tell is a `#[cfg(not(test))] let _ = x;` — the spelling that admits `x` is
//! read by nothing on a production build. Three of those in this file bind
//! widgets that exist whatever the build (`scroll`, `area`, `row`), and they cost
//! nothing to leave alone. A fourth bound the crumb's tail, and *that* one was
//! `format!`ed and `to_owned`ed on every frame the inspector stood open, for a
//! `String` only `InspectorProbe` ever looked at.
//!
//! So the scan is narrow on purpose: it flags a probe-only value **whose binding
//! allocates**, and lets the widget handles through.

/// The file this ratchet governs, relative to the crate root.
const SWEPT: &str = "src/ui_inspector.rs";

/// The spellings that make a binding cost a heap allocation at its own site.
const ALLOCATES: [&str; 4] = ["format!", ".to_owned()", ".to_string()", "String::"];

/// One `let` statement, from its `let` to the `;` that ends it.
///
/// Brace-aware on purpose. A tail bound by a `match` runs over arms full of their
/// own `;`, and stopping at the first of those is how an earlier spelling of this
/// scan read the pre-sweep source as clean — the arm that allocated was three
/// statements further down than it ever looked.
fn statement(from: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in from.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ';' if depth == 0 => return &from[..i],
            _ => {}
        }
    }
    from
}

/// Names discarded under `#[cfg(not(test))]` whose binding allocates.
fn probe_only_allocations(src: &str) -> Vec<String> {
    let mut flagged = Vec::new();
    for discard in src.split("#[cfg(not(test))]").skip(1) {
        let Some(rest) = discard.trim_start().strip_prefix("let _ = ") else {
            continue;
        };
        let Some(name) = rest.split(';').next().map(str::trim) else {
            continue;
        };
        // The binding this discards, if the file still carries one. A name bound
        // by a `match`, an `if` or a call chain runs to the statement's end, so
        // the search takes everything up to the next `;` at any depth — wide, and
        // the scan errs toward flagging.
        for opener in [format!("let {name}"), format!("let mut {name}")] {
            let Some(at) = src.find(&opener) else {
                continue;
            };
            // `let tailing = ..` must not answer for `tail`.
            let next = src[at + opener.len()..].chars().next();
            if next.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let binding = &src[at..];
            if ALLOCATES.iter().any(|a| statement(binding).contains(a)) {
                flagged.push(name.to_owned());
                break;
            }
        }
    }
    flagged
}

/// The scan sees an allocating probe-only binding, and lets a widget handle by.
///
/// The first sample is the shape this file carried before the sweep, cut down to
/// the two lines that matter; without it, silence over the real file below would
/// mean nothing.
#[test]
fn the_scan_separates_an_allocated_value_from_a_widget_handle() {
    // The pre-sweep shape, not a simplification of it: the allocating arm sits
    // below statements of its own, which is what a scan that stops at the first
    // `;` walks straight past.
    let was = "    let tail: String = match sel {\n        A => {\n                           ui.label(RichText::new(\"App\").strong());\n            ui.label(\"x\");\n               \"Settings\".to_owned()\n        }\n    };\n    #[cfg(not(test))]\n                   let _ = tail;\n";
    assert_eq!(
        probe_only_allocations(was),
        vec!["tail".to_owned()],
        "the scan cannot see a probe-only `String`, so its silence over the real \
         file would prove nothing"
    );

    let handle = "    let row = ui.horizontal(|ui| {});\n    #[cfg(not(test))]\n    \
                  let _ = row;\n";
    assert!(
        probe_only_allocations(handle).is_empty(),
        "the scan flags a widget handle, which a production frame builds for its \
         own reasons and which costs nothing to discard"
    );
}

/// Nothing in the inspector is allocated on a production frame purely for the probe.
#[test]
fn the_inspector_allocates_nothing_only_the_probe_reads() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SWEPT);
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the swept file must be readable at {}: {e}", path.display()));
    assert!(
        src.len() > 10_000,
        "{} read back {} bytes, so a silent scan below would prove nothing",
        path.display(),
        src.len()
    );
    assert!(
        src.contains("#[cfg(not(test))]"),
        "the swept file carries no probe-only discard at all, so this scan has \
         nothing to be silent about"
    );

    let flagged = probe_only_allocations(&src);
    assert!(
        flagged.is_empty(),
        "{flagged:?} in {} are allocated on every production frame and read by \
         nothing but `InspectorProbe`. Bind them under `#[cfg(test)]` instead.",
        path.display()
    );
}
