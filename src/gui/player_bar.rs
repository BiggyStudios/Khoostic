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
        if snap.state == PlaybackState::Playing && !snap.loading {
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
        let tx = art.right() + 18.0;
        match &snap.current {
            Some(e) => {
                let seed = e.track.album.as_deref().unwrap_or(&e.track.title);
                // The ring settles away when paused instead of freezing.
                let playing = snap.state == PlaybackState::Playing;
                let ring = ui.anim.to(
                    anim::id("np-ring"),
                    (self.cover_visualizer && playing) as u8 as f32,
                    anim::NORMAL,
                );
                if ring > 0.01 {
                    if playing {
                        self.ring.update(a, ui.anim.dt());
                    }
                    self.ring.draw(ui, art, ring);
                } else {
                    let beat = ui.anim.to(anim::id("np-bass"), a.bass, 18.0);
                    ui.c.glow(art, t.gradient(0.5).alpha(0.08 + 0.3 * beat), 10.0, 12.0);
                }
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

/// Bars around the cover. Interpolated between `BANDS`, and only up to `TOP_BAND`: the top
/// of the spectrum is nearly always quiet.
const RING_BARS: usize = 72;
const TOP_BAND: f32 = 55.0;
/// Space between the cover and the bars (more on a beat), and the longest bar.
const RING_GAP: f32 = 3.0;
const RING_LEN: f32 = 8.5;

/// The cover ring's motion between frames. Every bar gets its own level against its recent
/// range, so a loud, steady bass doesn't sit pinned at full while the highs barely move: each
/// part of the spectrum lights up when *it* changes. The spectrum runs once around the cover
/// (low to high, clockwise) and drifts slowly round; peaks hang and fall; beats push the ring
/// out and flash the glow.
pub(crate) struct CoverRing {
    avg: [f32; RING_BARS],
    high: [f32; RING_BARS],
    shown: [f32; RING_BARS],
    peak: [f32; RING_BARS],
    hold: [f32; RING_BARS],
    spin: f32,
    pulse: f32,
    beats: u64,
}

impl Default for CoverRing {
    fn default() -> Self {
        Self {
            avg: [0.0; RING_BARS],
            high: [0.0; RING_BARS],
            shown: [0.0; RING_BARS],
            peak: [0.0; RING_BARS],
            hold: [0.0; RING_BARS],
            spin: 0.0,
            pulse: 0.0,
            beats: 0,
        }
    }
}

impl CoverRing {
    fn update(&mut self, a: &AnalysisFrame, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        let ease = |rate: f32| 1.0 - (-rate * dt).exp();
        for i in 0..RING_BARS {
            let band = i as f32 / (RING_BARS - 1) as f32 * TOP_BAND;
            let lo = band.floor() as usize;
            let hi = (lo + 1).min(crate::audio::BANDS - 1);
            let v = (a.bands[lo] + (a.bands[hi] - a.bands[lo]) * band.fract()).clamp(0.0, 1.0);
            // Recent average and a slowly falling ceiling for this bar.
            self.avg[i] += (v - self.avg[i]) * ease(0.7);
            self.high[i] = v
                .max(self.high[i] - 0.3 * dt)
                .max(self.avg[i] + 0.25)
                .max(0.15);
            let floor = self.avg[i] * 0.8;
            let rel = ((v - floor) / (self.high[i] - floor)).clamp(0.0, 1.0);
            // Mostly relative, with some of the real level so loud parts still look bigger.
            let target = 0.75 * rel + 0.25 * v;
            let rate = if target > self.shown[i] { 40.0 } else { 7.0 };
            self.shown[i] += (target - self.shown[i]) * ease(rate);
            if self.shown[i] >= self.peak[i] {
                self.peak[i] = self.shown[i];
                self.hold[i] = 0.3;
            } else if self.hold[i] > 0.0 {
                self.hold[i] -= dt;
            } else {
                self.peak[i] = (self.peak[i] - 1.1 * dt).max(self.shown[i]);
            }
        }
        if a.beat_count != self.beats {
            self.beats = a.beat_count;
            self.pulse = 1.0;
        }
        self.pulse *= 1.0 - ease(5.0);
        self.spin = (self.spin + dt / 40.0).fract();
    }

    /// Draws the ring around `art`; `amount` fades it in and out.
    fn draw(&self, ui: &mut Ui, art: Rect, amount: f32) {
        let t = ui.theme.clone();
        let (cx, cy) = art.center();
        let gap = RING_GAP + 1.5 * self.pulse;
        let half = art.w / 2.0 + gap;
        let radius = 8.0 + gap;
        let perimeter = edge_perimeter(half, radius);
        // A flash in the theme's accent on each beat.
        let g = 4.0 + 3.0 * self.pulse;
        ui.c.glow(
            Rect::new(art.x - g, art.y - g, art.w + 2.0 * g, art.h + 2.0 * g),
            t.accent.alpha((0.08 + 0.32 * self.pulse) * amount),
            12.0,
            14.0,
        );
        for i in 0..RING_BARS {
            let f = i as f32 / (RING_BARS - 1) as f32;
            let s = (i as f32 / RING_BARS as f32 + self.spin) * perimeter;
            let ((x, y), (nx, ny)) = edge_point(half, radius, s);
            let (x0, y0) = (cx + x, cy + y);
            let v = self.shown[i];
            let at = |len: f32| (x0 + nx * len, y0 + ny * len);
            let len = 1.0 + RING_LEN * v * amount;
            // Colored by frequency from the theme's spectrum; loud bars run hot toward the
            // theme's foreground at the tip.
            let base = t.gradient(f);
            ui.c.line(
                at(0.0),
                at(len),
                2.0,
                base.alpha((0.35 + 0.65 * v) * amount),
            );
            if v > 0.45 {
                let tip = (v - 0.45) / 0.55;
                ui.c.line(
                    at(len * 0.6),
                    at(len),
                    2.0,
                    base.lerp(t.fg, 0.5).alpha(tip * amount),
                );
            }
            let p = self.peak[i];
            if p > 0.08 && p > v + 0.04 {
                let (px, py) = at(1.0 + RING_LEN * p * amount + 2.5);
                ui.c.circle(px, py, 1.1, base.lerp(t.fg, 0.4).alpha(0.85 * amount));
            }
        }
    }
}
fn edge_perimeter(half: f32, radius: f32) -> f32 {
    8.0 * (half - radius) + std::f32::consts::TAU * radius
}

/// The point `s` along a rounded square (half-size `half`, corner `radius`, centered on the
/// origin), walking clockwise from the top middle, and the outward normal there.
fn edge_point(half: f32, radius: f32, s: f32) -> ((f32, f32), (f32, f32)) {
    use std::f32::consts::FRAC_PI_2;
    let e = half - radius;
    let arc = FRAC_PI_2 * radius;
    let side = 2.0 * e + arc;
    // Start the walk at the top-left end of the top edge.
    let s = (s + e).rem_euclid(4.0 * side);
    let k = (s / side).floor().min(3.0);
    let s = s - k * side;
    // Work out the top side (edge, then the top-right corner) and rotate it into place.
    let (p, n) = if s < 2.0 * e {
        ((s - e, -half), (0.0, -1.0))
    } else {
        let ang = -FRAC_PI_2 + (s - 2.0 * e) / radius;
        let (sin, cos) = ang.sin_cos();
        ((e + radius * cos, -e + radius * sin), (cos, sin))
    };
    // A quarter turn clockwise on screen: (x, y) -> (-y, x).
    let rot = |(x, y): (f32, f32)| match k as u8 {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    };
    (rot(p), rot(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_shows_changes_not_just_loudness() {
        let mut ring = CoverRing::default();
        let mut a = AnalysisFrame::default();
        // A steady, loud low end settles mid-height instead of sitting pinned at full.
        a.bands[..20].fill(1.0);
        for _ in 0..300 {
            ring.update(&a, 1.0 / 60.0);
        }
        assert!(
            ring.shown[..10].iter().all(|&v| v > 0.35 && v < 0.7),
            "{:?}",
            &ring.shown[..10]
        );
        assert!(ring.shown[60..].iter().all(|&v| v < 0.05));
        // A hit up top lights up the highs and leaves a peak behind as it falls.
        a.bands[40..].fill(0.6);
        for _ in 0..6 {
            ring.update(&a, 1.0 / 60.0);
        }
        assert!(ring.shown[60] > 0.8, "{}", ring.shown[60]);
        a.bands[40..].fill(0.0);
        for _ in 0..12 {
            ring.update(&a, 1.0 / 60.0);
        }
        assert!(ring.peak[60] > ring.shown[60] + 0.2);
    }
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
            loading: false,
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

    #[test]
    fn ring_walks_the_rounded_square() {
        let (half, r) = (33.0, 11.0);
        let total = edge_perimeter(half, r);
        let close =
            |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        // Top middle, right middle, bottom middle, left middle.
        let cases = [
            (0.0, (0.0, -half), (0.0, -1.0)),
            (0.25, (half, 0.0), (1.0, 0.0)),
            (0.5, (0.0, half), (0.0, 1.0)),
            (0.75, (-half, 0.0), (-1.0, 0.0)),
        ];
        for (f, p, n) in cases {
            let (gp, gn) = edge_point(half, r, f * total);
            assert!(close(gp, p) && close(gn, n), "{f}: {gp:?} {gn:?}");
        }
        // Every point is on the shape: at most `half` out along each axis, and continuous.
        let mut prev = edge_point(half, r, 0.0).0;
        for i in 1..=400 {
            let (p, _) = edge_point(half, r, i as f32 / 400.0 * total);
            assert!(p.0.abs() <= half + 1e-3 && p.1.abs() <= half + 1e-3);
            let step = ((p.0 - prev.0).powi(2) + (p.1 - prev.1).powi(2)).sqrt();
            assert!(step <= total / 400.0 + 1e-2, "jump at {i}");
            prev = p;
        }
    }
}
