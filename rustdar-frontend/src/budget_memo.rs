//! The budget ladder's persisted position — the app-side half of
//! [`rustdar_device_profile::budget::BudgetMemo`].
//!
//! The read/write pair lives here rather than beside the struct because the
//! store seam was rustdar-egui's `ConfigStore` when the floor crate was cut
//! (WO-RD), and a policy floor must not depend on the UI crate. The kv lane
//! landed [`KvStore`] as its own leaf (WO-RK) in the same landing queue — the
//! seam objection is gone, and that lane's census plans this consumer's
//! landing in rustdar-device-profile; the re-home is the kv lane's step, in
//! writing, not a shortcut to take here. The on-disk key string is
//! untouchable — a reopen is 1:1, and a value learned by crashing the GPU must
//! read back on the next launch whatever crate the reader lives in.
//!
//! This module also hosts the two wgpu **agreement tests**: the floor crate
//! pins the WebGL2 2D/3D figures as literals (it never names wgpu — its
//! charter forbids it), and the tests here are what hold those literals to
//! `wgpu::Limits::downlevel_webgl2_defaults()`, so a wgpu revision surfaces as
//! a visible failure rather than a drift.

use rustdar_kv::KvStore;

/// Key the ladder position ([`BudgetMemo::steps_back`]) is persisted under.
///
/// [`BudgetMemo::steps_back`]: rustdar_device_profile::budget::BudgetMemo::steps_back
///
/// Its own `KvStore` entry, beside `crate::loop_pool::LOOP_POOL_KEY` and
/// for the identical reason: `autosave_config` writes the `UiConfig` blob on a
/// 3 s timer behind a string compare, so a value learned in the last three
/// seconds of a session is lost — and a session that has just lost its
/// rendering surface may not get three more seconds. One entry holding one
/// integer also means the blast radius of a corrupt value is one integer,
/// rather than every setting on the next load.
///
/// **One key for the whole struct, not one per field.** The ladder is an
/// ordering over subsystems and a per-field memo could not express it: three
/// separate counts could describe a machine that had surrendered its grid
/// without surrendering its lighting, which is a state this ladder says does
/// not exist.
pub const BUDGET_MEMO_KEY: &str = "budget_steps";

/// What a previous session learned, read back.
///
/// A decimal count of rungs and nothing else, the format
/// `crate::loop_pool::remembered` already argues for: one integer, not JSON,
/// because a format with structure gives a corrupt entry more ways to be
/// almost-readable. Anything unreadable is `None`, which is the same answer a
/// first launch gets — the cost of losing it is one re-probe, and configuration
/// is never allowed to be load-bearing.
pub fn remembered_steps(store: Option<&dyn KvStore>) -> Option<u32> {
    let raw = store?.load(BUDGET_MEMO_KEY)?;
    raw.trim().parse().ok().or_else(|| {
        log::warn!("budget memo is not a number ({raw:?}); starting this device at its ladder top");
        None
    })
}

