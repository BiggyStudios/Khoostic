//! Finding more music: song radio and autoplay.
//!
//! Suggestions come from Spotify's radio for a song and Last.fm's similar tracks, whichever
//! are available. Every suggestion is matched against the local library first, so songs you
//! own play from your files and the rest stream from Spotify. With neither service, radio
//! falls back to the library alone (same artist, then same genre).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use khoostic_core::matching::{is_song, track_key};
use khoostic_core::{EntryId, Pending, RepeatMode, Track, TrackLocator};
use khoostic_lastfm::Lastfm;
use khoostic_library::{Library, TrackOrder};
use khoostic_player::{PlayerCommand, PlayerHandle, State};
use khoostic_spotify::Spotify;

/// Start fetching more songs this close to the end of the last one, so playback continues
/// without a gap.
const AUTOPLAY_LEAD: Duration = Duration::from_secs(30);
/// Spotify lookups for Last.fm suggestions per radio (each is one search).
const MAX_LOOKUPS: usize = 30;

type Key = (String, String);

pub struct Discovery {
    library: Arc<Library>,
    spotify: Option<Arc<Spotify>>,
    lastfm: Option<Arc<Lastfm>>,
    autoplay: AtomicBool,
}

impl Discovery {
    pub fn new(
        library: Arc<Library>,
        spotify: Option<Arc<Spotify>>,
        lastfm: Option<Arc<Lastfm>>,
        autoplay: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            library,
            spotify,
            lastfm,
            autoplay: AtomicBool::new(autoplay),
        })
    }

    pub fn lastfm(&self) -> Option<&Arc<Lastfm>> {
        self.lastfm.as_ref()
    }

    pub fn autoplay(&self) -> bool {
        self.autoplay.load(Ordering::Acquire)
    }

    pub fn set_autoplay(&self, on: bool) {
        self.autoplay.store(on, Ordering::Release);
    }

    fn spotify(&self) -> Option<&Arc<Spotify>> {
        self.spotify.as_ref().filter(|s| s.is_connected())
    }

    fn lastfm_ready(&self) -> Option<&Arc<Lastfm>> {
        self.lastfm
            .as_ref()
            .filter(|l| l.status() != khoostic_lastfm::Status::NoApiKey)
    }

    /// Up to `limit` songs like `seed` (the seed itself excluded), in the background.
    pub fn radio(self: &Arc<Self>, seed: &Track, limit: usize) -> Pending<Vec<Track>> {
        let (me, seed) = (self.clone(), seed.clone());
        Pending::spawn("khoostic-radio", move || {
            me.similar(std::slice::from_ref(&seed), &HashSet::new(), limit)
        })
    }

    /// Songs like the last of `seeds`, skipping anything whose key is in `exclude` (and the
    /// seeds). Blocking: call from a worker thread.
    pub fn similar(
        &self,
        seeds: &[Track],
        exclude: &HashSet<Key>,
        limit: usize,
    ) -> anyhow::Result<Vec<Track>> {
        let Some(seed) = seeds.last() else {
            return Ok(Vec::new());
        };
        let artist = seed.primary_artist().unwrap_or_default().to_owned();
        let title = seed.title.clone();

        // Ask both services at once.
        let lastfm = self
            .lastfm_ready()
            .filter(|_| !artist.is_empty())
            .map(|l| l.similar_pending(&artist, &title, 40));
        let spotify_radio = self.spotify().and_then(|sp| {
            let id = self.spotify_id(sp, seed)?;
            Some(sp.radio(&id, 50))
        });

        let from_spotify: Vec<Track> = match spotify_radio.map(Pending::wait) {
            Some(Ok(t)) => t.into_iter().map(|t| self.prefer_local(t)).collect(),
            Some(Err(e)) => {
                tracing::debug!("Spotify radio: {e:#}");
                Vec::new()
            }
            None => Vec::new(),
        };
        let from_lastfm = match lastfm.map(Pending::wait) {
            Some(Ok(pairs)) => self.resolve(&pairs),
            Some(Err(e)) => {
                tracing::debug!("Last.fm similar: {e:#}");
                Vec::new()
            }
            None => Vec::new(),
        };

        let mut seen: HashSet<Key> = exclude.clone();
        seen.extend(seeds.iter().map(track_key));
        let mut out = Vec::new();
        let (mut a, mut b) = (from_lastfm.into_iter(), from_spotify.into_iter());
        // Alternate sources so one service's taste doesn't dominate.
        loop {
            let (x, y) = (a.next(), b.next());
            if x.is_none() && y.is_none() {
                break;
            }
            for t in [x, y].into_iter().flatten() {
                if seen.insert(track_key(&t)) {
                    out.push(t);
                }
            }
        }
        if out.len() < limit / 2 {
            out.extend(self.local_fallback(seed, &mut seen, limit - out.len()));
        }
        out.truncate(limit);
        Ok(out)
    }

    /// The Spotify id of `seed`: itself, or the best search match for a local song.
    fn spotify_id(&self, sp: &Spotify, seed: &Track) -> Option<String> {
        if let TrackLocator::Spotify { id } = &seed.locator {
            return Some(id.clone());
        }
        let artist = seed.primary_artist()?;
        let found = sp
            .search(&format!("{artist} {}", search_title(&seed.title)), 10)
            .wait()
            .ok()?;
        found
            .tracks
            .into_iter()
            .find(|t| is_song(t, artist, &seed.title))
            .and_then(|t| match t.locator {
                TrackLocator::Spotify { id } => Some(id),
                _ => None,
            })
    }

    /// The library's copy of a song, if there is one.
    fn find_local(&self, artist: &str, title: &str) -> Option<Track> {
        self.library
            .search(&format!("{artist} {}", search_title(title)), 8)
            .into_iter()
            .find(|t| is_song(t, artist, title))
    }

    fn prefer_local(&self, t: Track) -> Track {
        let artist = t.primary_artist().unwrap_or_default().to_owned();
        self.find_local(&artist, &t.title).unwrap_or(t)
    }

    /// Turns `(artist, title)` suggestions into playable tracks: local files first, then
    /// Spotify (searched in parallel).
    fn resolve(&self, pairs: &[(String, String)]) -> Vec<Track> {
        let mut slots: Vec<Option<Track>> = Vec::with_capacity(pairs.len());
        let mut lookups = Vec::new();
        for (i, (a, t)) in pairs.iter().enumerate() {
            match self.find_local(a, t) {
                Some(local) => slots.push(Some(local)),
                None => {
                    slots.push(None);
                    if let Some(sp) = self.spotify()
                        && lookups.len() < MAX_LOOKUPS
                    {
                        lookups.push((i, sp.search(&format!("{a} {}", search_title(t)), 5)));
                    }
                }
            }
        }
        for (i, p) in lookups {
            let (a, t) = &pairs[i];
            if let Ok(res) = p.wait() {
                slots[i] = res.tracks.into_iter().find(|x| is_song(x, a, t));
            }
        }
        slots.into_iter().flatten().collect()
    }

    /// Library-only suggestions: the seed's artist, then songs sharing its genre, then anything.
    fn local_fallback(&self, seed: &Track, seen: &mut HashSet<Key>, limit: usize) -> Vec<Track> {
        let mut pool: Vec<Track> = match seed.primary_artist() {
            Some(a) => self.library.artist_tracks(a),
            None => Vec::new(),
        };
        fastrand::shuffle(&mut pool);
        let mut rest = self.library.all_tracks(TrackOrder::Artist);
        fastrand::shuffle(&mut rest);
        let genre = seed.genre.as_deref().map(str::to_lowercase);
        let (same_genre, other): (Vec<Track>, Vec<Track>) = rest
            .into_iter()
            .partition(|t| genre.is_some() && t.genre.as_deref().map(str::to_lowercase) == genre);
        pool.extend(same_genre);
        pool.extend(other);
        pool.into_iter()
            .filter(|t| seen.insert(track_key(t)))
            .take(limit)
            .collect()
    }

    /// Keeps music going: when the last song in the queue nears its end, appends songs like
    /// the recent ones (if autoplay is on and repeat is off). Runs for the app's lifetime.
    pub fn start_autoplay(self: &Arc<Self>, player: PlayerHandle) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("khoostic-autoplay".into())
            .spawn(move || me.autoplay_loop(player));
    }

    fn autoplay_loop(self: Arc<Self>, player: PlayerHandle) {
        let mut handled: Option<EntryId> = None;
        let mut pending: Option<Pending<Vec<Track>>> = None;
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            if let Some(p) = &pending {
                let Some(result) = p.poll() else { continue };
                pending = None;
                match result {
                    Ok(tracks) if !tracks.is_empty() => {
                        let still_at_end = player.queue().read().peek_next().is_none();
                        if still_at_end && self.autoplay() {
                            tracing::info!(n = tracks.len(), "autoplay: adding similar songs");
                            let stopped = player.snapshot().state == State::Stopped;
                            player.send(if stopped {
                                PlayerCommand::EnqueueAndPlay(tracks)
                            } else {
                                PlayerCommand::Enqueue(tracks)
                            });
                        }
                    }
                    Ok(_) => tracing::debug!("autoplay: nothing similar found"),
                    Err(e) => tracing::debug!("autoplay: {e:#}"),
                }
                continue;
            }
            if !self.autoplay() {
                continue;
            }
            let snap = player.snapshot();
            let Some(current) = snap.current.clone() else {
                continue;
            };
            let (at_end, repeat, seeds, exclude) = {
                let q = player.queue().read();
                let entries = q.entries();
                let idx = q
                    .index_of(current.id)
                    .unwrap_or(entries.len().saturating_sub(1));
                let seeds: Vec<Track> = entries
                    [idx.saturating_sub(4)..=idx.min(entries.len().saturating_sub(1))]
                    .iter()
                    .map(|e| (*e.track).clone())
                    .collect();
                let exclude: HashSet<Key> = entries
                    .iter()
                    .rev()
                    .take(300)
                    .map(|e| track_key(&e.track))
                    .collect();
                (q.peek_next().is_none(), q.repeat(), seeds, exclude)
            };
            if !at_end || repeat != RepeatMode::Off || handled == Some(current.id) {
                continue;
            }
            let remaining = snap.duration.map(|d| d.saturating_sub(snap.position));
            if snap.state != State::Playing || remaining.is_none_or(|r| r > AUTOPLAY_LEAD) {
                continue;
            }
            handled = Some(current.id);
            let me = self.clone();
            pending = Some(Pending::spawn("khoostic-autoplay-fetch", move || {
                me.similar(&seeds, &exclude, 20)
            }));
        }
    }
}

