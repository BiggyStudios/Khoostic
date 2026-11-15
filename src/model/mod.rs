//! The domain model: tracks, the play queue, song matching and ordering.
//!
//! Plain data and pure functions only (no I/O, no audio), so everything else can share it.

pub mod matching;
pub mod queue;
pub mod sort;
pub mod track;

pub use queue::{EntryId, Queue, QueueEntry, RepeatMode};
pub use track::{CoverRef, SourceKind, Track, TrackLocator};
