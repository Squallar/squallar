use crate::constants::*;
use egui::{ColorImage, TextureHandle, TextureOptions};

/// Manages the framebuffer and egui texture for the world rendering.
///
/// This struct encapsulates:
/// - A reusable RGBA framebuffer that the world draws into
/// - The egui TextureHandle that displays the framebuffer content
/// - Logic for creating and updating the texture
pub struct TextureManager {
    /// Reusable buffer that stores RGBA pixel data
    reusable_framebuffer: Vec<u8>,
    /// Handle to the egui texture that displays the framebuffer
    texture_handle: Option<TextureHandle>,
}

impl TextureManager {
    /// Creates a new TextureManager with an empty framebuffer.
    pub fn new() -> Self {
        Self {
            reusable_framebuffer: vec![0u8; RENDER_WIDTH as usize * RENDER_HEIGHT as usize * 4],
            texture_handle: None,
        }
    }

    /// Returns a mutable reference to the framebuffer for drawing.
    pub fn framebuffer_mut(&mut self) -> &mut [u8] {
        &mut self.reusable_framebuffer
    }

    /// Updates the egui texture with the current framebuffer contents.
    /// Creates the texture if it doesn't exist yet.
    ///
    /// # Arguments
    /// * `ctx` - The egui context used to create/update textures
    pub fn update_texture(&mut self, ctx: &egui::Context) {
        if self.texture_handle.is_none() {
            // Create the texture for the first time
            let image = ColorImage::from_rgba_unmultiplied(
                [RENDER_WIDTH as usize, RENDER_HEIGHT as usize],
                &self.reusable_framebuffer,
            );

            self.texture_handle =
                Some(ctx.load_texture(SCREEN_TEXTURE_NAME, image, TextureOptions::NEAREST));
        } else {
            // Update existing texture
            if let Some(handle) = &mut self.texture_handle {
                let image = ColorImage::from_rgba_unmultiplied(
                    [RENDER_WIDTH as usize, RENDER_HEIGHT as usize],
                    &self.reusable_framebuffer,
                );
                handle.set(image, TextureOptions::NEAREST);
            }
        }
    }

    /// Returns a reference to the texture handle if it exists.
    pub fn texture_handle(&self) -> Option<&TextureHandle> {
        self.texture_handle.as_ref()
    }
}

impl Default for TextureManager {
    fn default() -> Self {
        Self::new()
    }
}
