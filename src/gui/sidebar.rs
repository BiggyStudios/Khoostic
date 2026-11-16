//! Your Library, on the left: Liked Songs, your Khoostic and Spotify playlists and the
//! artists you follow, with cover art. Filter chips, play on hover, right-click to hide (or
//! unfollow, or delete), and a collapsed mode that shows only the covers.

use crate::gui::app::{Gui, View};
use crate::gui::menu::LibraryMenu;
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::state::LibraryFilter;
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::search::spin_angle;
use crate::gui::views::spotify_green;
use crate::library::CoverSize;
use crate::model::CoverRef;
use crate::player::Player;
use crate::spotify::{ArtistSummary, Status};

/// One thing in Your Library.
#[derive(Clone)]
pub(crate) struct LibItem {
    /// Stable key, for hiding: "liked", "local:<id>", "sp:<id>", "artist:<id>".
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub cover: Option<CoverRef>,
    pub view: View,
    pub artist: Option<ArtistSummary>,
}

impl LibItem {
    fn is_artist(&self) -> bool {
        self.artist.is_some()
    }
}

const ROW_H: f32 = 64.0;
const ART: f32 = 48.0;
/// Narrower than this, the panel shows covers only.
const COMPACT: f32 = 150.0;

impl Gui {
    /// Everything Your Library lists, in order, before filtering and hiding.
    pub(crate) fn library_items(&mut self) -> Vec<LibItem> {
        let mut items = Vec::new();
        let liked = self.sp.liked.len() + self.sp.local_liked.len();
        items.push(LibItem {
            key: "liked".into(),
            title: "Liked Songs".into(),
            subtitle: format!("Playlist · {}", crate::gui::app::count(liked, "song")),
            cover: None,
            view: View::Liked,
            artist: None,
        });
        for (p, tracks) in &self.sp.local {
            items.push(LibItem {
                key: format!("local:{}", p.id),
                title: p.name.clone(),
                subtitle: "Playlist · Khoostic".into(),
                cover: tracks.iter().find_map(|t| t.cover.clone()),
                view: View::LocalPlaylist(p.id),
                artist: None,
            });
        }
        let me = match self.spotify.as_ref().map(|s| s.status()) {
            Some(Status::Connected(a)) => Some(a),
            _ => None,
        };
        if let Some(ps) = self.ensure_playlists() {
            for p in ps.iter() {
                let owner = match &me {
                    Some(a) if a.id == p.owner => a.name.clone(),
                    _ if p.owner == "spotify" => "Spotify".into(),
                    // Other people's accounts are opaque ids; their names aren't in the list.
                    _ if looks_like_id(&p.owner) => String::new(),
                    _ => p.owner.clone(),
                };
                items.push(LibItem {
                    key: format!("sp:{}", p.id),
                    title: p.name.clone(),
                    subtitle: if owner.is_empty() {
                        "Playlist".into()
                    } else {
                        format!("Playlist · {owner}")
                    },
                    cover: p.cover.clone(),
                    view: View::SpotifyPlaylist {
                        id: p.id.clone(),
                        name: p.name.clone(),
                    },
                    artist: None,
                });
            }
        }
        if let Some(f) = self.sp.followed.clone() {
            for a in f.iter() {
                items.push(LibItem {
                    key: format!("artist:{}", a.id),
                    title: a.name.clone(),
                    subtitle: "Artist".into(),
                    cover: a.image.clone(),
                    view: View::SpotifyArtist {
                        id: a.id.clone(),
                        name: a.name.clone(),
                    },
                    artist: Some(a.clone()),
                });
            }
        }
        items
    }

