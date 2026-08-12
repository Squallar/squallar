use crate::site_position::SitePosition;
use crate::types::EARTH_RADIUS_KM;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// Which height above mean sea level a caller means.
///
/// A site has two, and they are 30–115 ft apart: the ground the tower stands
/// on, and the feedhorn on top of it. A single number cannot say which, and
/// while the network shipped as a compiled-in table nobody had checked which
/// one 201 of its 207 rows were on — so every consumer that added a site
/// height to a beam height was choosing a datum by inheritance. This type
/// makes the choice a word in the call, and it outlives that table because the
/// two datums are a property of the measurement rather than of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datum {
    /// The ground under the tower — `site_height` in a Volume Data Block.
    ///
    /// **Learned-only, and asked for by nothing.** The first is pinned by
    /// `only_a_volume_can_answer_the_base_datum`; the second by the tripwire
    /// each geometry consumer already carries —
    /// `the_render_paths_site_height_is_the_feedhorn` in [`crate::render`],
    /// and the equivalents in [`crate::voxel`] and [`crate::xsect`], every one
    /// of which fails if its call is switched back to this variant.
    ///
    /// The only source that separates a base from a tower is a WSR-88D's own
    /// Volume Data Block, which reports the two fields independently. A
    /// published station record gives one elevation and no way to split it, so
    /// a radar this install has only ever read about answers `None` here. That
    /// became the common case when the compiled-in table was deleted: before,
    /// 161 rows shipped with both figures; now a row has both only if a volume
    /// this install decoded said so.
    ///
    /// It survives that narrowing because the distinction is 30–115 ft and is
    /// still in the data — [`SiteHeights::BaseAndTower`] is what every learned
    /// WSR-88D row holds — and because naming a datum at the call is what
    /// stops the four geometry consumers drifting back onto the ground. It is
    /// **not** what a beam height should be added to: [`crate::beam`] measures
    /// above the antenna, so that is [`Datum::Feedhorn`].
    SiteBase,
    /// The feedhorn — `site_height + tower_height`, the point [`crate::beam`]
    /// measures every height above, and the figure a published station record
    /// quotes as the radar's elevation.
    Feedhorn,
}

/// What a row knows about its own height, and on which datum.
///
/// Two shapes rather than one, because the archive genuinely reports two
/// shapes and flattening them would put the old ambiguity back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteHeights {
    /// The two heights a WSR-88D's Volume Data Block reports separately: the
    /// ground under the tower, and the tower above that ground.
    ///
    /// `tower_ft` is the archive's own figure converted, and the archive
    /// truncates to whole metres — the five standard towers read back as 14,
    /// 19, 24, 29 and 34 m against published heights of 48, 65, 81, 97 and
    /// 114 ft — so a feedhorn built from it sits up to 3 ft low. That is the
    /// precision of the source, and it is two orders below the 30–115 ft this
    /// type exists to stop losing.
    BaseAndTower { base_ft: i32, tower_ft: i32 },
    /// One height, on the feedhorn, with no separable tower.
    ///
    /// Every TDWR volume reports `tower_height` byte-identical to
    /// `site_height`, and no WSR-88D volume does — the correspondence is
    /// exact across all 205 volumes read. So a TDWR carries one figure, and
    /// the published station record agrees with it to 3.2 ft while agreeing
    /// with the *feedhorn* everywhere it can be checked on a WSR-88D. Hence
    /// feedhorn, and hence no answer at all for [`Datum::SiteBase`]: the base
    /// is unknown, not equal to this.
    ///
    /// This is also the only shape [`SiteFix::Network`] can produce, because a
    /// published station record quotes one elevation and offers nothing to
    /// split it with.
    FeedhornOnly { feedhorn_ft: i32 },
}

/// `PartialEq` because [`extended`] compares the row a fix would produce
/// against the row already in the table, and builds nothing when they are
/// equal. Without that comparison every startup would leak a fresh copy of the
/// whole table for a catalogue that had not changed since the last one.
#[derive(Debug, Clone, PartialEq)]
pub struct RadarSite {
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    /// The heights this row records, or `None` if it records none.
    ///
    /// No row any source can produce is `None` —
    /// `every_placed_row_records_an_elevation` keeps it that way over both
    /// [`SiteFix::Learned`] and [`SiteFix::Network`], because a missing
    /// elevation used to reach [`crate::eet::radar_height_ft_near`] and come
    /// back as sea level, which is a plausible-looking answer for a coastal
    /// site and a 292 ft error at KLWX.
    ///
    /// The `Option` stays because the field is public and the type must not
    /// let a hand-built row assert an elevation it does not have. A radar with
    /// no height *at all* is expressed by having no row —
    /// [`SiteTable::unplaced`] — not by a row full of zeros.
    pub heights: Option<SiteHeights>,
}

impl RadarSite {
    /// This site's height on `datum`, feet MSL, or `None` if the row does not
    /// record that datum.
    ///
    /// `None` is a real answer, not a formality: a [`SiteHeights::FeedhornOnly`]
    /// row has no base, and returning its feedhorn for [`Datum::SiteBase`]
    /// would be the same silent substitution this type was introduced to
    /// remove.
    pub fn height_ft(&self, datum: Datum) -> Option<i32> {
        match (self.heights?, datum) {
            (SiteHeights::BaseAndTower { base_ft, .. }, Datum::SiteBase) => Some(base_ft),
            (SiteHeights::BaseAndTower { base_ft, tower_ft }, Datum::Feedhorn) => {
                Some(base_ft + tower_ft)
            }
            (SiteHeights::FeedhornOnly { feedhorn_ft }, Datum::Feedhorn) => Some(feedhorn_ft),
            (SiteHeights::FeedhornOnly { .. }, Datum::SiteBase) => None,
        }
    }
}

/// The name a [`RadarSite`] carries when nothing in the build knows the
/// identifier.
///
/// It is a placeholder for a `&'static str` this crate cannot manufacture from
/// a four-byte ICAO read at runtime, **not** a claim that the row means
/// anything. A site carrying this name has no position of its own unless a
/// volume supplied one, and
/// [`ScanInfo::site_source`](crate::types::ScanInfo::site_source) is where a
/// caller finds out which.
pub const UNKNOWN_SITE_NAME: &str = "UNKNOWN";

/// Every radar this process knows about, resolved once and then extended.
///
/// # There is no compiled-in table under this
///
/// There used to be: a `const SEED: [RadarSite; 207]`, every row measured out
/// of that site's own Level II volume on one day. It was correct on the day it
/// was compiled and it rotted from then on — a radar commissioned, relocated
/// or re-surveyed after the build was one the binary could never learn about,
/// and a binary that carries a list of the network is a binary whose list is
/// wrong for the rest of its life.
///
/// So a fresh process starts with **no radars at all**, and everything below
/// answers `None` until something outside the binary says otherwise. The three
/// things that can are, in order of authority: a volume this install decoded
/// ([`SiteFix::Learned`]), the fetched network catalogue ([`SiteFix::Network`]),
/// and — for a radar the catalogue lists but cannot place — bare membership
/// ([`SiteFix::Unplaced`]).
///
/// An empty table is a real state and not a broken one, and every consumer is
/// written for it: `nearest_wsr88d_site` answers `None`, the map draws no
/// marker, and [`crate::eet::radar_height_ft_near`] answers `None` rather than
/// the sea level a missing row used to degrade to.
///
/// # Rows, and members with no row
///
/// [`rows`](Self::rows) is every radar this process can *place*. Some radars
/// are known to exist and cannot be placed — `TPBI` and `KCRI` have Level II
/// data in the archive bucket and 404 from `api.weather.gov/radar/stations` —
/// and those are [`unplaced`](Self::unplaced) instead.
///
/// Keeping them out of `rows` is deliberate: a row needs a latitude and a
/// longitude, and the only numbers available for one of these would be zeros,
/// which is a marker in the Gulf of Guinea drawn with exactly the confidence
/// of a real one. Keeping them *somewhere* is equally deliberate: Level II
/// data is fetched by identifier and not by position, so a user can open one
/// of these and the volume that arrives then places it for good.
///
/// # Why the rows are leaked
///
/// Held as `&'static [RadarSite]` because they are leaked on the way in, which
/// is what lets every lookup below keep handing out `&'static RadarSite` the
/// way the compiled-in array used to. Leaking is honest here rather than a
/// dodge: a table is resolved a bounded number of times per process — once at
/// startup, once more on Android when the config directory arrives, and once
/// per radar whose volume the session decodes — and then read for the life of
/// the process, so "lives forever" is a description of the value's real
/// lifetime and not a promise being smuggled past the borrow checker.
///
/// The name index is built with the rows and travels with them, so the two
/// cannot disagree about which radars exist. That was the one real hazard in
/// making the table resizable: a `LazyLock` name map beside a swappable row
/// list would answer for a table nobody could still see.
pub struct SiteTable {
    rows: &'static [RadarSite],
    by_name: HashMap<&'static str, &'static RadarSite>,
    /// Identifiers that exist but have no position, sorted and deduplicated.
    /// Disjoint from `rows` by construction — see [`extended`].
    unplaced: &'static [&'static str],
}

