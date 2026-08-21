use crate::site_position::SitePosition;
use rustdar_geo::EARTH_RADIUS_KM;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// Which height above mean sea level a caller means.
///
/// A site has two, and they are 30–115 ft apart: the ground the tower stands
/// on, and the feedhorn on top of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datum {
    /// The ground under the tower — `site_height` in a Volume Data Block, and
    /// also what `api.weather.gov/radar/stations` publishes.
    ///
    /// **Not** what a beam height should be added to: [`crate::beam`] measures
    /// above the antenna, so that is [`Datum::Feedhorn`]. Only a TDWR leaves
    /// it unknown, because a TDWR volume states one height twice.
    SiteBase,
    /// The feedhorn — `site_height + tower_height`, the point [`crate::beam`]
    /// measures every height above.
    ///
    /// Measured wherever the row was built from a WSR-88D volume, which
    /// reports the tower. Estimated on a row a published station record
    /// placed — see [`SiteHeights::GroundOnly`] and [`NOMINAL_TOWER_M`].
    Feedhorn,
}

/// The tower height assumed for a WSR-88D that only a published station record
/// has placed, metres.
///
/// `api.weather.gov` quotes the ground and every render path needs the
/// antenna, so a figure has to be assumed; `None` would send
/// [`crate::eet::radar_height_ft_near`] on to the nearest row that can answer.
///
/// Measured two ways, neither re-runnable from this tree — dated observations:
///
/// * 53 sites of a shared corpus, from each site's own Volume Data Block:
///   towers of 14 / 19 / 24 / 29 / 34 m at 5 / 14 / 7 / 12 / 15 sites.
/// * 145 of the network's 159 WSR-88D, as Level III feedhorn MSL minus the
///   station record's ground: 9 / 25 / 29 / 33 / 49 sites on the same five
///   builds, spanning 9.75–34.76 m, with `PABC` and `PAEC` at 9.76 and
///   9.75 m — a sixth, shorter build.
///
/// Both samples put the median on 29 m: a mean absolute error of 5.3 m and a
/// worst case of 19.3 m against the 145-site population. The figure it
/// replaces was the ground itself, low at every site by 27.6 m on average.
///
/// [`SiteFixRank::Learned`] outranks [`SiteFixRank::Network`], so the first
/// volume decoded replaces this. A floor under the answer, not the answer.
pub const NOMINAL_TOWER_M: i32 = 29;

/// What a row knows about its own height, and on which datum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteHeights {
    /// The two heights a WSR-88D's Volume Data Block reports separately: the
    /// ground under the tower, and the tower above that ground.
    ///
    /// `tower_ft` is the archive's own figure converted, and the archive
    /// truncates to whole metres — the five standard towers read back as 14,
    /// 19, 24, 29 and 34 m against published heights of 48, 65, 81, 97 and
    /// 114 ft — so a feedhorn built from it sits up to 3 ft low.
    BaseAndTower { base_ft: i32, tower_ft: i32 },
    /// One height, on the feedhorn, with no separable tower. **A TDWR.**
    ///
    /// Every TDWR volume reports `tower_height` byte-identical to `site_height`
    /// and no WSR-88D volume does — exact across all 205 volumes read, but that
    /// read survives on no branch. Hence no answer for [`Datum::SiteBase`]: the
    /// base is unknown, not equal to this.
    ///
    /// Which datum that single figure is on is **not settled**: one TDWR volume
    /// is available here, `TORD`, whose 226 m matches the station record's
    /// 226.77 m — equally consistent with both TDWR fields carrying the ground.
    FeedhornOnly { feedhorn_ft: i32 },
    /// One height, on the **ground under the tower**, with no tower to add:
    /// what a published station record gives for a WSR-88D, and the only shape
    /// [`SiteFix::Network`] produces for one.
    ///
    /// The datum is measured: `api.weather.gov/radar/stations` and a Volume
    /// Data Block's `site_height` differ by a mean of −0.004 m and an rms of
    /// 0.237 m over the 53 corpus WSR-88D; against the *feedhorn*, −25.7 m mean
    /// and 26.6 m rms. Network-wide: 145 of 159 with no exception.
    ///
    /// Answers [`Datum::SiteBase`] exactly, and [`Datum::Feedhorn`] as
    /// `ground_ft + `[`NOMINAL_TOWER_M`], which is an estimate and says so.
    GroundOnly { ground_ft: i32 },
}

/// Which radar network an identifier names.
///
/// **This classifies an IDENTIFIER, not a compiled list.** The `T` prefix rule
/// below is a function of four bytes; it is deliberately *not* a table of the
/// 45 terminal radars, because a binary that carries a list of the network is
/// a binary whose list is wrong for the rest of its life. The site table
/// itself stays learned — `SiteTable` starts empty and answers `None` until a
/// decoded volume or the fetched catalogue says otherwise — and this enum adds
/// nothing to it. The count "45" describes the network as it stood when it was
/// last counted; it does not describe this build, and no code reads it.
///
/// The identifier rule is an offline approximation of a fact the API states:
/// `api.weather.gov/radar/stations` gives a `stationType` per station, and the
/// prefix is what this crate can answer without it — for every identifier,
/// including the ones no station record places.
/// `the_prefix_rule_agrees_with_the_api_on_every_placed_station` is what keeps
/// the two from drifting apart in silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RadarNetwork {
    /// The WSR-88D network: dual-pol moments, 0.5° radials, and every Level III
    /// code this app fetches.
    Wsr88d,
    /// The Terminal Doppler Weather Radar network: single-pol, 1.0° radials,
    /// and legacy Level III codes this app does not fetch.
    Tdwr,
}

