//! The play queue.
//!
//! The queue keeps two orderings:
//! * `entries` — the user-visible order (what a queue view shows, what drag-and-drop edits);
//! * `order`   — the play order. Identical to `entries` unless shuffle is on.
//!
//! Every entry gets a unique [`EntryId`], so the same track can be queued several times and
//! the audio engine can refer to "the thing that is playing" without index bookkeeping.

use std::sync::Arc;

use crate::track::Track;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u64);

#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub id: EntryId,
    pub track: Arc<Track>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

#[derive(Debug, Default)]
pub struct Queue {
    entries: Vec<QueueEntry>,
    order: Vec<EntryId>,
    /// Index into `order` of the current entry.
    cursor: Option<usize>,
    repeat: RepeatMode,
    shuffle: bool,
    next_id: u64,
    version: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Monotonic counter bumped on every change. UIs use it to invalidate caches and the
    /// player uses it to know when a gapless preload has gone stale.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in user-visible order.
    pub fn entries(&self) -> &[QueueEntry] {
        &self.entries
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.repeat = repeat;
        self.bump();
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn get(&self, id: EntryId) -> Option<&QueueEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn current(&self) -> Option<&QueueEntry> {
        self.cursor.and_then(|c| self.get(self.order[c]))
    }

    pub fn current_id(&self) -> Option<EntryId> {
        self.cursor.map(|c| self.order[c])
    }

    /// Index of the current entry in user-visible order.
    pub fn current_index(&self) -> Option<usize> {
        let id = self.current_id()?;
        self.entries.iter().position(|e| e.id == id)
    }

    /// Appends a track to the end of the queue (or a random upcoming slot when shuffling).
    pub fn push(&mut self, track: impl Into<Arc<Track>>) -> EntryId {
        let entry = self.make_entry(track.into());
        let id = entry.id;
        self.entries.push(entry);
        let slot = if self.shuffle {
            let lo = self.cursor.map_or(0, |c| c + 1);
            fastrand::usize(lo..=self.order.len())
        } else {
            self.order.len()
        };
        self.order.insert(slot, id);
        self.bump();
        id
    }

    pub fn extend<I, T>(&mut self, tracks: I) -> Vec<EntryId>
    where
        I: IntoIterator<Item = T>,
        T: Into<Arc<Track>>,
    {
        tracks.into_iter().map(|t| self.push(t)).collect()
    }

    /// Inserts a track to be played right after the current one ("play next").
    pub fn push_next(&mut self, track: impl Into<Arc<Track>>) -> EntryId {
        let entry = self.make_entry(track.into());
        let id = entry.id;
        let at = self.current_index().map_or(self.entries.len(), |i| i + 1);
        self.entries.insert(at, entry);
        let slot = self.cursor.map_or(self.order.len(), |c| c + 1);
        self.order.insert(slot, id);
        self.bump();
        id
    }

    /// Removes an entry. Returns `true` if it was the current entry; in that case the cursor
    /// moves to whatever followed it (or is cleared at the end of the queue).
    pub fn remove(&mut self, id: EntryId) -> bool {
        let Some(pos) = self.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        self.entries.remove(pos);
        let opos = self
            .order
            .iter()
            .position(|&o| o == id)
            .expect("order out of sync");
        self.order.remove(opos);
        let was_current = self.cursor == Some(opos);
        self.cursor = match self.cursor {
            Some(c) if opos < c => Some(c - 1),
            Some(c) if opos == c && c >= self.order.len() => None,
            other => other,
        };
        self.bump();
        was_current
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.cursor = None;
        self.bump();
    }

    /// Moves an entry in user-visible order (drag and drop / `ddp` style moves).
    pub fn move_entry(&mut self, from: usize, to: usize) {
        if from >= self.entries.len() || to >= self.entries.len() || from == to {
            return;
        }
        let entry = self.entries.remove(from);
        self.entries.insert(to, entry);
        if !self.shuffle {
            let current = self.current_id();
            self.order = self.entries.iter().map(|e| e.id).collect();
            self.cursor = current.and_then(|id| self.order.iter().position(|&o| o == id));
        }
        self.bump();
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let current = self.current_id();
        let mut order: Vec<EntryId> = self.entries.iter().map(|e| e.id).collect();
        if on {
            // Current track stays first, everything else is shuffled after it.
            order.retain(|&id| Some(id) != current);
            fastrand::shuffle(&mut order);
            if let Some(id) = current {
                order.insert(0, id);
            }
        }
        self.cursor = current.and_then(|id| order.iter().position(|&o| o == id));
        self.order = order;
        self.bump();
    }

    /// Makes `id` the current entry. Returns `false` if it isn't in the queue.
    pub fn jump_to(&mut self, id: EntryId) -> bool {
        match self.order.iter().position(|&o| o == id) {
            Some(p) => {
                self.cursor = Some(p);
                self.bump();
                true
            }
            None => false,
        }
    }

    /// The entry that will play when the current one ends naturally, honoring repeat mode.
    /// Used for gapless preloading.
    pub fn peek_next(&self) -> Option<&QueueEntry> {
        self.next_cursor(true).and_then(|c| self.get(self.order[c]))
    }

    /// Advance because the current track ended on its own (honors `RepeatMode::One`).
    pub fn advance(&mut self) -> Option<&QueueEntry> {
        self.step(true)
    }

    /// Advance because the user asked for the next track (`RepeatMode::One` doesn't trap you).
    pub fn skip_next(&mut self) -> Option<&QueueEntry> {
        self.step(false)
    }

    /// Go to the previous entry in play order. Wraps when repeating all.
    pub fn skip_prev(&mut self) -> Option<&QueueEntry> {
        let len = self.order.len();
        if len == 0 {
            return None;
        }
        self.cursor = match self.cursor {
            Some(0) if self.repeat == RepeatMode::All => Some(len - 1),
            Some(0) => Some(0),
            Some(c) => Some(c - 1),
            None => Some(len - 1),
        };
        self.bump();
        self.current()
    }

    fn step(&mut self, natural: bool) -> Option<&QueueEntry> {
        self.cursor = self.next_cursor(natural);
        self.bump();
        self.current()
    }

    fn next_cursor(&self, natural: bool) -> Option<usize> {
        let len = self.order.len();
        if len == 0 {
            return None;
        }
        match self.cursor {
            None => Some(0),
            Some(c) if natural && self.repeat == RepeatMode::One => Some(c),
            Some(c) if c + 1 < len => Some(c + 1),
            Some(_) if self.repeat != RepeatMode::Off => Some(0),
            Some(_) => None,
        }
    }

    fn make_entry(&mut self, track: Arc<Track>) -> QueueEntry {
        self.next_id += 1;
        QueueEntry {
            id: EntryId(self.next_id),
            track,
        }
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: usize) -> (Queue, Vec<EntryId>) {
        let mut q = Queue::new();
        let ids = (0..n)
            .map(|i| q.push(Track::from_path(format!("{i}.flac"))))
            .collect();
        (q, ids)
    }

    #[test]
    fn linear_playback_stops_at_end() {
        let (mut q, ids) = q(3);
        assert_eq!(q.advance().unwrap().id, ids[0]);
        assert_eq!(q.advance().unwrap().id, ids[1]);
        assert_eq!(q.peek_next().unwrap().id, ids[2]);
        assert_eq!(q.advance().unwrap().id, ids[2]);
        assert!(q.peek_next().is_none());
        assert!(q.advance().is_none());
    }

    #[test]
    fn repeat_modes() {
        let (mut q, ids) = q(2);
        q.jump_to(ids[1]);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.peek_next().unwrap().id, ids[0]);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.peek_next().unwrap().id, ids[1]);
        assert_eq!(q.advance().unwrap().id, ids[1]);
        // A manual skip escapes repeat-one; with repeat-one we still wrap.
        assert_eq!(q.skip_next().unwrap().id, ids[0]);
    }

    #[test]
    fn prev_wraps_only_with_repeat_all() {
        let (mut q, ids) = q(3);
        q.jump_to(ids[0]);
        assert_eq!(q.skip_prev().unwrap().id, ids[0]);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.skip_prev().unwrap().id, ids[2]);
    }

    #[test]
    fn push_next_plays_after_current() {
        let (mut q, ids) = q(3);
        q.jump_to(ids[0]);
        let n = q.push_next(Track::from_path("next.flac"));
        assert_eq!(q.peek_next().unwrap().id, n);
        assert_eq!(q.entries()[1].id, n);
    }

    #[test]
    fn remove_current_moves_to_following() {
        let (mut q, ids) = q(3);
        q.jump_to(ids[1]);
        assert!(q.remove(ids[1]));
        assert_eq!(q.current_id(), Some(ids[2]));
        assert!(q.remove(ids[2]));
        assert_eq!(q.current_id(), None);
    }

    #[test]
    fn remove_before_current_keeps_current() {
        let (mut q, ids) = q(3);
        q.jump_to(ids[2]);
        assert!(!q.remove(ids[0]));
        assert_eq!(q.current_id(), Some(ids[2]));
    }

    #[test]
    fn shuffle_keeps_current_and_all_entries() {
        let (mut q, ids) = q(50);
        q.jump_to(ids[10]);
        q.set_shuffle(true);
        assert_eq!(q.current_id(), Some(ids[10]));
        let mut seen = vec![ids[10]];
        while let Some(e) = q.advance() {
            seen.push(e.id);
        }
        seen.sort();
        assert_eq!(seen, ids);
        // Turning shuffle off restores the visible order around the current entry.
        q.jump_to(ids[20]);
        q.set_shuffle(false);
        assert_eq!(q.peek_next().unwrap().id, ids[21]);
    }

    #[test]
    fn move_entry_updates_play_order() {
        let (mut q, ids) = q(3);
        q.jump_to(ids[0]);
        q.move_entry(2, 1);
        assert_eq!(q.peek_next().unwrap().id, ids[2]);
    }
}
