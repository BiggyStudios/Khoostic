//! Spotify data for the GUI: playlists, liked songs, albums and search, fetched in the
//! background and cached per session.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gui::app::{Gui, View, count};
use crate::model::Track;
use crate::pending::Pending;
use crate::spotify::{AddOutcome, Playlist, SearchResults, Status};

/// Debounce for Spotify search-as-you-type.
const SEARCH_DELAY: Duration = Duration::from_millis(350);

#[derive(Default)]
pub(crate) struct SpState {
    pub status_version: Option<u64>,
    pub playlists: Option<Arc<Vec<Playlist>>>,
    pub(crate) playlists_pending: Option<Pending<Vec<Playlist>>>,
    /// Track lists by key: "liked", "pl:<id>", "al:<id>".
    pub tracks: HashMap<String, Arc<Vec<Track>>>,
    pub(crate) tracks_pending: HashMap<String, Pending<Vec<Track>>>,
    pub search: Option<(String, Arc<SearchResults>)>,
    pub(crate) search_pending: Option<(String, Pending<SearchResults>)>,
    pub(crate) search_due: Option<(String, Instant)>,
    pub(crate) adds: Vec<(String, Pending<AddOutcome>)>,
    /// Khoostic (local DB) playlists with their tracks, refreshed with the library.
    pub local: Vec<(crate::library::Playlist, Arc<Vec<Track>>)>,
    pub(crate) local_version: Option<u64>,
}

impl Gui {
    /// Background results arrive here once per frame.
    pub(crate) fn poll_spotify(&mut self) {
        // Khoostic playlists follow the library version.
        let v = self.library.version();
        if self.sp.local_version != Some(v) {
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
        }
        let Some(sp) = self.spotify.clone() else {
            return;
        };
        if self.sp.status_version != Some(sp.version()) {
            self.sp.status_version = Some(sp.version());
            if !sp.is_connected() {
                self.sp.playlists = None;
                self.sp.tracks.clear();
                self.sp.search = None;
            }
        }
        if let Some(p) = &self.sp.playlists_pending
            && let Some(r) = p.poll()
        {
            self.sp.playlists_pending = None;
            match r {
                Ok(list) => self.sp.playlists = Some(Arc::new(list)),
                Err(e) => self.toast(format!("Spotify: {e:#}")),
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
                    self.sp.tracks.insert(k, Arc::new(t));
                }
                Err(e) => self.toast(format!("Spotify: {e:#}")),
            }
        }
        if let Some((q, p)) = &self.sp.search_pending
            && let Some(r) = p.poll()
        {
            let q = q.clone();
            self.sp.search_pending = None;
            match r {
                Ok(res) => self.sp.search = Some((q, Arc::new(res))),
                Err(e) => self.toast(format!("Spotify search: {e:#}")),
            }
        }
        if let Some((q, due)) = &self.sp.search_due
            && Instant::now() >= *due
            && sp.is_connected()
        {
            let q = q.clone();
            self.sp.search_due = None;
            self.sp.search_pending = Some((q.clone(), sp.search(&q, 30)));
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

    pub(crate) fn spotify_busy(&self) -> bool {
        self.sp.playlists_pending.is_some()
            || !self.sp.tracks_pending.is_empty()
            || self.sp.search_pending.is_some()
            || self.sp.search_due.is_some()
            || !self.sp.adds.is_empty()
            || self
                .spotify
                .as_ref()
                .is_some_and(|s| matches!(s.status(), Status::SigningIn | Status::Connecting))
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

    /// Schedules a (debounced) Spotify search for `q`.
    pub(crate) fn spotify_search(&mut self, q: &str) -> Option<Arc<SearchResults>> {
        let connected = self.spotify.as_ref().is_some_and(|s| s.is_connected());
        if !connected || q.is_empty() {
            return None;
        }
        match &self.sp.search {
            Some((sq, res)) if sq == q => return Some(res.clone()),
            _ => {}
        }
        let queued = self.sp.search_due.as_ref().map(|d| d.0.as_str()) == Some(q)
            || self.sp.search_pending.as_ref().map(|p| p.0.as_str()) == Some(q);
        if !queued {
            self.sp.search_due = Some((q.to_owned(), Instant::now() + SEARCH_DELAY));
        }
        self.sp.search.as_ref().map(|s| s.1.clone())
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
            View::Liked => self.sp.tracks.get("liked").cloned(),
            _ => None,
        }
    }
}