impl SiteTable {
    /// Every radar this table can place, in the order they were learned.
    ///
    /// Callers may walk this and may enumerate it, but must not store the
    /// enumeration index anywhere that outlives the walk: the next table can
    /// be a different length, and position `n` in it is a different radar.
    /// Keep the `&'static RadarSite` itself, which stays valid and keeps
    /// naming the row it named.
    pub fn rows(&self) -> &'static [RadarSite] {
        self.rows
    }

    /// Identifiers this table knows exist but cannot place, sorted.
    ///
    /// Never overlaps [`rows`](Self::rows): a radar that gains a position
    /// leaves this list in the same resolution that gives it a row.
    pub fn unplaced(&self) -> &'static [&'static str] {
        self.unplaced
    }

    /// Whether this table has heard of `site` at all, placed or not.
    ///
    /// The membership question, as distinct from the position question that
    /// [`get`](Self::get) asks. A site list wants this one; a map marker wants
    /// the other.
    pub fn knows(&self, site: &str) -> bool {
        self.by_name.contains_key(site) || self.unplaced.contains(&site)
    }

    /// The row for an ICAO identifier, or `None` if this table cannot place
    /// that radar — because it has never heard of it, or because it has heard
    /// of it and has no position for it.
    pub fn get(&self, site: &str) -> Option<&'static RadarSite> {
        self.by_name.get(site).copied()
    }

    /// The row closest to `lat`/`lon`, with its distance in kilometres.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
        self.nearest_where(lat, lon, |_| true)
    }

    /// The closest operational WSR-88D, with its distance in kilometres.
    pub fn nearest_wsr88d(&self, lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
        self.nearest_where(lat, lon, |site| site.is_wsr88d() && site.is_operational())
    }

    fn nearest_where(
        &self,
        lat: f64,
        lon: f64,
        accept: impl Fn(&RadarSite) -> bool,
    ) -> Option<(&'static RadarSite, f64)> {
        if !lat.is_finite() || !lon.is_finite() {
            return None;
        }
        self.rows
            .iter()
            .filter(|site| accept(site))
            .map(|site| (site, distance_km(lat, lon, site.lat, site.lon)))
            // `total_cmp`, not `partial_cmp().unwrap()`: the distances are
            // finite given a finite input, but the unwrap would be a panic
            // path in a startup routine to save nothing.
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
    }
}

/// How much authority one [`SiteFix`] carries, as a value that can be compared.
///
/// **A smaller variant outranks a larger one**, matching
/// [`SitePositionSource`](crate::site_position::SitePositionSource), whose
/// order this continues. Put end to end the two make one ladder, and it is the
/// design:
///
/// ```text
/// Volume    the volume in hand        ScanInfo::from_scan  SitePositionSource
/// Learned   a volume, earlier         SiteFix::Learned     SiteFixRank
/// Network   the fetched catalogue     SiteFix::Network     SiteFixRank
/// Unplaced  the catalogue, positionless  SiteFix::Unplaced SiteFixRank
/// ```
///
/// There is no rung below `Unplaced`. There used to be — a compiled-in seed
/// the fixes were applied *to* — and deleting it removed the bottom of the
/// ladder rather than changing the order of what is left: a radar no rung
/// speaks for is now a radar this process has never heard of.
/// `the_precedence_is_volume_learned_network_unplaced` pins all four rungs
/// together as one table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SiteFixRank {
    /// A volume this install decoded said so.
    Learned,
    /// The fetched network catalogue said so.
    Network,
    /// The catalogue lists the radar and cannot say where it is. The weakest
    /// claim there is — it asserts existence and nothing else — so any fix
    /// that carries a position displaces it.
    Unplaced,
}

/// One source's claim about where one radar is: the shape [`build_table`] and
/// [`resolve`] take.
///
/// # Why this is not a bare `SitePosition`
///
/// A [`SitePosition`] is specifically *a Volume Data Block's four fields* —
/// position in micro-degrees plus `site_height` and `tower_height` as two
/// separately-reported whole metres — and reading a published station record
/// into that shape would have had to invent a tower.
///
/// The network catalogue differs from a volume in both directions:
///
/// * it has **less** height information — one elevation, on the feedhorn, with
///   no separable tower — so it can never fill a
///   [`SiteHeights::BaseAndTower`] and must never overwrite one; and
/// * it has **less authority** — it is a record about the radar rather than a
///   report from it — so where both sources speak the volume wins.
///
/// And a bucket listing has less than either: it says a radar exists and
/// nothing more.
///
/// A tuple of numbers could say none of that. Naming the source says all of
/// it: [`rank`](Self::rank) makes the precedence a value, and
/// [`applied`](Self::applied) makes each source's height provenance a branch
/// instead of a convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteFix {
    /// What a volume this install decoded stated about itself. Carries the
    /// Volume Data Block's own fields, so it can speak to both height datums
    /// and be adjudicated against an earlier row's finer feet by
    /// [`SitePosition::heights_over`].
    Learned(SitePosition),
    /// What the fetched catalogue says: a position, and **one** elevation.
    ///
    /// The elevation is on the feedhorn. `api.weather.gov/radar/stations`
    /// agreed with the measured figures the deleted seed carried — read out of
    /// the volumes themselves — to a median of 1.5 m with no row past 187 m,
    /// and agrees with the *feedhorn* wherever a WSR-88D lets the two be told
    /// apart. There is no tower figure and none can be derived, which is why
    /// this is one number and not two.
    Network {
        /// Latitude, micro-degrees north.
        lat_udeg: i32,
        /// Longitude, micro-degrees east.
        lon_udeg: i32,
        /// Feedhorn height, whole metres MSL.
        feedhorn_m: i32,
    },
    /// The radar exists and nothing here knows where it is.
    ///
    /// The archive bucket lists an identifier that `api.weather.gov` does not
    /// place: `TPBI` and `KCRI` are both, and both have real Level II data. A
    /// claim with no numbers in it, so it produces no row — it produces
    /// membership, which is what [`SiteTable::unplaced`] holds and what keeps
    /// these radars in the site list instead of at Null Island.
    ///
    /// Weakest rung on purpose. A radar that is listed *and* placed gets both
    /// fixes in the same stream, and the placed one wins by rank rather than
    /// by whichever the caller happened to chain first.
    Unplaced,
}

impl SiteFix {
    /// How much authority this claim carries. See [`SiteFixRank`].
    pub fn rank(&self) -> SiteFixRank {
        match self {
            Self::Learned(_) => SiteFixRank::Learned,
            Self::Network { .. } => SiteFixRank::Network,
            Self::Unplaced => SiteFixRank::Unplaced,
        }
    }

