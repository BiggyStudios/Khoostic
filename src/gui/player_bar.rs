//! The player bar: now playing, transport, seek bar (with [`PlayClock`] smoothing), volume,
//! radio and autoplay.

use std::time::{Duration, Instant};

use crate::audio::{AnalysisFrame, PlaybackState};
use crate::gui::app::{Drag, Gui, View, fmt_time};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::Ui;
use crate::gui::ui::anim;
use crate::gui::ui::icons::{self, Icon};
use crate::library::CoverSize;
use crate::model::{EntryId, RepeatMode};
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

/// The playback position as shown: continuous between the engine's (buffer-sized) updates,
/// and held at a seek target until the engine gets there, so the bar never snaps back.
#[derive(Default)]
pub(crate) struct PlayClock {
    entry: Option<EntryId>,
    pos: f64,
    hold: Option<(f64, Instant)>,
    /// Drawn fill fraction of the seek bar; glides across jumps.
    pub bar: f32,
}

impl PlayClock {
    pub(crate) fn update(&mut self, snap: &PlayerSnapshot, dt: f32) {
        let id = snap.current.as_ref().map(|e| e.id);
        let actual = snap.position.as_secs_f64();
        if id != self.entry {
            self.entry = id;
            self.pos = actual;
            self.hold = None;
            return;
        }
        if let Some((target, at)) = self.hold {
            if (actual - target).abs() < 0.75 || at.elapsed() > Duration::from_secs(2) {
                self.hold = None;
            } else {
                self.pos = target;
                return;
            }
        }
        if snap.state == PlaybackState::Playing {
            self.pos += dt as f64;
            let err = actual - self.pos;
            if err.abs() > 0.4 {
                self.pos = actual; // a seek or a stall: jump (the bar eases separately)
            } else {
                self.pos += err * (dt as f64 * 6.0).min(1.0);
            }
        } else {
            self.pos = actual;
        }
        if let Some(d) = snap.duration {
            self.pos = self.pos.clamp(0.0, d.as_secs_f64());
        }
    }

    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(self.pos.max(0.0))
    }

    fn seeked(&mut self, to: Duration) {
        self.pos = to.as_secs_f64();
        self.hold = Some((self.pos, Instant::now()));
    }
}