/// Puts a finished radio in the queue. If `seed` is what's playing it keeps playing and the
/// radio replaces everything else; otherwise the seed starts, followed by the radio.
pub fn queue_radio(player: &PlayerHandle, seed: &Track, tracks: Vec<Track>) {
    let current = player.snapshot().current;
    match current.filter(|e| e.track.locator == seed.locator) {
        Some(cur) => {
            let others: Vec<EntryId> = player
                .queue()
                .read()
                .entries()
                .iter()
                .map(|e| e.id)
                .filter(|id| *id != cur.id)
                .collect();
            player.send(PlayerCommand::Remove(others));
            player.send(PlayerCommand::Enqueue(tracks));
        }
        None => {
            let mut all = vec![seed.clone()];
            all.extend(tracks);
            player.send(PlayerCommand::PlayTracks {
                tracks: all,
                start: 0,
            });
        }
    }
}

/// The title to put in a search box: without bracketed extras and " - Remastered" suffixes.
fn search_title(title: &str) -> &str {
    let cut = title
        .find(['(', '['])
        .into_iter()
        .chain(title.find(" - "))
        .min()
        .unwrap_or(title.len());
    let t = title[..cut].trim();
    if t.is_empty() { title } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_titles_drop_extras() {
        assert_eq!(search_title("Tsunami (Radio Edit)"), "Tsunami");
        assert_eq!(search_title("Bad - 2012 Remaster"), "Bad");
        assert_eq!(search_title("(Intro)"), "(Intro)");
        assert_eq!(search_title("Plain"), "Plain");
    }
}