    /// The row a radar called `name` becomes under this fix, given whatever
    /// heights the row it is displacing already recorded, or `None` if this
    /// fix carries no position and so produces no row.
    ///
    /// `known` is `None` for a radar the table has never heard of.
    ///
    /// # Position always moves; heights only improve
    ///
    /// The two placing arms take the fix's position outright — that is what a
    /// fix is for. They differ on heights, because their provenance differs:
    ///
    /// * **Learned** goes through [`SitePosition::heights_over`], which keeps
    ///   `known`'s finer foot figures wherever the volume's whole metre cannot
    ///   contradict them, and takes the volume's where it can.
    /// * **Network** keeps `known` untouched whenever there is one. Its single
    ///   feedhorn metre is strictly less than a `BaseAndTower` row already
    ///   holds, so overwriting would *lose* the base datum: a radar this
    ///   install has learned from a volume would stop answering
    ///   [`Datum::SiteBase`] the first time a catalogue landed on it. It fills
    ///   in only where the row had nothing.
    ///
    /// # Every row this produces records an elevation
    ///
    /// `heights` is `Some` on both placing arms and for both values of
    /// `known`, which is what keeps `every_placed_row_records_an_elevation`
    /// true. A row without one anchors a cross-section at sea level, which
    /// reads as a measurement rather than as a gap — 292 ft of it at `KLWX`.
    ///
    /// `Unplaced` produces no row at all, which is a different thing from a
    /// row with no elevation and is why the two cases are not one `Option`
    /// deeper in the type.
    fn applied(&self, name: &'static str, known: Option<SiteHeights>) -> Option<RadarSite> {
        match *self {
            Self::Learned(position) => Some(position.applied_to_named(name, known)),
            Self::Network {
                lat_udeg,
                lon_udeg,
                feedhorn_m,
            } => Some(RadarSite {
                name,
                lat: crate::site_position::degrees_from_micro(lat_udeg),
                lon: crate::site_position::degrees_from_micro(lon_udeg),
                heights: known.or(Some(SiteHeights::FeedhornOnly {
                    feedhorn_ft: crate::site_position::feet_from_metres(feedhorn_m),
                })),
            }),
            Self::Unplaced => None,
        }
    }
}

/// An empty table with `fixes` applied: a row for every radar one of them can
/// place, and membership for every radar they can only name.
///
/// This is the whole point of the exercise. The binary used to carry a list of
/// the network, and an identifier outside that list could not be named,
/// placed or drawn no matter what the application had learned about it:
/// `get_radar_site` answered `None`, `applied_to` fell back to
/// [`UNKNOWN_SITE_NAME`], the map drew no marker and the site list had no row.
/// Now nothing is carried, and every radar the user sees got here because
/// something outside the binary said it exists.
///
/// # The strongest fix per radar wins, and only that one is applied
///
/// `fixes` is a flat stream from every source at once — that is what lets a
/// caller `chain` its learned cache onto its catalogue without either knowing
/// about the other. Where two of them name the same radar, [`SiteFixRank`]
/// decides, once, before anything is built. A fetched position therefore never
/// reaches a row a learned one also claims: it is not overwritten afterwards,
/// it is never applied. Nor does bare membership ever hide a position that
/// arrived in the same stream.
///
/// # The name is leaked
///
/// [`RadarSite::name`] is `&'static str` and a four-byte identifier read out of
/// a volume header or a JSON body at runtime is not, so an *arrival*'s name is
/// leaked — a handful of bytes per radar, bounded by the catalogue's own size.
/// A radar the base table already has keeps the `&'static str` it already had,
/// so a fix landing on an existing row leaks no name at all.
pub fn build_table<'a, I>(fixes: I) -> &'static SiteTable
where
    I: IntoIterator<Item = (&'a str, SiteFix)>,
{
    extended(empty_table(), fixes).unwrap_or_else(empty_table)
}

/// `base` with `fixes` applied, or `None` if applying them would change
/// nothing.
///
/// `None` rather than an identical copy so a resolution that says nothing new —
/// an install that has learned nothing and fetched nothing, every fresh
/// install, every test that builds an app against an empty store, and every
/// *second* resolution against an unchanged catalogue cache — reuses the table
/// it already has instead of leaking a second copy beside it. That last case
/// is why the comparison is against the produced row and not merely against the
/// set of names: Android resolves twice with the same cache, and a name-only
/// check would leak a whole table on the second call.
///
/// # `rows` and `unplaced` stay disjoint
///
/// A radar can be in exactly one of them, and which one is decided by the
/// strongest fix that names it. So a radar that was only a member and then
/// gains a position leaves `unplaced` in the same resolution that gives it a
/// row — otherwise it would appear twice in a site list that walks both, which
/// is the `TOK2`/`DOP1` duplicate-row failure in a new place.
fn extended<'a, I>(base: &'static SiteTable, fixes: I) -> Option<&'static SiteTable>
where
    I: IntoIterator<Item = (&'a str, SiteFix)>,
{
    // The strongest claim per radar, decided before a row is built. Doing it
    // here rather than while walking the rows is what makes "a fetched position
    // never outranks a learned one" a property of the *input* rather than of
    // the order two loops happened to run in.
    let mut best: HashMap<&'a str, SiteFix> = HashMap::new();
    for (name, fix) in fixes {
        match best.entry(name) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(fix);
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                if fix.rank() < slot.get().rank() {
                    slot.insert(fix);
                }
            }
        }
    }
    if best.is_empty() {
        return None;
    }

    let mut rows: Vec<RadarSite> = base.rows().to_vec();
    let mut changed = false;
    for row in &mut rows {
        // `remove`, so what is left in `best` afterwards is exactly the set of
        // radars this table has no row for.
        if let Some(fix) = best.remove(row.name) {
            // A row that is already placed is never *un*placed: `Unplaced` is
            // the catalogue saying it cannot help, not that the position this
            // install already has is wrong.
            if let Some(next) = fix.applied(row.name, row.heights)
                && next != *row
            {
                *row = next;
                changed = true;
            }
        }
    }

    // Sorted, so a table resolved from the same inputs is the same table
    // whichever order a `HashMap` felt like iterating in. Nothing addresses a
    // row by position, but a table that varies run to run is one no test can
    // pin.
    let mut arrivals: Vec<(&'a str, SiteFix)> = best.into_iter().collect();
    arrivals.sort_unstable_by_key(|(name, _)| *name);

    let mut unplaced: Vec<&'static str> = base.unplaced().to_vec();
    for (name, fix) in arrivals {
        // A member already listed keeps the `&'static str` it was leaked
        // under, so re-reading the same catalogue every launch leaks nothing.
        if let Some(known) = unplaced.iter().find(|listed| **listed == name).copied() {
            // `Some` means it was a member and is now placed: one row, and it
            // leaves the member list. `None` means it is still only a member,
            // which is nothing to do and nothing to leak.
            if let Some(row) = fix.applied(known, None) {
                unplaced.retain(|listed| *listed != known);
                rows.push(row);
                changed = true;
            }
            continue;
        }
        let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
        match fix.applied(name, None) {
            Some(row) => rows.push(row),
            None => unplaced.push(name),
        }
        changed = true;
    }
    if !changed {
        return None;
    }

    unplaced.sort_unstable();
    let rows: &'static [RadarSite] = Box::leak(rows.into_boxed_slice());
    let by_name = rows.iter().map(|row| (row.name, row)).collect();
    Some(Box::leak(Box::new(SiteTable {
        rows,
        by_name,
        unplaced: Box::leak(unplaced.into_boxed_slice()),
    })))
}

/// No radars, built once.
///
/// Where every process starts. A process that never resolves anything stays
/// here, and that is the honest answer for it: nothing has told this binary
/// that any radar exists.
///
/// One shared value rather than a fresh allocation per call, so
/// `std::ptr::eq` against it is a meaningful test of "nothing was built".
fn empty_table() -> &'static SiteTable {
    static EMPTY: LazyLock<&'static SiteTable> = LazyLock::new(|| {
        &*Box::leak(Box::new(SiteTable {
            rows: &[],
            by_name: HashMap::new(),
            unplaced: &[],
        }))
    });
    *EMPTY
}

/// The table this process resolved, or `None` until something resolves one.
static RESOLVED: RwLock<Option<&'static SiteTable>> = RwLock::new(None);

/// The table every lookup in this module reads.
///
/// Falls back to [`empty_table`] rather than panicking when nothing has
/// resolved yet: a crate-level test, a benchmark or a tool that never builds
/// an app gets no radars, answers `None` to every question about one, and does
/// not pretend otherwise.
pub fn table() -> &'static SiteTable {
    // `into_inner` rather than `expect`: a poisoned lock here means some other
    // thread panicked while swapping a pointer, and the pointer it was
    // swapping is valid either way. Refusing to draw the map over it would
    // turn an unrelated panic into a second one.
    match *RESOLVED.read().unwrap_or_else(|e| e.into_inner()) {
        Some(table) => table,
        None => empty_table(),
    }
}

