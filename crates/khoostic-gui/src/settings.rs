//! The Settings page: frame rates (saved to config.toml), library, audio and Spotify status.

use khoostic_config::Config;
use khoostic_player::{Player, PlayerSnapshot};
use khoostic_spotify::Status;

use crate::anim;
use crate::app::{Gui, count};
use crate::canvas::{Rect, Weight};
use crate::ui::Ui;

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
        let mut y = body.y;
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
    }

    fn save(&mut self, key: &str, value: u32) {
        let Some(path) = self.config_path.clone() else {
            return self.toast("No config location; the change lasts until you quit");
        };
        if let Err(e) = Config::save_int(&path, key, value as i64) {
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
