//! The sidebar: navigation, the sliding selection indicator and the Spotify section.

use crate::gui::app::{Gui, View};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::Ui;
use crate::gui::ui::anim;
use crate::gui::ui::icons::{self, Icon};
use crate::player::Player;
use crate::spotify::Status;

/// What the Spotify row in the sidebar does when clicked.
type SpotifyAction = fn(&crate::spotify::Spotify);

const LIBRARY_NAV: [(View, &str, Icon); 4] = [
    (View::Albums, "Albums", Icon::Album),
    (View::Artists, "Artists", Icon::Artist),
    (View::Songs, "Songs", Icon::Song),
    (View::Playlists, "Playlists", Icon::Playlist),
];

const PLAYER_NAV: [(View, &str, Icon); 3] = [
    (View::Queue, "Queue", Icon::Queue),
    (View::Files, "Files", Icon::Folder),
    (View::Visualizer, "Visualizer", Icon::Visualizer),
];

impl Gui {
    pub(crate) fn sidebar(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let t = ui.theme.clone();
        let mark = Rect::new(r.x + 24.0, r.y + 18.0, r.w, 34.0);
        ui.c.circle(mark.x + 4.0, mark.y + mark.h / 2.0, 4.0, t.accent);
        ui.label(
            "khoostic",
            mark.x + 16.0,
            mark,
            r.w,
            18.0,
            Weight::Bold,
            t.fg,
        );

        let field = Rect::new(r.x + 16.0, r.y + 66.0, r.w - 24.0, 38.0);
        let was_empty = self.search.text.is_empty();
        if ui.text_field(&mut self.search, field, "Search") {
            if self.search.text.trim().is_empty() {
                if self.view == View::Search {
                    self.back();
                }
            } else if self.view != View::Search {
                if was_empty {
                    self.navigate(View::Search);
                } else {
                    self.view = View::Search;
                }
            }
            self.list("search").selected = 0;
        }

        // Lay out every row first so the selection indicator can glide between them.
        struct NavRow {
            view: View,
            label: String,
            icon: Icon,
            rect: Rect,
            badge: Option<String>,
        }
        let queue_len = player.queue().read().len();
        let mut rows: Vec<NavRow> = Vec::new();
        let mut headers: Vec<(&str, f32)> = Vec::new();
        let mut y = r.y + 124.0;
        for (title, items) in [("Library", &LIBRARY_NAV[..]), ("Playing", &PLAYER_NAV[..])] {
            headers.push((title, y));
            y += 26.0;
            for (view, name, icon) in items {
                let badge = match view {
                    View::Queue if queue_len > 0 => Some(queue_len.to_string()),
                    _ => None,
                };
                rows.push(NavRow {
                    view: view.clone(),
                    label: name.to_string(),
                    icon: *icon,
                    rect: Rect::new(r.x + 12.0, y, r.w - 20.0, 36.0),
                    badge,
                });
                y += 38.0;
            }
            y += 16.0;
        }
        let spotify_y = y;
        let settings = Rect::new(r.x + 12.0, r.bottom() - 52.0, r.w - 20.0, 36.0);
        rows.push(NavRow {
            view: View::Settings,
            label: "Settings".into(),
            icon: Icon::Settings,
            rect: settings,
            badge: None,
        });
        let active = self.view.section();
        let liked_rect = Rect::new(r.x + 12.0, spotify_y + 26.0, r.w - 20.0, 36.0);
        let target = rows
            .iter()
            .find(|row| row.view == active)
            .map(|row| row.rect.y)
            .or((active == View::Liked).then_some(liked_rect.y));
        if let Some(ty) = target {
            let iy = ui.anim.to(anim::id("nav-y"), ty, 18.0);
            ui.c.rect(
                Rect::new(r.x + 12.0, iy, r.w - 20.0, 36.0),
                t.wash(0.075),
                t.radius,
            );
        }
        for (title, hy) in headers {
            ui.label(
                title,
                r.x + 26.0,
                Rect::new(r.x, hy, r.w, 22.0),
                r.w,
                11.5,
                Weight::Medium,
                t.dim.alpha(0.75),
            );
        }
        for row in &rows {
            let is_active = row.view == active;
            let id = anim::id(("nav", &row.label));
            let h = ui.hover_t(id, row.rect);
            if !is_active {
                ui.c.rect(row.rect, t.wash(0.04 * h), t.radius);
            }
            let on = ui.anim.to(id ^ 3, is_active as u8 as f32, anim::NORMAL);
            let icon_color = t.dim.lerp(t.fg, h * 0.6).lerp(t.accent, on);
            icons::draw(
                ui.c,
                row.icon,
                Rect::new(row.rect.x + 12.0, row.rect.y + 9.0, 18.0, 18.0),
                icon_color,
            );
            ui.label(
                &row.label,
                row.rect.x + 42.0,
                row.rect,
                row.rect.w - 80.0,
                t.font_size,
                Weight::Medium,
                t.dim.lerp(t.fg, (h * 0.7 + on).min(1.0)),
            );
            if let Some(b) = &row.badge {
                ui.label_right(
                    b,
                    row.rect.right() - 12.0,
                    row.rect,
                    11.5,
                    Weight::Medium,
                    t.dim.alpha(0.8),
                );
            }
            if ui.clicked(row.rect) {
                self.search.focused = false;
                self.switch(row.view.clone());
            }
        }
        self.sidebar_spotify(ui, r, spotify_y);
    }
}

