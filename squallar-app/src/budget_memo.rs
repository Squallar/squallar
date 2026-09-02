//! The key the budget ladder's position used to be persisted under.
//!
//! Nothing is learned across sessions: the ladder starts at its top on every
//! launch, pressure is answered within the session, and no code in this crate
//! reads or writes this key any more. It is kept because the store has no
//! delete — an install that wrote one keeps it — and a named stale entry is
//! better than an unexplained string in someone's config. The position itself
//! is [`squallar_device_profile::budget::BudgetMemo::steps_back`] on the
//! device profile, for the life of the process.

/// The key a stale ladder position sits under in an older install's store.
/// Never read, never written; harmless where it exists.
pub const BUDGET_MEMO_KEY: &str = "budget_steps";

#[cfg(test)]
mod tests {
    use super::*;
    use egui_wgpu::wgpu;
    use squallar_device_profile::constants::{
        WASM_LONG_RANGE_IMAGE_SIZE, WASM_LOOP_IMAGE_SIZE, WEBGL2_MAX_TEXTURE_DIMENSION_3D,
    };
    use squallar_radar::types::WASM_IMAGE_SIZE;

    /// **Nothing is learned across sessions.** Neither memo key is read or
    /// written anywhere in this crate's production source: each appears exactly
    /// once, at its own definition, and the two files that used to write them
    /// on a lost surface and read them at construction name neither.
    ///
    /// This file and `loop_pool.rs` are scraped up to their own `#[cfg(test)]`,
    /// so this test's needles do not count against it; `app.rs` and
    /// `app_render.rs` keep their tests in files of their own and are read
    /// whole.
    #[test]
    fn neither_memo_key_is_read_or_written_by_production_source() {
        fn production(source: &'static str) -> &'static str {
            source
                .split_once("#[cfg(test)]")
                .map_or(source, |(production, _)| production)
        }
        let memo = production(include_str!("budget_memo.rs"));
        let pool = production(include_str!("loop_pool.rs"));

        // Presence controls: the two definitions are where the scrape says they
        // are, spelling the values the constants really carry, so a zero below
        // is a zero and not an unread file.
        assert!(memo.contains(&format!(
            "pub const BUDGET_MEMO_KEY: &str = {BUDGET_MEMO_KEY:?};"
        )));
        assert!(pool.contains(&format!(
            "pub const LOOP_POOL_KEY: &str = {:?};",
            crate::loop_pool::LOOP_POOL_KEY
        )));
        assert_eq!(memo.matches("BUDGET_MEMO_KEY").count(), 1);
        assert_eq!(pool.matches("LOOP_POOL_KEY").count(), 1);

        for (name, source) in [
            ("budget_memo.rs", memo),
            ("loop_pool.rs", pool),
            ("app.rs", include_str!("app.rs")),
            ("app_render.rs", include_str!("app_render.rs")),
        ] {
            for needle in [
                "remembered_steps",
                "remember_steps",
                "store_now(BUDGET",
                "store_now(LOOP",
            ] {
                assert_eq!(
                    source.matches(needle).count(),
                    0,
                    "{name} spells `{needle}`: something reads or writes a memo key \
                     again, and what a session learns is supposed to die with it",
                );
            }
        }
        for (name, source) in [
            ("app.rs", include_str!("app.rs")),
            ("app_render.rs", include_str!("app_render.rs")),
        ] {
            for key in ["BUDGET_MEMO_KEY", "LOOP_POOL_KEY"] {
                assert_eq!(
                    source.matches(key).count(),
                    0,
                    "{name} names `{key}` in production source",
                );
            }
        }
    }

    /// The web image fits what a browser is *guaranteed* to accept.
    #[test]
    fn the_web_image_fits_the_texture_size_webgl2_guarantees() {
        let guaranteed = wgpu::Limits::downlevel_webgl2_defaults().max_texture_dimension_2d;
        assert_eq!(
            squallar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D as u32,
            guaranteed,
            "squallar_radar's copy of the WebGL2 2D floor has drifted from wgpu's"
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
            "squallar-device-profile's WebGL2 3D floor literal has drifted from \
             wgpu's downlevel default — the value the device request is held to"
        );
    }
}
