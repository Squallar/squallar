use super::*;
use rustdar_radar::types::{IMAGE_SIZE, NATIVE_IMAGE_SIZE, WASM_IMAGE_SIZE};
use rustdar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

/// One device class's share of every cascade in this file.
///
/// The four budget invariants below used to read the `cfg`-selected
/// constants directly, which meant each of them checked one arm out of
/// three and left the other two free — the same one-sided shape 3292e8d
/// fixed for the voxel grid, and it was still here for the budgets. The
/// arms all have names now, so a table can be built and every invariant
/// run against every row.
struct Arm {
    name: &'static str,
    /// `rustdar_radar::types::IMAGE_SIZE` for this class — the side a static
    /// plan-view render takes at the base side. It is a *two*-arm cascade —
    /// mobile is native — so this is where the two cascade shapes in this
    /// workspace are reconciled.
    image_size: usize,
    /// The side a static render may grow to past the floor. Equal to
    /// `image_size` on the arm where adaptivity is off.
    long_range_image_size: usize,
    /// The side a **loop** frame is rendered at, whatever its sweep reaches.
    /// This, not `image_size`, is what the loop budgets are computed from.
    loop_image_size: usize,
    /// `rustdar_radar::xsect::SECTION_WIDTH` for this class. Pinned per target
    /// rather than derived from `image_size`, so it is carried here rather
    /// than reconstructed.
    section_width: usize,
    concurrent_renders: usize,
    loop_frames: usize,
    render_budget: usize,
    /// The whole application's loop allowance on a device that reports nothing.
    /// **Not** a per-pane figure any more — see [`LOOP_POOL_FLOOR_BYTES`].
    pool_floor: usize,
    /// The most this class will ever spend on loops, however much the device
    /// claims. See [`LOOP_POOL_CEILING_BYTES`].
    pool_ceiling: usize,
    grid: [u32; 3],
    volume_budget: usize,
    /// Frames — and so resident voxel grids — a 3D loop holds on this class.
    /// See [`MAX_LOOP_VOLUME_FRAMES`]: for this loop kind the two are one
    /// number, which `the_3d_loop_holds_exactly_what_it_marches` is about.
    volume_loop_frames: usize,
    /// The application-wide ceiling on those grids.
    volume_loop_budget: usize,
    /// The per-pane raymarch offscreen ceiling.
    offscreen_budget: usize,
    /// Panes this class can show at once. Not a `cfg` cascade like everything
    /// else here — it is `rustdar_egui`'s, selected at runtime by width class
    /// — which is precisely why the multiplication below had nothing checking
    /// it: the two halves live in different crates.
    max_panes: usize,
    /// The whole-application ceiling the row must fit.
    app_budget: usize,
}

impl Arm {
    /// Bytes one loop frame's texture occupies: RGBA at `image_size²`.
    /// Loop frames carry no value grid — `poll_loop_render_results` stores an
    /// empty one — so this is the whole cost, unlike a static pane render.
    fn loop_frame_bytes(&self) -> usize {
        self.loop_image_size * self.loop_image_size * 4
    }

    /// Bytes one **static** pane render's texture occupies, worst case: the
    /// long-range side, since that is the one a device that passes the gate can
    /// be asked to hold. Not part of `app_texture_bytes` — see
    /// `the_static_render_textures_are_named_even_though_the_ceiling_omits_them`.
    fn static_frame_bytes(&self) -> usize {
        self.long_range_image_size * self.long_range_image_size * 4
    }

    /// Bytes one **cross-section** loop frame's texture occupies: RGBA at
    /// `SECTION_WIDTH × SECTION_HEIGHT`.
    ///
    /// The width comes from this arm's own `section_width` so that every row
    /// is checkable from one host build — the reason `arms()` exists — and the
    /// halving is read from `SECTION_HEIGHT`'s definition through the
    /// assertion below rather than restated beside it. Section loop frames
    /// carry no value or status plane, for the reason plan-view frames carry
    /// no value grid — see `rustdar_egui::pane::SectionImageData`.
    fn section_frame_bytes(&self) -> usize {
        assert_eq!(
            (
                rustdar_radar::xsect::SECTION_WIDTH,
                rustdar_radar::xsect::SECTION_HEIGHT
            ),
            (
                self.section_width_for_this_target(),
                self.section_width_for_this_target() / 2
            ),
            "the section raster is no longer SECTION_WIDTH by half of it, so the \
             per-arm reconstruction above no longer describes it",
        );
        self.section_width * (self.section_width / 2) * 4
    }

    /// This *build's* section width, for the consistency check above — the
    /// per-arm figure is `section_width` and is what the budget rows use.
    fn section_width_for_this_target(&self) -> usize {
        if cfg!(target_arch = "wasm32") {
            WASM_SECTION_WIDTH
        } else {
            NATIVE_SECTION_WIDTH
        }
    }

    /// Frames that hold a texture at once. `evict_textures_outside_render_set`
    /// runs every dispatch with `MAX_LOOP_RENDER_BUDGET`, so a loop of
    /// `MAX_LOOP_FRAMES` keeps only the render set textured.
    fn textured_frames(&self) -> usize {
        self.render_budget.min(self.loop_frames)
    }

    /// Bytes one pane's 3D volume texture occupies: every mip level of the
    /// grid at `crate::volume::VOLUME_TEXTURE_FORMAT`'s four bytes a cell, plus
    /// the RGBA table those cells index.
    ///
    /// Read from `volume::raymarch::grid_bytes_with_mips` rather than
    /// recomputed, so the budget is checked against the arithmetic the upload
    /// path actually allocates by — including the coarse level, which the
    /// earlier hand-written product silently left out of the budget entirely.
    ///
    /// Four bytes per cell is not an assumption to be tidied away: the format
    /// is `Rg16Float` because the march reconstructs `R̄ / Ḡ` from a
    /// coverage-premultiplied index and a coverage channel — which needs a
    /// filter error that scales with the sample rather than with the format,
    /// or the quotient is wrong by the whole palette at an echo edge — and
    /// because `Rg16Float` is *filterable* under `Features::empty()` where
    /// `R32Float` is not.
    fn volume_bytes(&self) -> usize {
        crate::volume::raymarch::grid_bytes_with_mips(self.grid)
            .expect("a shipped grid shape cannot overflow")
            + VOLUME_LUT_BYTES
    }

    /// Every GPU texture the application budgets at once, worst case.
    ///
    /// **The loop term is not multiplied by the pane count, and that is the
    /// whole change.** It used to be `max_panes × a per-pane loop budget` plus
    /// a separate flat term for the 3D loop, which was the one loop kind whose
    /// grids lived in one application-wide store. Every loop kind is now: the
    /// pool is *divided* among the loops that want one, so there is a single
    /// loop term and it is the pool's ceiling.
    ///
    /// Multiplying it again would be the regression this test exists to catch,
    /// and so would giving the 3D loop a term of its own — that would be the
    /// double-count `the_3d_set_is_not_double_counted_across_two_panes` rules
    /// out, arrived at from the budget side.
    ///
    /// The offscreen term *is* still per pane, correctly: each 3D pane
    /// raymarches into its own target, and no two panes share one.
    fn app_texture_bytes(&self) -> usize {
        self.pool_ceiling + self.max_panes * self.offscreen_budget
    }
}

/// Every device class this workspace builds for, exactly once.
fn arms() -> [Arm; 3] {
    [
        Arm {
            name: "wasm32",
            image_size: WASM_IMAGE_SIZE,
            long_range_image_size: WASM_LONG_RANGE_IMAGE_SIZE,
            loop_image_size: WASM_LOOP_IMAGE_SIZE,
            section_width: WASM_SECTION_WIDTH,
            concurrent_renders: WASM_MAX_CONCURRENT_RENDERS,
            loop_frames: WASM_MAX_LOOP_FRAMES,
            render_budget: WASM_MAX_LOOP_RENDER_BUDGET,
            pool_floor: WASM_LOOP_POOL_FLOOR_BYTES,
            pool_ceiling: WASM_LOOP_POOL_CEILING_BYTES,
            grid: WASM_VOLUME_GRID_CELLS,
            volume_budget: WASM_VOLUME_TEXTURE_BUDGET_BYTES,
            volume_loop_frames: WASM_MAX_LOOP_VOLUME_FRAMES,
            volume_loop_budget: WASM_VOLUME_LOOP_TEXTURE_BUDGET_BYTES,
            offscreen_budget: WASM_VOLUME_OFFSCREEN_BUDGET_BYTES,
            max_panes: rustdar_egui::pane::MAX_PANES_DESKTOP,
            app_budget: WASM_APP_TEXTURE_BUDGET_BYTES,
        },
        Arm {
            name: "mobile",
            image_size: NATIVE_IMAGE_SIZE,
            long_range_image_size: MOBILE_LONG_RANGE_IMAGE_SIZE,
            loop_image_size: MOBILE_LOOP_IMAGE_SIZE,
            section_width: NATIVE_SECTION_WIDTH,
            concurrent_renders: MOBILE_MAX_CONCURRENT_RENDERS,
            loop_frames: MOBILE_MAX_LOOP_FRAMES,
            render_budget: MOBILE_MAX_LOOP_RENDER_BUDGET,
            pool_floor: MOBILE_LOOP_POOL_FLOOR_BYTES,
            pool_ceiling: MOBILE_LOOP_POOL_CEILING_BYTES,
            grid: MOBILE_VOLUME_GRID_CELLS,
            volume_budget: MOBILE_VOLUME_TEXTURE_BUDGET_BYTES,
            volume_loop_frames: MOBILE_MAX_LOOP_VOLUME_FRAMES,
            volume_loop_budget: MOBILE_VOLUME_LOOP_TEXTURE_BUDGET_BYTES,
            offscreen_budget: MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES,
            max_panes: rustdar_egui::pane::MAX_PANES_MOBILE,
            app_budget: MOBILE_APP_TEXTURE_BUDGET_BYTES,
        },
        Arm {
            name: "desktop",
            image_size: NATIVE_IMAGE_SIZE,
            long_range_image_size: DESKTOP_LONG_RANGE_IMAGE_SIZE,
            loop_image_size: DESKTOP_LOOP_IMAGE_SIZE,
            section_width: NATIVE_SECTION_WIDTH,
            concurrent_renders: DESKTOP_MAX_CONCURRENT_RENDERS,
            loop_frames: DESKTOP_MAX_LOOP_FRAMES,
            render_budget: DESKTOP_MAX_LOOP_RENDER_BUDGET,
            pool_floor: DESKTOP_LOOP_POOL_FLOOR_BYTES,
            pool_ceiling: DESKTOP_LOOP_POOL_CEILING_BYTES,
            grid: DESKTOP_VOLUME_GRID_CELLS,
            volume_budget: DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES,
            volume_loop_frames: DESKTOP_MAX_LOOP_VOLUME_FRAMES,
            volume_loop_budget: DESKTOP_VOLUME_LOOP_TEXTURE_BUDGET_BYTES,
            offscreen_budget: DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
            max_panes: rustdar_egui::pane::MAX_PANES_DESKTOP,
            app_budget: DESKTOP_APP_TEXTURE_BUDGET_BYTES,
        },
    ]
}

