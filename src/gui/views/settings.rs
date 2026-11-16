//! The Settings page: frame rates, playback, Last.fm, library, audio and Spotify (saved to
//! config.toml where it applies).

use crate::config::Config;
use crate::gui::app::{Gui, count};
use crate::gui::render::Color;
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::icons::Icon;
use crate::gui::ui::themes::{self, Preset};
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
        player: &Player,
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

        section(ui, &mut y, "APPEARANCE");
        let gap = 14.0;
        let card_w = ((body.w - 2.0 * gap) / 3.0).min(168.0);
        let mut picked = None;
        for (i, p) in themes::PRESETS.iter().enumerate() {
            let r = Rect::new(lx + i as f32 * (card_w + gap), y, card_w, 104.0);
            if theme_card(ui, r, p, self.theme.key == p.key) {
                picked = Some(p.key);
            }
        }
        if let Some(key) = picked {
            self.set_theme(key);
        }
        y += 132.0;

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
        row_text(
            ui,
            y,
            "Cover visualizer",
            "Bars around the playing song's cover move to the music",
        );
        let on = self.cover_visualizer as usize;
        if let Some(i) = ui.segmented(
            anim::id("set-cover-vis"),
            control_x,
            y + 3.0,
            &["Off", "On"],
            on,
        ) && i != on
        {
            self.cover_visualizer = i == 1;
            self.save_bool("gui.cover_visualizer", i == 1);
        }
        y += 66.0;

        section(ui, &mut y, "VISUALIZER");
        let analysis = player.analysis();
        self.visualizer_settings(ui, body, control_x, &mut y, &analysis);

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

        if let Some(d) = self.discord.clone() {
            use crate::discord::Status as D;
            section(ui, &mut y, "DISCORD");
            row_text(
                ui,
                y,
                "Show on Discord",
                "Your Discord status shows the song you're listening to",
            );
            let on = d.enabled() as usize;
            if let Some(i) = ui.segmented(
                anim::id("set-discord"),
                control_x,
                y + 3.0,
                &["Off", "On"],
                on,
            ) && i != on
            {
                d.set_enabled(i == 1);
                self.save_bool("discord.enabled", i == 1);
            }
            y += 58.0;
            let status = d.status();
            if status != D::Off {
                let (title, hint) = match &status {
                    D::Connected(name) => (
                        format!("Connected as {name}"),
                        "Shows while something plays".to_string(),
                    ),
                    D::NotRunning => (
                        "Waiting for Discord".into(),
                        "Open the Discord app; Khoostic connects on its own".into(),
                    ),
                    D::Failed(e) => ("Not connected".into(), e.clone()),
                    _ => (
                        "Needs an application id".into(),
                        "Make an app named Khoostic there, paste its id below".into(),
                    ),
                };
                row_text(ui, y, &title, &hint);
                if !matches!(status, D::Connected(_))
                    && ui
                        .pill("Open Discord's developer page", control_x, y + 3.0, false)
                        .0
                {
                    let _ = open::that("https://discord.com/developers/applications");
                }
                y += 58.0;
                row_text(
                    ui,
                    y,
                    "Application id",
                    "From discord.com/developers/applications",
                );
                if !self.discord_id.focused && self.discord_id.text.is_empty() {
                    self.discord_id.text = d.client_id();
                }
                let w = (body.right() - control_x - 90.0).clamp(160.0, 260.0);
                ui.text_input(
                    &mut self.discord_id,
                    Rect::new(control_x, y, w, 36.0),
                    "Application id",
                    None,
                );
                let id = self.discord_id.text.trim().to_owned();
                let changed = id != d.client_id();
                let enter = self.discord_id.focused
                    && ui
                        .input
                        .keys
                        .iter()
                        .any(|k| k.key == crate::gui::ui::Key::Enter);
                let valid = id.len() >= 15 && id.bytes().all(|b| b.is_ascii_digit());
                if (ui
                    .pill("Save", control_x + w + 10.0, y + 1.0, changed && valid)
                    .0
                    || enter)
                    && changed
                {
                    if valid {
                        d.set_client_id(&id);
                        self.save_str("discord.client_id", &id);
                        self.discord_id.focused = false;
                        self.toast("Saved. Connecting to Discord…");
                    } else {
                        self.toast("An application id is a long number");
                    }
                }
                y += 58.0;
            }
        }
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