impl RadarNetwork {
    /// The network an ICAO identifier names.
    ///
    /// The `T` prefix identifies the TDWRs, with one exception a naive
    /// `starts_with('T')` gets wrong: `TJUA` is San Juan's WSR-88D. Anything
    /// that is not a terminal identifier — including an empty string and
    /// [`UNKNOWN_SITE_NAME`] — is [`Wsr88d`](Self::Wsr88d), which is exactly
    /// what the tree answered before this enum existed.
    ///
    /// A `const fn` so a `const` site fixture can call it; the rule is spelled
    /// over bytes for that reason alone, and
    /// `of_id_is_the_prefix_rule_it_replaced` holds it byte-for-byte against
    /// the `starts_with('T') && != "TJUA"` expression it was moved from.
    pub const fn of_id(site: &str) -> Self {
        let bytes = site.as_bytes();
        if bytes.is_empty() || bytes[0] != b'T' {
            return Self::Wsr88d;
        }
        if bytes.len() == 4 && bytes[1] == b'J' && bytes[2] == b'U' && bytes[3] == b'A' {
            return Self::Wsr88d;
        }
        Self::Tdwr
    }
}

/// `PartialEq` because [`extended`] compares the row a fix would produce
/// against the row already in the table, and builds nothing when they are
/// equal.
#[derive(Debug, Clone, PartialEq)]
pub struct RadarSite {
    pub name: &'static str,
    /// Which network this radar belongs to.
    ///
    /// Always [`RadarNetwork::of_id`] of [`name`](Self::name) — there is no
    /// other legal source, and `every_row_carries_the_network_its_name_implies`
    /// says so as a test. Equal names therefore imply equal networks, which is
    /// why adding this field leaves the derived `PartialEq` above meaning
    /// exactly what it meant before.
    pub network: RadarNetwork,
    pub lat: f64,
    pub lon: f64,
    /// The heights this row records, or `None` if it records none.
    ///
    /// No row any source can produce is `None`; the `Option` stays because the
    /// field is public. A radar with no height at all has no row
    /// ([`SiteTable::unplaced`]) rather than a row full of zeros.
    pub heights: Option<SiteHeights>,
}

impl RadarSite {
    /// This site's height on `datum`, feet MSL, or `None` if the row does not
    /// record that datum.
    ///
    /// One answer is an estimate: a [`SiteHeights::GroundOnly`] row reaches
    /// [`Datum::Feedhorn`] by adding [`NOMINAL_TOWER_M`]. Others are exact.
    pub fn height_ft(&self, datum: Datum) -> Option<i32> {
        match (self.heights?, datum) {
            (SiteHeights::BaseAndTower { base_ft, .. }, Datum::SiteBase) => Some(base_ft),
            (SiteHeights::BaseAndTower { base_ft, tower_ft }, Datum::Feedhorn) => {
                Some(base_ft + tower_ft)
            }
            (SiteHeights::FeedhornOnly { feedhorn_ft }, Datum::Feedhorn) => Some(feedhorn_ft),
            (SiteHeights::FeedhornOnly { .. }, Datum::SiteBase) => None,
            (SiteHeights::GroundOnly { ground_ft }, Datum::SiteBase) => Some(ground_ft),
            (SiteHeights::GroundOnly { ground_ft }, Datum::Feedhorn) => {
                Some(ground_ft + crate::site_position::feet_from_metres(NOMINAL_TOWER_M))
            }
        }
    }
}

/// The name a [`RadarSite`] carries when nothing in the build knows the
/// identifier. A placeholder for a `&'static str` this crate cannot
/// manufacture from a four-byte ICAO read at runtime, not a claim that the
/// row means anything.
pub const UNKNOWN_SITE_NAME: &str = "UNKNOWN";

/// Every radar this process knows about, resolved once and then extended.
///
/// There is no compiled-in table under this: a fresh process starts with no
/// radars and answers `None` until something outside the binary says
/// otherwise. In order of authority: a volume this install decoded
/// ([`SiteFix::Learned`]), the fetched catalogue ([`SiteFix::Network`]), and
/// bare membership ([`SiteFix::Unplaced`]). An empty table is a real state.
///
/// [`rows`](Self::rows) is every radar this process can *place*; `TPBI` and
/// `KCRI` have Level II data and 404 from `api.weather.gov/radar/stations`, so
/// they are [`unplaced`](Self::unplaced) — the only numbers available would be
/// zeros, a marker in the Gulf of Guinea with the confidence of a real one.
///
/// The rows are leaked on the way in, which is what lets every lookup hand out
/// `&'static RadarSite`. The name index is built with the rows and travels
/// with them, so the two cannot disagree about which radars exist.
pub struct SiteTable {
    rows: &'static [RadarSite],
    by_name: HashMap<&'static str, &'static RadarSite>,
    /// Identifiers that exist but have no position, sorted and deduplicated.
    /// Disjoint from `rows` by construction — see [`extended`].
    unplaced: &'static [&'static str],
    /// Where the fetched catalogue put each radar it placed, micro-degrees.
    ///
    /// A row is the *answer* — whichever source won. This is one source's
    /// claim, the only one a volume cannot have written and so the only one a
    /// volume can honestly be checked against.
    catalogued: HashMap<&'static str, (i32, i32)>,
}

