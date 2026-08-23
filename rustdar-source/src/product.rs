//! The field a source offers, as data every consumer reads and nobody matches
//! on: [`FieldId`] and [`ProductSpec`].
//!
//! A field is one renderable quantity a source publishes — a radar moment, an
//! HRRR parameter. The UI asks this table what a field is called, what units it
//! speaks, what colours it wears and what its slider may travel over; it never
//! asks *which* field it is. That is the whole point of the split: adding a
//! field is a row in its own crate's `products()`, not an arm in the UI.

use std::borrow::Cow;

use rustdar_units::Quantity;
use serde::{Deserialize, Serialize};

/// An open-string identity for one field.
///
/// `#[serde(transparent)]`: a `FieldId` serializes as the bare string. The
/// spellings the radar crate registers are **byte-identical to the product
/// enum's own `Serialize` output**, which is what makes the move to this type a
/// zero-migration change — a config file written before it existed loads
/// unchanged, and one written after it loads on a build from before.
///
/// **Deliberately NOT `Copy`**: a `Cow<'static, str>` cannot be, and consumers
/// must decide where they clone (`Cow::Borrowed` clones are pointer-cheap).
///
/// **Deliberately DERIVED `Debug`**: `{:?}` prints `FieldId("Reflectivity")`,
/// not `Reflectivity`, so a leftover `format!("{:?}")` keying site produces a
/// visibly wrong persistence key. Key by [`FieldId::as_str`], never by `{:?}`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldId(Cow<'static, str>);

impl FieldId {
    /// A `FieldId` borrowing a static spelling. `const` so a registry can be
    /// plain `pub const` items.
    pub const fn from_static(s: &'static str) -> Self {
        FieldId(Cow::Borrowed(s))
    }

    /// A `FieldId` owning an arbitrary spelling: ids read from a config file
    /// need not be ones this build registers to exist.
    pub fn new(s: impl Into<String>) -> Self {
        FieldId(Cow::Owned(s.into()))
    }

