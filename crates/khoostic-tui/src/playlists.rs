//! The TUI's Playlists view: Khoostic playlists, Spotify liked songs, Spotify search results and
//! Spotify playlists in one list; Enter opens one, Enter on a track plays from there.

use std::sync::Arc;

use khoostic_core::{Track, TrackLocator};
use khoostic_library::Library;
use khoostic_spotify::{AddOutcome, Pending, Playlist, Spotify, Status};

use crate::app::ListView;

#[derive(Debug, Clone)]
pub enum Entry {
    Local {
        id: i64,
        name: String,
        tracks: usize,
    },
    Liked,
    Search {
        query: String,
        tracks: usize,
    },
    Spotify(Playlist),
}

impl Entry {
    pub fn title(&self) -> String {
        match self {
            Entry::Local { name, .. } => name.clone(),
            Entry::Liked => "Liked Songs".into(),
            Entry::Search { query, .. } => format!("Search: {query}"),
            Entry::Spotify(p) => p.name.clone(),
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Entry::Local { tracks, .. } => format!("khoostic · {tracks} songs"),
            Entry::Liked => "spotify".into(),
            Entry::Search { tracks, .. } => format!("spotify · {tracks} results"),
            Entry::Spotify(p) if p.owned_by_me => format!("spotify · {} songs", p.tracks),
            Entry::Spotify(p) => format!("spotify · by {}", p.owner),
        }
    }
}

pub struct PlState {
    pub spotify: Option<Arc<Spotify>>,
    local: Vec<(khoostic_library::Playlist, Vec<Track>)>,
    local_version: Option<u64>,
    sp_playlists: Vec<Playlist>,
    sp_pending: Option<Pending<Vec<Playlist>>>,
    sp_version: Option<u64>,
    search: Option<(String, Vec<Track>)>,
    search_pending: Option<(String, Pending<khoostic_spotify::SearchResults>)>,
    tracks_pending: Option<(String, Pending<Vec<Track>>)>,
    adds: Vec<(String, Pending<AddOutcome>)>,
    /// The open playlist: (title, tracks).
    pub open: Option<(String, Vec<Track>)>,
    pub root_view: ListView,
    pub tracks_view: ListView,
}

impl PlState {
    pub fn new(spotify: Option<Arc<Spotify>>) -> Self {
        Self {
            spotify,
            local: Vec::new(),
            local_version: None,
            sp_playlists: Vec::new(),
            sp_pending: None,
            sp_version: None,
            search: None,
            search_pending: None,
            tracks_pending: None,
            adds: Vec::new(),
            open: None,
            root_view: ListView::default(),
            tracks_view: ListView::default(),
        }
    }

    pub fn connected(&self) -> bool {
        self.spotify.as_ref().is_some_and(|s| s.is_connected())
    }

    pub fn status_line(&self) -> String {
        match self.spotify.as_ref().map(|s| s.status()) {
            None => String::new(),
            Some(Status::Disconnected) => "spotify off".into(),
            Some(Status::SigningIn) => "spotify: finish in browser".into(),
            Some(Status::Connecting) => "spotify: connecting".into(),
            Some(Status::Connected(a)) if a.premium => format!("spotify: {}", a.name),
            Some(Status::Connected(a)) => format!("spotify: {} (no premium)", a.name),
            Some(Status::Failed(_)) => "spotify: error".into(),
        }
    }

