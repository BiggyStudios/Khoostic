//! Khoostic domain model: tracks, the play queue and other source-agnostic types.
//!
//! This crate has no I/O and no audio dependencies so every frontend (TUI, GUI, MPD server,
//! jam sessions) can share it cheaply.

pub mod queue;
pub mod sort;
pub mod track;

pub use queue::{EntryId, Queue, QueueEntry, RepeatMode};
pub use track::{CoverRef, SourceKind, Track, TrackLocator};
