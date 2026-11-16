//! Online data for the GUI: Spotify playlists, liked songs, followed artists, artist pages
//! and search, plus lyrics and genres. Everything is fetched in the background, polled once per
//! frame and cached for the session.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gui::app::{Gui, View, count};
use crate::model::{Lyrics, Track, TrackLocator};
use crate::pending::Pending;
use crate::spotify::{AddOutcome, ArtistPage, ArtistSummary, Playlist, SearchResults, Status};

/// Debounce for Spotify search-as-you-type: long enough to skip most keystrokes of a fast
/// typist, short enough to feel immediate.
const SEARCH_DELAY: Duration = Duration::from_millis(140);
/// Searches remembered (backspacing shows earlier results instantly).
const SEARCH_CACHE: usize = 64;

#[derive(Default)]
pub(crate) struct SpState {
    pub status_version: Option<u64>,
    pub playlists: Option<Arc<Vec<Playlist>>>,
    pub(crate) playlists_pending: Option<Pending<Vec<Playlist>>>,
    /// Track lists by key: "liked", "pl:<id>", "al:<id>".
    pub tracks: HashMap<String, Arc<Vec<Track>>>,
    pub(crate) tracks_pending: HashMap<String, Pending<Vec<Track>>>,
    /// Play this list as soon as it has loaded (a play button on something not loaded yet).
    pub(crate) play_when_loaded: Option<String>,
    /// Finished searches by query, and the latest one (shown while a newer one runs).
    pub(crate) searches: HashMap<String, Arc<SearchResults>>,
    pub(crate) search_order: Vec<String>,
    pub(crate) last_search: Option<(String, Arc<SearchResults>)>,
    pub(crate) search_pending: Vec<(String, Pending<SearchResults>)>,
    pub(crate) search_due: Option<(String, Instant)>,
    pub(crate) adds: Vec<(String, Pending<AddOutcome>)>,
    /// Khoostic (local DB) playlists with their tracks, refreshed with the library.
    pub local: Vec<(crate::library::Playlist, Arc<Vec<Track>>)>,
    /// Liked songs from your files (uris), and as tracks.
    pub local_liked: HashSet<String>,
    pub local_liked_tracks: Arc<Vec<Track>>,
    pub(crate) local_version: Option<u64>,
    /// Spotify track ids in Liked Songs.
    pub liked: HashSet<String>,
    pub(crate) liked_pending: Option<Pending<HashSet<String>>>,
    pub followed: Option<Arc<Vec<ArtistSummary>>>,
    pub(crate) followed_pending: Option<Pending<Vec<ArtistSummary>>>,
    /// Likes and follows on their way to Spotify: (what failed, if it does; request).
    pub(crate) writes: Vec<(String, Pending<()>)>,
    pub artists: HashMap<String, Arc<ArtistPage>>,
    pub(crate) artists_pending: HashMap<String, Pending<ArtistPage>>,
    /// Pictures for artists seen in search results, by id.
    pub artist_pics: HashMap<String, ArtistSummary>,
    pub(crate) pics_pending: Option<(Vec<String>, Pending<Vec<ArtistSummary>>)>,
    /// Genres by artist name.
    pub genres: HashMap<String, Vec<String>>,
    pub(crate) genres_pending: HashMap<String, Pending<Vec<String>>>,
    /// Lyrics for the song with this uri (`None` inside: there are none).
    pub lyrics: Option<(String, Option<Arc<Lyrics>>)>,
    pub(crate) lyrics_pending: Option<(String, Pending<Option<Arc<Lyrics>>>)>,
    /// Liked Songs as shown (local likes + Spotify's), built from these two lists.
    pub(crate) liked_view: Option<(usize, usize, Arc<Vec<Track>>)>,
}

