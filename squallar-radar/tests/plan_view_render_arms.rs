//! **Which arm a plan-view render takes decides whether it touches the pooled
//! pair at all** — and how much of the product ladder still takes the arm that
//! does.
//!
//! `render pools` read 620.3 MiB on the six-pane arm: `7362² × 12`, the cell
//! buffer at eight bytes a pixel beside its RGBA texture at four, at the side
//! the data asks for. Unlike the byte cap behind `render cache`, **that figure
//! is not arithmetic over a bracket**: the pool is sized by `demand`, a
//! decaying two-generation high-water over the pixels renders actually asked
//! for, every checkout and recycle is weighed against it, and
//! `squallar_app::render_pool_trim` gives the slots back after a quiet window.
//! There is no constant in it to fit.
//!
//! What decides the figure is how often anything still asks for the pair, and
//! that is a **product roster**, not a policy. `render::codes::r8_fidelity`
//! admits five products outright and a sixth on an eight-bit wire; the other
//! eleven are refused a plane and render exactly the raster this build has
//! always produced. Every one of those checks out the pooled pair.
//!
//! This file pins the two ends of that fork on ONE sweep:
//!
//! * a product a plane carries checks out **neither** buffer and begins no
//!   plan-view render at all — which is the whole mechanism by which the
//!   polar surface removes this term, rather than a detail of it;
//! * a product the wire's own word width refuses renders a raster, begins one,
//!   and reaches the pair.
//!
//! **The admission is asserted first and alone.** Every reason a plane is
//! refused is a fidelity reason and the answer to all of them is the raster —
//! so a sweep this build declines is not a failure of the subject, it is the
//! disappearance of it, and every assertion below would then pin the fallback
//! as the specification.
//!
//! **The fixture is a surveillance cut's own shape**, 720 radials of 1832
//! gates at 250 m, and not the 9×40 and 36×120 sweeps every other plane
//! fixture in this workspace uses. The refusal under test is
//! `PlaneUnavailable::WideWireWord` — a property of the *moment block's word
//! width*, carried here by a real sixteen-bit differential phase moment beside
//! an eight-bit reflectivity one, one volume holding both sides of the R8
//! admission. A toy sweep would answer the same question about a picture of
//! four gates.
//!
//! **The refusal is asserted as a REASON and not as a `None`.** Two
//! independent guards refuse this product — the wire-width check in
//! `sweep_code_plane`, and `CodePlane::build`'s own reading of the
//! `r8_fidelity` table — and either alone answers `None`. A test reading only
//! the `None` stays green with the width guard deleted, which was measured by
//! deleting it. Asserting the variant is what makes the guard this file names
//! the guard it tests.
//!
//! **The side is held to `IMAGE_SIZE` only so the render is cheap**, and
//! nothing here reads an absolute figure off it: the raster arm's cost is
//! asserted and reported *per pixel*, which is the quantity the 12 B/px thesis
//! is about and the one that does not move with a ceiling.
//!
//! **The peak and the parked row are not the same figure, and the difference
//! does not scale with the side.** `render pools` is what the slots hold
//! between renders, 12 B a pixel. What the allocator sees at the instant of a
//! render is more, and the histogram this file prints says by how much and of
//! what: the polar field's value grid is taken twice, `radials × gates × 4`
//! each time, and that term is a property of the CUT and not of the picture —
//! so it is a fixed addition at every side rather than a per-pixel one. Any
//! reader converting a per-pixel figure to the shipped 7362 px side has to
//! carry the two separately.
//!
//! **Nothing here gates the pair at twelve bytes.** Eight of them are the cell
//! buffer, which no change of representation removes; the other four are the
//! texture the colouring pass writes while the cells are still live. A test
//! that demanded twelve would red-gate the one repair worth making, so the
//! assertion is the eight and the measured figure is printed.
//!
//! **One `#[test]`.** The pools, the demand behind them, `renders_begun` and
//! the allocator's peak are all process-wide; a second test in this binary
//! would be reading this one's window.

// Native only, and the reason is the counter rather than the subject.
// `squallar-alloc` is a dev-dependency under
// `cfg(not(target_arch = "wasm32"))`, because `--all-targets` builds dev-deps
// and an ungated entry breaks the wasm check. A binary that names it must
// carry the same condition; without this the CI row
// `wasm-threads.sh cargo check --workspace --all-targets` is red on an
// unresolved crate, which is what it read before this line existed.
#![cfg(not(target_arch = "wasm32"))]

use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};
use squallar_radar::nyquist::DeclaredNyquist;
use squallar_radar::render::{
    pooled_bytes, recycle_image, render_from_sized, render_sweep_plane, renders_begun, trim_pools,
};
use squallar_radar::render_input::RenderInput;
use squallar_radar::types::{IMAGE_SIZE, RadarProduct};

#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;

/// A surveillance cut's own shape: 0.5° radials all the way round, 1832 gates
/// of 250 m from 2.125 km — the cut the 7362 px raster side is derived from.
const RADIALS: usize = 720;
const GATES: usize = 1832;
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

