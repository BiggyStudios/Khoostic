//! The top bar: logo, back/forward, home, the search box and settings.

use crate::gui::app::{Gui, View};
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::icons::Icon;
use crate::gui::ui::{Ui, anim};
use crate::spotify::Status;

/// Width the search box grows to on wide windows.
const SEARCH_W: f32 = 480.0;
const SEARCH_H: f32 = 44.0;

impl Gui {
    /// `r` is the whole bar; `content_x` is where the main panel starts (back/forward line up
    /// with it).
    pub(crate) fn top_bar(&mut self, ui: &mut Ui, r: Rect, content_x: f32) {
        let t = ui.theme.clone();
        let cy = r.y + r.h / 2.0;

        // Logo.
        ui.c.circle(r.x + 28.0, cy, 4.5, t.accent);
        ui.label(
            "khoostic",
            r.x + 42.0,
            Rect::new(r.x, r.y, 200.0, r.h),
            160.0,
            18.0,
            Weight::Bold,
            t.fg,
        );

        // Back / forward, dimmed when there's nowhere to go.
        let nav = [
            (Icon::Back, !self.history.is_empty(), "top-back"),
            (Icon::Forward, !self.forward.is_empty(), "top-fwd"),
        ];
        for (i, (icon, enabled, key)) in nav.into_iter().enumerate() {
            let x = content_x + 18.0 + i as f32 * 40.0;
            ui.c.circle(x, cy, 16.0, t.surface);
            let color = if enabled {
                t.fg.alpha(0.85)
            } else {
                t.dim.alpha(0.45)
            };
            let hover = if enabled { t.fg } else { color };
            if ui.icon_button(anim::id(key), icon, (x, cy), 16.0, color, hover) && enabled {
                if i == 0 {
                    self.back();
                } else {
                    self.go_forward();
                }
            }
        }

        // Home + search, centered on the window, but never over the buttons on either side.
        let left = content_x + 140.0; // past back/forward and the home button
        let right = r.right() - 110.0; // before the account and settings buttons
        let sw = SEARCH_W.min(right - left).max(120.0);
        let sx = (r.x + (r.w - sw) / 2.0 + 28.0).clamp(left, (right - sw).max(left));
        let home = (sx - 34.0, cy);
        let on_home = self.view == View::Albums;
        ui.c.circle(home.0, home.1, SEARCH_H / 2.0, t.surface);
        let hc = if on_home { t.fg } else { t.dim };
        if ui.icon_button(anim::id("top-home"), Icon::Home, home, 20.0, hc, t.fg) {
            self.search.text.clear();
            self.search.cursor = 0;
            self.search.focused = false;
            self.switch(View::Albums);
        }
        let field = Rect::new(sx, cy - SEARCH_H / 2.0, sw, SEARCH_H);
        let was_empty = self.search.text.is_empty();
        let placeholder = if self.spotify.as_ref().is_some_and(|s| s.is_connected()) {
            "What do you want to play?"
        } else {
            "Search your library"
        };
        if ui.text_field(&mut self.search, field, placeholder) {
            self.search_changed(was_empty);
        }
        if !self.search.text.is_empty()
            && ui.icon_button(
                anim::id("top-clear"),
                Icon::Close,
                (field.right() - 22.0, cy),
                14.0,
                t.dim,
                t.fg,
            )
        {
            self.search.text.clear();
            self.search.cursor = 0;
            self.search_changed(false);
        }

        // Settings, and the Spotify account as an initial in a circle.
        let mut x = r.right() - 30.0;
        let on_settings = self.view == View::Settings;
        let gc = if on_settings { t.accent } else { t.dim };
        if ui.icon_button(
            anim::id("top-settings"),
            Icon::Settings,
            (x, cy),
            19.0,
            gc,
            t.fg,
        ) {
            if on_settings {
                self.back();
            } else {
                self.navigate(View::Settings);
            }
        }
        if let Some(Status::Connected(a)) = self.spotify.as_ref().map(|s| s.status()) {
            x -= 44.0;
            let disc = Rect::new(x - 16.0, cy - 16.0, 32.0, 32.0);
            let h = ui.hover_t(anim::id("top-account"), disc);
            ui.c.circle(x, cy, 16.0 + 1.5 * h, t.surface_hover);
            ui.c.circle(x, cy, 13.0, t.accent.alpha(0.9));
            let initial: String = a.name.chars().take(1).collect::<String>().to_uppercase();
            ui.label_center(&initial, disc, 13.0, Weight::Bold, t.background);
            if ui.clicked(disc) {
                self.navigate(View::Settings);
            }
        }
    }

    /// Typing in the search box opens the results page; clearing it goes back.
    pub(crate) fn search_changed(&mut self, was_empty: bool) {
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
}