impl SiteTable {
    /// Every radar this table can place, in the order they were learned.
    ///
    /// Callers must not store an enumeration index anywhere that outlives the
    /// walk: the next table can be a different length, and position `n` in it
    /// is a different radar. Keep the `&'static RadarSite` itself.
    pub fn rows(&self) -> &'static [RadarSite] {
        self.rows
    }

    /// Identifiers this table knows exist but cannot place, sorted. Never
    /// overlaps [`rows`](Self::rows).
    pub fn unplaced(&self) -> &'static [&'static str] {
        self.unplaced
    }

    /// Whether this table has heard of `site` at all, placed or not.
    pub fn knows(&self, site: &str) -> bool {
        self.static_name(site).is_some()
    }

    /// This table's own `&'static str` for `site`, placed or not.
    ///
    /// Reaches the **unplaced** members too: `TPBI` is a TDWR with real Level
    /// II data that `api.weather.gov` will not place, and without this
    /// [`is_tdwr_id`] would answer `false` for a terminal radar.
    pub fn static_name(&self, site: &str) -> Option<&'static str> {
        self.by_name.get(site).map(|row| row.name).or_else(|| {
            self.unplaced
                .iter()
                .find(|listed| **listed == site)
                .copied()
        })
    }

    /// The row for an ICAO identifier, or `None` if this table cannot place
    /// that radar.
    pub fn get(&self, site: &str) -> Option<&'static RadarSite> {
        self.by_name.get(site).copied()
    }

    /// Where the **fetched catalogue** put `site`, degrees, or `None` if no
    /// catalogue this process read has placed it. Deliberately not
    /// [`get`](Self::get): this answers what the one source outside the volume
    /// stream says.
    pub fn catalogue_position(&self, site: &str) -> Option<(f64, f64)> {
        self.catalogued.get(site).map(|&(lat_udeg, lon_udeg)| {
            (
                crate::site_position::degrees_from_micro(lat_udeg),
                crate::site_position::degrees_from_micro(lon_udeg),
            )
        })
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
            // `total_cmp`, not `partial_cmp().unwrap()`: the unwrap would be a
            // panic path in a startup routine to save nothing.
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
    }
}

/// How much authority one [`SiteFix`] carries, as a value that can be compared.
///
/// **A smaller variant outranks a larger one**, continuing
/// [`SitePositionSource`](crate::site_position::SitePositionSource):
///
/// ```text
/// Volume    the volume in hand        ScanInfo::from_scan  SitePositionSource
/// Learned   a volume, earlier         SiteFix::Learned     SiteFixRank
/// Network   the fetched catalogue     SiteFix::Network     SiteFixRank
/// Unplaced  the catalogue, positionless  SiteFix::Unplaced SiteFixRank
/// ```
///
/// There is no rung below `Unplaced`.
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
/// Not a bare [`SitePosition`], which is specifically a Volume Data Block's
/// four fields. The network catalogue has one elevation and no separable
/// tower, so it can never fill a [`SiteHeights::BaseAndTower`], and it is a
/// record about the radar rather than a report from it. A bucket listing says
/// a radar exists and nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteFix {
    /// What a volume this install decoded stated about itself. Carries the
    /// Volume Data Block's own fields, so it can speak to both height datums.
    Learned(SitePosition),
    /// What the fetched catalogue says: a position, and **one** elevation, on
    /// the ground under the tower.
    ///
    /// `api.weather.gov/radar/stations` matches a Volume Data Block's
    /// `site_height` to −0.004 m mean, 0.237 m rms over the 53 corpus WSR-88D,
    /// and misses the feedhorn by −25.7 m mean. No tower figure can be derived.
    Network {
        /// Latitude, micro-degrees north.
        lat_udeg: i32,
        /// Longitude, micro-degrees east.
        lon_udeg: i32,
        /// The station record's one elevation, whole metres MSL. The ground for
        /// a WSR-88D; for a TDWR it agrees with the single height its volume
        /// states twice, which [`SiteHeights::FeedhornOnly`] reads as unsettled.
        elevation_m: i32,
    },
    /// The radar exists and nothing here knows where it is: the archive bucket
    /// lists an identifier `api.weather.gov` does not place (`TPBI`, `KCRI`).
    /// A claim with no numbers in it, so it produces membership rather than a
    /// row. Weakest rung on purpose.
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
    /// heights the row it is displacing already recorded, or `None` if this fix
    /// carries no position. `known` is `None` for a radar never heard of.
    ///
    /// Position always moves; heights only improve. **Learned** goes through
    /// [`SitePosition::heights_over`], which keeps `known`'s finer foot figures
    /// wherever the volume's whole metre cannot contradict them. **Network**
    /// keeps `known` untouched whenever there is one and fills in only where
    /// the row had nothing.
    ///
    /// A station record does not say which datum it is on, so the identifier
    /// decides — a WSR-88D's is the ground, a TDWR's the feedhorn — and the two
    /// sources for one radar cannot disagree by arriving in a different order.
    /// [`is_tdwr_id`] is the one spelling of that split.
    ///
    /// `Unplaced` produces no row at all, which is a different thing from a row
    /// with no elevation.
    fn applied(&self, name: &'static str, known: Option<SiteHeights>) -> Option<RadarSite> {
        match *self {
            Self::Learned(position) => Some(position.applied_to_named(name, known)),
            Self::Network {
                lat_udeg,
                lon_udeg,
                elevation_m,
            } => Some(RadarSite {
                name,
                network: RadarNetwork::of_id(name),
                lat: crate::site_position::degrees_from_micro(lat_udeg),
                lon: crate::site_position::degrees_from_micro(lon_udeg),
                heights: known.or(Some({
                    let ft = crate::site_position::feet_from_metres(elevation_m);
                    if is_tdwr_id(name) {
                        SiteHeights::FeedhornOnly { feedhorn_ft: ft }
                    } else {
                        SiteHeights::GroundOnly { ground_ft: ft }
                    }
                })),
            }),
            Self::Unplaced => None,
        }
    }
}