    pub(crate) fn sidebar(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let t = ui.theme.clone();
        ui.c.rect(r, t.surface, 12.0);
        let compact = r.w < COMPACT;

        // Header: "Your Library" (collapses the panel) and a new-playlist button.
        let head = Rect::new(r.x + 8.0, r.y + 10.0, r.w - 16.0, 40.0);
        let toggle = if compact {
            head
        } else {
            let w = ui.measure("Your Library", 15.0, Weight::Bold) + 52.0;
            Rect::new(head.x, head.y, w, head.h)
        };
        let th = ui.hover_t(anim::id("lib-toggle"), toggle);
        let icon_x = if compact {
            r.x + (r.w - 24.0) / 2.0
        } else {
            toggle.x + 12.0
        };
        icons::draw(
            ui.c,
            Icon::Library,
            Rect::new(icon_x, toggle.y + 8.0, 24.0, 24.0),
            t.dim.lerp(t.fg, th),
        );
        if !compact {
            ui.label(
                "Your Library",
                toggle.x + 46.0,
                toggle,
                toggle.w,
                15.0,
                Weight::Bold,
                t.dim.lerp(t.fg, 0.75 + 0.25 * th),
            );
        }
        ui.pointer |= ui.hovered(toggle);
        if ui.clicked(toggle) {
            self.ui_state.sidebar_collapsed = !self.ui_state.sidebar_collapsed;
            self.save_ui_state();
        }
        if !compact {
            let plus = (head.right() - 18.0, head.y + 20.0);
            ui.c.circle(plus.0, plus.1, 16.0, t.wash(0.06));
            if ui.icon_button(anim::id("lib-new"), Icon::Plus, plus, 16.0, t.dim, t.fg) {
                let name = format!("My Playlist #{}", self.sp.local.len() + 1);
                match self.library.create_playlist(&name, &[]) {
                    Ok(id) => {
                        self.toast(format!("Created “{name}”: right-click songs to add them"));
                        self.navigate(View::LocalPlaylist(id));
                    }
                    Err(e) => self.toast(format!("Couldn't create a playlist: {e:#}")),
                }
            }
        }

        // Filter chips.
        let mut y = head.bottom() + 8.0;
        if !compact {
            let filters = [
                (LibraryFilter::Playlists, "Playlists"),
                (LibraryFilter::Artists, "Artists"),
            ];
            let mut x = r.x + 16.0;
            for (f, label) in filters {
                let w = ui.measure(label, 13.0, Weight::Medium) + 26.0;
                let chip = Rect::new(x, y, w, 32.0);
                let on = self.ui_state.library_filter == f;
                let id = anim::id(("lib-chip", label));
                let h = ui.hover_t(id, chip);
                let o = ui.anim.to(id ^ 3, on as u8 as f32, anim::NORMAL);
                ui.c.rect(chip, t.wash(0.08 + 0.05 * h).lerp(t.fg, o), 16.0);
                ui.label_center(
                    label,
                    chip,
                    13.0,
                    Weight::Medium,
                    t.fg.lerp(t.background, o),
                );
                ui.pointer |= ui.hovered(chip);
                if ui.clicked(chip) {
                    self.ui_state.library_filter = if on { LibraryFilter::All } else { f };
                    self.list("sidebar").target = 0.0;
                    self.save_ui_state();
                }
                x += w + 8.0;
            }
            y += 46.0;
        }

        // The list.
        let filter = self.ui_state.library_filter;
        let all = self.library_items();
        let hidden_count = all
            .iter()
            .filter(|i| self.ui_state.hidden.contains(&i.key))
            .count();
        let items: Vec<LibItem> = all
            .into_iter()
            .filter(|i| match filter {
                _ if compact => true,
                LibraryFilter::All => true,
                LibraryFilter::Playlists => !i.is_artist(),
                LibraryFilter::Artists => i.is_artist(),
            })
            .filter(|i| self.show_hidden || !self.ui_state.hidden.contains(&i.key))
            .collect();
        let footer_h = self.sidebar_footer_height(compact, hidden_count);
        let list = Rect::new(r.x, y, r.w, (r.bottom() - y - footer_h).max(0.0));
        let row_h = if compact { 60.0 } else { ROW_H };
        let mut ls = self.lists.get("sidebar").cloned().unwrap_or_default();
        ls.height = list.h;
        if ui.hovered(list) {
            ls.target += ui.input.scroll;
        }
        ls.clamp(items.len(), row_h);
        ui.c.push_clip(list);
        let active_key = self.active_library_key();
        let mut tooltip = None;
        for (i, item) in items.iter().enumerate() {
            let ry = list.y + i as f32 * row_h - ls.scroll;
            if ry + row_h < list.y || ry > list.bottom() {
                continue;
            }
            let row = Rect::new(r.x + 8.0, ry, r.w - 16.0, row_h - 4.0);
            let hover = ui.hovered(row) && ui.hovered(list);
            let id = anim::id(("lib-row", item.key.as_str()));
            let h = ui.anim.to(id, hover as u8 as f32, anim::FAST);
            let active = active_key.as_deref() == Some(item.key.as_str());
            let fill = if active { 0.09 } else { 0.05 * h };
            if fill > 0.001 {
                ui.c.rect(row, t.wash(fill), t.radius);
            }
            let hidden = self.ui_state.hidden.contains(&item.key);
            let alpha = if hidden { 0.45 } else { 1.0 };
            let art = if compact {
                Rect::new(
                    r.x + (r.w - ART) / 2.0,
                    ry + (row_h - 4.0 - ART) / 2.0,
                    ART,
                    ART,
                )
            } else {
                Rect::new(row.x + 8.0, ry + (row_h - 4.0 - ART) / 2.0, ART, ART)
            };
            ui.c.push_fx(0.0, 0.0, alpha);
            draw_art(ui, art, item);
            ui.c.pop_fx();
            if !compact {
                let tx = art.right() + 12.0;
                let tw = row.right() - tx - 8.0;
                ui.label(
                    &item.title,
                    tx,
                    Rect::new(tx, ry + 11.0, tw, 20.0),
                    tw,
                    t.font_size,
                    Weight::Medium,
                    if active { t.accent } else { t.fg }.alpha(alpha),
                );
                let sx = tx;
                ui.label(
                    &item.subtitle,
                    sx,
                    Rect::new(sx, ry + 32.0, tw, 18.0),
                    tw - (sx - tx),
                    12.5,
                    Weight::Regular,
                    t.dim.alpha(alpha),
                );
            } else if hover {
                tooltip = Some((item.title.clone(), art));
            }
            ui.pointer |= hover;
            let on_play = play_overlay(ui, art, h, item.artist.is_some());
            if hover && ui.input.pressed {
                if on_play {
                    let v = item.view.clone();
                    self.play_view(&v, player);
                } else {
                    match &item.artist {
                        Some(a) => {
                            let (name, aid) = (a.name.clone(), a.id.clone());
                            self.open_artist(&name, Some(&aid));
                        }
                        None => self.navigate(item.view.clone()),
                    }
                }
            }
            if hover && ui.input.right_pressed {
                self.open_library_menu(LibraryMenu {
                    item: item.clone(),
                    hidden,
                });
            }
        }
        ui.c.pop_clip();
        crate::gui::views::lists::scrollbar_rows(
            ui,
            Rect {
                x: list.x - 14.0,
                ..list
            },
            &ls,
            items.len(),
            row_h,
        );
        self.lists.insert("sidebar", ls);

        // Names of covers in the collapsed panel, beside it (drawn over everything later).
        if let Some((name, art)) = tooltip {
            self.tooltip = Some((name, (r.right() + 10.0, art.y + art.h / 2.0)));
        }

        self.sidebar_footer(ui, r, compact, hidden_count);
    }