/// Resolve the process-wide site table from what this install knows.
///
/// # Resolve before the paint that would be affected
///
/// A site's position or name that arrives *late* moves a marker, a label and
/// a section's height datum under a user who is already looking at them,
/// which is the one thing the reopen-is-1:1 rule forbids. So the resolutions
/// that could move an existing row belong with the rest of the startup read,
/// beside the config load and ahead of any frame — `App::new`, and again in
/// `set_config_dir` where Android finally has a store to read from. Both run
/// before a frame exists.
///
/// Resolving is therefore not one-shot: Android genuinely resolves twice, and
/// a `OnceLock` would have let the empty first attempt win and silently
/// discarded every learned position on the one platform that needs the second
/// call.
///
/// Two later resolutions exist and neither can move anything a user is
/// looking at, which is why they are allowed to run mid-session:
///
/// * **the first catalogue on a fresh install**, applied the moment it lands
///   rather than on the next launch. With nothing carried in the binary the
///   table before it is *empty*, so there is no marker to move and no label to
///   change — there is only the app finding out that radars exist.
/// * **a volume this session decoded**, which adds a row for the radar whose
///   data the user just asked for, at the position that volume states. The map
///   is already drawn from that same position by way of
///   [`crate::types::ScanInfo`], so the row agrees with what is on screen the
///   moment it exists. Without it, a first session would render every MSL
///   height against an empty table — which is
///   [`crate::eet::radar_height_ft_near`] answering `None` and the render
///   paths falling back to sea level.
///
/// # Resolution never forgets a radar
///
/// Each call extends the table already in hand rather than rebuilding from
/// nothing, so the process can learn about a radar but never lose one, and a
/// later resolution with nothing to say cannot undo an earlier one that had
/// something. That is what the Android pair needs: `App::new` resolves with no
/// store at all and `set_config_dir` resolves with the real one.
///
/// A resolution that changes nothing is a genuine no-op — it does not even
/// take the write lock — so the common startup costs nothing.
///
/// # Rows do move
///
/// A fix displaces the row it lands on (see [`build_table`]), so a
/// `&'static RadarSite` taken before a resolution keeps naming the radar it
/// named but may describe where that radar was believed to be a moment
/// earlier. That is not a hazard in practice and must not become one: see the
/// four call sites above, each of which either precedes the first frame or
/// cannot contradict it.
///
/// An index into [`radars()`] was never valid across a resolution and still is
/// not, which is why nothing outside this module can reach the rows by
/// position at all.
pub fn resolve<'a, I>(fixes: I) -> &'static SiteTable
where
    I: IntoIterator<Item = (&'a str, SiteFix)>,
{
    // The write lock is held across the read *and* the build, not just the
    // store. Reading the current table, extending it and then storing it as
    // three steps is a lost update: two resolutions that start from the same
    // table each produce one that lacks the other's radars, and whichever
    // stores second silently discards the first. A test caught this doing
    // exactly that. Building under the lock costs a startup a few
    // microseconds it has.
    let mut resolved = RESOLVED.write().unwrap_or_else(|e| e.into_inner());
    let current = resolved.unwrap_or_else(empty_table);
    match extended(current, fixes) {
        Some(extended) => {
            *resolved = Some(extended);
            extended
        }
        None => current,
    }
}

/// Every radar this process can place.
///
/// Empty until something resolves a table. Walk it, filter it, count it — but
/// see [`SiteTable::rows`] on why an index into it must not outlive the walk,
/// and [`unplaced`] for the radars that exist without appearing here.
pub fn radars() -> &'static [RadarSite] {
    table().rows()
}

/// Identifiers this process knows exist and cannot place.
///
/// Never overlaps [`radars()`]. See [`SiteTable::unplaced`].
pub fn unplaced() -> &'static [&'static str] {
    table().unplaced()
}

/// Whether this process has heard of `site` at all, placed or not.
pub fn knows_site(site: &str) -> bool {
    table().knows(site)
}

/// The row for an ICAO identifier, or `None` if this process cannot place that
/// radar.
pub fn get_radar_site(site: &str) -> Option<&'static RadarSite> {
    table().get(site)
}

/// Great-circle distance between two coordinates, in kilometres.
///
/// Haversine rather than the cheaper equirectangular approximation: the caller
/// compares sites up to a continent apart (a fix in Hawaii against a table that
/// is mostly CONUS), and the flat approximation's error grows with both
/// separation and latitude — exactly the regime the comparison runs in.
///
/// On [`crate::types::EARTH_RADIUS_KM`], the workspace's one sphere. This
/// used to carry its own `6371.0088`, the IUGG mean to four more decimals;
/// the two are 1.4e-6 % apart, which is 9 m across a continent and cannot
/// change which radar is nearest.
pub fn distance_km(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    let (lat_a_rad, lat_b_rad) = (lat_a.to_radians(), lat_b.to_radians());
    let d_lat = (lat_b - lat_a).to_radians();
    let d_lon = (lon_b - lon_a).to_radians();

    let h = (d_lat / 2.0).sin().powi(2)
        + lat_a_rad.cos() * lat_b_rad.cos() * (d_lon / 2.0).sin().powi(2);
    // `asin(sqrt(h))` rather than `atan2`: h is clamped below, so the numerically
    // delicate case `atan2` exists to handle cannot arise here.
    2.0 * EARTH_RADIUS_KM * h.clamp(0.0, 1.0).sqrt().asin()
}

impl RadarSite {
    /// Whether this is a Terminal Doppler Weather Radar rather than a WSR-88D.
    ///
    /// Not a difference in availability: both networks land in the same Level
    /// II archive under the same `YYYY/MM/DD/SITE/` prefix, a TDWR's objects
    /// ending `_V08` where a WSR-88D's end `_V06`, and nothing in the fetch or
    /// decode path branches on which of the two a site is. What differs is the
    /// instrument. A TDWR is single-pol, so no differential phase or
    /// correlation coefficient and nothing derived from them; it watches one
    /// airport, so its Doppler cuts reach ~89 km (592 gates of 150 m, read off
    /// `TPIT`) beside one surveillance cut out to ~417 km (1,390 of 300 m);
    /// and its radials are 1.0° apart rather than 0.5°.
    ///
    /// Its Level III products differ in kind, not only in number: a TDWR
    /// publishes the legacy single-pol codes (`TZL`, `TZ0`-`TZ2`, `TV0`-`TV2`,
    /// `NCR`, `NHI`, `NMD` and the like), and not one of the four codes
    /// [`crate::level3`] fetches — checked 2026-08-11 by listing that bucket
    /// for `PIT`, `OKC`, `MIA` and `DCA`, where `EET`, `DVL`, `DPR` and `N0K`
    /// return no key at all while `TLX` has all four.
    ///
    /// [`radars()`] lists both networks, because the map draws a marker for
    /// every site it knows.
    ///
    /// A rule over the identifier rather than a flag on a row, which is why it
    /// survived the compiled-in table. See [`is_tdwr_id`], which is the rule
    /// itself and the only place it is spelled.
    pub fn is_tdwr(&self) -> bool {
        is_tdwr_id(self.name)
    }

    /// Whether this site is a WSR-88D — the network with dual-pol moments,
    /// 0.5° radials, and every Level III code this app fetches.
    ///
    /// A capability question rather than a label, and the one to ask before
    /// offering or fetching anything that needs a dual-pol moment or a Level
    /// III object: for a TDWR the answer is no on both counts, and it is no
    /// for the whole site rather than for a particular volume, so a caller can
    /// decide once. See [`is_tdwr`](Self::is_tdwr) for what was measured.
    pub fn is_wsr88d(&self) -> bool {
        !self.is_tdwr()
    }