    /// The identity as the bare string — the ONE sanctioned spelling for
    /// persistence keys and lookups.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FieldId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The dBZ colours **every layer that draws reflectivity agrees on**: 0 through
/// 70, sixteen stops, ascending.
///
/// Radar tilts, the MRMS national mosaic and the HRRR forecast composite are
/// read side by side in one pane. Before this table they were three separate
/// stop lists, and the radar one was offset by roughly one 5 dBZ band through
/// the green-to-red region — green began at 25 rather than 20, red at 45 rather
/// than 50, and 55 dBZ was pink where the other two were dark red. A reader
/// comparing an observed tilt against the mosaic beside it was reading two
/// different bars.
///
/// **This is the agreement, and it stops at 70.** Above it the layers diverge
/// on purpose: [`REFLECTIVITY_RADAR_STOPS`] and [`REFLECTIVITY_OVERLAY_STOPS`]
/// are the two ladders built out of this core, and
/// [`REFLECTIVITY_DIVERGENCE_DBZ`] is where they part. Nothing outside this
/// module builds a dBZ bar from the core alone — a layer names the ladder it
/// draws — which is what makes the divergence a declaration rather than an
/// accident of two slices.
///
/// **The provenance of these colours is not recorded anywhere, and this comment
/// will not invent one.** What is known: the 5 dBZ-and-up stops are the ones
/// MRMS and HRRR have always drawn, described in their own docs as "the classic
/// NWS reflectivity ladder" — a claim in prose, with no transcription and no
/// cited source of the kind `SPECTRUM_WIDTH` carries (an ORPG build and a file
/// path). They were chosen as the survivor because two of the three layers
/// already drew them, not because they are the more authoritative table. The
/// two stops below 5 dBZ are radar's own, and those *do* have a traceable
/// shape: they are the discretisation of the `dbz / 5.0 * 128.0` grey ramp that
/// a hand-written `if`-chain painted the low end with until `8334139b` turned
/// the chain into a table.
///
/// The core starts at 0 rather than at 5 because radar's own bar needs a stop
/// there: `voxel::tests::the_table_is_the_palette_function_not_its_stops`
/// asserts the reflectivity stops carry a `0.0` entry, which is what keeps the
/// 3D transfer table from being built out of them and painting everything below
/// zero opaque. The overlays start at [`REFLECTIVITY_OVERLAY_FLOOR`] because
/// their first stop is a transparency floor — clear air in a mosaic or a
/// forecast grid is a small number rather than a missing value, and a bar
/// starting at 0 would paint the whole CONUS domain.
///
/// **Banding is deliberately NOT unified either.** A radar tilt is a continuous
/// field read as a wash; a mosaic and a forecast composite are read by which
/// band a pixel is in. That decision stays with each source, in
/// [`LegendScale::is_gradient`].
///
/// Held equal across the three, through 70 and no further, by
/// `hrrr::fields::tests::the_three_reflectivity_ladders_agree_through_seventy`,
/// `palette::tests::the_reflectivity_ladder_is_the_substrates_radar_one` and
/// `ui::map::pane_render::legend_ladder_tests::
/// every_layer_that_draws_dbz_paints_the_same_ladder_through_seventy`.
pub const REFLECTIVITY_SHARED_STOPS: [(f32, [u8; 3]); 16] = [
    (0.0, [0x00, 0x00, 0x00]),
    (2.5, [0x40, 0x40, 0x40]),
    (5.0, [0x40, 0xe8, 0xe3]),
    (10.0, [0x26, 0xa4, 0xfa]),
    (15.0, [0x0a, 0x2f, 0xc0]),
    (20.0, [0x00, 0xff, 0x00]),
    (25.0, [0x00, 0xc8, 0x00]),
    (30.0, [0x00, 0x90, 0x00]),
    (35.0, [0xff, 0xff, 0x00]),
    (40.0, [0xe7, 0xc0, 0x00]),
    (45.0, [0xff, 0x90, 0x00]),
    (50.0, [0xff, 0x00, 0x00]),
    (55.0, [0xd6, 0x00, 0x00]),
    (60.0, [0xc0, 0x00, 0x00]),
    (65.0, [0xff, 0x00, 0xff]),
    (70.0, [0x99, 0x55, 0xc9]),
];

/// The dBZ the three layers stop agreeing at: the first stop above
/// [`REFLECTIVITY_SHARED_STOPS`], and **the one value in this file that has two
/// colours on purpose**.
///
/// Radar paints it sky-blue, the bottom of [`REFLECTIVITY_HAIL_TAIL`]. The two
/// overlay layers paint it white, [`REFLECTIVITY_OVERLAY_CAP`], and stop there.
pub const REFLECTIVITY_DIVERGENCE_DBZ: f32 = 75.0;

/// Radar's tail above [`REFLECTIVITY_SHARED_STOPS`]: the **hail band**, five
/// stops from [`REFLECTIVITY_DIVERGENCE_DBZ`] to 95 dBZ.
///
/// **These are radar's and only radar's, and that is the decision rather than
/// an oversight.** A tilt is one instrument's measurement of one volume and it
/// does reach up here: the sky-blue at 75 is the marker the stop's own comment
/// has called "hail" for as long as the table has existed, and the ladder keeps
/// climbing through it so a reader can tell 78 dBZ from 92 dBZ instead of
/// reading both as one flat top band.
///
/// MRMS and HRRR do **not** get this tail. The mosaic is a column maximum
/// blended across sites and the forecast composite is a model diagnostic;
/// neither grid produces values up here, and a bar advertising a range its own
/// raster cannot reach is a worse lie than a documented divergence. They end at
/// [`REFLECTIVITY_OVERLAY_CAP`].
///
/// The 2026-08-23 unification (`e6091e47`) dropped these four upper stops and
/// capped radar at 75 white, which silently painted every hail core — and
/// everything else at or above 75 — the same flat white on a tilt. They are
/// restored here byte-for-byte from `e6091e47^:rustdar-radar/src/palette.rs`.
pub const REFLECTIVITY_HAIL_TAIL: [(f32, [u8; 3]); 5] = [
    (75.0, [135, 206, 235]),
    (80.0, [173, 216, 230]),
    (85.0, [255, 140, 0]),
    (90.0, [255, 69, 0]),
    (95.0, [255, 255, 255]),
];

/// The overlay layers' single stop above [`REFLECTIVITY_SHARED_STOPS`]: 75 dBZ,
/// white, and the top of both their bars.
///
/// The same dBZ as [`REFLECTIVITY_HAIL_TAIL`]'s first stop and deliberately a
/// different colour — see [`REFLECTIVITY_DIVERGENCE_DBZ`].
pub const REFLECTIVITY_OVERLAY_CAP: (f32, [u8; 3]) = (75.0, [0xff, 0xff, 0xff]);

/// The index in [`REFLECTIVITY_SHARED_STOPS`] the overlay layers' bars begin
/// at: the 5 dBZ stop, which is their transparency floor.
///
/// A named constant rather than a `2` at each call site, so a stop added to the
/// low end moves both slices at once instead of silently shifting MRMS's floor
/// to 2.5 dBZ.
pub const REFLECTIVITY_OVERLAY_FLOOR: usize = 2;

/// **Radar's** dBZ ladder: [`REFLECTIVITY_SHARED_STOPS`] whole, then
/// [`REFLECTIVITY_HAIL_TAIL`]. Twenty-one stops, 0 → 95 dBZ, drawn as a
/// gradient.
pub const REFLECTIVITY_RADAR_STOPS: [(f32, [u8; 3]); 21] = radar_ladder();

/// **The overlay layers'** dBZ ladder: [`REFLECTIVITY_SHARED_STOPS`] from
/// [`REFLECTIVITY_OVERLAY_FLOOR`] up, then [`REFLECTIVITY_OVERLAY_CAP`].
/// Fifteen stops, 5 → 75 dBZ, drawn as bands. MRMS and HRRR both draw exactly
/// this — neither slices it further.
pub const REFLECTIVITY_OVERLAY_STOPS: [(f32, [u8; 3]); 15] = overlay_ladder();

/// The alpha **every layer that draws dBZ paints at**.
///
/// Radar's rasters painted through the radar crate's own `TRANSPARENCY` (180)
/// and the gridded overlay path through its own `ALPHA` (160), so a tilt and
/// the MRMS mosaic drawn in the same pane rendered the same quantity at two
/// opacities. Neither number had a recorded reason; **160 is the one that was
/// kept, and no third value was invented.**
///
/// 160 rather than 180 for three reasons, in the order that decided it:
///
/// * a dBZ raster is a ground layer over the basemap, and the reader places the
///   storm by what shows through it — county lines, town names, the coast. The
///   lighter of the two arbitrary numbers is the one that leaves that legible;
/// * a radar tilt and the MRMS mosaic can be enabled on the **same** pane, and
///   translucency compounds. Two layers at 160 leave about 14 % of the basemap
///   showing where two at 180 leave about 9 %;
/// * moving the overlays up to 180 instead would have dragged non-dBZ fields
///   with them — `render::gridded::ALPHA` also paints MRMS precipitation rate
///   and all four GMGSI channels — or forced a per-field alpha nobody asked
///   for. Choosing 160 changes exactly one field's appearance, radar
///   reflectivity's, and leaves every other bar in the tree where it was.
///
/// **This is not a radar-wide change.** The radar crate's other scales keep
/// `TRANSPARENCY`; only the dBZ field reads this. What it does reach, because
/// the palette is one function, is radar's 3D transfer table: reflectivity's
/// volume alphas are this number scaled per value, so its ceiling came down
/// with it — 180 to 160 — while the count of see-through entries did not move.
/// `voxel::tests::the_default_transparency_profile_is_measured_per_product`
/// records both columns for all nine volume products.
///
/// The two paths are held equal **at the same dBZ**, not merely against the same
/// literal, by `ui::map::pane_render::legend_ladder_tests::
/// a_tilt_and_a_mosaic_paint_the_same_dbz_at_the_same_opacity`.
pub const REFLECTIVITY_ALPHA: u8 = 160;

/// [`REFLECTIVITY_RADAR_STOPS`], concatenated at compile time rather than
/// transcribed. The `assert!` is what stops a stop added to the core from
/// leaving a default `(0.0, [0, 0, 0])` hole in the middle of the ladder.
const fn radar_ladder() -> [(f32, [u8; 3]); 21] {
    assert!(
        REFLECTIVITY_SHARED_STOPS.len() + REFLECTIVITY_HAIL_TAIL.len() == 21,
        "the shared core plus radar's hail tail no longer make twenty-one \
         stops; widen REFLECTIVITY_RADAR_STOPS to match them",
    );
    let mut out = [(0.0f32, [0u8; 3]); 21];
    let mut i = 0;
    while i < REFLECTIVITY_SHARED_STOPS.len() {
        out[i] = REFLECTIVITY_SHARED_STOPS[i];
        i += 1;
    }
    let mut j = 0;
    while j < REFLECTIVITY_HAIL_TAIL.len() {
        out[REFLECTIVITY_SHARED_STOPS.len() + j] = REFLECTIVITY_HAIL_TAIL[j];
        j += 1;
    }
    out
}

/// [`REFLECTIVITY_OVERLAY_STOPS`], sliced and capped at compile time. The same
/// `assert!` for the same reason as [`radar_ladder`]'s.
const fn overlay_ladder() -> [(f32, [u8; 3]); 15] {
    assert!(
        REFLECTIVITY_SHARED_STOPS.len() - REFLECTIVITY_OVERLAY_FLOOR + 1 == 15,
        "the shared core above the overlay floor, plus the overlays' cap, no \
         longer make fifteen stops; widen REFLECTIVITY_OVERLAY_STOPS to match \
         them",
    );
    let mut out = [(0.0f32, [0u8; 3]); 15];
    let mut i = 0;
    while i < 15 - 1 {
        out[i] = REFLECTIVITY_SHARED_STOPS[REFLECTIVITY_OVERLAY_FLOOR + i];
        i += 1;
    }
    out[15 - 1] = REFLECTIVITY_OVERLAY_CAP;
    out
}

/// A colour bar: the stops a field's values are painted through.
///
/// Lives here rather than in the radar crate because [`ProductSpec::scale`]
/// names it and this crate sits below every source. The radar crate re-exports
/// the type, so no consumer's spelling changed when it moved.
///
/// **This doc used to say the radar crate builds the values, "its palette
/// tables are the physics", and that a source therefore owns its own ramp.
/// That is no longer the rule for a quantity two sources both publish.**
/// Reflectivity was drawn through three tables — radar's, MRMS's and HRRR's —
/// and the radar one was offset roughly one 5 dBZ band through the green-to-red
/// region, so the same storm read 45 dBZ red on a radar tilt and orange on the
/// mosaic beside it. The colours for such a quantity now live here as `const`s
/// (see [`REFLECTIVITY_SHARED_STOPS`]) and every layer draws the ladder built
/// from them; what stays the source's own is *how* it paints them, which is
/// [`LegendScale::is_gradient`]. A ramp only one source publishes still lives
/// with that source.
///
/// **"Shared" is not "identical", and the sharing is bounded where the layers
/// stop meaning the same thing.** The dBZ ladders agree through 70 and part
/// above it — [`REFLECTIVITY_DIVERGENCE_DBZ`] — because a tilt reaches into the
/// hail band and the two gridded layers do not. What moved down here is the
/// agreement, not a decree that the layers be the same object.
#[derive(Clone, Debug, PartialEq)]
pub struct LegendScale {
    /// Colour stops, sorted ascending by value.
    pub thresholds: Vec<(f32, [u8; 3])>,
    /// Whether the renderer interpolates between stops or paints flat bands.
    pub is_gradient: bool,
    pub min_value: f32,
    pub max_value: f32,
}

/// Everything a consumer may know about one field, in one row.
///
/// **No field has a `Default`, and that is deliberate**: every one of the
/// eleven is a claim about the field that only the source registering it can
/// make. A defaulted `value_domain` is a fabricated slider; a defaulted
/// `vertical` is a 3D editor offered for a field with no vertical extent. A
/// source that cannot answer one of these does not have a field to register.
pub struct ProductSpec {
    /// The field's open-string identity — the persistence key and the lookup key.
    pub id: FieldId,
    /// Display name, as the UI writes it.
    pub name: &'static str,
    /// Short lowercase identifier (`"ref"`, `"vel"`, …).
    pub code: &'static str,
    /// Order fields are listed in within their group.
    pub sort_order: u8,
    /// The group label the UI files this field under ("Radar products",
    /// "HRRR parameters"). A `&'static str` rather than an enum: a new source
    /// brings its own group and the UI needs no arm for it.
    pub group: &'static str,
    /// The unit domain this field's values live in — the binding that honours
    /// the user's unit preferences.
    pub quantity: Quantity,
    /// The colour bar, built once and borrowed. Consumers read this object;
    /// they never rebuild a colour table from values.
    pub scale: &'static LegendScale,
    /// The inclusive range a threshold slider over this field may travel —
    /// ergonomics, in the field's own units.
    pub value_domain: (f32, f32),
    /// What a threshold over this field *means*, as a short prefix (`"\u{2265}"`,
    /// `"|\u{b1}| \u{2265}"`, `"\u{2264}"`) and the unit suffix that follows the number.
    pub domain_label_ends: (&'static str, &'static str),
    /// Whether this field has vertical extent, i.e. whether it renders in the
    /// 3D views at all. The 3D editors gate on this.
    pub vertical: bool,
    /// Whether this field exists at individual tilts, as opposed to being a
    /// whole-volume composite or column integral.
    pub tilted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladders' colours, as literals, because after the unification nothing
    /// else in the tree holds them.
    ///
    /// **This is the property the old arrangement had by accident and the new
    /// one has to state.** While radar, MRMS and HRRR each kept their own
    /// transcription, the test that held the two overlay ladders equal was
    /// comparing two independent copies, so a typo in either reddened it. One
    /// table has no second copy to disagree with: every agreement test in the
    /// workspace now reads *these* arrays, so a stop edited here moves all
    /// three layers together and every one of them still agrees. A mutation
    /// that repaints 45 dBZ would have gone green.
    ///
    /// So the second copy lives here, spelled out, and it is meant to be
    /// annoying: changing a colour means changing it twice, on purpose, with
    /// this comment in front of you. That now covers three arrays — the shared
    /// core, radar's hail tail and the overlays' cap — because the hail tail is
    /// exactly the part no agreement test can speak for: it is one layer's, so
    /// there is no second layer to disagree with it.
    #[test]
    fn the_reflectivity_ladders_colours_are_the_ones_that_were_reviewed() {
        assert_eq!(
            REFLECTIVITY_SHARED_STOPS,
            [
                (0.0, [0x00, 0x00, 0x00]),
                (2.5, [0x40, 0x40, 0x40]),
                (5.0, [0x40, 0xe8, 0xe3]),
                (10.0, [0x26, 0xa4, 0xfa]),
                (15.0, [0x0a, 0x2f, 0xc0]),
                (20.0, [0x00, 0xff, 0x00]),
                (25.0, [0x00, 0xc8, 0x00]),
                (30.0, [0x00, 0x90, 0x00]),
                (35.0, [0xff, 0xff, 0x00]),
                (40.0, [0xe7, 0xc0, 0x00]),
                (45.0, [0xff, 0x90, 0x00]),
                (50.0, [0xff, 0x00, 0x00]),
                (55.0, [0xd6, 0x00, 0x00]),
                (60.0, [0xc0, 0x00, 0x00]),
                (65.0, [0xff, 0x00, 0xff]),
                (70.0, [0x99, 0x55, 0xc9]),
            ],
            "a shared reflectivity stop moved. Every layer that draws dBZ \
             moved with it — that is what this table is for — so no agreement \
             test can tell you. If the change was meant, re-record it here and \
             say in the commit which colour the reader will now see at which \
             dBZ.",
        );
        assert_eq!(
            REFLECTIVITY_HAIL_TAIL,
            [
                (75.0, [135, 206, 235]),
                (80.0, [173, 216, 230]),
                (85.0, [255, 140, 0]),
                (90.0, [255, 69, 0]),
                (95.0, [255, 255, 255]),
            ],
            "radar's hail band moved. Only one layer draws it, so no agreement \
             test compares it with anything: this literal is the whole gate. \
             These five are `e6091e47^:rustdar-radar/src/palette.rs` verbatim.",
        );
        assert_eq!(
            REFLECTIVITY_OVERLAY_CAP,
            (75.0, [0xff, 0xff, 0xff]),
            "the overlay bars' top stop moved. It is the top of MRMS's and \
             HRRR's bars and nothing else reads it.",
        );
        assert_eq!(
            REFLECTIVITY_ALPHA, 160,
            "the dBZ alpha moved. It is one of the two values that existed \
             before the unification — 160, the gridded overlay path's — and a \
             third number is not an option: see the constant's own doc.",
        );
    }