/// Write what this session settled on, synchronously. See [`BUDGET_MEMO_KEY`].
///
/// [`KvStore::store_now`] is what makes "synchronously" true. The ordinary
/// `store` hands the bytes to a writer thread that a dying process never gets
/// back to, and this is called off a lost rendering surface — the moment the
/// process is most likely to be killed. A dropped memo means the next session
/// opens at the top rung and loses the same surface again: the ladder would
/// never descend, which is the entire guarantee this key exists to provide. One
/// integer costs nothing to wait for.
pub fn remember_steps(store: Option<&dyn KvStore>, steps: u32) {
    let Some(store) = store else {
        return;
    };
    if let Err(e) = store.store_now(BUDGET_MEMO_KEY, &steps.to_string()) {
        log::warn!("could not persist the budget ladder position: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_wgpu::wgpu;
    use rustdar_device_profile::budget::{
        AdapterCeilings, BudgetLimits, BudgetMemo, DeviceProfile, Platform, resolve,
    };
    use rustdar_device_profile::constants::{
        WASM_LONG_RANGE_IMAGE_SIZE, WASM_LOOP_IMAGE_SIZE, WEBGL2_MAX_TEXTURE_DIMENSION_3D,
    };
    use rustdar_device_profile::quality::DeviceClass;
    use rustdar_radar::types::WASM_IMAGE_SIZE;

    /// **What a machine learned by crashing survives the restart, and reads
    /// back as the same budgets.**
    ///
    /// The 1:1 reopen rule, on the one value that must not be lost to the 3 s
    /// autosave timer. A decimal count of rungs in its own key, so a corrupt entry
    /// costs one integer rather than every setting on the next load — and an
    /// unreadable one is the same answer a first launch gets.
    #[test]
    fn a_ladder_position_survives_its_own_config_entry() {
        use rustdar_kv::MemoryKvStore;

        let store = MemoryKvStore::default();
        assert_eq!(remembered_steps(Some(&store)), None, "nothing learned yet");

        remember_steps(Some(&store), 2);
        assert_eq!(remembered_steps(Some(&store)), Some(2));
        assert_eq!(store.load(BUDGET_MEMO_KEY).as_deref(), Some("2"));

        // The desktop-bracket profile at its most conservative reading, memo
        // aside — `shipped_profile` in the floor crate's own tests, inlined
        // here because a test helper does not cross a crate boundary.
        let reopened = DeviceProfile {
            platform: Platform::Native,
            limits: BudgetLimits::DESKTOP,
            class: DeviceClass::Discrete,
            adapter: AdapterCeilings::WEBGL2_GUARANTEE,
            vram_bytes: None,
            system_ram_bytes: None,
            parallelism: 1,
            form_factor: None,
            memo: Some(BudgetMemo {
                loop_pool_bytes: None,
                steps_back: remembered_steps(Some(&store)).unwrap_or(0),
            }),
        };
        assert_eq!(resolve(&reopened).steps_back, 2);

        // Anything unreadable is a re-probe rather than a panic or a zero written
        // over the top of what the machine actually said.
        store
            .store(BUDGET_MEMO_KEY, "not a number")
            .expect("the memory store cannot fail");
        assert_eq!(remembered_steps(Some(&store)), None);
        assert_eq!(remembered_steps(None), None);
    }

    /// The web image fits what a browser is *guaranteed* to accept.
    ///
    /// `rustdar_radar` states the 2048 floor as a literal because it has no wgpu
    /// dependency and must not grow one — it hands finished RGBA buffers to the
    /// crate that owns the GPU. rustdar-device-profile is a literal for the same
    /// charter reason, so this crate — the one that reaches wgpu — is where the
    /// floor gets checked against wgpu's own downlevel limits rather than
    /// against a number someone typed. Without it,
    /// `WEBGL2_MAX_TEXTURE_DIMENSION_2D` could be raised to accommodate an
    /// over-large image instead of the image being the thing that gives.
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

    /// The floor crate's WebGL2 3D-texture literal is wgpu's own downlevel
    /// default.
    ///
    /// rustdar-device-profile pins 256 as a literal because its charter
    /// forbids a wgpu dependency; this is the agreement test that holds the
    /// literal to the value the device request is actually held to. A wgpu
    /// bump that moved the floor is a visible failure to be reviewed here,
    /// rather than a grid bound that silently drifts from what
    /// `rustdar_gpu::device::device_limits` enforces. (Replaces the floor crate's old
    /// source-scrape of its own derivation — same intent, new mechanism,
    /// WO-RD.)
    #[test]
    fn the_webgl2_3d_floor_is_wgpus_downlevel_default() {
        assert_eq!(
            WEBGL2_MAX_TEXTURE_DIMENSION_3D,
            wgpu::Limits::downlevel_webgl2_defaults().max_texture_dimension_3d,
            "rustdar-device-profile's WebGL2 3D floor literal has drifted from \
             wgpu's downlevel default — the value the device request is held to"
        );
    }
}