impl Gui {
    pub(crate) fn now_playing(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();

        // Left: cover + title/artist (click to open the album).
        let art = Rect::new(r.x + 18.0, r.y + (r.h - 60.0) / 2.0, 60.0, 60.0);
        let info_w = (r.w * 0.26).min(300.0);
        let tx = art.right() + 14.0;
        match &snap.current {
            Some(e) => {
                let seed = e.track.album.as_deref().unwrap_or(&e.track.title);
                let beat = ui.anim.to(anim::id("np-bass"), a.bass, 18.0);
                ui.c.glow(art, t.gradient(0.5).alpha(0.08 + 0.3 * beat), 10.0, 12.0);
                ui.cover(art, e.track.cover.as_ref(), CoverSize::Large, 8.0, seed);
                // Title → album, artist → artist, like Spotify (underlined on hover).
                let text_w = info_w - 34.0;
                let title_w = ui
                    .measure(&e.track.title, t.font_size + 0.5, Weight::Medium)
                    .min(text_w);
                let title_r = Rect::new(tx, r.y + 26.0, title_w, 22.0);
                let th = ui.hovered(title_r) || ui.hovered(art);
                ui.label(
                    &e.track.title,
                    tx,
                    title_r,
                    text_w,
                    t.font_size + 0.5,
                    Weight::Medium,
                    t.fg,
                );
                if th {
                    ui.c.rect(
                        Rect::new(tx, title_r.bottom() - 2.0, title_w, 1.0),
                        t.fg.alpha(0.8),
                        0.0,
                    );
                }
                let artist = e.track.artists.first().cloned().unwrap_or_default();
                let sub = match e.track.artist_line() {
                    s if s.is_empty() => crate::gui::views::lists::album_of(&e.track),
                    s => s,
                };
                let sub_w = ui
                    .measure(&sub, t.font_size - 1.5, Weight::Regular)
                    .min(text_w);
                let sub_r = Rect::new(tx, r.y + 49.0, sub_w, 20.0);
                let ah = !artist.is_empty() && ui.hovered(sub_r);
                ui.label(
                    &sub,
                    tx,
                    sub_r,
                    text_w,
                    t.font_size - 1.5,
                    Weight::Regular,
                    if ah { t.fg } else { t.dim },
                );
                if ah {
                    ui.c.rect(
                        Rect::new(tx, sub_r.bottom() - 2.0, sub_w, 1.0),
                        t.fg.alpha(0.7),
                        0.0,
                    );
                }
                ui.pointer |= th || ah;
                let track = (*e.track).clone();
                if th && ui.input.pressed {
                    self.open_album_of(&track);
                } else if ah && ui.input.pressed {
                    let id = track.artist_id(0).map(str::to_owned);
                    self.open_artist(&artist, id.as_deref());
                }
                // Save to / remove from Liked Songs.
                let liked = self.is_liked(&track);
                let lx = tx + title_w.max(sub_w) + 22.0;
                let lid = anim::id("np-like");
                let lo = ui.anim.to(lid ^ 9, liked as u8 as f32, anim::NORMAL);
                let icon = if liked {
                    Icon::HeartFilled
                } else {
                    Icon::Heart
                };
                // The theme's accent, like everything else that is "on".
                let liked_color = t.accent;
                if ui.icon_button(
                    lid,
                    icon,
                    (lx, r.y + 47.0),
                    17.0,
                    t.dim.lerp(liked_color, lo),
                    t.fg.lerp(liked_color, lo),
                ) {
                    self.set_liked(&track, !liked);
                }
            }
            None => {
                ui.c.rect(art, t.wash(0.05), 8.0);
                icons::draw(
                    ui.c,
                    Icon::Song,
                    Rect::new(art.x + 19.0, art.y + 19.0, 22.0, 22.0),
                    t.dim.alpha(0.6),
                );
                ui.label(
                    "Nothing playing",
                    tx,
                    Rect::new(tx, r.y, info_w, r.h),
                    info_w,
                    t.font_size,
                    Weight::Medium,
                    t.dim,
                );
            }
        }

        // Center: transport.
        let cx = r.x + r.w / 2.0;
        let cy = r.y + 32.0;
        let playing = snap.state == PlaybackState::Playing;
        let play = Rect::new(cx - 20.0, cy - 20.0, 40.0, 40.0);
        let pid = anim::id("np-play");
        let h = ui.hover_t(pid, play);
        let p = ui.press_t(pid, play);
        let rad = 19.0 * (1.0 + 0.06 * h - 0.08 * p);
        ui.c.circle(cx, cy, rad, t.fg);
        let morph = ui.anim.to(pid ^ 5, playing as u8 as f32, 20.0);
        let s = 18.0 * (1.0 - 0.08 * p);
        let icon_r = Rect::new(cx - s / 2.0, cy - s / 2.0, s, s);
        let ink = t.background;
        if morph < 0.999 {
            ui.c.push_fx(0.0, 0.0, 1.0 - morph);
            icons::draw(ui.c, Icon::Play, icon_r, ink);
            ui.c.pop_fx();
        }
        if morph > 0.001 {
            ui.c.push_fx(0.0, 0.0, morph);
            icons::draw(ui.c, Icon::Pause, icon_r, ink);
            ui.c.pop_fx();
        }
        if ui.clicked(play) {
            player.send(Cmd::TogglePause);
        }
        let (dim, fg, accent) = (t.dim, t.fg, t.accent);
        if ui.icon_button(
            anim::id("np-prev"),
            Icon::Prev,
            (cx - 50.0, cy),
            20.0,
            fg.alpha(0.85),
            fg,
        ) {
            player.send(Cmd::Prev);
        }
        if ui.icon_button(
            anim::id("np-next"),
            Icon::Next,
            (cx + 50.0, cy),
            20.0,
            fg.alpha(0.85),
            fg,
        ) {
            player.send(Cmd::Next);
        }
        let toggles = [
            (Icon::Shuffle, snap.shuffle, -96.0, Cmd::ToggleShuffle),
            (
                if snap.repeat == RepeatMode::One {
                    Icon::RepeatOne
                } else {
                    Icon::Repeat
                },
                snap.repeat != RepeatMode::Off,
                96.0,
                Cmd::CycleRepeat,
            ),
        ];
        for (icon, on, dx, cmd) in toggles {
            let id = anim::id(("np-toggle", dx as i32));
            let o = ui.anim.to(id ^ 9, on as u8 as f32, anim::NORMAL);
            let base = dim.lerp(accent, o);
            if ui.icon_button(id, icon, (cx + dx, cy), 18.0, base, fg.lerp(accent, o)) {
                player.send(cmd);
            }
            if o > 0.01 {
                ui.c.circle(cx + dx, cy + 15.0, 2.0 * o, accent.alpha(o));
            }
        }

        // Seek bar.
        let bar_w = (r.w - 2.0 * (info_w + 140.0)).clamp(240.0, 560.0);
        let seek = Rect::new(cx - bar_w / 2.0, r.y + 62.0, bar_w, 18.0);
        let dur = snap.duration.unwrap_or_default();
        let frac_at = |x: f32| ((x - seek.x) / seek.w).clamp(0.0, 1.0);
        if ui.clicked(seek) && !dur.is_zero() {
            self.drag = Some(Drag::Seek(frac_at(ui.input.mouse.unwrap_or_default().0)));
        }
        if let (Some(Drag::Seek(_)), Some((mx, _))) = (self.drag, ui.input.mouse) {
            self.drag = Some(Drag::Seek(frac_at(mx)));
            if ui.input.released {
                let to = dur.mul_f32(frac_at(mx));
                player.send(Cmd::Seek(to));
                self.clock.seeked(to);
            }
        }
        let (frac, shown) = match self.drag {
            Some(Drag::Seek(f)) => (f, dur.mul_f32(f)),
            _ if dur.is_zero() => (0.0, Duration::ZERO),
            _ => {
                let pos = self.clock.position().min(dur);
                (pos.as_secs_f32() / dur.as_secs_f32(), pos)
            }
        };
        // Follow small, continuous motion exactly; glide across jumps (seeks, track changes).
        let dragging = matches!(self.drag, Some(Drag::Seek(_)));
        let jump = (frac - self.clock.bar).abs() * seek.w;
        if dragging || jump < 2.0 {
            self.clock.bar = frac;
        } else {
            self.clock.bar += (frac - self.clock.bar) * (1.0 - (-18.0 * ui.anim.dt()).exp());
            ui.anim.keep_alive();
        }
        let sid = anim::id("np-seek");
        let hot = ui.hovered(seek) || dragging;
        ui.pointer |= hot;
        let hs = ui.anim.to(sid, hot as u8 as f32, anim::FAST);
        draw_slider(ui, seek, self.clock.bar, hs, t.fg.lerp(t.accent, hs));
        let times = Rect::new(seek.x - 54.0, seek.y, 44.0, seek.h);
        ui.label_right(
            &fmt_time(shown),
            times.right(),
            times,
            11.5,
            Weight::Regular,
            t.dim,
        );
        let end = if dur.is_zero() {
            "–:––".to_string()
        } else {
            fmt_time(dur)
        };
        ui.label(
            &end,
            seek.right() + 10.0,
            seek,
            50.0,
            11.5,
            Weight::Regular,
            t.dim,
        );

        // Right: queue, volume.
        let vol = Rect::new(r.right() - 132.0, r.y + (r.h - 18.0) / 2.0, 104.0, 18.0);
        let vid = anim::id("np-vol");
        if ui.clicked(vol) {
            self.drag = Some(Drag::Volume(frac_vol(vol, ui.input.mouse)));
        }
        if let Some(Drag::Volume(_)) = self.drag {
            let v = frac_vol(vol, ui.input.mouse);
            self.drag = Some(Drag::Volume(v));
            player.send(Cmd::SetVolume(v));
        }
        if ui.hovered(vol) && ui.input.scroll != 0.0 {
            player.send(Cmd::VolumeBy(-ui.input.scroll.signum() * 0.05));
        }
        let volume = match self.drag {
            Some(Drag::Volume(v)) => v,
            _ => snap.volume,
        };
        let shown_vol = ui.anim.to(vid ^ 4, volume, 24.0);
        let hot = ui.hovered(vol) || matches!(self.drag, Some(Drag::Volume(_)));
        ui.pointer |= hot;
        let hv = ui.anim.to(vid, hot as u8 as f32, anim::FAST);
        draw_slider(ui, vol, shown_vol, hv, t.fg.lerp(t.accent, hv));
        if ui.icon_button(
            anim::id("np-mute"),
            Icon::Volume(volume),
            (vol.x - 22.0, vol.y + vol.h / 2.0),
            18.0,
            dim,
            fg,
        ) {
            if volume > 0.001 {
                self.unmute_to = volume;
                player.send(Cmd::SetVolume(0.0));
            } else {
                player.send(Cmd::SetVolume(self.unmute_to.max(0.05)));
            }
        }
        // Buttons left of the volume, most used nearest it. On narrow windows the ones that
        // would run into the transport are left out.
        let ay = vol.y + vol.h / 2.0;
        let min_x = cx + 128.0;
        let mut bx = vol.x - 64.0;
        for button in [
            Button::Queue,
            Button::Lyrics,
            Button::Autoplay,
            Button::Radio,
            Button::Visualizer,
        ] {
            if bx < min_x {
                break;
            }
            let (icon, on, id) = match button {
                Button::Queue => (Icon::Queue, self.queue_open, "np-queue"),
                Button::Lyrics => (Icon::Mic, self.view == View::Lyrics, "np-lyrics"),
                Button::Autoplay => (Icon::Autoplay, self.discovery.autoplay(), "np-autoplay"),
                Button::Radio => (Icon::Radio, self.radio.is_some(), "np-radio"),
                Button::Visualizer => (Icon::Visualizer, self.view == View::Visualizer, "np-vis"),
            };
            let id = anim::id(id);
            let o = ui.anim.to(id ^ 9, on as u8 as f32, anim::NORMAL);
            if ui.icon_button(
                id,
                icon,
                (bx, ay),
                18.0,
                dim.lerp(accent, o),
                fg.lerp(accent, o),
            ) {
                match button {
                    Button::Queue => self.queue_open = !self.queue_open,
                    Button::Lyrics => self.toggle_lyrics(),
                    Button::Autoplay => {
                        let auto = !on;
                        self.discovery.set_autoplay(auto);
                        self.save_bool("playback.autoplay", auto);
                        self.toast(if auto {
                            "Autoplay on: similar songs keep playing when the queue ends"
                        } else {
                            "Autoplay off"
                        });
                    }
                    Button::Radio if !on => match &snap.current {
                        Some(e) => self.start_radio((*e.track).clone()),
                        None => {
                            self.toast("Play something first, or right-click a song 'Start radio'")
                        }
                    },
                    Button::Radio => {}
                    Button::Visualizer => {
                        if on {
                            self.back();
                        } else {
                            self.navigate(View::Visualizer);
                        }
                    }
                }
            }
            if o > 0.01
                && matches!(
                    button,
                    Button::Autoplay | Button::Lyrics | Button::Visualizer
                )
            {
                ui.c.circle(bx, ay + 15.0, 2.0 * o, accent.alpha(o));
            }
            bx -= 42.0;
        }
    }
}

