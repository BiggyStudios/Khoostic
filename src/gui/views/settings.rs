//! The Settings page: frame rates, playback, Last.fm, library, audio and Spotify (saved to
//! config.toml where it applies).

use crate::config::Config;
use crate::gui::app::{Gui, count};
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::icons::Icon;
use crate::gui::ui::{Ui, anim};
use crate::lyrics::Provider;
use crate::player::{Player, PlayerSnapshot};
use crate::spotify::Status;

const FPS_OPTIONS: [u32; 3] = [30, 60, 120];
const BACKGROUND_OPTIONS: [u32; 3] = [10, 30, 60];

impl Gui {
    pub(crate) fn settings_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        _player: &Player,
        snap: &PlayerSnapshot,
    ) {
        let t = ui.theme.clone();
        let body = self.header(ui, r, "Settings", "Changes apply right away");
        let mut ls = self.lists.get("settings").cloned().unwrap_or_default();
        ls.height = body.h;
        if ui.hovered(body) {
            ls.target += ui.input.scroll;
        }
        ui.c.push_clip(Rect::new(
            body.x - 10.0,
            body.y - 4.0,
            body.w + 20.0,
            body.h + 4.0,
        ));
        let top = body.y - ls.scroll;
        let mut y = top;
        let lx = body.x;
        let control_x = body.x + (body.w * 0.5).clamp(300.0, 460.0);

        let section = |ui: &mut Ui, y: &mut f32, title: &str| {
            ui.label(
                title,
                lx,
                Rect::new(lx, *y, body.w, 24.0),
                body.w,
                12.0,
                Weight::Bold,
                t.dim.alpha(0.8),
            );
            *y += 32.0;
        };
        let row_text = |ui: &mut Ui, y: f32, title: &str, hint: &str| {
            ui.label(
                title,
                lx,
                Rect::new(lx, y + 2.0, control_x - lx, 20.0),
                control_x - lx - 24.0,
                t.font_size,
                Weight::Medium,
                t.fg,
            );
            ui.label(
                hint,
                lx,
                Rect::new(lx, y + 22.0, control_x - lx, 18.0),
                control_x - lx - 24.0,
                12.0,
                Weight::Regular,
                t.dim,
            );
        };

        section(ui, &mut y, "DISPLAY");
        row_text(
            ui,
            y,
            "Frame rate",
            "While the window is focused and something moves",
        );
        let labels: Vec<String> = FPS_OPTIONS.iter().map(|f| format!("{f} fps")).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let sel = nearest(&FPS_OPTIONS, self.fps);
        if let Some(i) = ui.segmented(anim::id("set-fps"), control_x, y + 3.0, &refs, sel) {
            self.fps = FPS_OPTIONS[i];
            self.save("gui.fps", self.fps);
        }
        y += 58.0;
        row_text(
            ui,
            y,
            "In the background",
            "When another window has focus. Minimized, Khoostic doesn't draw at all",
        );
        let labels: Vec<String> = BACKGROUND_OPTIONS
            .iter()
            .map(|f| format!("{f} fps"))
            .collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let sel = nearest(&BACKGROUND_OPTIONS, self.background_fps);
        if let Some(i) = ui.segmented(anim::id("set-bg-fps"), control_x, y + 3.0, &refs, sel) {
            self.background_fps = BACKGROUND_OPTIONS[i];
            self.save("gui.background_fps", self.background_fps);
        }
        y += 54.0;
        let measured = format!("Drawing at about {:.0} fps right now", self.measured_fps);
        ui.label(
            &measured,
            control_x + 4.0,
            Rect::new(control_x, y - 6.0, body.w, 18.0),
            body.right() - control_x,
            12.0,
            Weight::Regular,
            t.dim.alpha(0.8),
        );
        y += 34.0;

        section(ui, &mut y, "LIBRARY");
        let dirs = if self.music_dirs.is_empty() {
            "No music folders set ([library] music_dirs in config.toml)".to_string()
        } else {
            self.music_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join("  ·  ")
        };
        let stats = format!(
            "{} · {}",
            count(self.cache.songs.len(), "song"),
            count(self.cache.albums.len(), "album")
        );
        row_text(ui, y, &stats, &dirs);
        let scanning = self.library.is_scanning();
        let label = if scanning { "Scanning…" } else { "Rescan" };
        if ui.pill(label, control_x, y + 3.0, scanning).0 && !scanning {
            if self.music_dirs.is_empty() {
                self.toast("Add music folders to config.toml first");
            } else {
                self.library.start_scan(self.music_dirs.clone());
                self.toast("Rescanning your music");
            }
        }
        y += 66.0;

        section(ui, &mut y, "AUDIO");
        let device = if snap.device.is_empty() {
            "No output device".to_string()
        } else {
            snap.device.clone()
        };
        row_text(
            ui,
            y,
            &device,
            &format!(
                "{} Hz · set [audio] device in config.toml to change",
                snap.sample_rate
            ),
        );
        y += 66.0;

        section(ui, &mut y, "PLAYBACK");
        row_text(
            ui,
            y,
            "Autoplay",
            "When the queue ends, keep playing songs like the last ones",
        );
        let on = self.discovery.autoplay() as usize;
        if let Some(i) = ui.segmented(
            anim::id("set-autoplay"),
            control_x,
            y + 3.0,
            &["Off", "On"],
            on,
        ) && i != on
        {
            self.discovery.set_autoplay(i == 1);
            self.save_bool("playback.autoplay", i == 1);
        }
        y += 66.0;

        // Lyrics sources: on/off each, and the order they're tried in (the arrows).
        section(ui, &mut y, "LYRICS");
        let enabled = self.lyrics.providers();
        let mut rows: Vec<(Provider, bool)> = enabled.iter().map(|&p| (p, true)).collect();
        rows.extend(
            Provider::ALL
                .iter()
                .filter(|p| !enabled.contains(p))
                .map(|&p| (p, false)),
        );
        let mut change: Option<Vec<Provider>> = None;
        for (i, &(p, on)) in rows.iter().enumerate() {
            let (title, hint) = match p {
                Provider::Local => (
                    "Your files",
                    ".lrc files next to your songs, or lyrics saved in their tags",
                ),
                Provider::Spotify => (
                    "Spotify",
                    "Spotify's synced lyrics, also for your own songs it can match",
                ),
                Provider::Lrclib => (
                    "LRCLIB",
                    "lrclib.net, a free public lyrics library (sent the song's title and artist)",
                ),
            };
            let title = if on {
                format!("{}. {title}", i + 1)
            } else {
                title.to_string()
            };
            row_text(ui, y, &title, hint);
            if on {
                let cy = y + 20.0;
                let up = i > 0;
                let down = i + 1 < enabled.len();
                let arrows = [(Icon::Up, up, 14.0), (Icon::Down, down, 46.0)];
                for (icon, active, dx) in arrows {
                    let color = if active { t.dim } else { t.dim.alpha(0.3) };
                    let hover = if active { t.fg } else { color };
                    let id = anim::id(("set-lyrics-move", p.key(), dx as i32));
                    if ui.icon_button(id, icon, (control_x + dx, cy), 18.0, color, hover) && active
                    {
                        let mut list = enabled.clone();
                        let j = if icon == Icon::Up { i - 1 } else { i + 1 };
                        list.swap(i, j);
                        change = Some(list);
                    }
                }
            }
            let id = anim::id(("set-lyrics", p.key()));
            if let Some(s) =
                ui.segmented(id, control_x + 76.0, y + 3.0, &["Off", "On"], on as usize)
                && (s == 1) != on
            {
                let mut list = enabled.clone();
                if on {
                    list.retain(|&q| q != p);
                } else {
                    list.push(p);
                }
                change = Some(list);
            }
            y += 58.0;
        }
        if enabled.is_empty() {
            ui.label(
                "Lyrics are off: turn on at least one source",
                control_x + 4.0,
                Rect::new(control_x, y - 6.0, body.w, 18.0),
                body.right() - control_x,
                12.0,
                Weight::Regular,
                t.dim.alpha(0.8),
            );
            y += 24.0;
        }
        if let Some(list) = change {
            self.set_lyrics_providers(list);
        }
        y += 8.0;

        if let Some(lf) = self.discovery.lastfm().cloned() {
            use crate::lastfm::Status as L;
            section(ui, &mut y, "LAST.FM");
            match lf.status() {
                L::NoApiKey => {
                    row_text(
                        ui,
                        y,
                        "API account",
                        "Last.fm needs one per app: create it free at last.fm/api/account/create",
                    );
                    let w = (body.right() - control_x).clamp(200.0, 360.0);
                    ui.text_input(
                        &mut self.lf_key,
                        Rect::new(control_x, y, w, 36.0),
                        "API key",
                        None,
                    );
                    ui.text_input(
                        &mut self.lf_secret,
                        Rect::new(control_x, y + 44.0, w, 36.0),
                        "Shared secret",
                        None,
                    );
                    let ready = !self.lf_key.text.trim().is_empty()
                        && !self.lf_secret.text.trim().is_empty();
                    if ui.pill("Save", control_x, y + 88.0, ready).0 && ready {
                        let (k, s) = (
                            self.lf_key.text.trim().to_owned(),
                            self.lf_secret.text.trim().to_owned(),
                        );
                        match lf.set_keys(&k, &s) {
                            Ok(()) => {
                                self.save_str("lastfm.api_key", &k);
                                self.save_str("lastfm.api_secret", &s);
                                self.lf_key.focused = false;
                                self.lf_secret.focused = false;
                                self.toast("Saved. Now connect your Last.fm account");
                            }
                            Err(e) => self.toast(format!("Last.fm: {e:#}")),
                        }
                    }
                    y += 140.0;
                }
                status => {
                    let (title, hint, action) = match &status {
                        L::Connected(name) => (
                            format!("Scrobbling as {name}"),
                            match lf.queued() {
                                0 => "Everything you've listened to is sent".to_string(),
                                n => format!("{} waiting to be sent", count(n, "listen")),
                            },
                            "Sign out",
                        ),
                        L::SigningIn => (
                            "Waiting for the browser…".into(),
                            "Allow Khoostic in the Last.fm tab that opened".into(),
                            "Cancel",
                        ),
                        L::Failed(e) => ("Not connected".into(), e.clone(), "Connect"),
                        _ => (
                            "Not connected".into(),
                            "Sign in through your browser to scrobble".into(),
                            "Connect",
                        ),
                    };
                    row_text(ui, y, &title, &hint);
                    if ui.pill(action, control_x, y + 3.0, false).0 {
                        match action {
                            "Sign out" => {
                                lf.sign_out();
                                self.toast("Signed out of Last.fm");
                            }
                            "Cancel" => lf.cancel_sign_in(),
                            _ => lf.sign_in(),
                        }
                    }
                    y += 58.0;
                    row_text(
                        ui,
                        y,
                        "Scrobbling",
                        "Send what you play (songs count after half, or 4 minutes)",
                    );
                    let on = lf.scrobbling.load(std::sync::atomic::Ordering::Acquire) as usize;
                    if let Some(i) = ui.segmented(
                        anim::id("set-scrobble"),
                        control_x,
                        y + 3.0,
                        &["Off", "On"],
                        on,
                    ) && i != on
                    {
                        lf.scrobbling
                            .store(i == 1, std::sync::atomic::Ordering::Release);
                        self.save_bool("lastfm.scrobble", i == 1);
                    }
                    y += 66.0;
                }
            }
        }

        if let Some(sp) = self.spotify.clone() {
            section(ui, &mut y, "SPOTIFY");
            let (title, hint, action) = match sp.status() {
                Status::Connected(a) => (
                    format!("Signed in as {}", a.name),
                    if a.premium {
                        "Premium: streaming available".to_string()
                    } else {
                        "Premium is needed to play Spotify tracks".to_string()
                    },
                    Some("Sign out"),
                ),
                Status::SigningIn => (
                    "Waiting for the browser…".into(),
                    "Approve Khoostic in the tab that opened".into(),
                    Some("Cancel"),
                ),
                Status::Connecting => ("Connecting…".into(), String::new(), None),
                Status::Failed(e) => ("Not connected".into(), e, Some("Connect")),
                Status::Disconnected => (
                    "Not connected".into(),
                    "Sign in with your Spotify account in the browser".into(),
                    Some("Connect"),
                ),
            };
            row_text(ui, y, &title, &hint);
            if let Some(label) = action
                && ui.pill(label, control_x, y + 3.0, false).0
            {
                match label {
                    "Sign out" => {
                        sp.sign_out();
                        self.toast("Signed out of Spotify");
                    }
                    "Cancel" => sp.cancel_sign_in(),
                    _ => sp.sign_in(),
                }
            }
        }
        y += 66.0;
        ui.c.pop_clip();
        // Scrolls when the window is too short for everything.
        let content = y - top;
        ls.target = ls.target.clamp(0.0, (content - body.h).max(0.0));
        self.lists.insert("settings", ls);
    }

    pub(crate) fn save_bool(&mut self, key: &str, value: bool) {
        if let Some(path) = self.config_path.clone()
            && let Err(e) = Config::save_value(&path, key, value)
        {
            self.toast(format!("Couldn't save: {e:#}"));
        }
    }

    pub(crate) fn save_str(&mut self, key: &str, value: &str) {
        if let Some(path) = self.config_path.clone()
            && let Err(e) = Config::save_value(&path, key, value)
        {
            self.toast(format!("Couldn't save: {e:#}"));
        }
    }

    /// Uses these lyrics sources from now on and saves them to the config.
    fn set_lyrics_providers(&mut self, list: Vec<Provider>) {
        self.lyrics.set_providers(list.clone());
        // Look the playing song up again with the new sources.
        self.sp.lyrics = None;
        self.sp.lyrics_pending = None;
        self.lyrics_layout = None;
        let Some(path) = self.config_path.clone() else {
            return self.toast("No config location; the change lasts until you quit");
        };
        let keys: toml_edit::Array = list.iter().map(|p| p.key()).collect();
        if let Err(e) = Config::save_value(&path, "lyrics.providers", keys) {
            self.toast(format!("Couldn't save: {e:#}"));
        }
    }

    fn save(&mut self, key: &str, value: u32) {
        let Some(path) = self.config_path.clone() else {
            return self.toast("No config location; the change lasts until you quit");
        };
        if let Err(e) = Config::save_value(&path, key, value as i64) {
            self.toast(format!("Couldn't save: {e:#}"));
        }
    }
}

fn nearest(options: &[u32], v: u32) -> usize {
    options
        .iter()
        .enumerate()
        .min_by_key(|(_, o)| o.abs_diff(v))
        .map_or(0, |(i, _)| i)
}
