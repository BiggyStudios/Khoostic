//! Khoostic: a music player with a Vulkan-rendered GUI (the main interface) and a vim-style TUI
//! over one shared player core.
//!
//! ```text
//!  gui / tui ──commands──► player ──► audio engine ──► speakers
//!      ▲                     │  ▲            └──► analysis (visualizers)
//!      └──── snapshots ──────┘  └── sources: local files (library), Spotify
//!
//!  services: library index, Spotify, Last.fm scrobbling, discovery (radio, autoplay)
//! ```
//!
//! Modules are layered: each one only uses modules from the groups listed before it.

// Foundations: plain data, helpers, settings.
pub mod config;
pub mod model;
pub mod pending;

// Playback.
pub mod audio;
pub mod player;

// Music sources and online services.
pub mod lastfm;
pub mod library;
pub mod spotify;

// Built on the services above.
pub mod discovery;
pub mod services;

// Interfaces.
pub mod gui;
pub mod tui;
