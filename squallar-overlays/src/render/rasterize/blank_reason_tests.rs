//! **The [`BlankReason`] table is a table, and every reading off it is
//! reversible.**
//!
//! A reason travels three ways and each one indexes it differently: it is a
//! byte on the worker wire ([`BlankReason::wire_code`]), a slot in the
//! ledger's counter array ([`BlankReason::index`]), and a word in an always-on
//! log line ([`BlankReason::name`]) that the Tier-2 rig reads. **Two of those
//! three used to be the same function**, and stopped being one the day
//! `ExtentDeclaredEmpty` was appended: the wire's numbers may never move, so
//! it took code 6 while sitting at index 5 of a 7-wide array.
//!
//! That divergence is the whole reason this file exists. A transposition
//! between the two — or a variant added to one spelling and not the other —
//! puts a real count under the wrong name on a counter nothing else checks,
//! and the failure is silent by construction: every figure still balances,
//! every total is still right, and only the attribution is wrong. Which is
//! precisely the class of defect the enum was added to end, arriving through
//! the enum itself.

use super::BlankReason;

/// `ALL` is the ledger's array order, exactly.
///
/// The ledger sizes its counters `[AtomicU64; COUNT]` and writes at
/// `index()`; `Totals::blanks_over_covered_ground` reads them back by walking
/// `ALL`. If those two orders disagree, every blank is counted under a
/// neighbour's name.
#[test]
fn every_variant_sits_at_its_own_index_in_all() {
    assert_eq!(
        BlankReason::ALL.len(),
        BlankReason::COUNT,
        "`ALL` and `COUNT` disagree, so the ledger's array is the wrong width",
    );
    for (slot, reason) in BlankReason::ALL.iter().enumerate() {
        assert_eq!(
            reason.index(),
            slot,
            "{reason:?} answers index {} but sits at slot {slot} of `ALL`",
            reason.index(),
        );
    }
}

/// Every wire code round-trips, and no two variants share one.
///
/// **`from_wire_code` is the decoder's refusal**, so this also pins that it
/// refuses: a code past the table answers `None` rather than some reason of
/// ours, because defaulting an unknown code is how a wrong figure lands on an
/// always-on counter with nothing to say so.
#[test]
fn every_wire_code_round_trips_and_an_unknown_one_is_refused() {
    let mut seen = Vec::new();
    for reason in BlankReason::ALL {
        let code = reason.wire_code();
        assert!(
            !seen.contains(&code),
            "{reason:?} shares wire code {code} with an earlier variant; two \
             different blanks would decode to one",
        );
        seen.push(code);
        assert_eq!(
            BlankReason::from_wire_code(code),
            Some(reason),
            "{reason:?} did not survive its own wire code",
        );
    }
    // One past the largest code in use, and a byte far outside the table.
    let highest = seen.iter().copied().max().expect("the table is not empty");
    for code in [highest + 1, 200, 255] {
        assert_eq!(
            BlankReason::from_wire_code(code),
            None,
            "wire code {code} is not a reason this build knows, and reading \
             it as one would file a newer build's blank under a name of ours",
        );
    }
}

/// Every name is its own word.
///
/// The rig reads these out of the `overlay blanks:` line by name. Two variants
/// sharing one would merge two mechanisms into a single figure, and a name
/// that is a prefix of another would let a loose pattern match the wrong one.
#[test]
fn every_name_is_distinct_and_none_is_a_prefix_of_another() {
    for a in BlankReason::ALL {
        for b in BlankReason::ALL {
            if a == b {
                continue;
            }
            assert!(
                !a.name().starts_with(b.name()),
                "{a:?} is named `{}`, which starts with {b:?}'s `{}`",
                a.name(),
                b.name(),
            );
        }
    }
}

/// **Exactly one reason is a proven-correct clear, and it is the one proven by
/// an observation.**
///
/// Spelled as a literal list rather than re-derived from
/// `clears_covered_ground`, which would be the predicate agreeing with itself.
/// This is the one judgement in the enum that a reader acts on — it is what
/// separates `blanks_over_covered_ground` from a raw blank count — so a
/// variant that quietly joined the correct set would silently shrink the
/// figure a correctness reading is taken from.
///
/// **`ExtentDeclaredEmpty` is asserted to be OUTSIDE the set**, and that arm is
/// the point of the test rather than a detail of it. It is the variant that
/// most looks like a correct clear and is the only one whose truth is an
/// unchecked claim; admitting it would make a leg of wrong `paints_in`
/// refusals report zero blanks over covered ground.
#[test]
fn the_only_proven_correct_clear_is_the_one_the_cell_walk_observed() {
    let correct: Vec<BlankReason> = BlankReason::ALL
        .into_iter()
        .filter(|r| !r.clears_covered_ground())
        .collect();
    assert_eq!(
        correct,
        vec![BlankReason::OutsideCoverage],
        "the set of blanks counted as CORRECT has changed. Every other reason \
         may have cleared a pane over ground the layer still covers, which is \
         a picture the user lost; moving a reason into this set removes it \
         from `Totals::blanks_over_covered_ground` and makes a defect read as \
         normal behaviour",
    );
    assert!(
        BlankReason::ExtentDeclaredEmpty.clears_covered_ground(),
        "a handler's own `paints_in` refusal was counted as a correct clear. \
         Nothing downstream checks that claim, so a build whose refusals were \
         all wrong would report zero blanks over covered ground — the counter \
         reading perfect at the moment it is needed",
    );
    // The conjunct that keeps the assertion above from being vacuous if the
    // predicate were ever hard-wired one way.
    assert!(
        BlankReason::ALL
            .into_iter()
            .any(|r| r.clears_covered_ground()),
        "no reason counts against covered ground, so the split says nothing",
    );
}