    pub fn entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .local
            .iter()
            .map(|(p, t)| Entry::Local {
                id: p.id,
                name: p.name.clone(),
                tracks: t.len(),
            })
            .collect();
        if self.connected() {
            out.push(Entry::Liked);
            if let Some((q, t)) = &self.search {
                out.push(Entry::Search {
                    query: q.clone(),
                    tracks: t.len(),
                });
            }
            out.extend(self.sp_playlists.iter().cloned().map(Entry::Spotify));
        }
        out
    }

    /// Polls background work. Returns messages to show (errors, add results).
    pub fn tick(&mut self, library: &Library) -> Vec<(String, bool)> {
        let mut msgs = Vec::new();
        self.reload_local(library);
        let Some(sp) = self.spotify.clone() else {
            return msgs;
        };
        if self.sp_version != Some(sp.version()) {
            self.sp_version = Some(sp.version());
            match sp.status() {
                Status::Connected(_) => self.sp_pending = Some(sp.playlists()),
                Status::Failed(e) => msgs.push((format!("spotify: {e}"), true)),
                _ => self.sp_playlists.clear(),
            }
        }
        if let Some(r) = self.sp_pending.as_ref().and_then(|p| p.poll()) {
            self.sp_pending = None;
            match r {
                Ok(p) => self.sp_playlists = p,
                Err(e) => msgs.push((format!("spotify: {e:#}"), true)),
            }
        }
        if let Some(r) = self.search_pending.as_ref().and_then(|p| p.1.poll()) {
            let q = self.search_pending.take().expect("checked").0;
            match r {
                Ok(res) => {
                    msgs.push((
                        format!("spotify: {} results for “{q}”", res.tracks.len()),
                        false,
                    ));
                    self.open = Some((format!("Search: {q}"), res.tracks.clone()));
                    self.tracks_view = ListView::default();
                    self.search = Some((q, res.tracks));
                }
                Err(e) => msgs.push((format!("spotify search: {e:#}"), true)),
            }
        }
        if let Some(r) = self.tracks_pending.as_ref().and_then(|p| p.1.poll()) {
            let title = self.tracks_pending.take().expect("checked").0;
            match r {
                Ok(t) => {
                    self.open = Some((title, t));
                    self.tracks_view = ListView::default();
                }
                Err(e) => msgs.push((format!("spotify: {e:#}"), true)),
            }
        }
        self.adds.retain(|(name, p)| match p.poll() {
            None => true,
            Some(Ok(o)) if o.added == 0 && o.skipped_local > 0 => {
                msgs.push((
                    "local tracks can't be added to Spotify playlists".into(),
                    true,
                ));
                false
            }
            Some(Ok(o)) => {
                let skipped = if o.skipped_local > 0 {
                    format!(" (skipped {} local)", o.skipped_local)
                } else {
                    String::new()
                };
                msgs.push((format!("added {} to {name}{skipped}", o.added), false));
                false
            }
            Some(Err(e)) => {
                msgs.push((format!("{name}: {e:#}"), true));
                false
            }
        });
        msgs
    }

    /// Re-reads Khoostic playlists if the library changed.
    fn reload_local(&mut self, library: &Library) {
        let v = library.version();
        if self.local_version != Some(v) {
            self.local_version = Some(v);
            self.local = library
                .playlists()
                .into_iter()
                .map(|p| {
                    let t = library.playlist_tracks(p.id);
                    (p, t)
                })
                .collect();
        }
    }

    pub fn busy(&self) -> bool {
        self.sp_pending.is_some()
            || self.search_pending.is_some()
            || self.tracks_pending.is_some()
            || !self.adds.is_empty()
    }

    pub fn search(&mut self, query: &str) -> Result<(), String> {
        let sp = self.spotify.clone().ok_or("Spotify isn't available")?;
        if !sp.is_connected() {
            return Err("connect first: :spotify login".into());
        }
        self.search_pending = Some((query.to_owned(), sp.search(query, 50)));
        Ok(())
    }

    /// Opens the entry at `index` (loading Spotify tracks in the background).
    pub fn open_entry(&mut self, index: usize) {
        let Some(entry) = self.entries().get(index).cloned() else {
            return;
        };
        let title = entry.title();
        match entry {
            Entry::Local { id, .. } => {
                let tracks = self
                    .local
                    .iter()
                    .find(|(p, _)| p.id == id)
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                self.open = Some((title, tracks));
                self.tracks_view = ListView::default();
            }
            Entry::Search { .. } => {
                let tracks = self
                    .search
                    .as_ref()
                    .map(|s| s.1.clone())
                    .unwrap_or_default();
                self.open = Some((title, tracks));
                self.tracks_view = ListView::default();
            }
            Entry::Liked => {
                if let Some(sp) = &self.spotify {
                    self.tracks_pending = Some((title, sp.saved_tracks()));
                }
            }
            Entry::Spotify(p) => {
                if let Some(sp) = &self.spotify {
                    self.tracks_pending = Some((title, sp.playlist_tracks(&p.id)));
                }
            }
        }
    }

    /// Adds tracks to the playlist whose name starts with `name` (case-insensitive).
    /// Khoostic playlists take anything; Spotify playlists refuse local tracks.
    pub fn add_to(
        &mut self,
        library: &Library,
        name: &str,
        tracks: Vec<Track>,
    ) -> Result<String, String> {
        // A playlist created moments ago (e.g. by :newpl) must be findable right away.
        self.reload_local(library);
        let needle = name.to_lowercase();
        if let Some((p, existing)) = self
            .local
            .iter()
            .find(|(p, _)| p.name.to_lowercase().starts_with(&needle))
        {
            let mut all = existing.clone();
            let n = tracks.len();
            all.extend(tracks);
            library
                .set_playlist_tracks(p.id, &all)
                .map_err(|e| e.to_string())?;
            return Ok(format!("added {n} to {}", p.name));
        }
        let target = self
            .sp_playlists
            .iter()
            .find(|p| p.owned_by_me && p.name.to_lowercase().starts_with(&needle))
            .cloned()
            .ok_or_else(|| format!("no playlist named “{name}” (create one: :newpl <name>)"))?;
        if tracks
            .iter()
            .all(|t| !matches!(t.locator, TrackLocator::Spotify { .. }))
        {
            return Err("local tracks can't be added to Spotify playlists".into());
        }
        let sp = self.spotify.clone().ok_or("Spotify isn't available")?;
        self.adds
            .push((target.name.clone(), sp.add_to_playlist(&target.id, &tracks)));
        Ok(format!("adding to {}…", target.name))
    }
}