    /// Whether this site runs an operational scan an ordinary viewer can rely on.
    ///
    /// `KCRI` is the Radar Operations Center's test bed in Norman. It is a real
    /// WSR-88D and it does reach the archive, but it scans to whatever schedule
    /// the ROC is testing that day rather than continuously. It also sits 0.4 km
    /// closer to downtown Oklahoma City than `KTLX` does, so *every* automatic
    /// pick for the Oklahoma City metro would land on it and intermittently show
    /// an empty map.
    ///
    /// Only automatic selection consults this. The site stays in [`radars()`],
    /// the map still draws it, and a user who picks it by hand still gets it.
    ///
    /// `KCRI` is also one of the two identifiers the archive bucket lists and
    /// `api.weather.gov` will not place, so on most installs it reaches the
    /// user through [`SiteTable::unplaced`] and has no row for this to be
    /// called on at all.
    pub fn is_operational(&self) -> bool {
        self.name != "KCRI"
    }
}

/// Whether an identifier names a Terminal Doppler Weather Radar.
///
/// The `T` prefix identifies the TDWRs — 45 of them when the network was last
/// counted — with one exception a naive `starts_with('T')` gets wrong: `TJUA`
/// is San Juan's WSR-88D.
///
/// A free function over a bare `&str` rather than only a method on
/// [`RadarSite`], because a radar this process knows of and cannot place has
/// no row to ask — see [`SiteTable::unplaced`] — and the site list has to mark
/// it correctly all the same. One spelling, so the placed and unplaced halves
/// of that list cannot come to disagree about `TJUA`.
pub fn is_tdwr_id(site: &str) -> bool {
    site.starts_with('T') && site != "TJUA"
}

/// The radar site closest to `lat`/`lon`, with its distance in kilometres.
///
/// Considers every site including TDWRs. Callers picking a site to *display*
/// almost certainly want [`nearest_wsr88d_site`] instead.
///
/// `None` for a non-finite input — a NaN coordinate would otherwise compare
/// `false` against every candidate and silently yield whichever site happens
/// to sit first in [`radars()`], which reads as a deliberate choice — and
/// `None` when this process can place no radars at all, which is where every
/// fresh install starts.
///
/// No distance cap: a caller in Europe gets the nearest NEXRAD and a very large
/// number, and it is the caller's business whether that is useful. Callers that
/// care should test the returned distance rather than expect `None`.
pub fn nearest_radar_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    table().nearest(lat, lon)
}

