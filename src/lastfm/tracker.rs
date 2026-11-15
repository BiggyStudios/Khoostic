//! When a play counts: Last.fm's rules, fed by periodic player snapshots.
//!
//! A track is "now playing" as soon as it plays, and scrobbled once it has been *listened to*
//! (time actually playing, not the position, so seeking ahead doesn't count) for half its
//! length or four minutes, whichever comes first. Tracks of 30 seconds or less never count.

use std::time::Duration;

use crate::lastfm::client::Scrobble;
use crate::model::{EntryId, Track};
use crate::player::{PlayerSnapshot, State};

const MIN_LENGTH: Duration = Duration::from_secs(30);
const MAX_NEEDED: Duration = Duration::from_secs(240);
/// Gaps between observations longer than this (sleep, debugger) don't count as listening.
const MAX_STEP: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    NowPlaying(Scrobble),
    Scrobble(Scrobble),
}

struct Play {
    entry: EntryId,
    scrobble: Option<Scrobble>,
    length: Option<Duration>,
    listened: Duration,
    last_pos: Duration,
    announced: bool,
    counted: bool,
}

#[derive(Default)]
pub struct Tracker {
    play: Option<Play>,
}

/// The scrobble for a track, if it has what Last.fm requires (an artist and a title).
pub fn scrobble_for(track: &Track, timestamp: i64) -> Option<Scrobble> {
    let artist = track.primary_artist()?.trim();
    if artist.is_empty() || track.title.trim().is_empty() {
        return None;
    }
    Some(Scrobble {
        artist: artist.to_owned(),
        track: track.title.clone(),
        album: track.album.clone().filter(|a| !a.is_empty()),
        album_artist: track.album_artist.clone(),
        duration_secs: track.duration.map(|d| d.as_secs() as u32),
        timestamp,
    })
}

