//! The domain model: tracks, the play queue, lyrics, song matching and ordering.
//!
//! Plain data and pure functions only (no I/O, no audio), so everything else can share it.

pub mod lyrics;
pub mod matching;
pub mod queue;
pub mod sort;
pub mod track;

pub use lyrics::{LyricLine, Lyrics};
pub use queue::{EntryId, Queue, QueueEntry, RepeatMode};
pub use track::{CoverRef, SourceKind, Track, TrackLocator};
