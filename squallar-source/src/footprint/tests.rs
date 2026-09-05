//! The pricing is against **real allocations**, so every test here compares a
//! figure to the capacity the container reports rather than to a model of it.

use super::*;

#[test]
fn an_empty_container_owns_nothing() {
    assert_eq!(Vec::<u64>::new().owned_bytes(), 0);
    assert_eq!(String::new().owned_bytes(), 0);
    assert_eq!(Option::<String>::None.owned_bytes(), 0);
    assert_eq!(HashMap::<u32, String>::new().owned_bytes(), 0);
}

#[test]
fn a_vec_prices_its_capacity_not_its_length() {
    let mut v: Vec<u64> = Vec::with_capacity(64);
    v.push(1);
    assert_eq!(
        v.owned_bytes(),
        64 * 8,
        "a Vec holding one element still holds the buffer it reserved",
    );
}

#[test]
fn nesting_reaches_every_owned_allocation() {
    // Two strings of known capacity inside a vector of known capacity.
    let rows: Vec<String> = vec![String::with_capacity(100), String::with_capacity(250)];
    assert_eq!(rows.owned_bytes(), (2 * size_of::<String>() + 350) as u64);
}

/// The whole reason the `Arc` body is a free function: a body priced by two
/// holders is a double count inside one census figure.
#[test]
fn an_arc_body_is_priced_once_by_its_creator() {
    let body: Arc<Vec<u64>> = Arc::new(Vec::with_capacity(16));
    let shared = Arc::clone(&body);
    let priced = arc_body(&body);
    assert_eq!(
        priced,
        (2 * size_of::<usize>() + size_of::<Vec<u64>>()) as u64 + 16 * 8,
    );
    // The second holder prices the same body identically — which is exactly
    // why only one of them may ask.
    assert_eq!(arc_body(&shared), priced);
}

#[test]
fn the_installed_level_moves_by_the_difference_and_never_by_the_figure() {
    let level = AtomicU64::new(500);
    move_level(&level, 200, 350);
    assert_eq!(level.load(Relaxed), 650);
    move_level(&level, 350, 0);
    assert_eq!(level.load(Relaxed), 300, "a release subtracts what it held");
}