    /// The library key of the open page, to highlight its row.
    fn active_library_key(&self) -> Option<String> {
        Some(match &self.view {
            View::Liked => "liked".into(),
            View::LocalPlaylist(id) => format!("local:{id}"),
            View::SpotifyPlaylist { id, .. } => format!("sp:{id}"),
            View::SpotifyArtist { id, .. } => format!("artist:{id}"),
            _ => return None,
        })
    }

    fn sidebar_footer_height(&self, compact: bool, hidden: usize) -> f32 {
        let spotify_row = self.spotify.as_ref().is_some_and(|s| !s.is_connected());
        let mut h = 8.0;
        if spotify_row {
            h += if compact { 56.0 } else { 64.0 };
        }
        if hidden > 0 && !compact {
            h += 36.0;
        }
        h
    }

    /// "Show hidden" and, when Spotify isn't connected, the connect button.
    fn sidebar_footer(&mut self, ui: &mut Ui, r: Rect, compact: bool, hidden: usize) {
        let t = ui.theme.clone();
        let mut y = r.bottom() - self.sidebar_footer_height(compact, hidden) + 4.0;
        if hidden > 0 && !compact {
            let label = if self.show_hidden {
                "Don't show hidden".to_string()
            } else {
                format!("Show {hidden} hidden")
            };
            let row = Rect::new(r.x + 16.0, y, r.w - 32.0, 30.0);
            let hover = ui.hovered(row);
            ui.pointer |= hover;
            ui.label(
                &label,
                row.x,
                row,
                row.w,
                12.5,
                Weight::Medium,
                if hover { t.fg } else { t.dim },
            );
            if ui.clicked(row) {
                self.show_hidden = !self.show_hidden;
            }
            y += 36.0;
        }
        let Some(sp) = self.spotify.clone() else {
            return;
        };
        let status = sp.status();
        if matches!(status, Status::Connected(_)) {
            return;
        }
        let row = Rect::new(r.x + 8.0, y, r.w - 16.0, if compact { 52.0 } else { 56.0 });
        let h = ui.hover_t(anim::id("lib-spotify"), row);
        ui.c.rect(row, t.wash(0.04 + 0.04 * h), t.radius);
        let busy = matches!(status, Status::SigningIn | Status::Connecting);
        let ic = if compact {
            Rect::new(r.x + (r.w - 24.0) / 2.0, row.y + 14.0, 24.0, 24.0)
        } else {
            Rect::new(row.x + 14.0, row.y + 16.0, 24.0, 24.0)
        };
        if busy {
            ui.anim.keep_alive();
            let a = spin_angle();
            ui.c.arc(
                ic.x + 12.0,
                ic.y + 12.0,
                9.0,
                a,
                a + 4.2,
                2.0,
                spotify_green(),
            );
        } else {
            icons::draw(ui.c, Icon::Spotify, ic, spotify_green());
        }
        if !compact {
            let (title, sub) = match &status {
                Status::SigningIn => ("Waiting for your browser…", "Click to cancel".to_string()),
                Status::Connecting => ("Connecting to Spotify…", String::new()),
                Status::Failed(e) => ("Connect Spotify", e.clone()),
                _ => (
                    "Connect Spotify",
                    "Your playlists, liked songs and artists".to_string(),
                ),
            };
            let tx = ic.right() + 14.0;
            let tw = row.right() - tx - 8.0;
            ui.label(
                title,
                tx,
                Rect::new(tx, row.y + 9.0, tw, 20.0),
                tw,
                t.font_size - 0.5,
                Weight::Medium,
                t.fg,
            );
            ui.label(
                &sub,
                tx,
                Rect::new(tx, row.y + 29.0, tw, 18.0),
                tw,
                12.0,
                Weight::Regular,
                t.dim,
            );
        }
        ui.pointer |= ui.hovered(row);
        if ui.clicked(row) {
            match status {
                Status::SigningIn => sp.cancel_sign_in(),
                Status::Connecting => {}
                _ => sp.sign_in(),
            }
        }
    }
}