/// **One loop, on the worst device this target admits, gets exactly what one
/// pane used to get.**
///
/// The property that makes the pool safe to ship, and the reason
/// [`LOOP_POOL_FLOOR_BYTES`] carries the numbers the per-pane budget carried
/// rather than numbers of its own. Nobody with a single loop open loses a
/// frame of history on any device; what changed is that six of them no longer
/// cost six times it.
///
/// This is the table in [`LOOP_POOL_FLOOR_BYTES`]' doc comment, executed, on
/// **every** arm rather than the one this build compiled.
#[test]
fn one_loop_at_the_floor_is_exactly_what_a_pane_used_to_get() {
    for arm in arms() {
        let total = arm.textured_frames() * arm.loop_frame_bytes();
        assert!(
            total <= arm.pool_floor,
            "{}: {} textured frames x {}^2 x 4B = {} MiB, over the {} MiB floor \
             — a single loop on this target no longer gets the history it does \
             today",
            arm.name,
            arm.textured_frames(),
            arm.loop_image_size,
            total / (1024 * 1024),
            arm.pool_floor / (1024 * 1024),
        );
        // And a section loop, which is half the frame, comfortably so — the
        // pool is bytes, so this needs no table of its own any more.
        assert!(arm.textured_frames() * arm.section_frame_bytes() <= arm.pool_floor);
    }
}

/// **The floor seats a full screen of loops without blanking one.**
///
/// [`MIN_LOOP_FRAMES_PER_PANE`] is what makes the degradation smooth rather
/// than a cliff, and it is worth nothing unless the floor can actually pay for
/// it on every pane the width class admits. Without this, adding the last pane
/// on a browser would take a loop to zero frames, which reads as a bug and
/// which the user has no way to undo except by guessing.
///
/// wasm32's row is **exact** — six loops at two frames of 4 MiB is 48 MiB to
/// the byte — so this is the line a change to the browser floor, the minimum,
/// the pane count or the web image size has to come past.
#[test]
fn the_floor_seats_every_pane_without_blanking_one() {
    for arm in arms() {
        let needed = arm.max_panes * MIN_LOOP_FRAMES_PER_PANE * arm.loop_frame_bytes();
        assert!(
            needed <= arm.pool_floor,
            "{}: {} panes x {MIN_LOOP_FRAMES_PER_PANE} frames x {} MiB = {} MiB, \
             over the {} MiB floor — a full screen of loops would be cut below \
             the minimum and one of them would blank",
            arm.name,
            arm.max_panes,
            arm.loop_frame_bytes() / (1024 * 1024),
            needed / (1024 * 1024),
            arm.pool_floor / (1024 * 1024),
        );
    }
}

/// The bounds are a pair, and the floor is the one that wins.
///
/// `LoopPoolLimits::hold` is a `clamp`, and `clamp` **panics** on a crossed
/// pair — at startup, on one target only, which is the arm no host test can
/// reach. The compile-time block beside the constants asserts it for the
/// compiled arm; this is the other two.
#[test]
fn every_pool_ceiling_is_at_least_its_own_floor() {
    for arm in arms() {
        assert!(
            arm.pool_floor <= arm.pool_ceiling,
            "{}: a {} MiB floor above a {} MiB ceiling is a `clamp` that panics",
            arm.name,
            arm.pool_floor / (1024 * 1024),
            arm.pool_ceiling / (1024 * 1024),
        );
    }
}

/// A section loop can never be the binding case, and that is a property of the
/// raster's shape rather than of the numbers chosen for it.
///
/// `SECTION_HEIGHT` is half `SECTION_WIDTH`, and `SECTION_WIDTH` is pinned per
/// target at the same figure a loop frame takes, so a section frame is exactly
/// half a plan-view loop frame on every target. That used to fall out of both
/// following `IMAGE_SIZE`; the web plan view moving to 2048 while its loops and
/// sections stayed at 1024 is what turned it into two decisions that have to
/// agree. Pinned so that changing either has to come here and re-argue the
/// budget rather than quietly making a section loop the largest thing on the
/// screen.
#[test]
fn a_section_loop_frame_is_half_a_plan_view_one() {
    for arm in arms() {
        assert_eq!(
            arm.section_frame_bytes() * 2,
            arm.loop_frame_bytes(),
            "{}: a section loop frame is no longer half a plan-view one, so the \
             section rows of the LOOP_TEXTURE_BUDGET_BYTES table are wrong",
            arm.name,
        );
    }
}

/// The **3D volume** row of the loop table, executed.
///
/// The third loop kind, and the one whose frames are resident inputs rather
/// than cached pictures — so this is a claim about GPU texture memory that is
/// actually enforced at runtime by `VolumeStore::enforce_budget`, unlike the
/// two rows above.
#[test]
fn volume_loop_grids_fit_the_application_texture_budget() {
    for arm in arms() {
        let total = arm.volume_loop_frames * arm.volume_bytes();
        assert!(
            total <= arm.volume_loop_budget,
            "{}: {} resident grids x {} B = {:.1} MiB, over the {} MiB budget",
            arm.name,
            arm.volume_loop_frames,
            arm.volume_bytes(),
            total as f64 / (1024.0 * 1024.0),
            arm.volume_loop_budget / (1024 * 1024),
        );
    }
}

/// **A full 3D loop leaves room for one live 3D grid beside it.**
///
/// Fitting the set alone is not enough, and the ~1.5% of headroom desktop had
/// under it was not slack — it was a defect one ordinary layout away. A second
/// 3D pane showing a live volume is one more grid in the same
/// application-wide store, and `VolumeStore::enforce_budget` evicts **oldest
/// first**: the loop started first, so what goes is the loop's frame 0, not
/// the live grid that pushed the store over. The dispatcher then re-plans that
/// frame on the very next pass, rebuilds it (a frame-thread extraction and an
/// ~89 ms worker resample), and the store evicts frame 1 to make room. That is
/// a permanent rebuild treadmill with a hot CPU and a loop visibly missing a
/// frame as its only symptoms.
///
/// So the frame count is chosen against `budget − one grid`, which
/// `the_3d_loop_holds_exactly_what_it_marches` computes. This is the same
/// claim stated as the property rather than as the formula, because it is the
/// property that is load-bearing: the formula could be changed to agree with a
/// wrong count and this would still fail.
///
/// It bounds the *reachable* layouts rather than every conceivable one — a
/// third distinct live grid is over the line again, and the eviction's answer
/// there is the same. What it buys is that the common two-pane case never
/// reaches the eviction at all.
#[test]
fn a_full_3d_loop_leaves_room_for_a_live_grid_beside_it() {
    for arm in arms() {
        let resident = arm.volume_loop_frames * arm.volume_bytes();
        assert!(
            resident + arm.volume_bytes() <= arm.volume_loop_budget,
            "{}: {} resident grids + one live grid = {:.1} MiB against a {} MiB \
             budget, so a 3D loop beside a live 3D pane makes the store evict \
             the loop's own oldest frame and rebuild it for ever",
            arm.name,
            arm.volume_loop_frames,
            (resident + arm.volume_bytes()) as f64 / (1024.0 * 1024.0),
            arm.volume_loop_budget / (1024 * 1024),
        );
    }
}