/// An empty table with `fixes` applied: a row for every radar one of them can
/// place, and membership for every radar they can only name.
///
/// The strongest fix per radar wins, and only that one is applied. `fixes` is
/// a flat stream from every source at once, and where two name the same radar
/// [`SiteFixRank`] decides once, before anything is built. An arrival's name
/// is leaked; a radar the base table already has keeps the name it had.
pub fn build_table<'a, I>(fixes: I) -> &'static SiteTable
where
    I: IntoIterator<Item = (&'a str, SiteFix)>,
{
    extended(empty_table(), fixes).unwrap_or_else(empty_table)
}

/// `base` with `fixes` applied, or `None` if applying them would change
/// nothing.
///
/// The comparison is against the produced row and not merely the set of names:
/// Android resolves twice with the same cache, and a name-only check would
/// leak a whole table on the second call.
///
/// `rows` and `unplaced` stay disjoint: a radar that gains a position leaves
/// `unplaced` in the same resolution that gives it a row.
fn extended<'a, I>(base: &'static SiteTable, fixes: I) -> Option<&'static SiteTable>
where
    I: IntoIterator<Item = (&'a str, SiteFix)>,
{
    // The strongest claim per radar, decided before a row is built, so "a
    // fetched position never outranks a learned one" is a property of the input
    // rather than of the order two loops happened to run in.
    let mut best: HashMap<&'a str, SiteFix> = HashMap::new();
    // Every catalogue claim in this resolution, recorded *before* ranking:
    // ranking is lossy on purpose, and the discarded half is what this keeps.
    let mut claimed: Vec<(&'a str, (i32, i32))> = Vec::new();
    for (name, fix) in fixes {
        if let SiteFix::Network {
            lat_udeg, lon_udeg, ..
        } = fix
        {
            claimed.push((name, (lat_udeg, lon_udeg)));
        }
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
            // the catalogue saying it cannot help.
            if let Some(next) = fix.applied(row.name, row.heights)
                && next != *row
            {
                *row = next;
                changed = true;
            }
        }
    }

    // Sorted, so a table resolved from the same inputs is the same table
    // whichever order a `HashMap` felt like iterating in.
    let mut arrivals: Vec<(&'a str, SiteFix)> = best.into_iter().collect();
    arrivals.sort_unstable_by_key(|(name, _)| *name);

    let mut unplaced: Vec<&'static str> = base.unplaced().to_vec();
    for (name, fix) in arrivals {
        // A member already listed keeps the `&'static str` it was leaked
        // under, so re-reading the same catalogue every launch leaks nothing.
        if let Some(known) = unplaced.iter().find(|listed| **listed == name).copied() {
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
    // Keyed by the row's own `&'static str` rather than by the borrowed name,
    // so a catalogue re-read every launch reuses the identifier already leaked.
    let mut catalogued = base.catalogued.clone();
    let placed: HashMap<&str, &'static str> = rows.iter().map(|row| (row.name, row.name)).collect();
    for (name, position) in claimed {
        if let Some(&name) = placed.get(name)
            && catalogued.insert(name, position) != Some(position)
        {
            changed = true;
        }
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
        catalogued,
    })))
}

/// No radars, built once. One shared value rather than a fresh allocation per
/// call, so `std::ptr::eq` against it is a meaningful test of "nothing built".
fn empty_table() -> &'static SiteTable {
    static EMPTY: LazyLock<&'static SiteTable> = LazyLock::new(|| {
        &*Box::leak(Box::new(SiteTable {
            rows: &[],
            by_name: HashMap::new(),
            unplaced: &[],
            catalogued: HashMap::new(),
        }))
    });
    *EMPTY
}

/// The table this process resolved, or `None` until something resolves one.
static RESOLVED: RwLock<Option<&'static SiteTable>> = RwLock::new(None);

/// The table every lookup in this module reads. Falls back to [`empty_table`]
/// rather than panicking when nothing has resolved yet.
pub fn table() -> &'static SiteTable {
    // `into_inner` rather than `expect`: a poisoned lock here means some other
    // thread panicked while swapping a pointer, and the pointer it was
    // swapping is valid either way.
    match *RESOLVED.read().unwrap_or_else(|e| e.into_inner()) {
        Some(table) => table,
        None => empty_table(),
    }
}