/// A library row's picture: cover art, a round portrait, or Liked Songs' heart tile.
fn draw_art(ui: &mut Ui, art: Rect, item: &LibItem) {
    if item.key == "liked" {
        ui.c.gradient(art, ui.theme.gradient(0.7), ui.theme.accent, 6.0);
        let s = art.w * 0.42;
        icons::draw(
            ui.c,
            Icon::HeartFilled,
            Rect::new(
                art.x + (art.w - s) / 2.0,
                art.y + (art.h - s) / 2.0 + 1.0,
                s,
                s,
            ),
            Color::WHITE,
        );
        return;
    }
    let radius = if item.artist.is_some() {
        art.w / 2.0
    } else {
        6.0
    };
    ui.cover(
        art,
        item.cover.as_ref(),
        CoverSize::Small,
        radius,
        &item.title,
    );
}

/// A dark veil with a play icon over a hovered cover. Returns whether the mouse is on it.
fn play_overlay(ui: &mut Ui, art: Rect, h: f32, round: bool) -> bool {
    if h <= 0.01 {
        return false;
    }
    let on = ui.hovered(art);
    if !on {
        return false;
    }
    let radius = if round { art.w / 2.0 } else { 6.0 };
    ui.c.set_layer(1);
    ui.c.rect(art, Color::BLACK.alpha(0.45 * h), radius);
    let s = art.w * 0.36;
    icons::draw(
        ui.c,
        Icon::Play,
        Rect::new(
            art.x + (art.w - s) / 2.0 + 1.0,
            art.y + (art.h - s) / 2.0,
            s,
            s,
        ),
        Color::WHITE.alpha(h),
    );
    ui.c.set_layer(0);
    true
}

/// A generated Spotify account id ("imj05t9vwafa4xydp502owjrx") rather than a chosen name.
fn looks_like_id(s: &str) -> bool {
    s.len() >= 20
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    #[test]
    fn account_ids() {
        assert!(super::looks_like_id("imj05t9vwafa4xydp502owjrx"));
        assert!(!super::looks_like_id("kubanana"));
        assert!(!super::looks_like_id("spotify"));
    }
}