impl Tracker {
    /// Feeds one snapshot taken `dt` after the previous one, at unix time `now`.
    pub fn observe(&mut self, snap: &PlayerSnapshot, now: i64, dt: Duration) -> Vec<Event> {
        let mut out = Vec::new();
        let Some(entry) = &snap.current else {
            self.play = None;
            return out;
        };
        let pos = snap.position;
        let restarted = self.play.as_ref().is_some_and(|p| {
            // Same entry again (repeat one, or "previous" restarting it).
            p.entry == entry.id
                && pos + Duration::from_secs(5) < p.last_pos
                && pos < Duration::from_secs(10)
        });
        let fresh = restarted || self.play.as_ref().is_none_or(|p| p.entry != entry.id);
        if fresh {
            let started = now - pos.as_secs() as i64;
            self.play = Some(Play {
                entry: entry.id,
                scrobble: scrobble_for(&entry.track, started),
                length: snap.duration.or(entry.track.duration),
                listened: Duration::ZERO,
                last_pos: pos,
                announced: false,
                counted: false,
            });
        }
        let p = self.play.as_mut().expect("set above");
        p.last_pos = pos;
        if p.length.is_none() {
            p.length = snap.duration;
        }
        let Some(s) = p.scrobble.clone() else {
            return out;
        };
        if snap.state != State::Playing {
            return out;
        }
        if !fresh {
            // The first sighting says nothing about how long it already played.
            p.listened += dt.min(MAX_STEP);
        }
        if !p.announced {
            p.announced = true;
            out.push(Event::NowPlaying(s.clone()));
        }
        if let Some(len) = p.length
            && !p.counted
            && len > MIN_LENGTH
            && p.listened >= (len / 2).min(MAX_NEEDED)
        {
            p.counted = true;
            let mut s = s;
            s.duration_secs = Some(len.as_secs() as u32);
            out.push(Event::Scrobble(s));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::model::QueueEntry;

    use super::*;

    fn snap(id: u64, pos: f64, state: State, len: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            state,
            current: Some(QueueEntry {
                id: EntryId(id),
                track: Arc::new(Track {
                    title: format!("Song {id}"),
                    artists: vec!["Artist".into()],
                    ..Default::default()
                }),
            }),
            position: Duration::from_secs_f64(pos),
            duration: Some(Duration::from_secs(len)),
            ..Default::default()
        }
    }

    /// Plays entry `id` from `from` seconds for `secs` seconds in 1 s steps.
    fn play(
        t: &mut Tracker,
        id: u64,
        from: f64,
        secs: usize,
        len: u64,
        now: &mut i64,
    ) -> Vec<Event> {
        let mut ev = Vec::new();
        for i in 0..=secs {
            ev.extend(t.observe(
                &snap(id, from + i as f64, State::Playing, len),
                *now,
                Duration::from_secs(1),
            ));
            *now += 1;
        }
        ev
    }

    fn scrobbles(ev: &[Event]) -> usize {
        ev.iter()
            .filter(|e| matches!(e, Event::Scrobble(_)))
            .count()
    }

    #[test]
    fn counts_after_half_the_track() {
        let (mut t, mut now) = (Tracker::default(), 1_000_000);
        let ev = play(&mut t, 1, 0.0, 99, 200, &mut now);
        assert!(matches!(ev[0], Event::NowPlaying(_)));
        assert_eq!(scrobbles(&ev), 0, "99 s of a 200 s track isn't enough");
        let ev = play(&mut t, 1, 100.0, 5, 200, &mut now);
        assert_eq!(scrobbles(&ev), 1);
        let Event::Scrobble(s) = &ev.iter().find(|e| matches!(e, Event::Scrobble(_))).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(s.timestamp, 1_000_000, "timestamped when it started");
        assert_eq!(
            scrobbles(&play(&mut t, 1, 106.0, 50, 200, &mut now)),
            0,
            "only once"
        );
    }

    #[test]
    fn long_tracks_need_four_minutes_and_short_ones_never_count() {
        let (mut t, mut now) = (Tracker::default(), 0);
        assert_eq!(scrobbles(&play(&mut t, 1, 0.0, 239, 1200, &mut now)), 0);
        assert_eq!(scrobbles(&play(&mut t, 1, 240.0, 2, 1200, &mut now)), 1);
        assert_eq!(scrobbles(&play(&mut t, 2, 0.0, 30, 30, &mut now)), 0);
    }

    #[test]
    fn seeking_ahead_or_pausing_does_not_count_as_listening() {
        let (mut t, mut now) = (Tracker::default(), 0);
        play(&mut t, 1, 0.0, 10, 200, &mut now);
        // Jump to the end: position says 190 s, but only ~15 s were heard.
        assert_eq!(scrobbles(&play(&mut t, 1, 190.0, 5, 200, &mut now)), 0);
        // Paused for a long time.
        for _ in 0..300 {
            t.observe(
                &snap(1, 195.0, State::Paused, 200),
                now,
                Duration::from_secs(1),
            );
        }
        assert_eq!(scrobbles(&play(&mut t, 1, 195.0, 3, 200, &mut now)), 0);
    }

    #[test]
    fn repeat_one_counts_each_listen() {
        let (mut t, mut now) = (Tracker::default(), 0);
        assert_eq!(scrobbles(&play(&mut t, 1, 0.0, 60, 100, &mut now)), 1);
        assert_eq!(scrobbles(&play(&mut t, 1, 0.0, 60, 100, &mut now)), 1);
    }

    #[test]
    fn untagged_tracks_are_skipped() {
        let mut s = snap(1, 0.0, State::Playing, 200);
        if let Some(e) = &mut s.current {
            e.track = Arc::new(Track::from_path("C:/music/unknown.flac"));
        }
        let mut t = Tracker::default();
        for i in 0..200 {
            s.position = Duration::from_secs(i);
            assert!(t.observe(&s, i as i64, Duration::from_secs(1)).is_empty());
        }
    }
}