impl Gui {
    /// Background results arrive here once per frame.
    pub(crate) fn poll_spotify(&mut self, player: &crate::player::Player) {
        self.poll_local();
        self.poll_extras();
        let Some(sp) = self.spotify.clone() else {
            return;
        };
        if self.sp.status_version != Some(sp.version()) {
            self.sp.status_version = Some(sp.version());
            if sp.is_connected() {
                // Load what the library panel and like buttons need.
                self.sp.liked_pending.get_or_insert_with(|| sp.liked_ids());
                self.sp
                    .followed_pending
                    .get_or_insert_with(|| sp.followed_artists());
            } else {
                self.sp.playlists = None;
                self.sp.tracks.clear();
                self.sp.searches.clear();
                self.sp.last_search = None;
                self.sp.liked.clear();
                self.sp.followed = None;
            }
        }
        if let Some(r) = self.sp.playlists_pending.as_ref().and_then(Pending::poll) {
            self.sp.playlists_pending = None;
            match r {
                Ok(list) => self.sp.playlists = Some(Arc::new(list)),
                Err(e) => self.toast(format!("Spotify: {e:#}")),
            }
        }
        if let Some(r) = self.sp.liked_pending.as_ref().and_then(Pending::poll) {
            self.sp.liked_pending = None;
            match r {
                Ok(ids) => self.sp.liked = ids,
                Err(e) => tracing::warn!("liked songs: {e:#}"),
            }
        }
        if let Some(r) = self.sp.followed_pending.as_ref().and_then(Pending::poll) {
            self.sp.followed_pending = None;
            match r {
                Ok(list) => self.sp.followed = Some(Arc::new(list)),
                Err(e) => tracing::warn!("followed artists: {e:#}"),
            }
        }
        let done: Vec<(String, anyhow::Result<Vec<Track>>)> = self
            .sp
            .tracks_pending
            .iter()
            .filter_map(|(k, p)| p.poll().map(|r| (k.clone(), r)))
            .collect();
        for (k, r) in done {
            self.sp.tracks_pending.remove(&k);
            match r {
                Ok(t) => {
                    if self.sp.play_when_loaded.as_deref() == Some(k.as_str()) {
                        self.sp.play_when_loaded = None;
                        let tracks = self.liked_merged(&k, &t);
                        self.play_tracks(tracks, player, false);
                    }
                    self.sp.tracks.insert(k, Arc::new(t));
                }
                Err(e) => self.toast(format!("Spotify: {e:#}")),
            }
        }
        let mut finished = Vec::new();
        self.sp.search_pending.retain(|(q, p)| match p.poll() {
            Some(r) => {
                finished.push((q.clone(), r));
                false
            }
            None => true,
        });
        for (q, r) in finished {
            match r {
                Ok(res) => {
                    let res = Arc::new(res);
                    self.remember_search(q.clone(), res.clone());
                    self.sp.last_search = Some((q, res));
                }
                Err(e) => tracing::debug!("Spotify search: {e:#}"),
            }
        }
        if let Some((q, due)) = &self.sp.search_due
            && Instant::now() >= *due
            && sp.is_connected()
        {
            let q = q.clone();
            self.sp.search_due = None;
            self.sp.search_pending.push((q.clone(), sp.search(&q, 20)));
        }
        let done: Vec<(String, anyhow::Result<ArtistPage>)> = self
            .sp
            .artists_pending
            .iter()
            .filter_map(|(k, p)| p.poll().map(|r| (k.clone(), r)))
            .collect();
        for (id, r) in done {
            self.sp.artists_pending.remove(&id);
            match r {
                Ok(page) => {
                    if self.sp.play_when_loaded.as_deref() == Some(&format!("artist:{id}")) {
                        self.sp.play_when_loaded = None;
                        player.send(crate::player::PlayerCommand::PlayTracks {
                            tracks: page.top_tracks.clone(),
                            start: 0,
                        });
                    }
                    self.sp.artists.insert(id, Arc::new(page));
                }
                Err(e) => self.toast(format!("Couldn't load the artist: {e:#}")),
            }
        }
        if let Some(r) = self.sp.pics_pending.as_ref().and_then(|p| p.1.poll()) {
            let (asked, _) = self.sp.pics_pending.take().expect("checked");
            match r {
                Ok(found) => {
                    for a in found {
                        self.sp.artist_pics.insert(a.id.clone(), a);
                    }
                }
                Err(e) => tracing::debug!("artist pictures: {e:#}"),
            }
            // Don't ask again for ones Spotify had no answer for.
            for id in asked {
                self.sp
                    .artist_pics
                    .entry(id.clone())
                    .or_insert_with(|| ArtistSummary {
                        id,
                        name: String::new(),
                        image: None,
                    });
            }
        }
        let mut failed = Vec::new();
        self.sp.writes.retain(|(what, p)| match p.poll() {
            Some(Err(e)) => {
                failed.push(format!("{what}: {e:#}"));
                false
            }
            Some(Ok(())) => false,
            None => true,
        });
        if !failed.is_empty() {
            // Spotify is the truth: reload likes and follows.
            self.sp.liked_pending = Some(sp.liked_ids());
            self.sp.followed_pending = Some(sp.followed_artists());
            self.toast(failed.join("; "));
        }
        let mut finished = Vec::new();
        self.sp.adds.retain(|(name, p)| match p.poll() {
            Some(r) => {
                finished.push((name.clone(), r));
                false
            }
            None => true,
        });
        for (name, r) in finished {
            match r {
                Ok(o) if o.skipped_local > 0 && o.added == 0 => {
                    self.toast("Local tracks can't be added to Spotify playlists");
                }
                Ok(o) if o.skipped_local > 0 => self.toast(format!(
                    "Added {} to {name}; skipped {} local",
                    count(o.added, "song"),
                    o.skipped_local
                )),
                Ok(o) => self.toast(format!("Added {} to {name}", count(o.added, "song"))),
                Err(e) => self.toast(format!("Couldn't add to {name}: {e:#}")),
            }
            // The playlist changed on Spotify: refetch next time it's shown.
            self.sp.tracks.retain(|k, _| !k.starts_with("pl:"));
            self.sp.playlists = None;
        }
    }