/// A 3D loop holds exactly what it marches: the frame list **is** the resident
/// set.
///
/// The other two loop kinds hold `MAX_LOOP_FRAMES` and texture
/// `MAX_LOOP_RENDER_BUDGET` of them, re-rendering as the playhead walks back
/// into a window it had left. Re-entering a resident 3D window costs ~140 ms
/// against a 200 ms interval at `DEFAULT_LOOP_SPEED_FPS` and 33 ms at
/// `MAX_LOOP_SPEED_FPS`, so that treadmill does not close here.
///
/// What this pins is that `MAX_LOOP_VOLUME_FRAMES` is *one* number rather than
/// a held count and a resident count that could drift apart — and that it is at
/// or under the budget the arm above just checked, with no second number in
/// between. The dispatcher reads the same constant for both, and
/// `app_render::volume_loop_tests` drives it end to end.
#[test]
fn the_3d_loop_holds_exactly_what_it_marches() {
    for arm in arms() {
        assert!(
            arm.volume_loop_frames >= 2,
            "{}: a one-frame loop is not a loop",
            arm.name,
        );
        // The frame count is the tighter of two bounds, computed rather than
        // restated, and it is an *equality* — a list shorter than both bounds
        // allow is history thrown away for nothing, and a longer one is the
        // treadmill this loop kind cannot afford.
        //
        //  * what the byte budget admits **beside one live grid**, which binds
        //    desktop (13 of the 30 frames a plan-view loop textures). The
        //    subtracted grid is not padding: see
        //    `a_full_3d_loop_leaves_room_for_a_live_grid_beside_it` for the
        //    layout it is there for and what happens without it.
        //  * `MAX_LOOP_RENDER_BUDGET`, which binds wasm32 and mobile — a 3D
        //    loop is not licensed to hold *more* history than the plan-view
        //    loop beside it on the same device just because its grids happen
        //    to be small there.
        let admits = arm.volume_loop_budget.saturating_sub(arm.volume_bytes()) / arm.volume_bytes();
        assert_eq!(
            arm.volume_loop_frames,
            admits.min(arm.render_budget),
            "{}: the budget admits {admits} grids and the loop render budget is \
             {}, so the frame list should be their minimum, not {}",
            arm.name,
            arm.render_budget,
            arm.volume_loop_frames,
        );
    }
}

/// The whole application's GPU texture memory, against a ceiling — the line
/// nothing drew before, because the pane count and the per-pane budgets live in
/// different crates.
///
/// This is the table in [`APP_TEXTURE_BUDGET_BYTES`]' doc, executed. It fails
/// if `MAX_PANES_DESKTOP` grows, if any per-pane budget grows, or if the 3D
/// loop's grids are ever made per-pane instead of application-wide.
#[test]
fn the_whole_application_fits_its_gpu_ceiling() {
    for arm in arms() {
        let total = arm.app_texture_bytes();
        assert!(
            total <= arm.app_budget,
            "{}: a {} MiB loop pool + {} panes x {} MiB of raymarch offscreen = \
             {} MiB, over the {} MiB whole-application ceiling",
            arm.name,
            arm.pool_ceiling / (1024 * 1024),
            arm.max_panes,
            arm.offscreen_budget / (1024 * 1024),
            total / (1024 * 1024),
            arm.app_budget / (1024 * 1024),
        );
    }
}

/// The whole-application ceiling is snug, on the same reasoning
/// `the_volume_budget_is_not_slack_enough_to_hide_a_doubling` gives: a ceiling
/// several times the real figure passes the check above while admitting a
/// silent doubling of any term inside it.
///
/// 1.25x, which is tighter than the ~1.33x the per-subsystem budgets keep,
/// because this one is a sum of already-padded figures rather than a raw
/// allocation with alignment overhead to absorb.
#[test]
fn the_app_ceiling_is_not_slack_enough_to_hide_a_doubling() {
    for arm in arms() {
        let total = arm.app_texture_bytes();
        assert!(
            arm.app_budget * 4 <= total * 5,
            "{}: the {} MiB ceiling is more than 1.25x the {} MiB it bounds, so \
             a term inside it could double unnoticed",
            arm.name,
            arm.app_budget / (1024 * 1024),
            total / (1024 * 1024),
        );
    }
}

/// The 3D loop's pacing cap is a real cap, on the same terms
/// [`MAX_LOOP_SECTION_CUTS_PER_FRAME`]'s is.
///
/// The upper bound is the whole point: a cap at or above
/// [`MAX_CONCURRENT_RENDERS`] would let one dispatch pass run every
/// `extract_volume_parts` it could start back to back on the frame that starts
/// the loop, which is exactly the hitch the constant exists to prevent.
#[test]
fn the_volume_build_cap_paces_rather_than_stalls() {
    const { assert!(MAX_LOOP_VOLUME_BUILDS_PER_FRAME >= 1) };
    for arm in arms() {
        assert!(
            MAX_LOOP_VOLUME_BUILDS_PER_FRAME <= arm.concurrent_renders,
            "{}: the per-frame build cap ({MAX_LOOP_VOLUME_BUILDS_PER_FRAME}) is \
             above the concurrent render budget ({}), so it caps nothing",
            arm.name,
            arm.concurrent_renders,
        );
    }
}

/// The pacing cap is a real cap: at least one cut per pass, and fewer than the
/// concurrent render budget on every arm.
///
/// The lower bound is what makes a section loop progress at all; the upper is
/// the whole point of the constant, since a cap at or above
/// `MAX_CONCURRENT_RENDERS` would let a dispatch pass run every extraction it
/// could start back to back on one frame — which is the hitch the cap exists to
/// prevent. See [`MAX_LOOP_SECTION_CUTS_PER_FRAME`].
#[test]
fn the_section_cut_cap_paces_rather_than_stalls() {
    const { assert!(MAX_LOOP_SECTION_CUTS_PER_FRAME >= 1) };
    for arm in arms() {
        assert!(
            MAX_LOOP_SECTION_CUTS_PER_FRAME <= arm.concurrent_renders,
            "{}: the per-frame cut cap ({MAX_LOOP_SECTION_CUTS_PER_FRAME}) is \
             above the concurrent render budget ({}), so it caps nothing",
            arm.name,
            arm.concurrent_renders,
        );
    }
}

/// The budget is meant to be snug. A ceiling several times the real figure would
/// pass the check above while permitting a silent doubling of any constant in it.
#[test]
fn the_budget_is_not_slack_enough_to_hide_a_doubling() {
    for arm in arms() {
        let total = arm.textured_frames() * arm.loop_frame_bytes();
        assert!(
            total * 2 > arm.pool_floor,
            "{}: floor {} MiB is more than twice the {} MiB one full loop costs \
                 — it would not catch a regression, and it would mean the floor \
                 is no longer 'what one pane used to get'",
            arm.name,
            arm.pool_floor / (1024 * 1024),
            total / (1024 * 1024),
        );
    }
}

/// The eviction budget is what bounds memory, so it has to be the smaller of the
/// two. If it ever exceeded the frame cap, `render_set_indices` would clamp it
/// back to the frame count and every held frame would stay textured — silently
/// restoring the `MAX_LOOP_FRAMES × frame` figure the budget above rules out.
/// The ordering itself is asserted at compile time next to the constants — but
/// only for the compiled arm, which is why it is asserted for all three here.
#[test]
fn the_render_budget_is_what_bounds_the_textured_frames() {
    for arm in arms() {
        assert_eq!(arm.textured_frames(), arm.render_budget, "{}", arm.name);
        // A zero anywhere in the cascade is a loop that renders nothing, and
        // the compile-time block next to the constants only sees one arm.
        assert!(arm.render_budget > 0, "{}", arm.name);
        assert!(arm.concurrent_renders > 0, "{}", arm.name);
    }
}

/// Every arm is held to its own volume budget, exactly as
/// `one_loop_at_the_floor_is_exactly_what_a_pane_used_to_get` holds it to its
/// loop budget.
#[test]
fn the_volume_grid_fits_the_target_texture_budget() {
    for arm in arms() {
        let total = arm.volume_bytes();
        assert!(
            total <= arm.volume_budget,
            "{}: a {:?} grid plus a {VOLUME_LUT_BYTES} B table is {total} B, \
                 over the {} B budget",
            arm.name,
            arm.grid,
            arm.volume_budget,
        );
    }
}

/// The sibling of `the_budget_is_not_slack_enough_to_hide_a_doubling`, and for
/// the same reason: a ceiling several times the real figure passes the check
/// above while permitting any axis to be silently doubled.
///
/// Doubling one axis is the realistic regression here, not doubling the whole
/// grid — and it is exactly what this catches, because doubling any single
/// axis doubles the total.
///
/// **What it no longer covers, since the grid's shape became a runtime
/// answer.** `arm.grid` is the *budget* triple now, not the shape the frontend
/// requests, so this is a claim about two constants: that the ceiling is snug
/// against the budget. It is still worth making — a loose ceiling is how a term
/// inside it doubles unnoticed — but the tripwire half, the one that would have
/// caught the Android build asking for 2.4× what it budgeted, has moved to
/// [`the_requested_shape_never_outgrows_the_budget_it_was_computed_against`],
/// which sweeps what a device is actually asked for against what its arm was
/// sized at. Neither is redundant: this one binds the ceiling to the budget,
/// that one binds the request to the budget.
#[test]
fn the_volume_budget_is_not_slack_enough_to_hide_a_doubling() {
    for arm in arms() {
        let total = arm.volume_bytes();
        assert!(
            total * 2 > arm.volume_budget,
            "{}: budget {} B is more than twice the actual {total} B — it \
                 would not catch a doubled grid axis",
            arm.name,
            arm.volume_budget,
        );
    }
}

