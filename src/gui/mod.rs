//! The GUI (Khoostic's main interface): a winit window rendered with wgpu on Vulkan, and an
//! immediate-mode UI rebuilt from state every frame.
//!
//! * [`window`]: event loop and frame pacing; [`headless`]: the same GUI offscreen.
//! * [`app`]: GUI state and the frame layout, around [`sidebar`], [`views`] (one module per
//!   page) and [`player_bar`]; [`menu`] is the song context menu, [`spotify`] the Spotify data.
//! * [`ui`]: widgets, animation and icons; [`render`]: canvas, GPU pipelines and text.

// Drawing calls naturally take position, size, style and color together.
#![allow(clippy::too_many_arguments)]

pub mod app;
pub mod headless;
pub mod menu;
pub mod player_bar;
pub mod render;
pub mod sidebar;
pub mod spotify;
pub mod ui;
pub mod views;
pub mod window;

pub use headless::Headless;
pub use window::run;