    /// Khoostic playlists and local likes follow the library version.
    fn poll_local(&mut self) {
        let v = self.library.version();
        if self.sp.local_version == Some(v) {
            return;
        }
        self.sp.local_version = Some(v);
        self.sp.local = self
            .library
            .playlists()
            .into_iter()
            .map(|p| {
                let tracks = Arc::new(self.library.playlist_tracks(p.id));
                (p, tracks)
            })
            .collect();
        self.sp.local_liked = self.library.liked_uris();
        self.sp.local_liked_tracks = Arc::new(self.library.liked_tracks());
    }

    /// Lyrics and genres (not Spotify-specific).
    fn poll_extras(&mut self) {
        if let Some(r) = self.sp.lyrics_pending.as_ref().and_then(|p| p.1.poll()) {
            let (uri, _) = self.sp.lyrics_pending.take().expect("checked");
            let found = r.unwrap_or_else(|e| {
                tracing::debug!("lyrics: {e:#}");
                None
            });
            self.sp.lyrics = Some((uri, found));
        }
        let done: Vec<(String, anyhow::Result<Vec<String>>)> = self
            .sp
            .genres_pending
            .iter()
            .filter_map(|(k, p)| p.poll().map(|r| (k.clone(), r)))
            .collect();
        for (name, r) in done {
            self.sp.genres_pending.remove(&name);
            let genres = r.unwrap_or_else(|e| {
                tracing::debug!("genres for {name}: {e:#}");
                Vec::new()
            });
            self.sp.genres.insert(name, genres);
        }
    }

    fn remember_search(&mut self, q: String, res: Arc<SearchResults>) {
        if self.sp.searches.insert(q.clone(), res).is_none() {
            self.sp.search_order.push(q);
            if self.sp.search_order.len() > SEARCH_CACHE {
                let old = self.sp.search_order.remove(0);
                self.sp.searches.remove(&old);
            }
        }
    }

    pub(crate) fn spotify_busy(&self) -> bool {
        self.sp.playlists_pending.is_some()
            || !self.sp.tracks_pending.is_empty()
            || !self.sp.search_pending.is_empty()
            || self.sp.search_due.is_some()
            || !self.sp.adds.is_empty()
            || !self.sp.writes.is_empty()
            || !self.sp.artists_pending.is_empty()
            || self.sp.pics_pending.is_some()
            || self.sp.liked_pending.is_some()
            || self.sp.followed_pending.is_some()
            || !self.sp.genres_pending.is_empty()
            || self.sp.lyrics_pending.is_some()
            || self
                .spotify
                .as_ref()
                .is_some_and(|s| matches!(s.status(), Status::SigningIn | Status::Connecting))
    }

    pub(crate) fn spotify_connected(&self) -> bool {
        self.spotify.as_ref().is_some_and(|s| s.is_connected())
    }

    pub(crate) fn ensure_playlists(&mut self) -> Option<Arc<Vec<Playlist>>> {
        let sp = self.spotify.clone()?;
        if !sp.is_connected() {
            return None;
        }
        if self.sp.playlists.is_none() && self.sp.playlists_pending.is_none() {
            self.sp.playlists_pending = Some(sp.playlists());
        }
        self.sp.playlists.clone()
    }