/// The closest site an automatic pick should open on, with its distance in km.
///
/// This is the one startup site selection wants: the nearest operational
/// WSR-88D. Downtown Oklahoma City illustrates both filters at once — the
/// literal nearest site is the TDWR `TOKC`, and the nearest WSR-88D is the ROC
/// test bed `KCRI`, which scans to whatever schedule the ROC is testing that
/// day. The site a person there actually wants is the third one out, `KTLX`.
///
/// The TDWR filter is about what an *unattended* pick should open on, not
/// about what the archive holds — it holds TDWR volumes under the same prefix
/// as any other site's. The reason is the instrument (see
/// [`RadarSite::is_tdwr`]): ~89 km of Doppler range around one airport,
/// single-pol so no dual-pol moment and nothing derived from one, and none of
/// the Level III codes this app fetches. Opening there by default would
/// quietly narrow what a viewer can see without their having asked for a
/// terminal radar. Picking one by hand is unaffected, and
/// [`nearest_radar_site`] is the unfiltered form.
///
/// `None` until this process knows where at least one WSR-88D is. A caller
/// that wants to open on *something* has to wait for that rather than fall
/// back to a compiled-in guess, because there is no longer one to fall back
/// to — see [`SiteTable`].
pub fn nearest_wsr88d_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    table().nearest_wsr88d(lat, lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // Fixtures
    //
    // Every table below is built here, from named fixes, and nothing reads
    // the process-wide table. That is forced rather than stylistic: this
    // module used to interrogate a compiled-in `SEED` through the free
    // functions, and with the seed deleted those answer `None` in a test
    // binary that resolves nothing — so a test written that way would pass
    // because there was nothing to find. Building the table the assertion is
    // about is what keeps each one able to fail.
    //
    // The coordinates are deliberately synthetic and far from the real
    // network. A fixture that used real ICAOs at real positions would be the
    // deleted table growing back one row per test.
    // ---------------------------------------------------------------------

    /// A WSR-88D-shaped volume report: two separately-stated heights.
    fn learned(lat_udeg: i32, lon_udeg: i32, site_m: i32, tower_m: i32) -> SiteFix {
        SiteFix::Learned(SitePosition {
            lat_udeg,
            lon_udeg,
            site_height_m: site_m,
            tower_height_m: tower_m,
        })
    }

    /// A TDWR-shaped volume report: `tower_height` byte-identical to
    /// `site_height`, which is how the two instruments are told apart.
    fn learned_single_height(lat_udeg: i32, lon_udeg: i32, height_m: i32) -> SiteFix {
        learned(lat_udeg, lon_udeg, height_m, height_m)
    }

    /// A published station record: a position and one feedhorn elevation.
    fn network(lat_udeg: i32, lon_udeg: i32, feedhorn_m: i32) -> SiteFix {
        SiteFix::Network {
            lat_udeg,
            lon_udeg,
            feedhorn_m,
        }
    }

    // -- distance ---------------------------------------------------------

    /// Two points ~111 km apart along a meridian, where the expected answer is
    /// a definition rather than a measurement.
    #[test]
    fn one_degree_of_latitude_is_about_111_km() {
        let d = distance_km(35.0, -97.0, 36.0, -97.0);
        assert!((d - 111.19).abs() < 0.5, "{d}");
    }

    #[test]
    fn a_point_is_zero_km_from_itself() {
        assert_eq!(distance_km(35.3331, -97.2778, 35.3331, -97.2778), 0.0);
    }

    /// Longitude wrapping is the case an unsigned subtraction gets wrong: these
    /// are 2° apart across the antimeridian, not 358°.
    #[test]
    fn distance_wraps_across_the_antimeridian() {
        let d = distance_km(51.0, 179.0, 51.0, -179.0);
        assert!(d < 200.0, "{d} km — the meridian wrap was not handled");
    }

    // -- the deletion itself ----------------------------------------------

    /// **The binary carries no radars.**
    ///
    /// This is the whole change, stated once. A process that has resolved
    /// nothing can place nothing, name nothing and find nothing near
    /// anywhere — because the only things that could tell it a radar exists
    /// are outside the binary.
    ///
    /// Every clause fails the moment a compiled-in table comes back, whatever
    /// it is called: a base table with rows in it cannot be empty, cannot fail
    /// to name its own rows, and cannot answer `None` to a nearest-search over
    /// the middle of Oklahoma.
    #[test]
    fn a_process_that_has_resolved_nothing_knows_no_radars() {
        let base = empty_table();
        assert!(base.rows().is_empty(), "{} rows", base.rows().len());
        assert!(base.unplaced().is_empty());
        assert!(base.get("KTLX").is_none(), "no identifier is compiled in");
        assert!(!base.knows("KTLX"), "not even as a member");
        assert!(base.nearest(35.3331, -97.2778).is_none());
        assert!(base.nearest_wsr88d(35.3331, -97.2778).is_none());

        // And through the constructor the application calls, not only through
        // the private constant behind it.
        let built = build_table(std::iter::empty());
        assert!(built.rows().is_empty());
        assert!(
            std::ptr::eq(built, base),
            "building from nothing must not allocate a table",
        );
    }

    /// A nearest-search over an empty table is `None`, not a panic and not a
    /// nearest-of-nothing.
    ///
    /// Separated from the test above because this is the property every
    /// *consumer* depends on — `radar_height_ft_near` returning `None` here is
    /// what stops a render anchoring itself at sea level, and startup site
    /// selection reading `None` is what stops it opening on a radar it cannot
    /// fetch.
    #[test]
    fn an_empty_table_answers_no_search_rather_than_answering_wrongly() {
        for (lat, lon) in [(35.0, -97.0), (0.0, 0.0), (90.0, 180.0)] {
            assert!(empty_table().nearest(lat, lon).is_none(), "{lat},{lon}");
        }
    }

    // -- rows arrive from outside -----------------------------------------

    /// An identifier nothing knew of becomes a real row: named, placed and
    /// elevated.
    ///
    /// The refactor's reason to exist, and now the *only* way a row is ever
    /// produced. Before it, an identifier the compiled-in array did not list
    /// could not be named at all — `applied_to` fell through to
    /// `UNKNOWN_SITE_NAME`, the map drew no marker and the site list had no
    /// row — and a binary could only ever know the network as it stood on the
    /// day it was built.
    #[test]
    fn a_radar_nothing_knew_of_becomes_a_real_row() {
        let table = build_table([("ZZZA", learned(-30_000_000, -140_000_000, 100, 20))]);

        let row = table.get("ZZZA").expect("findable by name");
        assert_eq!(row.name, "ZZZA", "it carries its own ICAO, not UNKNOWN");
        assert_eq!((row.lat, row.lon), (-30.0, -140.0), "at its own position");
        assert!(
            row.heights.is_some(),
            "a row with no elevation anchors a section at sea level",
        );
        assert_eq!(
            row.height_ft(Datum::Feedhorn),
            Some(394),
            "120 m of ground and tower, in feet",
        );

        assert_eq!(table.rows().len(), 1, "and it is the only row");
        assert!(table.rows().iter().any(|r| r.name == "ZZZA"));
    }

    /// The added radar answers a nearest-search, which is the route startup
    /// site selection and `radar_height_ft_near` both take.
    ///
    /// Being in the name map is not enough: a row nothing can *find* is a row
    /// the user cannot be placed on.
    #[test]
    fn an_added_radar_answers_a_nearest_search() {
        let table = build_table([
            ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
            ("ZZZB", learned(-10_000_000, -140_000_000, 100, 20)),
        ]);

        let (found, dist) = table.nearest(-30.0, -140.0).expect("a finite coordinate");
        assert_eq!(found.name, "ZZZA", "at {dist} km");
        assert!(dist < 0.001, "it is its own nearest neighbour: {dist} km");

        // And through the WSR-88D filter, which is what an automatic pick
        // uses. Neither identifier starts with `T`, so neither is a TDWR.
        let (found, _) = table
            .nearest_wsr88d(-30.0, -140.0)
            .expect("a finite coordinate");
        assert_eq!(found.name, "ZZZA");

        // The other row is 2200 km away and must not answer for it.
        let (other, _) = table.nearest(-10.0, -140.0).expect("a finite coordinate");
        assert_eq!(
            other.name, "ZZZB",
            "adding a radar must not move the others"
        );
    }

    /// Every site is its own nearest neighbour, which catches a transposed
    /// latitude and longitude anywhere on the path that builds a row.
    ///
    /// It used to guard a hand-maintained table of literals. The literals are
    /// gone and the hazard is not: `degrees_from_micro` is applied to two
    /// fields in three places, and swapping them in any one of them would put
    /// every radar somewhere else while every other assertion still passed.
    ///
    /// The fixture is deliberately asymmetric — latitude and longitude differ
    /// in magnitude *and* sign for every row — because a transposition between
    /// two similar numbers is invisible.
    #[test]
    fn every_site_is_its_own_nearest_neighbour() {
        let table = build_table([
            ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
            ("ZZZB", learned(12_000_000, -80_000_000, 30, 10)),
            ("TZZC", network(-5_000_000, 60_000_000, 400)),
            ("ZZZD", learned_single_height(48_000_000, 9_000_000, 500)),
        ]);
        assert_eq!(table.rows().len(), 4, "precondition: four distinct rows");

        for site in table.rows() {
            let (found, dist) = table
                .nearest(site.lat, site.lon)
                .expect("a built row's coordinates are finite");
            assert_eq!(
                found.name, site.name,
                "{} resolved to {} at {} km",
                site.name, found.name, dist
            );
        }
    }

    /// A NaN must not silently degrade to "the first row in the table".
    ///
    /// Built over a populated table on purpose: over an empty one every
    /// assertion here would hold for the wrong reason, and the test would have
    /// stopped being able to fail the moment the seed was deleted.
    #[test]
    fn a_non_finite_coordinate_has_no_nearest_site() {
        let table = build_table([("ZZZA", learned(-30_000_000, -140_000_000, 100, 20))]);
        assert!(
            table.nearest_wsr88d(-30.0, -140.0).is_some(),
            "precondition: this table can answer a finite question",
        );

        assert!(table.nearest_wsr88d(f64::NAN, -97.0).is_none());
        assert!(table.nearest_wsr88d(35.0, f64::INFINITY).is_none());
        assert!(table.nearest(f64::NEG_INFINITY, -97.0).is_none());
    }

    // -- the two filters an automatic pick applies -------------------------

    /// The nearest *usable* site is not the nearest site, and both filters are
    /// doing work.
    ///
    /// This used to be spelled as downtown Oklahoma City resolving past `TOKC`
    /// and `KCRI` to `KTLX`, which asserted the filters and the seed's
    /// coordinates at once and could only be written while the coordinates
    /// were compiled in. The property is the same and it is about the rules:
    /// nearest of all is a terminal radar — 89 km of single-pol Doppler around
    /// one airport, not an absence of archive data — nearest WSR-88D is the
    /// ROC test bed that scans intermittently, and the answer an unattended
    /// pick wants is the third one out. See [`nearest_wsr88d_site`].
    #[test]
    fn an_automatic_pick_skips_the_tdwr_and_the_test_bed() {
        let table = build_table([
            (
                "TZZA",
                learned_single_height(-30_000_000, -140_000_000, 100),
            ),
            ("KCRI", learned(-30_100_000, -140_000_000, 100, 20)),
            ("ZZZC", learned(-30_200_000, -140_000_000, 100, 20)),
        ]);

        let (nearest, _) = table.nearest(-30.0, -140.0).expect("a finite coordinate");
        assert_eq!(nearest.name, "TZZA", "the literal nearest site is a TDWR");

        let (nearest_88d, _) = table
            .nearest_where(-30.0, -140.0, RadarSite::is_wsr88d)
            .expect("a finite coordinate");
        assert_eq!(
            nearest_88d.name, "KCRI",
            "the nearest WSR-88D is the ROC test bed",
        );

        let (pick, _) = table
            .nearest_wsr88d(-30.0, -140.0)
            .expect("a finite coordinate");
        assert_eq!(pick.name, "ZZZC", "and the pick is the one past both");
    }

    /// `TJUA` is San Juan's WSR-88D, not a TDWR, and a `starts_with('T')` test
    /// would wrongly exclude the only Level II site serving Puerto Rico.
    ///
    /// The exception lives in the rule rather than in a row, so it outlived
    /// the table it was written against and is checked here on a row built
    /// from a fix like any other.
    #[test]
    fn tjua_is_not_treated_as_a_tdwr() {
        let table = build_table([
            ("TJUA", network(18_474_000, -66_180_000, 867)),
            ("TZZA", network(-30_000_000, -140_000_000, 100)),
        ]);

        let tjua = table.get("TJUA").expect("built from its fix");
        assert!(tjua.is_wsr88d());
        assert!(!tjua.is_tdwr());

        let tdwr = table.get("TZZA").expect("built from its fix");
        assert!(tdwr.is_tdwr(), "every other T is one");
    }

    // -- elevations: the hole that must stay closed ------------------------

    /// **Every row any source can build records an elevation.**
    ///
    /// The successor to `every_site_records_an_elevation`, which walked the
    /// compiled-in table. Six of its rows once recorded none — KDGX, KFSX,
    /// KLWX, KRTX, KSRX, KVWX, all `-99999` sentinels turned into `None` by an
    /// `Option<i32>` refactor and never filled in — and a row without one is
    /// not inert: it is the datum a cross-section's height axis is anchored
    /// on, and the old lookup answered **sea level** for it, which reads as a
    /// measurement rather than as a gap. 292 ft of it at KLWX.
    ///
    /// With the table deleted the hazard moved rather than went away: rows are
    /// built by `SiteFix::applied`, so this walks a table built from every
    /// shape a fix can take — a WSR-88D volume, a TDWR volume, a station
    /// record onto nothing, and a station record onto a row that already had
    /// heights.
    #[test]
    fn every_placed_row_records_an_elevation() {
        let base = build_table([("ZZZD", learned(1_000_000, 1_000_000, 300, 25))]);
        let table = extended(
            base,
            [
                ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
                ("ZZZB", learned_single_height(-10_000_000, -140_000_000, 60)),
                ("ZZZC", network(-20_000_000, -140_000_000, 400)),
                // Onto the row the base already carries.
                ("ZZZD", network(1_500_000, 1_000_000, 400)),
            ],
        )
        .expect("these fixes change the table");
        assert_eq!(table.rows().len(), 4, "precondition: all four shapes built");

        let missing: Vec<&str> = table
            .rows()
            .iter()
            .filter(|s| s.heights.is_none())
            .map(|s| s.name)
            .collect();
        assert!(
            missing.is_empty(),
            "these rows record no elevation and would anchor a section at sea \
             level: {missing:?}",
        );

        // Recording *an* elevation is not enough: it has to be the one every
        // render path asks for. A table where every row carried only a base
        // would satisfy the assertion above and answer 0 ft to every feedhorn
        // lookup — the same sea-level hole in a new shape.
        let no_feedhorn: Vec<&str> = table
            .rows()
            .iter()
            .filter(|s| s.height_ft(Datum::Feedhorn).is_none())
            .map(|s| s.name)
            .collect();
        assert!(
            no_feedhorn.is_empty(),
            "no feedhorn height: {no_feedhorn:?}"
        );
    }

    /// Only a volume can answer [`Datum::SiteBase`], and it answers only for a
    /// WSR-88D.
    ///
    /// The successor to `only_the_single_height_rows_lack_a_base`, which named
    /// the 46 shipped rows that could not answer. The set is no longer a list
    /// of radars, it is a rule about sources: a Volume Data Block reports the
    /// ground and the tower as two fields, a published station record reports
    /// one elevation with nothing to split it by, and a TDWR volume reports
    /// the same number twice — so the base is *unknown* for both of the
    /// latter, not equal to the feedhorn.
    ///
    /// This is what the seed's deletion did to the datum, made explicit: what
    /// used to be answerable for 161 shipped rows is now answerable only where
    /// this install has decoded that radar's own WSR-88D volume.
    #[test]
    fn only_a_volume_can_answer_the_base_datum() {
        let table = build_table([
            ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
            ("ZZZB", learned_single_height(-10_000_000, -140_000_000, 60)),
            ("ZZZC", network(-20_000_000, -140_000_000, 400)),
        ]);

        let wsr88d = table.get("ZZZA").expect("a learned two-field row");
        assert_eq!(wsr88d.height_ft(Datum::SiteBase), Some(328), "100 m");
        assert_eq!(wsr88d.height_ft(Datum::Feedhorn), Some(394), "120 m");

        for (name, why) in [
            ("ZZZB", "a TDWR volume states one height twice"),
            ("ZZZC", "a station record states one height"),
        ] {
            let row = table.get(name).expect("built from its fix");
            assert_eq!(row.height_ft(Datum::SiteBase), None, "{name}: {why}");
            assert!(
                row.height_ft(Datum::Feedhorn).is_some(),
                "{name}: and the feedhorn is still answerable",
            );
        }
    }

    /// The two datums are a tower apart wherever both are recorded.
    ///
    /// This is the property the old single `elev` could not express and the
    /// reason a consumer has to name one: had the gap been a foot or two,
    /// nothing about [`Datum`] would matter. The five standard towers are 48,
    /// 65, 81, 97 and 114 ft, and the archive states them in truncated whole
    /// metres, so the gap a row records is those figures to within 3 ft.
    #[test]
    fn the_two_datums_are_a_tower_apart() {
        for (tower_m, want_ft) in [(14, 46), (19, 62), (24, 79), (29, 95), (34, 112)] {
            let table = build_table([("ZZZA", learned(-30_000_000, -140_000_000, 100, tower_m))]);
            let row = table.get("ZZZA").expect("a learned row");
            let gap = row.height_ft(Datum::Feedhorn).expect("feedhorn")
                - row.height_ft(Datum::SiteBase).expect("base");
            assert_eq!(gap, want_ft, "{tower_m} m of tower");
            assert!(
                (30..=115).contains(&gap),
                "{gap} ft is not a tower — the datums have collapsed",
            );
        }
    }

    // -- precedence and identity -------------------------------------------

    /// A row already in the table takes the fix that lands on it, **in place**.
    ///
    /// Two rows of one name is the `TOK2`/`DOP1` failure — two entries at one
    /// position, where which of them a nearest-search answers with is
    /// arbitrary — and it is the reason those second-stream identifiers were
    /// deliberately never given rows.
    #[test]
    fn an_existing_row_takes_the_fix_that_lands_on_it() {
        let base = build_table([("ZZZA", learned(-30_000_000, -140_000_000, 100, 20))]);
        let before = base.get("ZZZA").expect("a row").clone();

        let table = extended(base, [("ZZZA", learned(100_000, 100_000, 1, 1))])
            .expect("the position moved, so the table is rebuilt");

        let after = table.get("ZZZA").expect("still there");
        assert_eq!((after.lat, after.lon), (0.1, 0.1), "it moved");
        assert_ne!((after.lat, after.lon), (before.lat, before.lon));
        assert_eq!(table.rows().len(), 1, "nothing was appended beside it");
        assert_eq!(
            table.rows().iter().filter(|r| r.name == "ZZZA").count(),
            1,
            "one row per radar, whatever moved it",
        );
    }

    /// A fix that agrees with the row it lands on builds nothing.
    ///
    /// The cheap half of the 1:1 rule and the reason `RadarSite` is
    /// `PartialEq`: Android resolves twice from the same cached catalogue, and
    /// a rebuild there would leak a whole table — every row of it — for a
    /// catalogue that had not changed since the last call.
    #[test]
    fn a_fix_that_changes_nothing_reuses_the_table() {
        let base = build_table([("ZZZA", learned(-30_000_000, -140_000_000, 100, 20))]);
        let identical = SiteFix::Network {
            lat_udeg: -30_000_000,
            lon_udeg: -140_000_000,
            // Whatever the row already records wins, so this is ignored.
            feedhorn_m: 1,
        };

        assert!(
            extended(base, [("ZZZA", identical)]).is_none(),
            "a fix that reproduces the row it lands on must not build a table",
        );
    }

    /// A station record can move a row without taking its base datum.
    ///
    /// A `SiteFix::Network` carries one feedhorn metre where a learned row
    /// carries a base and a tower. Overwriting would make every
    /// [`Datum::SiteBase`] query about that radar answer `None` the first time
    /// a catalogue landed — a silent loss with no failing call.
    #[test]
    fn a_network_fix_moves_a_row_and_leaves_its_heights_alone() {
        let base = build_table([("ZZZA", learned(1_000_000, 1_000_000, 100, 20))]);
        let learned_heights = base.get("ZZZA").expect("a row").heights;

        let table = extended(base, [("ZZZA", network(-30_000_000, -140_000_000, 1))])
            .expect("the position moved");

        let after = table.get("ZZZA").expect("still there");
        assert_eq!(
            (after.lat, after.lon),
            (-30.0, -140.0),
            "the position moved"
        );
        assert_eq!(after.heights, learned_heights, "the heights did not");
        assert_eq!(after.height_ft(Datum::SiteBase), Some(328));
    }

    /// Two entries for one radar file one row, not two radars of the same name.
    #[test]
    fn a_repeated_identifier_files_one_row() {
        let fix = learned(-30_000_000, -140_000_000, 100, 20);
        let table = build_table([("ZZZA", fix), ("ZZZA", fix)]);
        assert_eq!(table.rows().iter().filter(|r| r.name == "ZZZA").count(), 1);
    }

    /// Resolving nothing over nothing reuses the table in hand rather than
    /// leaking a copy of it.
    #[test]
    fn a_resolution_that_learns_nothing_is_the_same_table() {
        let once = build_table(std::iter::empty());
        assert!(
            std::ptr::eq(once, empty_table()),
            "an empty overlay must not leak a second table",
        );
    }

    // -- radars that exist and cannot be placed ----------------------------

    /// A radar the catalogue lists and cannot place is a **member without a
    /// row**: no marker, and no disappearance either.
    ///
    /// `TPBI` and `KCRI` are the real cases — both have Level II data in the
    /// archive bucket and both 404 from `api.weather.gov/radar/stations` — and
    /// while the compiled-in table existed they were placed by it. Without it
    /// there are only two honest options, and drawing them at (0, 0) is not
    /// one: a marker in the Gulf of Guinea has exactly the confidence of a
    /// real one.
    ///
    /// So they are listed, selectable, and absent from every question that
    /// needs a position.
    #[test]
    fn a_member_with_no_position_is_listed_and_is_not_a_row() {
        let table = build_table([
            ("TPBI", SiteFix::Unplaced),
            ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
        ]);

        assert_eq!(table.unplaced(), ["TPBI"], "it is a member");
        assert!(table.knows("TPBI"), "and the site list can find it");

        assert!(table.get("TPBI").is_none(), "with no position to hand out");
        assert!(
            table.rows().iter().all(|r| r.name != "TPBI"),
            "and no row, so nothing draws it",
        );

        // The specific wrong answer this exists to prevent.
        let (found, _) = table.nearest(0.0, 0.0).expect("a finite coordinate");
        assert_eq!(
            found.name, "ZZZA",
            "a member with no position must never answer a search at Null Island",
        );
    }

    /// A member that gains a position becomes a row and stops being a member.
    ///
    /// The two lists are disjoint, and this is the transition that would break
    /// it: opening `TPBI` fetches its data by identifier, the volume states
    /// where it is, and the next resolution places it. A site list that walks
    /// rows and members would otherwise show it twice.
    #[test]
    fn a_member_that_gains_a_position_leaves_the_member_list() {
        let base = build_table([("TPBI", SiteFix::Unplaced)]);
        assert_eq!(base.unplaced(), ["TPBI"], "precondition");
        assert!(base.rows().is_empty(), "precondition");

        let table = extended(base, [("TPBI", learned(26_688_000, -80_273_000, 5, 5))])
            .expect("a member gaining a position changes the table");

        assert!(table.unplaced().is_empty(), "no longer merely a member");
        assert_eq!(table.rows().len(), 1, "exactly one row, not two");
        let row = table.get("TPBI").expect("now placed");
        assert_eq!(row.name, "TPBI");
        assert!(row.height_ft(Datum::Feedhorn).is_some());
        assert!(table.knows("TPBI"), "and still known, by the other route");
    }

    /// Bare membership never hides a position that arrived in the same stream.
    ///
    /// The catalogue emits both kinds from one `fixes()` and the frontend
    /// chains its learned cache onto it, so the two claims about one radar
    /// reach `extended` in whatever order the iterators happen to yield.
    /// [`SiteFixRank`] is what makes that order irrelevant, and this asserts it
    /// **both ways round** — the arm that only works in one order is the bug
    /// this catches.
    #[test]
    fn a_placed_fix_beats_bare_membership_in_either_order() {
        let placed = learned(-30_000_000, -140_000_000, 100, 20);
        for (case, fixes) in [
            (
                "membership first",
                [("ZZZA", SiteFix::Unplaced), ("ZZZA", placed)],
            ),
            (
                "membership last",
                [("ZZZA", placed), ("ZZZA", SiteFix::Unplaced)],
            ),
        ] {
            let table = build_table(fixes);
            assert!(table.unplaced().is_empty(), "{case}");
            let row = table
                .get("ZZZA")
                .unwrap_or_else(|| panic!("{case}: the position must win"));
            assert_eq!((row.lat, row.lon), (-30.0, -140.0), "{case}");
        }
    }

    /// The ranks, as an ordering, before anything is built on them.
    ///
    /// Trivial and worth having: `extended` picks the strongest fix per radar
    /// by comparing these, so a variant reordering would silently invert the
    /// ladder while every structural test above still passed.
    #[test]
    fn the_ranks_run_learned_then_network_then_unplaced() {
        assert!(SiteFixRank::Learned < SiteFixRank::Network);
        assert!(SiteFixRank::Network < SiteFixRank::Unplaced);
        assert_eq!(learned(0, 0, 1, 2).rank(), SiteFixRank::Learned,);
        assert_eq!(network(0, 0, 1).rank(), SiteFixRank::Network);
        assert_eq!(SiteFix::Unplaced.rank(), SiteFixRank::Unplaced);
    }

    /// Re-reading the same catalogue does not leak a fresh copy of a member's
    /// name every launch.
    ///
    /// A member's identifier is leaked exactly as a row's is, and the reuse
    /// path for a row — the `PartialEq` comparison — has no counterpart for a
    /// bare name. Without the lookup in `extended` this would allocate on
    /// every resolution, forever, on the one platform that resolves twice per
    /// launch.
    #[test]
    fn re_listing_the_same_member_builds_nothing() {
        let base = build_table([("TPBI", SiteFix::Unplaced)]);
        assert!(
            extended(base, [("TPBI", SiteFix::Unplaced)]).is_none(),
            "a member that was already a member is not a change",
        );
    }
}