    /// The two things every reader of these tables assumes: the stops ascend,
    /// and [`REFLECTIVITY_OVERLAY_FLOOR`] still points at 5 dBZ.
    ///
    /// The floor is an index rather than a value, so a stop inserted at the low
    /// end would move MRMS's and HRRR's transparency floor down without either
    /// crate changing a line. That is the failure this catches.
    #[test]
    fn the_reflectivity_ladder_ascends_and_its_overlay_floor_is_five_dbz() {
        for (name, ladder) in [
            ("radar", &REFLECTIVITY_RADAR_STOPS[..]),
            ("overlay", &REFLECTIVITY_OVERLAY_STOPS[..]),
        ] {
            for pair in ladder.windows(2) {
                assert!(
                    pair[1].0 > pair[0].0,
                    "the {name} reflectivity stops must ascend, but {} follows \
                     {}",
                    pair[1].0,
                    pair[0].0,
                );
            }
        }
        assert_eq!(
            REFLECTIVITY_RADAR_STOPS[0].0, 0.0,
            "radar's 3D transfer table is built from a scale that must carry a \
             0 dBZ stop",
        );
        assert_eq!(
            REFLECTIVITY_SHARED_STOPS[REFLECTIVITY_OVERLAY_FLOOR].0, 5.0,
            "the overlay floor index no longer names the 5 dBZ stop, so MRMS \
             and HRRR would begin their bars somewhere else",
        );
        assert_eq!(
            REFLECTIVITY_OVERLAY_STOPS[0].0, 5.0,
            "the overlay ladder no longer begins at its transparency floor",
        );
    }