    pub(crate) fn ensure_tracks(&mut self, key: &str) -> Option<Arc<Vec<Track>>> {
        if let Some(t) = self.sp.tracks.get(key) {
            return Some(t.clone());
        }
        let sp = self.spotify.clone()?;
        if sp.is_connected() && !self.sp.tracks_pending.contains_key(key) {
            let pending = if key == "liked" {
                sp.saved_tracks()
            } else if let Some(id) = key.strip_prefix("pl:") {
                sp.playlist_tracks(id)
            } else {
                let id = key.strip_prefix("al:")?;
                sp.album_tracks(id)
            };
            self.sp.tracks_pending.insert(key.to_owned(), pending);
        }
        None
    }

    /// Plays a Spotify list now if it's loaded, else as soon as it is.
    pub(crate) fn play_list_key(&mut self, key: &str, player: &crate::player::Player) {
        match self.ensure_tracks(key) {
            Some(t) => {
                let tracks = self.liked_merged(key, &t);
                self.play_tracks(tracks, player, false);
            }
            None if key == "liked" && !self.spotify_connected() => {
                let t = self.sp.local_liked_tracks.to_vec();
                self.play_tracks(t, player, false);
            }
            None => self.sp.play_when_loaded = Some(key.to_owned()),
        }
    }

    /// Liked Songs is your Spotify likes plus liked songs from your files.
    pub(crate) fn liked_merged(&self, key: &str, spotify: &[Track]) -> Vec<Track> {
        let mut out = spotify.to_vec();
        if key == "liked" {
            out.splice(0..0, self.sp.local_liked_tracks.iter().cloned());
        }
        out
    }

    /// Schedules a (debounced) Spotify search for `q`. Returns the results for `q` if known,
    /// else the latest results (so the page doesn't blank while typing).
    pub(crate) fn spotify_search(&mut self, q: &str) -> Option<(bool, Arc<SearchResults>)> {
        if !self.spotify_connected() || q.is_empty() {
            return None;
        }
        if let Some(res) = self.sp.searches.get(q) {
            return Some((true, res.clone()));
        }
        let queued = self.sp.search_due.as_ref().map(|d| d.0.as_str()) == Some(q)
            || self.sp.search_pending.iter().any(|p| p.0 == q);
        if !queued {
            self.sp.search_due = Some((q.to_owned(), Instant::now() + SEARCH_DELAY));
        }
        self.sp.last_search.as_ref().map(|s| (false, s.1.clone()))
    }