/// The radars this crate's tests render against.
///
/// # Why a fixture and not the shipped data
///
/// Because there is no shipped data. Deleting `SEED` did not only remove a
/// list — it removed the reason a test binary had any radars at all, and a
/// geometry test that renders a cross-section over `KTLX` still needs *some*
/// site at *some* position with *some* elevation to render against.
///
/// So the tests that need one say so, by calling [`install`]. The alternative
/// — letting them read whatever the process happened to resolve — is how a
/// test stops being able to fail: with nothing resolved, `radars()` is empty,
/// `radar_height_ft_near` answers `None`, and an assertion about a height
/// becomes an assertion about zero that passes for the wrong reason.
///
/// # Why these figures are real
///
/// Every row here is a real site at the position and heights its own Level II
/// volume reported, because the tests that read them assert *measured*
/// numbers: `KLWX`'s 292 ft is the ground a cross-section was once anchored
/// 89 m under, and `RKSG`'s 1440 ft is the Camp Humphreys figure that replaced
/// Osan's. A synthetic elevation would turn those into assertions about
/// arithmetic.
///
/// Nine rows, not two hundred. That is the difference between a fixture and a
/// table: this cannot rot into a wrong answer for a user, because it is
/// `#[cfg(test)]` and no build ships it, and it cannot silently go stale
/// either — a figure here is only ever compared against another figure in the
/// same test.
#[cfg(test)]
pub(crate) mod fixture {
    use super::{SiteFix, SiteTable};
    use crate::site_position::SitePosition;
    use std::sync::Once;