/// Bytes one cell costs: a cell is a `u64`, a `u32` key over a `u32` value.
/// The one figure here that is a constant of the representation rather than of
/// the scene, and the floor the assertion below is written against.
const CELL_BYTES: usize = 8;

/// Buckets of `squallar_alloc`'s large-grant histogram to walk when reporting
/// what a render took. Past the last, `large_grant` answers `None` and the
/// walk simply reports nothing.
const LARGE_BUCKETS: usize = 256;

/// The side every render here asks for. A production size, and the cheapest
/// one; see the module header for why nothing reads an absolute off it.
const SIDE: usize = IMAGE_SIZE;

/// One radial carrying an eight-bit reflectivity moment and a sixteen-bit
/// differential phase moment — one sweep on both sides of the R8 admission.
fn radial(index: usize) -> Radial {
    let spacing = 360.0 / RADIALS as f32;
    let refl: Vec<u8> = (0..GATES).map(|g| ((index + g) % 254 + 2) as u8).collect();
    let phi: Vec<u8> = (0..GATES as u16)
        .flat_map(|g| (g + 2).to_be_bytes())
        .collect();
    Radial::new(
        0,
        index as u16,
        index as f32 * spacing,
        spacing,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(MomentData::from_fixed_point(
            GATES as u16,
            FIRST_GATE_M,
            GATE_M,
            8,
            2.0,
            66.0,
            refl,
        )),
        None,
        None,
        None,
        Some(MomentData::from_fixed_point(
            GATES as u16,
            FIRST_GATE_M,
            GATE_M,
            16,
            2.8361,
            2.0,
            phi,
        )),
        None,
        None,
    )
}

fn surveillance_scan() -> Scan {
    let cut = ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        20,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    );
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![cut],
        ),
        vec![Sweep::new(1, (0..RADIALS).map(radial).collect())],
    )
}

fn input_for(scan: &Scan, product: RadarProduct) -> RenderInput {
    RenderInput::extract(scan, 0.5, product, LAT, LON, None, None)
        .expect("a surveillance-shaped cut extracts")
}