/// Resolve the process-wide site table from what this install knows.
///
/// A position or name that arrives *late* moves a marker, a label and a
/// section's height datum under a user already looking at them, so the
/// resolutions that could move an existing row belong with the startup read —
/// `App::new`, and again in `set_config_dir` where Android finally has a store.
/// A `OnceLock` would have let the empty first attempt win on the one platform
/// that needs the second call.
///
/// Two later resolutions run mid-session and neither can move anything a user
/// is looking at: the first catalogue on a fresh install (the table before it
/// is empty), and a volume this session decoded (the map is already drawn from
/// that same position by way of [`crate::types::ScanInfo`]).
///
/// Each call extends the table already in hand, so the process can learn about
/// a radar but never lose one. A fix displaces the row it lands on, so an
/// index into [`radars()`] was never valid across a resolution.
pub fn resolve<'a, I>(fixes: I) -> &'static SiteTable
where
    I: IntoIterator<Item = (&'a str, SiteFix)>,
{
    // The write lock is held across the read *and* the build. Reading,
    // extending and storing as three steps is a lost update: whichever of two
    // concurrent resolutions stores second discards the first.
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

/// Every radar this process can place. Empty until something resolves a table;
/// see [`SiteTable::rows`] on why an index into it must not outlive the walk.
pub fn radars() -> &'static [RadarSite] {
    table().rows()
}

/// Identifiers this process knows exist and cannot place. Never overlaps
/// [`radars()`].
pub fn unplaced() -> &'static [&'static str] {
    table().unplaced()
}

/// Whether this process has heard of `site` at all, placed or not.
pub fn knows_site(site: &str) -> bool {
    table().knows(site)
}

/// This process's own `&'static str` for `site`, placed or not.
pub fn static_name(site: &str) -> Option<&'static str> {
    table().static_name(site)
}

/// The row for an ICAO identifier, or `None` if this process cannot place that
/// radar.
pub fn get_radar_site(site: &str) -> Option<&'static RadarSite> {
    table().get(site)
}

/// Where the fetched catalogue put `site`, degrees, or `None` if none has.
pub fn catalogue_position(site: &str) -> Option<(f64, f64)> {
    table().catalogue_position(site)
}

/// Great-circle distance between two coordinates, in kilometres.
///
/// Haversine rather than the cheaper equirectangular approximation: the caller
/// compares sites up to a continent apart, and the flat approximation's error
/// grows with both separation and latitude. On
/// [`rustdar_geo::EARTH_RADIUS_KM`], the workspace's one sphere.
pub fn distance_km(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    let (lat_a_rad, lat_b_rad) = (lat_a.to_radians(), lat_b.to_radians());
    let d_lat = (lat_b - lat_a).to_radians();
    let d_lon = (lon_b - lon_a).to_radians();

    let h = (d_lat / 2.0).sin().powi(2)
        + lat_a_rad.cos() * lat_b_rad.cos() * (d_lon / 2.0).sin().powi(2);
    // `asin(sqrt(h))` rather than `atan2`: h is clamped below, so the
    // numerically delicate case `atan2` exists to handle cannot arise here.
    2.0 * EARTH_RADIUS_KM * h.clamp(0.0, 1.0).sqrt().asin()
}

impl RadarSite {
    /// Whether this is a Terminal Doppler Weather Radar rather than a WSR-88D.
    ///
    /// Not a difference in availability: both networks land in the same Level
    /// II archive under the same `YYYY/MM/DD/SITE/` prefix, a TDWR's objects
    /// ending `_V08` where a WSR-88D's end `_V06`. What differs is the
    /// instrument: single-pol, so no differential phase or correlation
    /// coefficient; Doppler cuts to ~89 km (592 gates of 150 m, read off
    /// `TPIT`) beside one surveillance cut to ~417 km (1,390 of 300 m); and
    /// radials 1.0° apart rather than 0.5°.
    ///
    /// Its Level III products differ in kind: the legacy single-pol codes
    /// (`TZL`, `TZ0`-`TZ2`, `TV0`-`TV2`, `NCR`, `NHI`, `NMD`), and not one of
    /// the four codes [`crate::level3`] fetches — checked 2026-08-11 by listing
    /// that bucket for `PIT`, `OKC`, `MIA` and `DCA`. See [`is_tdwr_id`].
    pub fn is_tdwr(&self) -> bool {
        matches!(self.network, RadarNetwork::Tdwr)
    }

    /// Whether this site is a WSR-88D — the network with dual-pol moments,
    /// 0.5° radials, and every Level III code this app fetches. The question
    /// to ask before offering anything that needs one.
    pub fn is_wsr88d(&self) -> bool {
        matches!(self.network, RadarNetwork::Wsr88d)
    }

    /// Whether this site runs an operational scan an ordinary viewer can rely on.
    ///
    /// `KCRI` is the Radar Operations Center's test bed in Norman: a real
    /// WSR-88D that scans to whatever schedule the ROC is testing that day, and
    /// 0.4 km closer to downtown Oklahoma City than `KTLX`. Only automatic
    /// selection consults this.
    pub fn is_operational(&self) -> bool {
        self.name != "KCRI"
    }
}

/// Whether an identifier names a Terminal Doppler Weather Radar.
///
/// The rule itself lives on [`RadarNetwork::of_id`]; this is the boolean
/// spelling of it. A free function over a bare `&str`, because a radar this
/// process knows of and cannot place has no row to ask — `TPBI` is the case,
/// and [`SiteTable::static_name`] is how the unplaced members reach here.
pub fn is_tdwr_id(site: &str) -> bool {
    matches!(RadarNetwork::of_id(site), RadarNetwork::Tdwr)
}

/// Whether an identifier is made of the bytes identifier handling assumes:
/// ASCII, and not empty.
///
/// [`crate::level3::site_code`] takes a byte range off the front, [`is_tdwr_id`]
/// reads the leading character, and the Level III bucket prefix interpolates
/// the code straight into an S3 key. Network identifiers are already filtered
/// to four uppercase characters, so the boundary this exists for is the
/// *persisted* one: a hand-editable field of `ui.json`. Deliberately not a
/// length or case rule — `site_code` is idempotent on the three-letter short
/// form and callers uppercase at the point of use.
pub fn is_ascii_site_id(site: &str) -> bool {
    !site.is_empty() && site.is_ascii()
}

