//! **A real granule's decode declares its bytes before it takes them, and the
//! ledger comes back to where it started.**
//!
//! The two halves of reserve-then-use, on the one source in this crate that
//! has a committed granule to decode:
//!
//! * **Before** — the netCDF header has given `shape("data")` and not one
//!   chunk has been inflated, so the allocation about to happen is already
//!   declared and a budget asked at that instant sees it.
//! * **After** — the reserve is reconciled against what the grid actually
//!   came out as, and the outstanding level returns to what it was. A reserve
//!   that did not retire would read as a scene that keeps growing, which is
//!   the exact symptom the budget model is watching for.
//!
//! Both arms, because only one of them is the interesting failure. A decode
//! that succeeds must settle; a decode that **gives up** must release, and
//! that is the path every fallible decoder actually takes on bad data — so it
//! is the one that must not read as a leak.

use squallar_overlays::gmgsi::{GmgsiChannel, decode};
use squallar_source::reserve;

const GRANULE: &[u8] = include_bytes!(
    "../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

/// The grid the fixture declares, from `data(time, yc, xc)`.
const POINTS: u64 = 3000 * 5000;

/// **These two tests share one ledger, so they may not run at once.**
///
/// The ledger is process-global by design — a reserve is a claim on the
/// machine's memory and the machine is one — and `cargo test` runs the tests
/// in a binary on parallel threads. Both tests below read the outstanding
/// level, decode, and assert the level came back to where it was, which is
/// only true if nothing else declared bytes in between. Without this lock the
/// pair failed about one run in six.
///
/// A lock rather than a tolerance: "the level returned to within a granule's
/// worth" would pass just as well on a decode that leaked one, which is the
/// defect these tests are for. Poisoning is stepped over so a genuine failure
/// in one test reports itself instead of turning the other into a panic about
/// a mutex.
static LEDGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serialised<T>(body: impl FnOnce() -> T) -> T {
    let _held = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    body()
}

/// A successful decode declares the high-water mark at the header and retires
/// it against the truth, leaving the ledger where it found it.
#[test]
fn a_decoded_granule_declares_its_bytes_and_retires_the_reserve() {
    serialised(|| {
        let led = reserve::global();
        let before = led.outstanding_bytes();
        let peak_before = led.peak_bytes();

        let grid = decode::decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr)
            .expect("the committed granule decodes");
        assert!(
            grid.grid.values.resident_bytes() > 0,
            "the fixture decoded to nothing",
        );

        assert_eq!(
            led.outstanding_bytes(),
            before,
            "the decode left bytes declared after its values were real",
        );

        // The declaration really happened: the high-water mark moved by at least
        // the granule's own points. Without this the assertion above would pass
        // just as well on a decode that never reserved at all — the vacuous
        // reading this test exists to rule out.
        assert!(
            led.peak_bytes() >= peak_before.max(POINTS),
            "the peak never rose, so nothing was declared: {} vs {}",
            led.peak_bytes(),
            POINTS,
        );
    });
}

/// **The path bad data takes.** A granule that will not parse must release its
/// declaration rather than leave it standing, and must not be counted as an
/// arrival that overshot its reserve — a decode that never happened did not
/// overshoot anything.
#[test]
fn a_granule_that_will_not_decode_releases_its_declaration() {
    serialised(|| {
        let led = reserve::global();
        let before = led.outstanding_bytes();
        let over_before = led.over_arrivals();

        // Truncated hard: enough to be a file, not enough to be a granule.
        let broken = GRANULE[..GRANULE.len() / 4].to_vec();
        assert!(
            decode::decode(broken, GmgsiChannel::LongwaveIr).is_err(),
            "the truncated fixture decoded, so this test proves nothing",
        );

        assert_eq!(
            led.outstanding_bytes(),
            before,
            "a refused decode left its reserve standing",
        );
        assert_eq!(
            led.over_arrivals(),
            over_before,
            "a refused decode was counted as an arrival over its reserve",
        );
    });
}