    /// `(ICAO, latitude, longitude, site_height_m, tower_height_m)`.
    ///
    /// Metres because that is what a Volume Data Block reports and what
    /// [`SiteFix::Learned`] carries; the feet every assertion is written in
    /// come back out through the same conversion production uses, so a change
    /// to that conversion fails these tests rather than sliding past them.
    ///
    /// `TOKC` is a TDWR and states one height twice, which is what makes the
    /// set contain a row that cannot answer `Datum::SiteBase` — the shape
    /// `a_row_that_cannot_answer_never_reports_sea_level` exists for.
    const SITES: [(&str, i32, i32, i32, i32); 9] = [
        ("KTLX", 35_333_060, -97_277_500, 370, 19),
        ("TOKC", 35_276_000, -97_510_000, 386, 386),
        ("RKSG", 37_207_570, 127_285_560, 439, 24),
        ("KDGX", 32_280_000, -89_984_000, 151, 34),
        ("KFSX", 34_574_000, -111_198_000, 2261, 29),
        ("KRTX", 45_715_000, -122_965_000, 492, 34),
        ("KSRX", 35_290_000, -94_361_000, 200, 24),
        ("KVWX", 38_260_000, -87_724_000, 156, 34),
        ("KLWX", 38_975_000, -77_477_000, 89, 34),
    ];

    /// Resolve the fixture into the process-wide table, once.
    ///
    /// Idempotent and safe to call from every test in parallel: [`resolve`]
    /// takes the write lock across read and build, only ever adds radars, and
    /// builds nothing when the fixes reproduce the rows already there. The
    /// [`Once`] is for tidiness rather than correctness.
    ///
    /// Tests that assert the *absence* of radars must not call this and must
    /// not read the process table at all — they go through `empty_table` and
    /// `build_table`, neither of which consults what was resolved.
    pub(crate) fn install() -> &'static SiteTable {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            super::resolve(SITES.map(|(name, lat_udeg, lon_udeg, site_m, tower_m)| {
                (
                    name,
                    SiteFix::Learned(SitePosition {
                        lat_udeg,
                        lon_udeg,
                        site_height_m: site_m,
                        tower_height_m: tower_m,
                    }),
                )
            }));
        });
        super::table()
    }
}