/// Player bar buttons beside the volume, in order of priority.
#[derive(Clone, Copy)]
enum Button {
    Queue,
    Lyrics,
    Autoplay,
    Radio,
    Visualizer,
}

fn frac_vol(vol: Rect, mouse: Option<(f32, f32)>) -> f32 {
    ((mouse.unwrap_or_default().0 - vol.x) / vol.w).clamp(0.0, 1.0)
}

/// A thin track with a fill and a knob that grows in on hover (`hot` 0..1).
pub(crate) fn draw_slider(ui: &mut Ui, r: Rect, frac: f32, hot: f32, fill: Color) {
    let t = ui.theme;
    let th = 4.0 + 2.0 * hot;
    let track = Rect::new(r.x, r.y + (r.h - th) / 2.0, r.w, th);
    ui.c.rect(track, t.wash(0.14), th / 2.0);
    let fw = (track.w * frac.clamp(0.0, 1.0)).max(th);
    ui.c.rect(Rect { w: fw, ..track }, fill, th / 2.0);
    if hot > 0.01 {
        let x = track.x + track.w * frac.clamp(0.0, 1.0);
        let cy = track.y + th / 2.0;
        ui.c.glow(
            Rect::new(x - 6.0 * hot, cy - 6.0 * hot, 12.0 * hot, 12.0 * hot),
            Color::BLACK.alpha(0.4 * hot),
            6.0,
            4.0,
        );
        ui.c.circle(x, cy, 6.5 * hot, t.fg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{QueueEntry, Track};
    use std::sync::Arc;

    fn snap(pos: f64, state: PlaybackState, id: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            state,
            current: Some(QueueEntry {
                id: EntryId(id),
                track: Arc::new(Track::default()),
            }),
            position: Duration::from_secs_f64(pos),
            duration: Some(Duration::from_secs(200)),
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            buffered: Duration::ZERO,
            sample_rate: 48_000,
            device: String::new(),
        }
    }

    #[test]
    fn clock_is_smooth_between_coarse_updates() {
        let mut c = PlayClock::default();
        c.update(&snap(10.0, PlaybackState::Playing, 1), 0.0);
        // The engine reports in 20 ms steps; we render at 120 fps.
        let mut last = c.pos;
        for i in 1..=60 {
            let engine = 10.0 + ((i as f64 / 120.0) / 0.02).floor() * 0.02;
            c.update(&snap(engine, PlaybackState::Playing, 1), 1.0 / 120.0);
            assert!(c.pos > last, "position must keep moving forward");
            assert!((c.pos - engine).abs() < 0.05);
            last = c.pos;
        }
    }

    #[test]
    fn seek_target_is_held_until_the_engine_arrives() {
        let mut c = PlayClock::default();
        c.update(&snap(10.0, PlaybackState::Playing, 1), 0.0);
        c.seeked(Duration::from_secs(120));
        // The engine still reports the old position for a few frames.
        c.update(&snap(10.02, PlaybackState::Playing, 1), 0.016);
        assert_eq!(c.position(), Duration::from_secs(120));
        c.update(&snap(120.01, PlaybackState::Playing, 1), 0.016);
        assert!(c.hold.is_none());
        assert!((c.pos - 120.0).abs() < 0.1);
    }

    #[test]
    fn new_track_resets_the_clock() {
        let mut c = PlayClock::default();
        c.update(&snap(150.0, PlaybackState::Playing, 1), 0.0);
        c.update(&snap(0.0, PlaybackState::Playing, 2), 0.016);
        assert!(c.pos < 0.01);
    }
}