    /// **The divergence, stated where it is built.** The two ladders are the
    /// same table through 70 dBZ and different from 75 up, and both halves of
    /// that are deliberate.
    ///
    /// An accidental re-convergence — someone deciding the two tails "should
    /// match" and capping radar at 75 white again, which is exactly what
    /// `e6091e47` did — reddens on the colour comparison at 75. An accidental
    /// *widening* — a stop drifting anywhere at or below 70 — reddens on the
    /// core comparison. The two are separate assertions on purpose; a single
    /// "the ladders differ" check would pass in both directions.
    #[test]
    fn the_two_dbz_ladders_share_a_core_and_part_at_the_divergence() {
        // Through 70: byte-identical, allowing for the overlays' floor.
        assert_eq!(
            REFLECTIVITY_RADAR_STOPS[..REFLECTIVITY_SHARED_STOPS.len()],
            REFLECTIVITY_SHARED_STOPS,
            "radar's ladder no longer opens with the shared core",
        );
        let shared_overlay = &REFLECTIVITY_SHARED_STOPS[REFLECTIVITY_OVERLAY_FLOOR..];
        assert_eq!(
            &REFLECTIVITY_OVERLAY_STOPS[..shared_overlay.len()],
            shared_overlay,
            "the overlay ladder no longer opens with the shared core from its \
             floor up",
        );
        assert!(
            REFLECTIVITY_SHARED_STOPS
                .iter()
                .all(|&(dbz, _)| dbz < REFLECTIVITY_DIVERGENCE_DBZ),
            "the shared core must end below the dBZ the layers part at",
        );

        // At 75: the same dBZ, deliberately two colours.
        let radar_at_divergence = REFLECTIVITY_RADAR_STOPS[REFLECTIVITY_SHARED_STOPS.len()];
        let overlay_at_divergence = REFLECTIVITY_OVERLAY_STOPS[shared_overlay.len()];
        assert_eq!(
            (radar_at_divergence.0, overlay_at_divergence.0),
            (REFLECTIVITY_DIVERGENCE_DBZ, REFLECTIVITY_DIVERGENCE_DBZ),
            "both ladders' first divergent stop must sit at the dBZ \
             REFLECTIVITY_DIVERGENCE_DBZ names",
        );
        assert_eq!(
            radar_at_divergence.1,
            [135, 206, 235],
            "radar paints {REFLECTIVITY_DIVERGENCE_DBZ} dBZ sky-blue — the \
             bottom of the hail band",
        );
        assert_eq!(
            overlay_at_divergence.1,
            [0xff, 0xff, 0xff],
            "the overlay bars cap at {REFLECTIVITY_DIVERGENCE_DBZ} dBZ white",
        );
        assert_ne!(
            radar_at_divergence.1, overlay_at_divergence.1,
            "the two ladders have re-converged at \
             {REFLECTIVITY_DIVERGENCE_DBZ} dBZ. They are meant to differ \
             there: radar shows the hail band above it and the overlay grids \
             do not produce values up there at all. If this was meant, the \
             hail tail is what has to go, and the reader loses 80/85/90/95.",
        );

        // Above 75: radar keeps climbing, the overlays have stopped.
        assert_eq!(
            REFLECTIVITY_OVERLAY_STOPS
                .last()
                .expect("the overlay ladder is non-empty")
                .0,
            REFLECTIVITY_DIVERGENCE_DBZ,
            "the overlay bars must stop at the divergence, not advertise a \
             range their grids cannot reach",
        );
        assert_eq!(
            REFLECTIVITY_RADAR_STOPS
                .last()
                .expect("the radar ladder is non-empty")
                .0,
            95.0,
            "radar's ladder must run to the top of the hail band",
        );
        assert_eq!(
            &REFLECTIVITY_RADAR_STOPS[REFLECTIVITY_SHARED_STOPS.len()..],
            &REFLECTIVITY_HAIL_TAIL,
            "radar's tail is not the hail band",
        );
    }
}