#[test]
fn a_plane_arm_touches_neither_pooled_buffer_and_a_refused_wire_word_reaches_both() {
    let scan = surveillance_scan();
    assert_eq!(
        pooled_bytes(),
        0,
        "premise: this binary's slots are not empty before it renders",
    );

    // ── The admission, first and alone ───────────────────────────────────────
    // Without it every figure below is about the raster this sweep fell back
    // to, pinned as though it were the specification.
    let begun_before_plane = renders_begun();
    let plane = render_sweep_plane(
        &scan,
        0.5,
        RadarProduct::Reflectivity,
        &DeclaredNyquist::empty(),
        true,
    )
    .expect("an eight-bit reflectivity surveillance cut is carried by a plane");
    assert!(
        plane.codes.is_some(),
        "the plane arm answered a render with no plane in it",
    );
    assert!(
        plane.image.is_empty(),
        "the plane arm also built a raster; that is both surfaces at once",
    );

    // **The mechanism, stated as two zeroes.** A plane render checks out
    // neither slot and is not a plan-view render by the count the idle trim
    // reads, so a session drawing only planes both leaves the pool empty and
    // looks quiet to the thing that empties it.
    assert_eq!(
        pooled_bytes(),
        0,
        "a plane render parked a pooled buffer; it allocates neither",
    );
    assert_eq!(
        renders_begun(),
        begun_before_plane,
        "a plane render was counted as a plan-view render; the idle trim reads \
         this count, and a plane-only session must read as quiet",
    );
    drop(plane);

    // ── The roster's live half ───────────────────────────────────────────────
    // Differential phase is `R8Fidelity::WireWordTooWide`: its moment block is
    // sixteen bits and truncating one would be a silent quantiser. Eleven of
    // the seventeen products are refused a plane for reasons of this kind, and
    // every one of them renders exactly this.
    // **The reason and the consequence, and the reason first.** `is_none()`
    // alone would not pin the arm this names: with the width guard removed the
    // producer still refuses a sixteen-bit moment further down, where the
    // code buffer fails to be `radials x gates`, so a test reading only the
    // `None` is green whichever of the two refused and cannot tell them apart.
    // Asserting the variant is what makes the width guard the thing under
    // test.
    assert_eq!(
        squallar_radar::render::plane::sweep_code_plane(
            scan.sweeps()[0].radials(),
            RadarProduct::DifferentialPhase,
            GATES,
        )
        .err(),
        Some(squallar_radar::render::plane::PlaneUnavailable::WideWireWord { word_bits: 16 }),
        "a sixteen-bit moment was refused for some other reason than its width",
    );
    assert!(
        render_sweep_plane(
            &scan,
            0.5,
            RadarProduct::DifferentialPhase,
            &DeclaredNyquist::empty(),
            true,
        )
        .is_none(),
        "the production fork carried a sixteen-bit moment on an eight-bit plane",
    );

    let phi = input_for(&scan, RadarProduct::DifferentialPhase);
    // The large-grant histogram is monotone, so a snapshot either side of the
    // render names every block it took. Read for the report below, not for a
    // gate.
    let snap: Vec<u64> = (0..LARGE_BUCKETS)
        .map(|i| squallar_alloc::large_grant(i).map_or(0, |g| g.count))
        .collect();
    let before = squallar_alloc::live_bytes().expect("this binary declares the counting allocator");
    let begun_before_raster = renders_begun();
    let warm = render_from_sized(&phi, SIDE).expect("the refused product renders a raster");
    let peak = squallar_alloc::live_peak_bytes().expect("the counter is installed");
    assert_eq!(
        renders_begun(),
        begun_before_raster + 1,
        "the fallback did not begin a plan-view render",
    );

    let pixels = warm.image.len() / 4;
    assert_eq!(
        pixels,
        SIDE * SIDE,
        "the render did not draw the side asked for"
    );

    // **Reported, not gated.** The campaign's thesis names this term as twelve
    // bytes a pixel -- a cell buffer at eight beside its texture at four. The
    // figure measured here is larger, and the histogram says why: the polar
    // field's value grid is taken TWICE per render, `radials x gates x 4` each
    // time, because `PolarBuffers::into_field` collects a fresh `Vec<f32>` out
    // of its `Vec<AtomicU32>` while the original is still alive. The two have
    // the same size, the same alignment and the same bytes -- `f32::from_bits`
    // is the identity on them -- so the second is a copy and not a conversion.
    // It is left as a copy because the safe zero-copy spelling
    // (`AtomicU32::from_mut_slice` over an owned `Vec<f32>`) is
    // `atomic_from_mut`, unstable on the toolchain this workspace pins.
    //
    // None of it is asserted. A gate on twelve bytes a pixel, or on any figure
    // this composition happens to reach, would red-gate the one repair worth
    // making to it -- and the campaign that would make it is not this one.
    // What IS asserted is the floor no change of representation removes: the
    // cell buffer, whose eight bytes hold a `u32` key and a `u32` value so an
    // overlapping radial resolves by `fetch_max` rather than by a race.
    let per_pixel = (peak - before) as f64 / pixels as f64;
    println!(
        "raster arm: {pixels} px, peak rose {} B over {before} B live, {per_pixel:.2} B/px",
        peak - before,
    );
    for (i, was) in snap.iter().enumerate() {
        if let Some(g) = squallar_alloc::large_grant(i)
            && g.count > *was
        {
            println!(
                "  large grants in {}..{} B: {} new, largest {} B",
                g.low,
                g.high,
                g.count - was,
                g.max,
            );
        }
    }
    assert!(
        peak - before >= (pixels * CELL_BYTES) as u64,
        "a raster render peaked {} B over a {pixels} px picture, under the cell \
         buffer alone at {CELL_BYTES} B a pixel",
        peak - before,
    );

    // ── A trim can never cost a picture ─────────────────────────────────────
    // The cost of an empty slot is an allocation and never a refusal: every
    // checkout falls back to the allocator. So no retention rule, no decayed
    // demand and no idle trim can cost a frame -- which is the one way a
    // memory cut here could reach fidelity, and it is closed by construction
    // rather than by policy.
    //
    // **Not a claim about what a reused buffer contains.** That a pooled
    // texture is indistinguishable from a fresh one is
    // `tests/render_output_pool.rs`'s subject, and it is pinned there with the
    // input that claim needs: a render that paints NOTHING after one that
    // painted everything, so the whole raster comes straight from whatever the
    // slot held. Two renders of one sweep, as here, write the same bytes
    // whether the buffer was reset or not, so restating it here would be a
    // conjunct that cannot fail.
    //
    // **Filling the slots takes two renders.** A slot only keeps a size the
    // demand *before* that render already showed -- `carry_ceiling_px` for the
    // cells, `demand::carry` for the texture -- so the first render of a size
    // hands both to the allocator. The state of the slots is therefore
    // asserted rather than assumed: without it the trim below would be
    // emptying slots that were already empty.
    recycle_image(warm.image.clone());
    let second = render_from_sized(&phi, SIDE).expect("a second render at the same size");
    recycle_image(second.image.clone());
    println!("both slots parked: {} B over {pixels} px", pooled_bytes());
    assert!(
        pooled_bytes() > pixels * CELL_BYTES,
        "the slots hold {} B, which is not more than the cell buffer alone at \
         {CELL_BYTES} B a pixel -- one of the two is empty and the trim below \
         would be emptying nothing",
        pooled_bytes(),
    );

    trim_pools();
    assert_eq!(pooled_bytes(), 0, "the trim left a slot filled");
    let after_trim = render_from_sized(&phi, SIDE).expect("a render on emptied slots is refused");
    assert_eq!(
        after_trim.image.len(),
        warm.image.len(),
        "a render on emptied slots came back a different size from one on warm \
         slots; a checkout that cannot reach the pool must reach the allocator",
    );

    trim_pools();
    assert_eq!(pooled_bytes(), 0, "the trim left a slot filled");
}