/// The literals behind the tables in the two budget doc comments.
///
/// The invariants above are relations, and a relation holds just as well
/// after both of its sides move together — which is the one change they
/// cannot see. `the_grid_dimensions_match_the_shapes_rustdar_radar_names`
/// pins the grid triples for the same reason; this is the rest of the row.
#[test]
fn the_documented_per_class_figures_are_what_the_arms_actually_say() {
    let expected = [
        // name, base, long range, loop, section width, concurrent, held,
        // textured, pool floor MiB, pool ceiling MiB, volume budget B
        (
            "wasm32",
            2048,
            2048,
            1024,
            1024,
            1,
            12,
            8,
            48,
            192,
            6 * 1024 * 1024,
        ),
        (
            "mobile",
            2048,
            4096,
            2048,
            2048,
            3,
            20,
            12,
            256,
            640,
            20 * 1024 * 1024,
        ),
        (
            "desktop",
            2048,
            4096,
            2048,
            2048,
            6,
            60,
            30,
            512,
            3072,
            48 * 1024 * 1024,
        ),
    ];
    for (
        arm,
        (
            name,
            image,
            long_range,
            loop_image,
            section_width,
            concurrent,
            held,
            textured,
            floor_mib,
            ceiling_mib,
            volume,
        ),
    ) in arms().into_iter().zip(expected)
    {
        assert_eq!(arm.name, name);
        assert_eq!(arm.image_size, image, "{name} image size");
        assert_eq!(
            arm.long_range_image_size, long_range,
            "{name} long-range image size"
        );
        assert_eq!(arm.loop_image_size, loop_image, "{name} loop image size");
        assert_eq!(arm.section_width, section_width, "{name} section width");
        // The three sides a plan-view raster can have on this class, ordered.
        // The loop never exceeds the base — a loop frame is the same picture
        // or a leaner one, never a larger one — and the long-range ceiling
        // never falls under it, or `raster_side_px` would shrink a raster the
        // moment its sweep reached further.
        assert!(
            arm.loop_image_size <= arm.image_size,
            "{name}: a loop frame is larger than a still one"
        );
        assert!(
            arm.image_size <= arm.long_range_image_size,
            "{name}: the long-range ceiling is under the base size"
        );
        assert_eq!(arm.concurrent_renders, concurrent, "{name} renders");
        assert_eq!(arm.loop_frames, held, "{name} held frames");
        assert_eq!(arm.render_budget, textured, "{name} render budget");
        assert_eq!(arm.pool_floor, floor_mib * 1024 * 1024, "{name} pool floor");
        assert_eq!(
            arm.pool_ceiling,
            ceiling_mib * 1024 * 1024,
            "{name} pool ceiling"
        );
        assert_eq!(arm.volume_budget, volume, "{name} volume budget");
    }
}

/// This target's cascades all selected the *same* arm as each other.
///
/// `cfg`-gated, because the selection is the one thing here no other target
/// can check on behalf of this one — and it is a real hazard rather than a
/// formality: the arms are six near-identical `#[cfg(all(…))]` lines per
/// constant, and a mismatched one gives a build a mobile frame budget with
/// a desktop texture ceiling, which passes every invariant above.
#[test]
fn every_cascade_in_this_file_selected_the_same_arm() {
    #[cfg(target_arch = "wasm32")]
    let arm = &arms()[0];
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    let arm = &arms()[1];
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    let arm = &arms()[2];

    assert_eq!(IMAGE_SIZE, arm.image_size, "{}", arm.name);
    assert_eq!(
        MAX_CONCURRENT_RENDERS, arm.concurrent_renders,
        "{}",
        arm.name
    );
    assert_eq!(MAX_LOOP_FRAMES, arm.loop_frames, "{}", arm.name);
    assert_eq!(MAX_LOOP_RENDER_BUDGET, arm.render_budget, "{}", arm.name);
    assert_eq!(LOOP_POOL_FLOOR_BYTES, arm.pool_floor, "{}", arm.name);
    assert_eq!(LOOP_POOL_CEILING_BYTES, arm.pool_ceiling, "{}", arm.name);
    assert_eq!(VOLUME_GRID_CELLS, arm.grid, "{}", arm.name);
    assert_eq!(
        VOLUME_TEXTURE_BUDGET_BYTES, arm.volume_budget,
        "{}",
        arm.name
    );
}

/// The `(cfg attribute, right-hand side)` of every `#[cfg]`-gated
/// definition of `name`, in source order.
fn cascade_arms(code: &str, name: &str) -> Vec<(String, String)> {
    let definition = format!("pub const {name}: ");
    let lines: Vec<&str> = code.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with(&definition))
        .map(|(i, line)| {
            let (_, rhs) = line
                .split_once(" = ")
                .unwrap_or_else(|| panic!("{name} has no right-hand side: {line}"));
            let cfg = lines[..i]
                .iter()
                .rev()
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with("//"))
                .unwrap_or_else(|| panic!("nothing at all precedes {name}"));
            (
                cfg.to_string(),
                rhs.trim().trim_end_matches(';').to_string(),
            )
        })
        .collect()
}