/// The radar site closest to `lat`/`lon`, with its distance in kilometres.
///
/// Considers every site including TDWRs; callers picking a site to *display*
/// almost certainly want [`nearest_wsr88d_site`]. `None` for a non-finite
/// input, and `None` when this process can place no radars at all.
pub fn nearest_radar_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    table().nearest(lat, lon)
}

/// The closest site an automatic pick should open on, with its distance in km.
///
/// Downtown Oklahoma City illustrates both filters: the literal nearest site
/// is the TDWR `TOKC`, the nearest WSR-88D is the ROC test bed `KCRI`, and the
/// site a person there actually wants is the third one out, `KTLX`.
pub fn nearest_wsr88d_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    table().nearest_wsr88d(lat, lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Fixtures ----------------------------------------------------------
    //
    // Every table below is built here, from named fixes; nothing reads the
    // process-wide table, which a test binary never resolves. The coordinates
    // are deliberately synthetic and far from the real network.

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

    /// A published station record: a position and one elevation, on the
    /// ground for a WSR-88D identifier and on the feedhorn for a TDWR one.
    fn network(lat_udeg: i32, lon_udeg: i32, elevation_m: i32) -> SiteFix {
        SiteFix::Network {
            lat_udeg,
            lon_udeg,
            elevation_m,
        }
    }

    // -- distance ---------------------------------------------------------

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

    #[test]
    fn a_process_that_has_resolved_nothing_knows_no_radars() {
        let base = empty_table();
        assert!(base.rows().is_empty(), "{} rows", base.rows().len());
        assert!(base.unplaced().is_empty());
        assert!(base.get("KTLX").is_none(), "no identifier is compiled in");
        assert!(!base.knows("KTLX"), "not even as a member");
        assert!(base.nearest(35.3331, -97.2778).is_none());
        assert!(base.nearest_wsr88d(35.3331, -97.2778).is_none());

        let built = build_table(std::iter::empty());
        assert!(built.rows().is_empty());
        assert!(
            std::ptr::eq(built, base),
            "building from nothing must not allocate a table",
        );
    }

    #[test]
    fn an_empty_table_answers_no_search_rather_than_answering_wrongly() {
        for (lat, lon) in [(35.0, -97.0), (0.0, 0.0), (90.0, 180.0)] {
            assert!(empty_table().nearest(lat, lon).is_none(), "{lat},{lon}");
        }
    }

    // -- rows arrive from outside -----------------------------------------

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

    #[test]
    fn an_added_radar_answers_a_nearest_search() {
        let table = build_table([
            ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
            ("ZZZB", learned(-10_000_000, -140_000_000, 100, 20)),
        ]);

        let (found, dist) = table.nearest(-30.0, -140.0).expect("a finite coordinate");
        assert_eq!(found.name, "ZZZA", "at {dist} km");
        assert!(dist < 0.001, "it is its own nearest neighbour: {dist} km");

        let (found, _) = table
            .nearest_wsr88d(-30.0, -140.0)
            .expect("a finite coordinate");
        assert_eq!(found.name, "ZZZA");

        let (other, _) = table.nearest(-10.0, -140.0).expect("a finite coordinate");
        assert_eq!(
            other.name, "ZZZB",
            "adding a radar must not move the others"
        );
    }

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

    #[test]
    fn every_identifier_shape_in_use_clears_the_ascii_floor() {
        for id in [
            "KTLX",
            "PAHG",
            "PHKI",
            "TJUA",
            "PGUA",
            "KCRI",
            "TOKC",
            "TLX",
            "ktlx",
            UNKNOWN_SITE_NAME,
        ] {
            assert!(is_ascii_site_id(id), "{id:?} is in use today");
        }
    }

    #[test]
    fn no_identifier_built_from_non_ascii_bytes_clears_the_floor() {
        for id in ["", "éab", "Ω12", "日a", "🌀", "aéb", "KTLX\u{200b}"] {
            assert!(!is_ascii_site_id(id), "{id:?} is not an identifier");
        }
    }

    // -- elevations: the hole that must stay closed ------------------------

    /// Six rows of the deleted compiled-in table once recorded none — KDGX,
    /// KFSX, KLWX, KRTX, KSRX, KVWX — and the old lookup answered sea level
    /// for them. 292 ft of it at KLWX.
    #[test]
    fn every_placed_row_records_an_elevation() {
        let base = build_table([("ZZZD", learned(1_000_000, 1_000_000, 300, 25))]);
        let table = extended(
            base,
            [
                ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
                ("ZZZB", learned_single_height(-10_000_000, -140_000_000, 60)),
                ("ZZZC", network(-20_000_000, -140_000_000, 400)),
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

    #[test]
    fn only_a_tdwr_row_cannot_answer_the_base_datum() {
        let table = build_table([
            ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
            ("ZZZB", learned_single_height(-10_000_000, -140_000_000, 60)),
            ("ZZZC", network(-20_000_000, -140_000_000, 400)),
            ("TZZZ", network(-40_000_000, -140_000_000, 400)),
        ]);

        let wsr88d = table.get("ZZZA").expect("a learned two-field row");
        assert_eq!(wsr88d.height_ft(Datum::SiteBase), Some(328), "100 m");
        assert_eq!(wsr88d.height_ft(Datum::Feedhorn), Some(394), "120 m");

        let fetched = table.get("ZZZC").expect("built from its fix");
        assert_eq!(
            fetched.height_ft(Datum::SiteBase),
            Some(1312),
            "400 m of ground is what the record states",
        );
        assert_eq!(
            fetched.height_ft(Datum::Feedhorn),
            Some(1312 + 95),
            "and the feedhorn is one nominal 29 m tower above it",
        );

        for (name, why) in [
            ("ZZZB", "a TDWR volume states one height twice"),
            ("TZZZ", "a TDWR station record states that same one height"),
        ] {
            let row = table.get(name).expect("built from its fix");
            assert_eq!(row.height_ft(Datum::SiteBase), None, "{name}: {why}");
            assert!(
                row.height_ft(Datum::Feedhorn).is_some(),
                "{name}: and the feedhorn is still answerable",
            );
        }
    }

    /// The five standard towers are 48, 65, 81, 97 and 114 ft, and the archive
    /// states them in truncated whole metres, so the gap a row records is those
    /// figures to within 3 ft.
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

        let table = build_table([("ZZZA", network(-30_000_000, -140_000_000, 100))]);
        let row = table.get("ZZZA").expect("a fetched row");
        let gap = row.height_ft(Datum::Feedhorn).expect("feedhorn")
            - row.height_ft(Datum::SiteBase).expect("base");
        assert_eq!(gap, 95, "the nominal tower, in feet");
        assert!(
            (30..=115).contains(&gap),
            "{gap} ft is not a tower — the nominal has left the range of real \
             ones and a catalogue-placed site is no longer plausibly placed",
        );
    }

    // -- the network an identifier names -----------------------------------

    /// The rule [`RadarNetwork::of_id`] was moved from, kept here as an
    /// independent oracle: this is the expression `is_tdwr_id` carried before
    /// the enum existed, byte-for-byte.
    fn the_prefix_rule_as_it_was_written(site: &str) -> bool {
        site.starts_with('T') && site != "TJUA"
    }

    #[test]
    fn of_id_is_the_prefix_rule_it_replaced() {
        // Real identifiers, the exception, the near-misses that would fool a
        // length-blind or a prefix-blind spelling, and the degenerate inputs a
        // hand-edited config can produce.
        let ids = [
            "KTLX", "KOUN", "KMPX", "KCRI", "PABC", "TJUA", "TPBI", "TOKC", "TDCA", "TMIA", "T",
            "TJ", "TJU", "TJUAX", "TJUB", "tjua", "tTLX", "UNKNOWN", "", " ", "XTJUA",
        ];
        for id in ids {
            let expected = the_prefix_rule_as_it_was_written(id);
            assert_eq!(
                RadarNetwork::of_id(id) == RadarNetwork::Tdwr,
                expected,
                "of_id disagrees with the rule it was moved from, on {id:?}",
            );
            assert_eq!(is_tdwr_id(id), expected, "the delegate drifted, on {id:?}");
        }
        // The oracle has to separate the two answers, or the loop above is a
        // walk over one constant.
        assert!(ids.iter().any(|id| the_prefix_rule_as_it_was_written(id)));
        assert!(ids.iter().any(|id| !the_prefix_rule_as_it_was_written(id)));
    }

    /// The serde spellings are persisted by [`crate::catalogue`]'s position
    /// cache, so they are a wire format, not an implementation detail.
    #[test]
    fn the_networks_serde_spellings_are_pinned() {
        for (network, spelling) in [
            (RadarNetwork::Wsr88d, "\"Wsr88d\""),
            (RadarNetwork::Tdwr, "\"Tdwr\""),
        ] {
            let json = serde_json::to_string(&network).expect("a unit variant serializes");
            assert_eq!(json, spelling, "the spelling on the wire moved");
            assert_eq!(
                serde_json::from_str::<RadarNetwork>(&json).expect("and reads back"),
                network,
            );
        }
    }

    /// Every row's network is a function of its name and of nothing else —
    /// which is what makes the derived `PartialEq` on [`RadarSite`] mean the
    /// same thing it meant before the field existed.
    #[test]
    fn every_row_carries_the_network_its_name_implies() {
        let table = build_table([
            ("ZZZA", learned(-30_000_000, -140_000_000, 100, 20)),
            ("TZZB", learned_single_height(-10_000_000, -140_000_000, 60)),
            ("ZZZC", network(-20_000_000, -140_000_000, 400)),
            ("TZZD", network(-40_000_000, -140_000_000, 400)),
            ("TJUA", network(18_000_000, -66_000_000, 100)),
            ("TZZE", SiteFix::Unplaced),
        ]);

        for row in table.rows() {
            assert_eq!(
                row.network,
                RadarNetwork::of_id(row.name),
                "{} carries a network its name does not imply",
                row.name,
            );
            assert_eq!(row.is_tdwr(), is_tdwr_id(row.name), "{}", row.name);
            assert_eq!(row.is_wsr88d(), !row.is_tdwr(), "{}", row.name);
        }

        // Both answers appear, so the walk above cannot pass by agreeing with
        // one constant. `TJUA` is here because it is the one identifier that
        // makes the two spellings of the rule differ.
        let networks: Vec<RadarNetwork> = table.rows().iter().map(|r| r.network).collect();
        assert!(networks.contains(&RadarNetwork::Wsr88d), "{networks:?}");
        assert!(networks.contains(&RadarNetwork::Tdwr), "{networks:?}");
        assert_eq!(
            table.get("TJUA").expect("a placed row").network,
            RadarNetwork::Wsr88d,
            "the exception is the whole reason the rule is not `starts_with`",
        );
    }

    // -- precedence and identity -------------------------------------------

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

    #[test]
    fn a_fix_that_changes_nothing_reuses_the_table() {
        let base = build_table([("ZZZA", learned(-30_000_000, -140_000_000, 100, 20))]);

        assert!(
            extended(
                base,
                [("ZZZA", learned(-30_000_000, -140_000_000, 100, 20))]
            )
            .is_none(),
            "a fix that reproduces the row it lands on must not build a table",
        );

        let identical = SiteFix::Network {
            lat_udeg: -30_000_000,
            lon_udeg: -140_000_000,
            elevation_m: 1,
        };
        let catalogued = extended(base, [("ZZZA", identical)])
            .expect("the first catalogue claim is something the table did not have");
        assert_eq!(
            catalogued.get("ZZZA"),
            base.get("ZZZA"),
            "and it is not a row change: the row is what it was",
        );
        assert_eq!(
            catalogued.catalogue_position("ZZZA"),
            Some((-30.0, -140.0)),
            "what it added is the claim itself",
        );
        assert_eq!(
            base.catalogue_position("ZZZA"),
            None,
            "which the table it was built from does not have",
        );

        assert!(
            extended(catalogued, [("ZZZA", identical)]).is_none(),
            "and the same catalogue a second time — Android's case — builds nothing",
        );
    }

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

    #[test]
    fn a_repeated_identifier_files_one_row() {
        let fix = learned(-30_000_000, -140_000_000, 100, 20);
        let table = build_table([("ZZZA", fix), ("ZZZA", fix)]);
        assert_eq!(table.rows().iter().filter(|r| r.name == "ZZZA").count(), 1);
    }

    #[test]
    fn a_resolution_that_learns_nothing_is_the_same_table() {
        let once = build_table(std::iter::empty());
        assert!(
            std::ptr::eq(once, empty_table()),
            "an empty overlay must not leak a second table",
        );
    }

    // -- radars that exist and cannot be placed ----------------------------

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

        let (found, _) = table.nearest(0.0, 0.0).expect("a finite coordinate");
        assert_eq!(
            found.name, "ZZZA",
            "a member with no position must never answer a search at Null Island",
        );
    }

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

    #[test]
    fn the_ranks_run_learned_then_network_then_unplaced() {
        assert!(SiteFixRank::Learned < SiteFixRank::Network);
        assert!(SiteFixRank::Network < SiteFixRank::Unplaced);
        assert_eq!(learned(0, 0, 1, 2).rank(), SiteFixRank::Learned,);
        assert_eq!(network(0, 0, 1).rank(), SiteFixRank::Network);
        assert_eq!(SiteFix::Unplaced.rank(), SiteFixRank::Unplaced);
    }

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
/// Every row is a real site at the position and heights its own Level II
/// volume reported, because the tests that read them assert measured numbers:
/// `KLWX`'s 292 ft is the ground a cross-section was once anchored 89 m under,
/// and `RKSG`'s 1440 ft is the Camp Humphreys figure that replaced Osan's.
#[cfg(test)]
pub(crate) mod fixture {
    use super::{SiteFix, SiteTable};
    use crate::site_position::SitePosition;
    use std::sync::Once;

    /// `(ICAO, latitude, longitude, site_height_m, tower_height_m)`.
    ///
    /// Metres because that is what a Volume Data Block reports; the feet every
    /// assertion is written in come back out through production's conversion.
    const SITES: [(&str, i32, i32, i32, i32); 14] = [
        ("KTLX", 35_333_060, -97_277_500, 370, 19),
        ("TOKC", 35_276_000, -97_510_000, 386, 386),
        // Pittsburgh's pair, and San Juan. Between them they are every answer
        // the `T` prefix rule has to give: a WSR-88D, the TDWR beside it, and
        // the WSR-88D whose name begins with `T`.
        ("KPBZ", 40_531_670, -80_218_060, 361, 30),
        ("TPIT", 40_501_000, -80_486_000, 366, 366),
        ("TJUA", 18_115_670, -66_078_160, 833, 34),
        ("RKSG", 37_207_570, 127_285_560, 439, 24),
        ("KDGX", 32_280_000, -89_984_000, 151, 34),
        ("KFSX", 34_574_000, -111_198_000, 2261, 29),
        ("KRTX", 45_715_000, -122_965_000, 492, 34),
        ("KSRX", 35_290_000, -94_361_000, 200, 24),
        ("KVWX", 38_260_000, -87_724_000, 156, 34),
        ("KLWX", 38_975_000, -77_477_000, 89, 34),
        // A high-latitude pair, placed so the two ways of ranking "nearest"
        // disagree. From (65°N, 150°W), `PAZA` is 1.0° north and `PAZB` is
        // 1.8° east: degree-squared makes that 1.00 against 3.24 and picks
        // `PAZA`, while the ground distances are 111 km against 85 km.
        ("PAZA", 66_000_000, -150_000_000, 100, 30),
        ("PAZB", 65_000_000, -148_200_000, 500, 30),
    ];

    /// Resolve the fixture into the process-wide table, once. Idempotent and
    /// safe to call from every test in parallel. Tests that assert the
    /// *absence* of radars must not read the process table at all.
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
