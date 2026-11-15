//! Offscreen rendering of the real GUI with scripted input, for screenshots and tests.

use std::sync::Arc;

use crate::config::Config;
use crate::gui::app;
use crate::gui::app::Gui;
use crate::gui::render::gpu::Gpu;
use crate::gui::render::{Canvas, Renderer};
use crate::gui::ui::Input;
use crate::library::Library;
use crate::player::Player;

/// Offscreen GUI for screenshots and tests: same UI code, no window.
pub struct Headless {
    pub gui: Gui,
    renderer: Renderer,
    pub size: (u32, u32),
    pub scale: f32,
}

impl Headless {
    pub fn new(
        config: &Config,
        library: Arc<Library>,
        spotify: Option<Arc<crate::spotify::Spotify>>,
        discovery: Arc<crate::discovery::Discovery>,
        size: (u32, u32),
        scale: f32,
    ) -> anyhow::Result<Self> {
        let gpu = pollster::block_on(Gpu::headless())?;
        let renderer = Renderer::new(
            gpu,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            Some(config.gui.font.clone()),
        );
        Ok(Self {
            gui: Gui::new(config, library, spotify, discovery).0,
            renderer,
            size,
            scale,
        })
    }

    /// Runs one UI frame with `input` and returns RGBA8 pixels. Keeps rendering (without new
    /// input) until cover art has finished loading, so captures are complete.
    pub fn frame(&mut self, input: &Input, player: &Player) -> anyhow::Result<Vec<u8>> {
        let mut input = input.clone();
        // After loading finishes, keep going long enough for fades and transitions to settle.
        let mut settle = 40;
        for _ in 0..400 {
            let mut canvas = Canvas::new(self.size, self.scale, self.gui.theme.background);
            let gfx = app::Gfx {
                text: &mut self.renderer.text,
                images: &mut self.renderer.images,
            };
            self.gui.frame(&mut canvas, gfx, &input, player);
            if !self.renderer.images.is_loading() {
                settle -= 1;
                if settle == 0 {
                    return self.renderer.capture(&canvas, self.size);
                }
            }
            self.renderer.text.end_frame();
            input.end_frame();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::bail!("images never finished loading")
    }
}