/// The name of every `const` whose wasm32 arm this file declares, sorted
/// and deduplicated.
///
/// Keyed on the wasm32 arm because that is the one no build on this machine
/// compiles. Two-arm `mobile` / `not(mobile)` cascades — the download and
/// render-cache caps — have no `target_arch` arm at all, so a host build
/// picks between the same two values a phone build would and they are not
/// device-class cascades in this sense.
///
/// Three near-misses this deliberately does *not* have, each of which was a
/// way to add a cascade the census could not see:
///
/// - **a doc comment between the attribute and the item.** Legal Rust,
///   `fmt`-clean, and a look at line `i + 1` alone walks straight past it.
///   So the look-ahead skips `///`, `//` and blank lines, exactly as
///   [`cascade_arms`] already does looking *back*.
/// - **`const` without `pub`, or an indented one.** Neither changes that the
///   value is `cfg`-selected.
/// - **a wasm arm spelled some other way**, e.g. `all(target_arch =
///   "wasm32")`. Matched on content rather than byte-for-byte: any `cfg`
///   naming the wasm arch, other than the `not(...)` guard the sibling arms
///   carry. The per-name check below then insists on the canonical spelling,
///   so an odd one fails there rather than vanishing here.
fn wasm_gated_constants(code: &str) -> Vec<&str> {
    let lines: Vec<&str> = code.lines().collect();
    let is_wasm_arm = |line: &str| {
        let line = line.trim();
        line.starts_with("#[cfg(")
            && line.contains(r#"target_arch = "wasm32""#)
            && !line.contains(r#"not(target_arch = "wasm32")"#)
    };
    let mut names: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_wasm_arm(line))
        .filter_map(|(i, _)| {
            lines[i + 1..]
                .iter()
                .map(|l| l.trim_start())
                .find(|l| !l.is_empty() && !l.starts_with("//"))
        })
        .map(|item| item.strip_prefix("pub ").unwrap_or(item))
        .filter_map(|item| item.strip_prefix("const "))
        .filter_map(|rest| rest.split_once(':'))
        .map(|(name, _)| name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Every `cfg` arm selects the constant named for *its own* device class.
///
/// `every_cascade_in_this_file_selected_the_same_arm` covers this for the
/// arm the running target compiles and can cover no other. That is not a
/// theoretical gap: pointing the wasm32 arm of `MAX_LOOP_FRAMES` at
/// `DESKTOP_MAX_LOOP_FRAMES` leaves every test in this workspace passing
/// and the wasm `cargo check` exiting 0, because nothing on a host ever
/// evaluates that line. It is the one mutation that survived the probe run
/// that landed these tests, which is why this exists.
///
/// So read the cascades as source instead. Three arms per constant in one
/// fixed shape: the `cfg` picks the device class, and the right-hand side
/// has to name the constant for that class. Reading the source is the weak
/// form of the check — it cannot see a wrongly *valued* constant, which is
/// what every test above is for — but it is the only form available without
/// a wasm test runner.
#[test]
fn every_cfg_arm_selects_the_constant_named_for_its_device_class() {
    let source = include_str!("../constants.rs");
    // The shipped half only: the expected strings below appear verbatim in
    // this test's own source.
    let (code, _) = source
        .split_once("#[cfg(test)]")
        .expect("constants.rs no longer has a test module");

    let expected = [
        (r#"#[cfg(target_arch = "wasm32")]"#, "WASM"),
        (
            r#"#[cfg(all(not(target_arch = "wasm32"), mobile))]"#,
            "MOBILE",
        ),
        (
            r#"#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]"#,
            "DESKTOP",
        ),
    ];

    let covered = [
        // The two raster-size cascades. `IMAGE_SIZE` itself is not here — it
        // is `rustdar_radar`'s, and its own crate's
        // `each_cfg_arm_selects_the_image_size_named_for_it` reads it the same
        // way.
        "LONG_RANGE_IMAGE_SIZE",
        "LOOP_IMAGE_SIZE",
        "MAX_CONCURRENT_RENDERS",
        "MAX_LOOP_RENDER_BUDGET",
        "MAX_LOOP_FRAMES",
        // The pool's two bounds, landed when the loop budget stopped being a
        // per-pane allowance. Both are cascades, so both have to be here.
        "LOOP_POOL_FLOOR_BYTES",
        "LOOP_POOL_CEILING_BYTES",
        "VOLUME_GRID_CELLS",
        "VOLUME_TEXTURE_BUDGET_BYTES",
        // Lifted by WP-I after this test first listed it as exempt. It is
        // covered here as well as by
        // `each_offscreen_budget_arm_selects_its_own_classs_constant`; the
        // overlap is deliberate, because that test checks one cascade and
        // this one checks that no cascade is missing.
        "VOLUME_OFFSCREEN_BUDGET_BYTES",
        // The 3D loop's two cascades, landed with it.
        "MAX_LOOP_VOLUME_FRAMES",
        "APP_TEXTURE_BUDGET_BYTES",
        // Lifted into three arms when the pane mirror gained an adaptive rung:
        // the ceiling stopped being "the guaranteed texture cap squared" (one
        // figure, true everywhere) and became a per-target decision about how
        // much supersampling a 3D floor is worth.
        "VOLUME_MIRROR_BYTES_MAX",
    ];

    // Cascades that still spell their arms as literals, and so cannot be
    // checked here. Written down rather than left implicit: a test named
    // "every cfg arm" that silently covered six of seven would be the same
    // shape of vacuity it exists to catch. Empty today, and the mechanism
    // stays because the next cascade to land will need it before it is
    // lifted — as `VOLUME_OFFSCREEN_BUDGET_BYTES` did for one commit.
    let exempt: [&str; 0] = [];

    // Every three-arm cascade in the file is one or the other, so adding a
    // new one is a failure here rather than a silent gap.
    let found = wasm_gated_constants(code);
    let mut accounted: Vec<&str> = covered.iter().chain(exempt.iter()).copied().collect();
    accounted.sort_unstable();
    assert_eq!(
        found, accounted,
        "the set of `cfg`-selected constants in this file has changed. A \
             new one has to be lifted into named arms and listed in `covered`, \
             or listed in `exempt` with the reason it cannot be."
    );

    // An exemption has to still *be* one. The rot that matters runs the
    // other way from the obvious one: a cascade gets lifted and nobody
    // moves it out of `exempt`, so it looks accounted for while its arms go
    // unchecked — which is exactly what happened to
    // `VOLUME_OFFSCREEN_BUDGET_BYTES` between this test landing and WP-I
    // lifting it, and the census did not notice. A lifted arm's right-hand
    // side is a bare `SCREAMING_CASE` name; a literal never is.
    for name in exempt {
        for (cfg, rhs) in cascade_arms(code, name) {
            assert!(
                !rhs.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "the {cfg} arm of {name} selects `{rhs}`, which is a named \
                     constant, so {name} has been lifted. Move it from `exempt` \
                     to `covered` — while it sits here its arms are checked by \
                     nothing."
            );
        }
    }

    for name in covered {
        let arms = cascade_arms(code, name);
        assert_eq!(
            arms.len(),
            expected.len(),
            "{name} has {} `cfg` arms, not {}: {arms:?}. The three-arm shape \
                 is what keeps them mutually exclusive — see MAX_LOOP_FRAMES' \
                 doc comment.",
            arms.len(),
            expected.len(),
        );
        for ((cfg, rhs), (want_cfg, class)) in arms.iter().zip(expected) {
            assert_eq!(cfg, want_cfg, "{name}");
            assert_eq!(
                rhs,
                &format!("{class}_{name}"),
                "the {cfg} arm of {name} selects `{rhs}`, which is not the \
                     {class} value. No host build can evaluate this line."
            );
        }
    }
}

/// The web image fits what a browser is *guaranteed* to accept.
///
/// `rustdar_radar` states the 2048 floor as a literal because it has no wgpu
/// dependency and must not grow one — it hands finished RGBA buffers to the
/// crate that owns the GPU. This is that crate, so this is where the floor
/// gets checked against wgpu's own downlevel limits rather than against a
/// number someone typed. Without it, `WEBGL2_MAX_TEXTURE_DIMENSION_2D` could
/// be raised to accommodate an over-large image instead of the image being
/// the thing that gives.
#[test]
fn the_web_image_fits_the_texture_size_webgl2_guarantees() {
    let guaranteed = wgpu::Limits::downlevel_webgl2_defaults().max_texture_dimension_2d;
    assert_eq!(
        rustdar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D as u32,
        guaranteed,
        "rustdar_radar's copy of the WebGL2 2D floor has drifted from wgpu's"
    );
    assert!(
        WASM_IMAGE_SIZE as u32 <= guaranteed,
        "the web radar image is {WASM_IMAGE_SIZE} px, over the {guaranteed} px \
             2D texture WebGL2 guarantees — every browser render would fail"
    );
    // The web arm sits *on* the guarantee rather than under it, and that is
    // the decision: `max_texture_dimension_2d` bounds each texture's each
    // axis, not a frame's total, and the overlay textures beside the radar
    // frame are sized from the viewport and clamped against the same limit
    // independently (`plan_overlay_texture`). The earlier ×2 headroom rule
    // was a policy resting on a misreading of the limit.
    assert_eq!(WASM_IMAGE_SIZE as u32, guaranteed);
    // Which is also why the web arm's long-range ceiling has to *be* the
    // guarantee: there is nothing above it to grow into, so `raster_side_px`
    // answers one size on the web and every browser render is exactly the size
    // every browser must accept. Inert in the *side* only — the extent is the
    // data's on every target, so a browser draws a 300.11 km Doppler cut on
    // these 2048 pixels at 3.4121 px/km rather than the floor's 4.4522.
    assert_eq!(
        WASM_LONG_RANGE_IMAGE_SIZE as u32, guaranteed,
        "the web long-range ceiling is over what WebGL2 guarantees, so a \
             long-reaching sweep would fail to upload in some browser"
    );
    // And the web loop frame is under it by construction.
    assert!(WASM_LOOP_IMAGE_SIZE as u32 <= guaranteed);
}

/// The reference pane fits this target's offscreen budget **at its own
/// quality ceiling**, i.e. without being degraded to get there.
///
/// The sibling of `the_volume_grid_fits_the_target_texture_budget`, with
/// one extra assertion it does not need: the grid either fits or it does
/// not, whereas the offscreen would silently step down a rung. A budget
/// that forced the reference pane to degrade would pass a plain "fits"
/// check while quietly halving the resolution of every volume on a display
/// this target is meant to render at full size.
#[test]
fn the_reference_pane_fits_the_target_offscreen_budget_undegraded() {
    let fitted = crate::volume::quality::reference_offscreen();
    assert!(
        fitted.bytes() <= VOLUME_OFFSCREEN_BUDGET_BYTES,
        "a {:?} offscreen is {} B, over the {VOLUME_OFFSCREEN_BUDGET_BYTES} \
             B budget",
        fitted.size,
        fitted.bytes(),
    );
    assert_eq!(
        fitted.quality,
        crate::volume::quality::PLATFORM_CEILING,
        "the {VOLUME_OFFSCREEN_REFERENCE_PANE_PX:?} reference pane cannot be \
             rendered at this target's own quality ceiling within a \
             {VOLUME_OFFSCREEN_BUDGET_BYTES} B budget, so the ceiling describes \
             a quality the budget never lets anything select"
    );
}

/// And the offscreen budget is snug, exactly as the other two are.
///
/// The realistic regression is the reference pane growing or the ceiling
/// moving up a rung — both of which double the figure, and both of which a
/// budget several times the real number would absorb without a word.
#[test]
fn the_offscreen_budget_is_not_slack_enough_to_hide_a_doubling() {
    let total = crate::volume::quality::reference_offscreen().bytes();
    assert!(
        total * 2 > VOLUME_OFFSCREEN_BUDGET_BYTES,
        "budget {VOLUME_OFFSCREEN_BUDGET_BYTES} B is more than twice the \
             actual {total} B — it would not catch a doubled reference pane"
    );
}

/// Both offscreen budget checks, on **all three** arms rather than the one
/// this build compiled.
///
/// The two tests above are one-sided in exactly the way
/// `the_grid_dimensions_match_the_shapes_rustdar_radar_names` was before
/// `3292e8d`: they read `VOLUME_OFFSCREEN_BUDGET_BYTES` and
/// `PLATFORM_CEILING`, both `cfg`-selected, so two of three arms went
/// unchecked. A budget that could not pay for its own reference pane on
/// wasm would be a browser whose every volume is quietly rendered a rung
/// coarser than intended, and no CI row would say so.
///
/// The pairing is the point: each arm is checked against **its own**
/// ceiling, because the ceiling is what decides how many pixels the
/// reference pane costs there.
#[test]
fn every_offscreen_budget_arm_pays_for_its_own_reference_pane() {
    use crate::volume::quality::{
        DESKTOP_PLATFORM_CEILING, MOBILE_PLATFORM_CEILING, WASM_PLATFORM_CEILING,
    };

    for (target, budget, ceiling) in [
        (
            "wasm",
            WASM_VOLUME_OFFSCREEN_BUDGET_BYTES,
            WASM_PLATFORM_CEILING,
        ),
        (
            "mobile",
            MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES,
            MOBILE_PLATFORM_CEILING,
        ),
        (
            "desktop",
            DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
            DESKTOP_PLATFORM_CEILING,
        ),
    ] {
        let fitted = ceiling.fit(VOLUME_OFFSCREEN_REFERENCE_PANE_PX, budget);
        assert_eq!(
            fitted.quality, ceiling,
            "the {target} budget of {budget} B cannot render the \
                 {VOLUME_OFFSCREEN_REFERENCE_PANE_PX:?} reference pane at its \
                 own {ceiling:?} ceiling — it degrades to {:?}, so the ceiling \
                 names a quality that target never reaches",
            fitted.quality
        );
        assert!(
            fitted.bytes() <= budget,
            "the {target} offscreen is {} B against a {budget} B budget",
            fitted.bytes()
        );
        assert!(
            fitted.bytes() * 2 > budget,
            "the {target} budget of {budget} B is more than twice its \
                 actual {} B — it would not catch a doubled reference pane",
            fitted.bytes()
        );
    }
}

/// Each offscreen budget arm selects **its own** class's constant.
///
/// Naming the arms outside the cascade pins their values and nothing else:
/// pointing the wasm32 arm at `DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES` was
/// measured to leave the whole workspace green with the wasm
/// `--all-targets` check at 0, because on a host the other two arms are
/// dead text. Reading the source is the only instrument that sees it.
///
/// Shares its reasoning, and its shape, with
/// `volume::quality::each_ceiling_arm_selects_its_own_classs_constant`.
#[test]
fn each_offscreen_budget_arm_selects_its_own_classs_constant() {
    let source = include_str!("../constants.rs");
    for (cfg, class) in [
        (r#"target_arch = "wasm32""#, "WASM"),
        (r#"all(not(target_arch = "wasm32"), mobile)"#, "MOBILE"),
        (
            r#"all(not(target_arch = "wasm32"), not(mobile))"#,
            "DESKTOP",
        ),
    ] {
        let definition = format!("#[cfg({cfg})]\npub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize =");
        let occurrences = source.matches(&definition).count();
        assert_eq!(
            occurrences, 1,
            "expected exactly one VOLUME_OFFSCREEN_BUDGET_BYTES definition \
                 under `#[cfg({cfg})]`, found {occurrences}"
        );
        let at = source.find(&definition).expect("just counted one");
        let (selected, _) = source[at + definition.len()..]
            .split_once(';')
            .expect("a const definition with no semicolon");
        let expected = format!("{class}_VOLUME_OFFSCREEN_BUDGET_BYTES");
        assert_eq!(
            selected.trim(),
            expected,
            "the `#[cfg({cfg})]` arm does not select `{expected}`. An arm \
                 pointing at another class's budget compiles and passes \
                 everything CI runs."
        );
    }
}

/// The compiled cascade selects one of the three named budgets.
///
/// Weaker than the scrape above and kept anyway: it is the one assertion
/// that survives the source being reformatted out from under the scrape.
#[test]
fn the_compiled_offscreen_budget_is_one_of_the_named_arms() {
    assert!(
        [
            WASM_VOLUME_OFFSCREEN_BUDGET_BYTES,
            MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES,
            DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
        ]
        .contains(&VOLUME_OFFSCREEN_BUDGET_BYTES),
        "VOLUME_OFFSCREEN_BUDGET_BYTES is {VOLUME_OFFSCREEN_BUDGET_BYTES}, \
             which is none of the three named arms"
    );
}

/// The WebGL2 3D-texture floor is wgpu's figure, not a hand-written 256.
///
/// Comparing the *value* against wgpu proves nothing on its own: a
/// `= 256;` literal satisfies that assertion exactly, because 256 is what
/// wgpu says today. What makes the constant honest is where it comes from, and
/// only the source says that. The realistic regression is someone replacing
/// the derivation with the literal in order to drop the `wgpu` import from
/// this file — at which point the doc comment above becomes false and the
/// bound stops tracking the limits the device request is held to.
#[test]
fn the_webgl2_3d_limit_is_derived_from_wgpu_rather_than_written_out() {
    let source = include_str!("../constants.rs");
    let definition = source
        .split_once("pub const WEBGL2_MAX_TEXTURE_DIMENSION_3D: u32 =")
        .and_then(|(_, rest)| rest.split_once(';'))
        .map(|(value, _)| value)
        .expect("WEBGL2_MAX_TEXTURE_DIMENSION_3D is no longer defined here");
    assert!(
        definition.contains("downlevel_webgl2_defaults()")
            && definition.contains("max_texture_dimension_3d"),
        "WEBGL2_MAX_TEXTURE_DIMENSION_3D is defined as `{}`, which does not \
             read wgpu's own WebGL2 downlevel limits. A literal cannot drift \
             *with* wgpu, so it stops describing what the device request is held \
             to the moment wgpu revises the figure.",
        definition.trim()
    );

    // And 256 is still what that derivation yields. Separate assertion so a
    // wgpu bump that raised the floor is a visible failure to be reviewed,
    // rather than a grid bound that silently loosened.
    assert_eq!(WEBGL2_MAX_TEXTURE_DIMENSION_3D, 256);
}

/// [`VOLUME_GRID_CELLS`] and `rustdar_radar::voxel`'s named shapes are two
/// hand-maintained copies of the same three triples, in two crates.
///
/// The split is forced, not accidental: only *this* crate has a `build.rs`
/// emitting `mobile`, so only this crate can pick the middle arm — while
/// the grid is *built* in `rustdar-radar`, which therefore has to name all
/// three as plain constants and let a caller choose. `voxel::default_shape`
/// says as much and deliberately cannot return the mobile one.
///
/// Two copies that agree today is exactly the shape of the
/// `needs_whole_volume` / `RenderInput::extract` divergence this campaign
/// already paid for once, where the copies were "obviously" the same until
/// one of them was not. They agree; this is what keeps them agreeing, and
/// it checks **all three** arms rather than only the one this target
/// compiles, because the arm a host build skips is the one nothing else
/// would catch.
#[test]
fn the_grid_dimensions_match_the_shapes_rustdar_radar_names() {
    use rustdar_radar::voxel::{DESKTOP_SHAPE, LUT_LEN, MOBILE_SHAPE, VoxelShape, WASM_SHAPE};

    let triple = |s: VoxelShape| [s.nx as u32, s.ny as u32, s.nz as u32];

    // **All three arms, unconditionally.** The first version of this test
    // bound only the arm the running target compiled, which left two of
    // the three free to drift — a reviewer changed the wasm triple to
    // `[160, 160, 80]` and the entire workspace suite passed 1507/0 with
    // the wasm `--all-targets` check exiting 0. Both sides are now named
    // constants, so both sides are reachable from any host.
    assert_eq!(WASM_VOLUME_GRID_CELLS, triple(WASM_SHAPE));
    assert_eq!(MOBILE_VOLUME_GRID_CELLS, triple(MOBILE_SHAPE));
    assert_eq!(DESKTOP_VOLUME_GRID_CELLS, triple(DESKTOP_SHAPE));

    // Pinned literals as well as the binding, so that editing *both* sides
    // in step — the one change the comparison above cannot see — still has
    // to be deliberate.
    assert_eq!(WASM_VOLUME_GRID_CELLS, [128, 128, 64]);
    assert_eq!(MOBILE_VOLUME_GRID_CELLS, [192, 192, 96]);
    assert_eq!(DESKTOP_VOLUME_GRID_CELLS, [256, 256, 128]);

    // And that this target's cascade selected the matching one. This half
    // *is* cfg-gated, because the cascade is the one thing here that no
    // other target can check on its behalf.
    #[cfg(target_arch = "wasm32")]
    assert_eq!(VOLUME_GRID_CELLS, WASM_VOLUME_GRID_CELLS);
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    assert_eq!(VOLUME_GRID_CELLS, MOBILE_VOLUME_GRID_CELLS);
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    assert_eq!(VOLUME_GRID_CELLS, DESKTOP_VOLUME_GRID_CELLS);

    // Every axis must clear the WebGL2 floor on **every** arm, not just
    // this one — that bound is the reason the triples are what they are,
    // and it was previously checked on one arm out of three.
    for cells in [
        WASM_VOLUME_GRID_CELLS,
        MOBILE_VOLUME_GRID_CELLS,
        DESKTOP_VOLUME_GRID_CELLS,
    ] {
        for axis in cells {
            assert!(
                (1..=WEBGL2_MAX_TEXTURE_DIMENSION_3D).contains(&axis),
                "{cells:?}"
            );
        }
    }

    // The table travels *inside* the grid, so its size is one number in
    // two places too.
    assert_eq!(VOLUME_LUT_BYTES, LUT_LEN);
}

/// The shape the frontend **asks** `build_voxels` for is the one this target's
/// budgets were computed from.
///
/// It was not, and nothing here could see it: `voxel_request_for` called
/// `voxel::default_shape()`, which takes one `is_wasm` bool and so cannot
/// return `MOBILE_SHAPE` at all. A mobile *native* build therefore budgeted
/// against `MOBILE_VOLUME_GRID_CELLS` — 3.375 MiB of indices — and requested
/// `DESKTOP_SHAPE`'s 8 MiB, 2.4× over, on the class least able to afford it.
///
/// Routed through [`shape_of`] for the reason `voxel::default_shape_for` and
/// `mobile_cfg.rs` are: a `cfg`-gated body is invisible to every target that
/// does not compile it, and this workspace runs `cargo test` on exactly one of
/// three. All three arms are checked here from any host; what stays unpinned
/// is only the `cfg` cascade in `VOLUME_GRID_CELLS`, which the test above
/// covers as far as a host can.
///
/// **The shape is a runtime answer now**, so what this can still state as an
/// identity is the *floor*: the shape a device reporting exactly the WebGL2
/// guarantee is asked for, which is the budget triple unchanged and is what
/// makes "nothing regresses" checkable. The claim about every other device is
/// a relation rather than an identity, and it is
/// [`the_requested_shape_never_outgrows_the_budget_it_was_computed_against`].
#[test]
fn the_requested_shape_is_the_one_this_targets_budget_was_computed_for() {
    use rustdar_radar::voxel::{DESKTOP_SHAPE, MOBILE_SHAPE, VoxelShape, WASM_SHAPE};

    // The axis order asserted rather than trusted. Every real triple has
    // `nx == ny`, so an x/y transposition here would be invisible on all
    // three of them and would only ever surface as a rectangular box drawn
    // with its axes exchanged.
    assert_eq!(
        shape_of([1, 2, 3]),
        VoxelShape {
            nx: 1,
            ny: 2,
            nz: 3
        },
        "VOLUME_GRID_CELLS is x, y, z",
    );

    assert_eq!(shape_of(WASM_VOLUME_GRID_CELLS), WASM_SHAPE);
    assert_eq!(shape_of(MOBILE_VOLUME_GRID_CELLS), MOBILE_SHAPE);
    assert_eq!(shape_of(DESKTOP_VOLUME_GRID_CELLS), DESKTOP_SHAPE);

    // The floor — the shape a device at the guarantee is asked for — is this
    // target's own budget triple, unchanged. That is the no-regression claim,
    // and it is the one arm of the cascade a host can check by name.
    #[cfg(target_arch = "wasm32")]
    assert_eq!(
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        rustdar_radar::voxel::shape_for_budget(WASM_SHAPE, 256),
    );
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    assert_eq!(
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        rustdar_radar::voxel::shape_for_budget(MOBILE_SHAPE, 256),
    );
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    assert_eq!(
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        DESKTOP_SHAPE
    );

    assert_eq!(
        VOLUME_GRID_FLOOR_SHAPE,
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        "the floor shape the const assert guards has to be the one a device \
         at the guarantee is actually asked for",
    );
}

/// The limits a real adapter might report, which every sweep below runs.
///
/// The guarantee, the two powers of two either side of it, the 704 an
/// unaligned reading of the desktop budget lands on, and a modern desktop's
/// own figure.
const REPORTED_LIMITS: [u32; 5] = [256, 512, 704, 1024, 2048];

/// The three budget triples, whatever this target's cascade selected.
const ALL_ARMS: [(&str, [u32; 3]); 3] = [
    ("wasm", WASM_VOLUME_GRID_CELLS),
    ("mobile", MOBILE_VOLUME_GRID_CELLS),
    ("desktop", DESKTOP_VOLUME_GRID_CELLS),
];

/// **The shape the frontend requests never costs more than the budget it was
/// computed against — on any device.**
///
/// # What this replaces, and why it is stronger
///
/// `the_volume_budget_is_not_slack_enough_to_hide_a_doubling` asserted that
/// each arm's ceiling was under twice what it bounds, so a silently doubled
/// grid axis could not hide inside the headroom. That test is the tripwire for
/// the shipped Android overrun — a build budgeting 192×192×96 while the radar
/// was asked for 256×256×128, 2.4× over — and it went **vacuous** the moment
/// the shape stopped being a constant: it compares two constants, and the thing
/// that can now be wrong is a *function of the device*.
///
/// So it is re-expressed rather than deleted, against the property the whole
/// rebalance rests on: rearranging a budget's cells is free because there are
/// never more of them. For every arm and every limit an adapter might report,
/// what is requested must fit the budget every allocation was sized against —
/// in cells, and in the bytes those cells actually cost with the coarse level
/// beside them. That is stronger than the doubling test in two directions: it
/// catches any overrun rather than only a factor of two, and it catches one
/// that only appears on some devices.
#[test]
fn the_requested_shape_never_outgrows_the_budget_it_was_computed_against() {
    for (name, budget) in ALL_ARMS {
        let budget_cells = budget.iter().map(|&n| n as usize).product::<usize>();
        let budget_bytes = crate::volume::raymarch::grid_bytes_with_mips(budget)
            .expect("a shipped budget cannot overflow");
        for limit in REPORTED_LIMITS {
            let shape = rustdar_radar::voxel::shape_for_budget(shape_of(budget), limit as usize);
            let cells = [shape.nx as u32, shape.ny as u32, shape.nz as u32];
            assert!(
                shape.cells() <= budget_cells,
                "{name} on a {limit}-reporting device: {cells:?} is {} cells \
                 against the {budget_cells} this target budgeted for",
                shape.cells(),
            );
            let bytes = crate::volume::raymarch::grid_bytes_with_mips(cells)
                .expect("a derived shape cannot overflow");
            assert!(
                bytes <= budget_bytes,
                "{name} on a {limit}-reporting device: {cells:?} costs \
                 {bytes} B of texture against the {budget_bytes} B \
                 {budget:?} was budgeted at",
            );
        }
    }
}

/// A device is never asked for an axis it did not say it could hold.
///
/// The runtime half of the guarantee the const assert in `constants.rs` used to
/// make about every shape. It can only make that claim about
/// [`VOLUME_GRID_FLOOR_SHAPE`] now, because that is the only shape that is
/// still a compile-time constant; this is what guards everything above it, and
/// it is the reason `voxel::MAX_AXIS` could be widened off the GLES 3.0
/// guarantee without any device being asked for more than it reports.
#[test]
fn every_axis_stays_within_the_limit_the_adapter_reported() {
    for (name, budget) in ALL_ARMS {
        for limit in REPORTED_LIMITS {
            let shape = rustdar_radar::voxel::shape_for_budget(shape_of(budget), limit as usize);
            for (axis, n) in [("nx", shape.nx), ("ny", shape.ny), ("nz", shape.nz)] {
                assert!(
                    n as u32 <= limit,
                    "{name} on a {limit}-reporting device: {axis} is {n}, \
                     which that device cannot allocate — and the failure \
                     would be a validation error inside a callback, where \
                     there is no Result to check",
                );
            }
        }
    }
}

/// The **device guarantee**, which is what `voxel::tests`'
/// `an_axis_outside_the_guarantee_is_refused` used to assert with a literal
/// 257.
///
/// It could not stay there. `MAX_AXIS` is 1625 now — the largest axis whose
/// cube fits a 32-bit cell count, which is the only bound `rustdar-radar` can
/// actually check, having no `wgpu` and no adapter. The guarantee is a fact
/// about a *device*, so it moved to the crate that meets one, and it is
/// asserted the way it is meant: a shape derived for a device reporting exactly
/// the guarantee has every axis inside it. Neither test was dropped; each went
/// where its subject lives.
#[test]
fn a_shape_derived_for_a_device_at_the_guarantee_stays_within_it() {
    for (name, budget) in ALL_ARMS {
        let shape = rustdar_radar::voxel::shape_for_budget(
            shape_of(budget),
            WEBGL2_MAX_TEXTURE_DIMENSION_3D as usize,
        );
        for (axis, n) in [("nx", shape.nx), ("ny", shape.ny), ("nz", shape.nz)] {
            assert!(
                n >= 1 && n as u32 <= WEBGL2_MAX_TEXTURE_DIMENSION_3D,
                "{name}: {axis} is {n}, outside the 3D texture size WebGL2 \
                 guarantees, so a phone browser reporting exactly the \
                 guarantee could not allocate it",
            );
        }
    }
}

/// `voxel::HORIZONTAL_AXIS_MULTIPLE` is the copy alignment expressed in cells,
/// and this is the only crate that can say so.
///
/// `rustdar-radar` rounds the grid's horizontal axis to 64 and documents why —
/// `copy_buffer_to_texture` holds every row to
/// `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`, and the staging ring's `PlaneLayout`
/// pads to it — but it has no `wgpu` to check the arithmetic against. This is
/// the binding, in the shape `the_grid_dimensions_match_the_shapes_rustdar_radar_names`
/// already uses for the triples: a number in two crates, tied by name so a
/// drift fails here rather than as 6% of a permanently resident staging ring
/// spent on padding.
#[test]
fn the_horizontal_axis_multiple_is_the_copy_alignment_in_cells() {
    assert_eq!(
        rustdar_radar::voxel::HORIZONTAL_AXIS_MULTIPLE,
        wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize
            / crate::volume::raymarch::GRID_BYTES_PER_CELL as usize,
    );
    // And that the two it is a quotient of are what the doc says, so a change
    // to either fails by name rather than by cancelling out.
    assert_eq!(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 256);
    assert_eq!(crate::volume::raymarch::GRID_BYTES_PER_CELL, 4);
}

/// The pane mirror's ceiling is the cap squared, four bytes a texel — and the
/// cap is the one the renderer actually applies.
///
/// Three numbers that have to agree across two crates: `MIRROR_MAX_SIDE` is the
/// side cap the fit falls back to, `VOLUME_MIRROR_BYTES_MAX`'s arms are what the
/// budget prose claims a mirror costs, and `mirror_plan` is what enforces both.
/// Spelling the products here means the documented figures cannot drift from the
/// enforced ones — the failure mode a budget written as a literal always has.
///
/// The lower bounds are the real content: these are single allocations for the
/// whole application, so a future raise has to come past this line rather than
/// land as a silently bigger texture.
#[test]
fn the_pane_mirrors_ceiling_is_the_cap_it_is_actually_halved_to() {
    let side = crate::egui_renderer::MIRROR_MAX_SIDE as usize;
    assert_eq!(
        WASM_VOLUME_MIRROR_BYTES_MAX,
        side * side * 4,
        "the wasm32 budget is not the guaranteed cap squared at four bytes a texel",
    );
    assert_eq!(
        WASM_VOLUME_MIRROR_BYTES_MAX,
        16 * 1024 * 1024,
        "the wasm32 mirror's worst case moved. WebGL2 guarantees only a 2048 \
         side, so this arm is pinned by the device as well as by the budget.",
    );
    assert_eq!(
        MOBILE_VOLUME_MIRROR_BYTES_MAX, WASM_VOLUME_MIRROR_BYTES_MAX,
        "mobile is held to the same 16 MiB the pre-adaptive design cost, so \
         landing the rung moved no phone's floor-on memory",
    );
    assert_eq!(
        DESKTOP_VOLUME_MIRROR_BYTES_MAX,
        64 * 1024 * 1024,
        "the desktop mirror's worst case moved. It is one allocation for the \
         whole application, so a change here is a change to the application's \
         floor-on memory, not to a per-pane cost.",
    );

    // The tight row of the desktop table: 1440p at the top rung, with no floor
    // strip under it. If this stops fitting, the prose's headroom claim is
    // wrong. *With* a strip it no longer fits and the rung is given up — the
    // deliberate loss `the_strips_verdict_is_the_one_the_table_states` pins.
    let bytes = |w: usize, h: usize| w * h * 4;
    assert!(
        bytes(5120, 2880) <= DESKTOP_VOLUME_MIRROR_BYTES_MAX,
        "1440p at rung 2 no longer fits the desktop budget",
    );
    assert!(
        bytes(3840 * 4, 2160 * 4) > DESKTOP_VOLUME_MIRROR_BYTES_MAX,
        "the desktop budget is slack enough to hide a rung-4 4K mirror",
    );

    // The scale is the only reduction that leaves egui's geometry alone —
    // `screen_size_in_points` is `size_in_pixels / pixels_per_point`, so both
    // must move together. A cap applied to one and not the other would scale
    // the frame's vertices instead of its sampling rate. That argument is about
    // a quotient, so it is direction-free: the rows below check it upwards too.
    let desktop = crate::egui_renderer::MirrorLimits {
        max_side: 8192,
        max_bytes: DESKTOP_VOLUME_MIRROR_BYTES_MAX,
    };
    // Points, not pixels: `mirror_plan` sizes a region of egui's own space.
    // 1280x720 points at 1.5 is a 1920x1080 frame.
    let plan = crate::egui_renderer::mirror_plan([1280.0, 720.0], 1.5, 2.0, desktop);
    assert_eq!(
        (plan.size_in_pixels, plan.pixels_per_point),
        ([3840, 2160], 3.0),
        "a desktop 1080p frame asked for rung 2 must get it, both halves moved",
    );
    assert!(!plan.is_degraded() && plan.tile_zoom_bias() == 1);
    let plan = crate::egui_renderer::mirror_plan([1920.0, 1080.0], 2.0, 2.0, desktop);
    assert_eq!(
        (
            plan.size_in_pixels,
            plan.pixels_per_point,
            plan.applied_scale
        ),
        ([3840, 2160], 2.0, 1.0),
        "a 4K frame cannot afford rung 2 and falls back to its own size — an \
         improvement on the old cap, which halved it to 1920x1080",
    );
    assert!(
        plan.is_degraded() && plan.tile_zoom_bias() == 0,
        "a degraded plan must not go on fetching a slippy level it cannot show",
    );

    // The wasm32 arm, where the device's own guarantee binds before the budget.
    let web = crate::egui_renderer::MirrorLimits {
        max_side: crate::egui_renderer::MIRROR_MAX_SIDE,
        max_bytes: WASM_VOLUME_MIRROR_BYTES_MAX,
    };
    let plan = crate::egui_renderer::mirror_plan([1280.0, 720.0], 2.0, 2.0, web);
    assert_eq!(
        (plan.size_in_pixels, plan.applied_scale),
        ([1280, 720], 0.5),
        "the WebGL2 floor still halves a 1440p frame twice, rung or no rung",
    );
    assert!(plan.is_degraded() && plan.tile_zoom_bias() == 0);

    // The pre-adaptive helper, unchanged for every frame with no 3D pane on it.
    let (size, scale) = crate::egui_renderer::mirror_size_for([1920.0, 1080.0], 2.0);
    assert_eq!((size, scale), ([1920, 1080], 1.0), "a 4K frame halves once");
    let (size, scale) = crate::egui_renderer::mirror_size_for([1280.0, 720.0], 1.5);
    assert_eq!(
        (size, scale),
        ([1920, 1080], 1.5),
        "a frame already under the cap is mirrored at its own size",
    );
    let (size, _) = crate::egui_renderer::mirror_size_for([8192.0, 8192.0], 1.0);
    assert!(
        size[0].max(size[1]) <= crate::egui_renderer::MIRROR_MAX_SIDE
            && size[0] * size[1] * 4 <= WASM_VOLUME_MIRROR_BYTES_MAX as u32,
        "a frame far over the cap must halve until it fits, got {size:?}",
    );
}

/// The static pane textures the app ceiling does **not** count, named so the
/// figure is on the record rather than absent.
///
/// [`APP_TEXTURE_BUDGET_BYTES`] bounds loops, 3D grids and raymarch
/// offscreens. A still pane's own radar texture has never been in it — it was
/// 4 MiB on the web and 16 MiB native, small against the loop term and not a
/// term anyone had to reason about. The long-range raster changes the size of
/// that omission, not its shape, and the honest thing is to say what it is:
///
/// | target  | panes | static texture | worst case |
/// |---------|------:|---------------:|-----------:|
/// | desktop |     6 |         64 MiB |    384 MiB |
/// | mobile  |     4 |         64 MiB |    256 MiB |
/// | wasm32  |     6 |         16 MiB |     96 MiB |
///
/// Reachable only with every pane on a sweep that reaches past 230 km on a
/// device that can hold one, and never at the same time as the loop term
/// above it — a pane showing a loop is not showing a still frame. It is not
/// added to the ceiling because doing so would fold two mutually exclusive
/// worst cases into one sum; it is pinned here so that a future change to
/// either raster size has to come past a stated number.
#[test]
fn the_static_render_textures_are_named_even_though_the_ceiling_omits_them() {
    let expected = [
        ("wasm32", 16 * 1024 * 1024, 96),
        ("mobile", 64 * 1024 * 1024, 256),
        ("desktop", 64 * 1024 * 1024, 384),
    ];
    for (arm, (name, frame, worst_mib)) in arms().into_iter().zip(expected) {
        assert_eq!(arm.name, name);
        assert_eq!(arm.static_frame_bytes(), frame, "{name} static frame");
        assert_eq!(
            arm.max_panes * arm.static_frame_bytes() / (1024 * 1024),
            worst_mib,
            "{name} worst-case static textures",
        );
    }
}

/// The closed set a finished raster's length is read against, and the
/// lengths that are not in it.
///
/// Every consumer of a render derives its side this way — see
/// [`raster_side_from_rgba_len`] — so this is the one place the set is
/// stated. The refusals matter as much as the acceptances: a length that
/// slipped through would reach `ColorImage::from_rgba_unmultiplied`'s
/// `assert_eq!`, which on a native render thread means no response ever
/// arrives and the pane stays blank for good.
#[test]
fn a_rasters_side_is_read_back_from_its_length_against_a_closed_set() {
    for side in [
        LOOP_IMAGE_SIZE,
        rustdar_radar::types::IMAGE_SIZE,
        LONG_RANGE_IMAGE_SIZE,
    ] {
        assert_eq!(
            raster_side_from_rgba_len(side * side * 4),
            Some(side),
            "{side} px is a size this build renders",
        );
    }
    for (len, why) in [
        (0, "an empty buffer"),
        (1, "a single byte"),
        (3, "a length that is not even a whole pixel"),
        (512 * 512 * 4, "a square raster of a size nothing renders"),
        (
            LONG_RANGE_IMAGE_SIZE * LONG_RANGE_IMAGE_SIZE * 4 - 4,
            "one pixel short of the long-range raster",
        ),
        (
            LONG_RANGE_IMAGE_SIZE * LONG_RANGE_IMAGE_SIZE * 4 + 4,
            "one pixel over it",
        ),
        (
            rustdar_radar::xsect::SECTION_WIDTH * rustdar_radar::xsect::SECTION_HEIGHT * 4,
            "a cross-section raster, which is not square and not a plan view",
        ),
    ] {
        assert_eq!(raster_side_from_rgba_len(len), None, "{why}");
    }
}
