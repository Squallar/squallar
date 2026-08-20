//! The budget ladder's persisted position — the app-side half of
//! [`rustdar_device_profile::budget::BudgetMemo`].

use rustdar_kv::KvStore;

/// Key the ladder position ([`BudgetMemo::steps_back`]) is persisted under.
pub const BUDGET_MEMO_KEY: &str = "budget_steps";

/// What a previous session learned, read back.
pub fn remembered_steps(store: Option<&dyn KvStore>) -> Option<u32> {
    let raw = store?.load(BUDGET_MEMO_KEY)?;
    raw.trim().parse().ok().or_else(|| {
        log::warn!("budget memo is not a number ({raw:?}); starting this device at its ladder top");
        None
    })
}

/// Write what this session settled on, synchronously. See [`BUDGET_MEMO_KEY`].
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
    #[test]
    fn a_ladder_position_survives_its_own_config_entry() {
        use rustdar_kv::MemoryKvStore;

        let store = MemoryKvStore::default();
        assert_eq!(remembered_steps(Some(&store)), None, "nothing learned yet");

        remember_steps(Some(&store), 2);
        assert_eq!(remembered_steps(Some(&store)), Some(2));
        assert_eq!(store.load(BUDGET_MEMO_KEY).as_deref(), Some("2"));

        // The desktop-bracket profile at its most conservative reading, memo aside —
        // `shipped_profile` in the floor crate's own tests.
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
        // The web arm sits *on* the guarantee rather than under it, and that is the
        // decision.
        assert_eq!(WASM_IMAGE_SIZE as u32, guaranteed);
        // Which is also why the web arm's long-range ceiling has to *be* the
        // guarantee: there is nothing above it to grow into.
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
