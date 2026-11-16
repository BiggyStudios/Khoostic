//! The top bar, which is also the window's title bar: back/forward, home, the search box,
//! settings and account, and the window buttons. Empty space drags the window; a double click
//! there maximizes it.

use crate::gui::app::{Gui, View, WindowCmd};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::spotify::Status;

/// Width the search box grows to on wide windows.
const SEARCH_W: f32 = 480.0;
const SEARCH_H: f32 = 44.0;
/// One window button (minimize, maximize, close), flush with the window's top-right corner.
const CAPTION_W: f32 = 46.0;
const CAPTION_H: f32 = 36.0;

impl Gui {
    /// `r` is the whole bar.
    pub(crate) fn top_bar(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme.clone();
        let cy = r.y + r.h / 2.0;

        // Back / forward, dimmed when there's nowhere to go.
        let nav = [
            (Icon::Back, !self.history.is_empty(), "top-back"),
            (Icon::Forward, !self.forward.is_empty(), "top-fwd"),
        ];
        for (i, (icon, enabled, key)) in nav.into_iter().enumerate() {
            let x = r.x + 28.0 + i as f32 * 40.0;
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

        // Window buttons, then settings and the account to their left.
        let captions = 3.0 * CAPTION_W;
        self.caption_buttons(
            ui,
            Rect::new(r.right() - captions, r.y, captions, CAPTION_H),
        );
        let mut x = r.right() - captions - 26.0;
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
            ui.pointer |= ui.hovered(disc);
            if ui.clicked(disc) {
                self.navigate(View::Settings);
            }
        }

        // Home + search, centered on the window, but never over the buttons on either side.
        let left = r.x + 140.0; // past back/forward and the home button
        let right = x - 40.0;
        let sw = SEARCH_W.min(right - left).max(120.0);
        let sx = (r.x + (r.w - sw) / 2.0 + 28.0).clamp(left, (right - sw).max(left));
        let home = (sx - 34.0, cy);
        let on_home = self.view.section() == View::Albums;
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

        // Anything not taken by a control moves the window.
        if ui.input.pressed && ui.hovered(r) && !ui.pointer {
            self.window_cmd = Some(if ui.input.double_click {
                WindowCmd::ToggleMaximize
            } else {
                WindowCmd::Drag
            });
        }
    }

    /// Minimize, maximize/restore and close, Windows style: flat, with close turning red.
    fn caption_buttons(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme.clone();
        let red = Color::rgba8(196, 43, 28, 255);
        let buttons = [
            (Icon::Minimize, WindowCmd::Minimize),
            (
                if self.maximized {
                    Icon::Restore
                } else {
                    Icon::Maximize
                },
                WindowCmd::ToggleMaximize,
            ),
            (Icon::Close, WindowCmd::Close),
        ];
        for (i, (icon, cmd)) in buttons.into_iter().enumerate() {
            let b = Rect::new(r.x + i as f32 * CAPTION_W, r.y, CAPTION_W, r.h);
            let h = ui.hover_t(anim::id(("caption", i)), b);
            let close = cmd == WindowCmd::Close;
            if h > 0.01 {
                let fill = if close {
                    red.alpha(h)
                } else {
                    t.wash(0.08 * h)
                };
                ui.c.rect(b, fill, 0.0);
            }
            let ink = if close {
                t.dim.lerp(Color::WHITE, h)
            } else {
                t.dim.lerp(t.fg, h)
            };
            let s = 15.0;
            icons::draw(
                ui.c,
                icon,
                Rect::new(b.x + (b.w - s) / 2.0, b.y + (b.h - s) / 2.0, s, s),
                ink,
            );
            if ui.hovered(b) {
                ui.pointer = true;
            }
            if ui.clicked(b) {
                self.window_cmd = Some(cmd);
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