/// A small picture of a theme (its page, panel, accent and visualizer colors) with its name.
/// True when clicked.
fn theme_card(ui: &mut Ui, r: Rect, p: &Preset, selected: bool) -> bool {
    let t = ui.theme.clone();
    let c = |hex: &str| Color::hex(hex).unwrap_or(t.fg);
    let id = anim::id(("theme-card", p.key));
    let hover = ui.hover_t(id, r);
    let sel = ui.anim.to(id ^ 1, selected as u8 as f32, anim::NORMAL);
    ui.pointer |= ui.hovered(r);
    let lift = 2.0 * hover;
    let r = Rect::new(r.x, r.y - lift, r.w, r.h);
    ui.c.glow(
        r,
        Color::BLACK.alpha(0.25 + 0.2 * hover),
        10.0,
        8.0 + 6.0 * hover,
    );

    // The theme's own colors: window, a page panel with a header wash, a few rows, a play
    // button and visualizer bars.
    let art = Rect::new(r.x, r.y, r.w, r.h - 30.0);
    ui.c.rect(art, c(p.background), 10.0);
    let panel = Rect::new(art.x + 28.0, art.y + 8.0, art.w - 36.0, art.h - 16.0);
    ui.c.rect(panel, c(p.surface), 6.0);
    ui.c.gradient(
        Rect::new(panel.x, panel.y, panel.w, panel.h * 0.6),
        c(p.accent).alpha(0.16),
        c(p.accent).alpha(0.0),
        6.0,
    );
    for k in 0..3 {
        let ry = art.y + 12.0 + k as f32 * 9.0;
        ui.c.rect(
            Rect::new(art.x + 8.0, ry, 14.0, 4.0),
            c(p.dim).alpha(0.7),
            2.0,
        );
    }
    ui.c.rect(
        Rect::new(panel.x + 8.0, panel.y + 8.0, panel.w * 0.45, 6.0),
        c(p.fg),
        3.0,
    );
    ui.c.rect(
        Rect::new(panel.x + 8.0, panel.y + 19.0, panel.w * 0.3, 4.0),
        c(p.dim),
        2.0,
    );
    ui.c.circle(panel.x + 15.0, panel.bottom() - 13.0, 7.0, c(p.accent));
    let bars = 9;
    let bx = panel.x + 30.0;
    let bw = ((panel.right() - 8.0 - bx) / bars as f32 - 2.0).max(1.0);
    for k in 0..bars {
        let f = k as f32 / (bars - 1) as f32;
        let h = 6.0 + 12.0 * (0.5 + 0.5 * (k as f32 * 1.7).sin()).powf(1.5);
        let n = p.spectrum.len().max(1) - 1;
        let col = c(p.spectrum[((f * n as f32).round() as usize).min(n)]);
        ui.c.rect(
            Rect::new(bx + k as f32 * (bw + 2.0), panel.bottom() - 6.0 - h, bw, h),
            col,
            bw / 2.0,
        );
    }

    // Name below, and the selection ring in the current accent.
    ui.label(
        p.name,
        r.x + 2.0,
        Rect::new(r.x + 2.0, art.bottom() + 6.0, r.w - 30.0, 20.0),
        r.w - 30.0,
        t.font_size,
        Weight::Medium,
        if selected {
            t.fg
        } else {
            t.dim.lerp(t.fg, hover)
        },
    );
    if sel > 0.01 {
        ui.c.outline(art.inset(-3.0, -3.0), t.accent.alpha(sel), 13.0, 2.0);
        crate::gui::ui::icons::draw(
            ui.c,
            Icon::Check,
            Rect::new(r.right() - 20.0, art.bottom() + 8.0, 16.0, 16.0),
            t.accent.alpha(sel),
        );
    }
    ui.clicked(r)
}