impl Gui {
    /// The Spotify block of the sidebar. Returns the next free y.
    pub(crate) fn sidebar_spotify(&mut self, ui: &mut Ui, r: Rect, mut y: f32) -> f32 {
        let Some(sp) = self.spotify.clone() else {
            return y;
        };
        let t = ui.theme.clone();
        let green = Color::hex("#1ed760").unwrap_or(t.accent);
        ui.label(
            "Spotify",
            r.x + 26.0,
            Rect::new(r.x, y, r.w, 22.0),
            r.w,
            11.5,
            Weight::Medium,
            t.dim.alpha(0.75),
        );
        y += 26.0;
        let row = Rect::new(r.x + 12.0, y, r.w - 20.0, 36.0);
        let status = sp.status();
        let (label, sub, action): (&str, Option<String>, Option<SpotifyAction>) = match &status {
            Status::Disconnected => ("Connect Spotify", None, Some(|s| s.sign_in())),
            Status::Failed(e) => ("Connect Spotify", Some(e.clone()), Some(|s| s.sign_in())),
            Status::SigningIn => (
                "Waiting for browser…",
                Some("Click to cancel".into()),
                Some(|s| s.cancel_sign_in()),
            ),
            Status::Connecting => ("Connecting…", None, None),
            Status::Connected(_) => ("Liked Songs", None, None),
        };
        let connected = matches!(status, Status::Connected(_));
        let busy = matches!(status, Status::SigningIn | Status::Connecting);
        let active = self.view == View::Liked;
        let id = anim::id("nav-spotify");
        let h = ui.hover_t(id, row);
        if !active {
            ui.c.rect(row, t.wash(0.04 * h), t.radius);
        }
        let on = ui.anim.to(id ^ 3, active as u8 as f32, anim::NORMAL);
        let ic = Rect::new(row.x + 12.0, row.y + 9.0, 18.0, 18.0);
        if busy {
            // A spinning arc while the browser / connection is in progress.
            ui.anim.keep_alive();
            let a = self.last_spin();
            ui.c.arc(ic.x + 9.0, ic.y + 9.0, 7.0, a, a + 4.2, 2.0, t.accent);
        } else if connected {
            icons::draw(
                ui.c,
                Icon::Heart,
                ic,
                t.dim.lerp(t.fg, h * 0.6).lerp(green, on.max(0.0)),
            );
        } else {
            icons::draw(ui.c, Icon::Spotify, ic, t.dim.lerp(green, h));
        }
        let color = if busy {
            t.accent
        } else {
            t.dim.lerp(t.fg, (h * 0.7 + on).min(1.0))
        };
        ui.label(
            label,
            row.x + 42.0,
            row,
            row.w - 50.0,
            t.font_size,
            Weight::Medium,
            color,
        );
        if ui.clicked(row) {
            match (action, &status) {
                (Some(f), _) => f(&sp),
                (None, Status::Connected(_)) => self.switch(View::Liked),
                _ => {}
            }
        }
        y += 38.0;
        let note = match &status {
            Status::Connected(a) if !a.premium => Some(format!("{} · Premium needed", a.name)),
            Status::Connected(a) => Some(a.name.clone()),
            _ => sub,
        };
        if let Some(note) = note {
            ui.label(
                &note,
                r.x + 54.0,
                Rect::new(r.x, y - 6.0, r.w, 18.0),
                r.w - 66.0,
                11.5,
                Weight::Regular,
                t.dim.alpha(0.8),
            );
            y += 18.0;
        }
        y + 10.0
    }

    /// Angle for loading spinners: one turn every ~0.9 s.
    fn last_spin(&self) -> f32 {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() % 900) as f32;
        ms / 900.0 * std::f32::consts::TAU
    }
}