    /// Loads pictures for these artists (search results) if not known yet.
    pub(crate) fn want_artist_pics(&mut self, ids: &[String]) {
        if self.sp.pics_pending.is_some() {
            return;
        }
        let Some(sp) = self.spotify.clone().filter(|s| s.is_connected()) else {
            return;
        };
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !self.sp.artist_pics.contains_key(*id))
            .take(16)
            .cloned()
            .collect();
        if !missing.is_empty() {
            let p = sp.artist_summaries(missing.clone());
            self.sp.pics_pending = Some((missing, p));
        }
    }

    pub(crate) fn ensure_artist(&mut self, id: &str) -> Option<Arc<ArtistPage>> {
        if let Some(a) = self.sp.artists.get(id) {
            return Some(a.clone());
        }
        let sp = self.spotify.clone().filter(|s| s.is_connected())?;
        if !self.sp.artists_pending.contains_key(id) {
            self.sp.artists_pending.insert(id.to_owned(), sp.artist(id));
        }
        None
    }

    /// Genres for an artist (empty until known, or if none).
    pub(crate) fn genres_of(&mut self, artist: &str) -> Vec<String> {
        if let Some(g) = self.sp.genres.get(artist) {
            return g.clone();
        }
        if !self.sp.genres_pending.contains_key(artist) {
            let p = self.genres.of(artist);
            self.sp.genres_pending.insert(artist.to_owned(), p);
        }
        Vec::new()
    }

    /// Lyrics for `track`: `None` while looking, `Some(None)` when there are none.
    pub(crate) fn lyrics_for(&mut self, track: &Track) -> Option<Option<Arc<Lyrics>>> {
        let uri = track.locator.to_uri();
        if let Some((u, l)) = &self.sp.lyrics
            && *u == uri
        {
            return Some(l.clone());
        }
        if self.sp.lyrics_pending.as_ref().map(|p| &p.0) != Some(&uri) {
            self.sp.lyrics_pending = Some((uri, self.lyrics.get(track)));
        }
        None
    }

    pub(crate) fn is_liked(&self, t: &Track) -> bool {
        match &t.locator {
            TrackLocator::Spotify { id } => self.sp.liked.contains(id),
            other => self.sp.local_liked.contains(&other.to_uri()),
        }
    }

    /// Likes or unlikes a song: Spotify songs on Spotify, everything else in the library.
    pub(crate) fn set_liked(&mut self, t: &Track, liked: bool) {
        match &t.locator {
            TrackLocator::Spotify { id } => {
                let Some(sp) = self.spotify.clone().filter(|s| s.is_connected()) else {
                    return self.toast("Connect Spotify to like Spotify songs");
                };
                if liked {
                    self.sp.liked.insert(id.clone());
                } else {
                    self.sp.liked.remove(id);
                }
                self.sp.tracks.remove("liked");
                let what = if liked {
                    "Couldn't add to Liked Songs"
                } else {
                    "Couldn't remove from Liked Songs"
                };
                self.sp
                    .writes
                    .push((what.into(), sp.set_liked(vec![id.clone()], liked)));
            }
            _ => {
                if let Err(e) = self.library.set_liked(t, liked) {
                    return self.toast(format!("Couldn't update Liked Songs: {e:#}"));
                }
            }
        }
        self.toast(if liked {
            "Added to Liked Songs"
        } else {
            "Removed from Liked Songs"
        });
    }

    pub(crate) fn is_following(&self, artist_id: &str) -> bool {
        self.sp
            .followed
            .as_ref()
            .is_some_and(|f| f.iter().any(|a| a.id == artist_id))
    }

    pub(crate) fn set_following(&mut self, artist: ArtistSummary, follow: bool) {
        let Some(sp) = self.spotify.clone().filter(|s| s.is_connected()) else {
            return;
        };
        let mut list = self
            .sp
            .followed
            .as_ref()
            .map(|f| f.to_vec())
            .unwrap_or_default();
        list.retain(|a| a.id != artist.id);
        if follow {
            list.insert(0, artist.clone());
        }
        self.sp.followed = Some(Arc::new(list));
        let what = if follow {
            format!("Couldn't follow {}", artist.name)
        } else {
            format!("Couldn't unfollow {}", artist.name)
        };
        self.sp
            .writes
            .push((what, sp.set_following(&artist.id, follow)));
    }

    /// Tracks of the open playlist / liked songs / Spotify album page, if loaded.
    pub(crate) fn collection_tracks(&self) -> Option<Arc<Vec<Track>>> {
        match &self.view {
            View::LocalPlaylist(id) => self
                .sp
                .local
                .iter()
                .find(|(p, _)| p.id == *id)
                .map(|(_, t)| t.clone()),
            View::SpotifyPlaylist { id, .. } => self.sp.tracks.get(&format!("pl:{id}")).cloned(),
            View::SpotifyAlbum { id, .. } => self.sp.tracks.get(&format!("al:{id}")).cloned(),
            View::Liked => self.sp.liked_view.as_ref().map(|c| c.2.clone()),
            _ => None,
        }
    }
}

impl Gui {
    /// Liked Songs: liked songs from your files first, then Spotify's (once loaded).
    /// `None` while Spotify's list is still loading.
    pub(crate) fn liked_tracks(&mut self) -> Option<Arc<Vec<Track>>> {
        let spotify = if self.spotify_connected() {
            Some(self.ensure_tracks("liked")?)
        } else {
            None
        };
        let local = self.sp.local_liked_tracks.clone();
        let key = (
            Arc::as_ptr(&local) as usize,
            spotify.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize),
        );
        if let Some((a, b, t)) = &self.sp.liked_view
            && (*a, *b) == key
        {
            return Some(t.clone());
        }
        let mut all = local.to_vec();
        if let Some(s) = &spotify {
            all.extend(s.iter().cloned());
        }
        let all = Arc::new(all);
        self.sp.liked_view = Some((key.0, key.1, all.clone()));
        Some(all)
    }
}
