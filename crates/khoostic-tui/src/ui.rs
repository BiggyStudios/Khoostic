//! Rendering. Minimal by design: no boxes around the main views, dim chrome, one accent color.

use std::time::Duration;

use khoostic_audio::PlaybackState;
use khoostic_core::{QueueEntry, RepeatMode, TrackLocator};
use khoostic_player::PlayerSnapshot;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::action::Action;
use crate::app::{App, ListView, Mode};
use crate::browser::EntryKind;
use crate::command::{COMMANDS, View};
use crate::keymap::KeyMode;
use crate::visualizer;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.buffer_mut().set_style(area, app.theme.base());
    let snap = app.player.snapshot();

    let [header, _, main, now, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(f.buffer_mut(), header, app, &snap);
    let main = pad_x(main, 1);
    match app.view {
        View::Queue => draw_queue(f.buffer_mut(), main, app, &snap),
        View::Browse => draw_browser(f.buffer_mut(), main, app),
        View::Visualizer => {
            let a = app.player.analysis();
            // One physics step per displayed frame, with the real area so particles fill it.
            app.vis.update(&a, main);
            app.vis.render(&a, main, f.buffer_mut(), &app.theme);
            let label = format!(" {} · c to cycle ", app.vis.style.name());
            let x = main.right().saturating_sub(label.width() as u16);
            f.buffer_mut().set_string(x, main.y, label, app.theme.dim());
        }
    }
    draw_now_playing(f.buffer_mut(), pad_x(now, 1), app, &snap);
    draw_status(f, status, app);
    if let Some(scroll) = app.help {
        let clamped = draw_help(f, area, app, scroll);
        app.help = Some(clamped);
    }
}

fn pad_x(r: Rect, n: u16) -> Rect {
    Rect {
        x: r.x + n,
        width: r.width.saturating_sub(n * 2),
        ..r
    }
}

fn draw_header(buf: &mut Buffer, area: Rect, app: &App, snap: &PlayerSnapshot) {
    let t = &app.theme;
    let mut spans = vec![
        Span::styled(" khoostic", t.accent().add_modifier(Modifier::BOLD)),
        Span::raw("   "),
    ];
    for v in View::ALL {
        let style = if v == app.view {
            Style::new()
                .fg(t.fg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            t.dim()
        };
        spans.push(Span::styled(v.title(), style));
        spans.push(Span::raw("  "));
    }
    Line::from(spans).render(area, buf);

    let shuffle = if snap.shuffle { t.accent() } else { t.dim() };
    let (repeat_txt, repeat_on) = match snap.repeat {
        RepeatMode::Off => ("repeat", false),
        RepeatMode::All => ("repeat all", true),
        RepeatMode::One => ("repeat one", true),
    };
    let right = Line::from(vec![
        Span::styled("shuffle", shuffle),
        Span::raw("  "),
        Span::styled(repeat_txt, if repeat_on { t.accent() } else { t.dim() }),
        Span::raw("  "),
        Span::styled(
            format!("vol {:>3}% ", (snap.volume * 100.0).round()),
            t.dim(),
        ),
    ]);
    let w = right.width() as u16;
    right.render(
        Rect {
            x: area.right().saturating_sub(w),
            width: w,
            ..area
        },
        buf,
    );
}

/// Row style: cursor > visual selection > playing > normal.
fn row_style(app: &App, index: usize, view: &ListView, playing: bool) -> Style {
    let t = &app.theme;
    let in_visual = match app.mode {
        Mode::Visual { anchor } => {
            let (a, b) = (anchor.min(view.selected), anchor.max(view.selected));
            (a..=b).contains(&index)
        }
        _ => false,
    };
    if index == view.selected {
        t.selected()
    } else if in_visual {
        Style::new().bg(t.visual_bg).fg(t.fg)
    } else if playing {
        Style::new().fg(t.playing)
    } else {
        Style::new().fg(t.fg)
    }
}

fn draw_queue(buf: &mut Buffer, area: Rect, app: &mut App, snap: &PlayerSnapshot) {
    let queue = app.player.queue().read();
    let len = queue.len();
    if len == 0 {
        drop(queue);
        let lines = vec![
            Line::styled("the queue is empty", app.theme.dim()),
            Line::raw(""),
            Line::from(vec![
                Span::styled("gb", app.theme.accent()),
                Span::styled(" browse files   ", app.theme.dim()),
                Span::styled(":add <path>", app.theme.accent()),
                Span::styled(" add a folder   ", app.theme.dim()),
                Span::styled("?", app.theme.accent()),
                Span::styled(" help", app.theme.dim()),
            ]),
        ];
        let y = area.y + area.height.saturating_sub(3) / 2;
        Paragraph::new(lines).centered().render(
            Rect {
                y,
                height: 3.min(area.height),
                ..area
            },
            buf,
        );
        return;
    }

    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    app.queue_view.height = body.height as usize;
    app.queue_view.scroll_into_view(len);
    let view = app.queue_view.clone();

    let num_w = len.to_string().len().max(2) as u16;
    let cols = queue_columns(area.width, num_w);
    let dim = app.theme.dim();
    let headers = ["", "#", "title", "artist", "album", "time"];
    for (i, (x, w)) in cols.iter().enumerate() {
        let text = if i == 1 || i == 5 {
            rpad(headers[i], *w)
        } else {
            headers[i].to_string()
        };
        buf.set_stringn(head.x + x, head.y, text, *w as usize, dim);
    }

    let playing_id = snap.current.as_ref().map(|e| e.id);
    let rows = queue
        .entries()
        .iter()
        .enumerate()
        .skip(view.offset)
        .take(body.height as usize);
    for (row, (i, entry)) in rows.enumerate() {
        let y = body.y + row as u16;
        let playing = Some(entry.id) == playing_id;
        let style = row_style(app, i, &view, playing);
        buf.set_style(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            style,
        );
        let marker = match (playing, snap.state) {
            (true, PlaybackState::Playing) => "▶",
            (true, PlaybackState::Paused) => "⏸",
            _ => " ",
        };
        let duration = if playing {
            snap.duration
        } else {
            entry.track.duration
        };
        let cells = [
            marker.to_string(),
            rpad(&(i + 1).to_string(), cols[1].1),
            entry.track.title.clone(),
            entry.track.artist_line(),
            album_of(entry),
            rpad(&duration.map(fmt_time).unwrap_or_default(), cols[5].1),
        ];
        for (c, ((x, w), text)) in cols.iter().zip(cells).enumerate() {
            let s = if c == 1 || c >= 3 {
                style.patch(muted(app, i, &view))
            } else {
                style
            };
            buf.set_stringn(body.x + x, y, truncate(&text, *w as usize), *w as usize, s);
        }
    }
}

/// Secondary columns are dimmed except on the cursor row.
fn muted(app: &App, i: usize, view: &ListView) -> Style {
    if i == view.selected {
        Style::new()
    } else {
        Style::new().fg(app.theme.dim)
    }
}

fn album_of(entry: &QueueEntry) -> String {
    if let Some(a) = &entry.track.album {
        return a.clone();
    }
    // Until tags are read, the containing folder is a decent stand-in for the album.
    match &entry.track.locator {
        TrackLocator::Local(p) => p
            .parent()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// (x offset, width) for: marker, number, title, artist, album, time.
fn queue_columns(width: u16, num_w: u16) -> [(u16, u16); 6] {
    let fixed = 2 + num_w + 2 + 7;
    let flex = width.saturating_sub(fixed + 4);
    let (title, artist) = (flex * 45 / 100, flex * 25 / 100);
    let album = flex - title - artist;
    let mut x = 0;
    let mut col = |w: u16, gap: u16| {
        let c = (x, w);
        x += w + gap;
        c
    };
    [
        col(2, 0),
        col(num_w, 2),
        col(title, 2),
        col(artist, 2),
        col(album, 1),
        col(6, 0),
    ]
}

fn draw_browser(buf: &mut Buffer, area: Rect, app: &mut App) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let path = app.browser.dir().display().to_string();
    buf.set_stringn(
        head.x,
        head.y,
        truncate_left(&path, head.width as usize),
        head.width as usize,
        app.theme.dim(),
    );

    let entries = app.browser.entries();
    if entries.is_empty() {
        let msg = app
            .browser
            .error()
            .unwrap_or("no folders or audio files here — h to go up");
        buf.set_stringn(
            body.x,
            body.y + 1,
            msg,
            body.width as usize,
            app.theme.dim(),
        );
        return;
    }
    app.browse_view.height = body.height as usize;
    app.browse_view.scroll_into_view(entries.len());
    let view = app.browse_view.clone();
    for (row, (i, e)) in entries
        .iter()
        .enumerate()
        .skip(view.offset)
        .take(body.height as usize)
        .enumerate()
    {
        let y = body.y + row as u16;
        let style = row_style(app, i, &view, false);
        buf.set_style(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            style,
        );
        let (icon, name) = match e.kind {
            EntryKind::Dir => ("▸ ", format!("{}/", e.name)),
            EntryKind::Audio => ("  ", e.name.clone()),
        };
        let icon_style = if i == view.selected {
            style
        } else {
            app.theme.accent()
        };
        buf.set_string(body.x, y, icon, icon_style);
        let w = body.width.saturating_sub(2) as usize;
        buf.set_stringn(body.x + 2, y, truncate(&name, w), w, style);
    }
}

fn draw_now_playing(buf: &mut Buffer, area: Rect, app: &App, snap: &PlayerSnapshot) {
    let t = &app.theme;
    let [line1, line2, _] = Layout::vertical([Constraint::Length(1); 3]).areas(area);
    let spec_w = (area.width / 4).clamp(8, 32);
    let [info, spec] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(spec_w)]).areas(line1);

    let icon = match snap.state {
        PlaybackState::Playing => "▶",
        PlaybackState::Paused => "⏸",
        PlaybackState::Stopped => "■",
    };
    let mut spans = vec![Span::styled(format!("{icon}  "), t.accent())];
    match &snap.current {
        Some(e) => {
            spans.push(Span::styled(
                e.track.title.clone(),
                Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
            ));
            let artist = e.track.artist_line();
            if !artist.is_empty() {
                spans.push(Span::styled(format!("  {artist}"), t.dim()));
            }
        }
        None => spans.push(Span::styled("nothing playing", t.dim())),
    }
    Line::from(spans).render(info, buf);
    let a = app.player.analysis();
    visualizer::render_mini_spectrum(&a, spec, buf, t);

    // Progress bar: elapsed ━━━━●──── total
    let pos = fmt_time(snap.position);
    let dur = snap
        .duration
        .map(fmt_time)
        .unwrap_or_else(|| "--:--".into());
    let bar_w = line2
        .width
        .saturating_sub((pos.len() + dur.len() + 4) as u16) as usize;
    let frac = match snap.duration {
        Some(d) if !d.is_zero() => (snap.position.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0),
        _ => 0.0,
    };
    let filled = ((bar_w.saturating_sub(1)) as f64 * frac).round() as usize;
    Line::from(vec![
        Span::styled(pos, t.dim()),
        Span::raw("  "),
        Span::styled("━".repeat(filled), t.accent()),
        Span::styled(if snap.current.is_some() { "●" } else { "─" }, t.accent()),
        Span::styled("─".repeat(bar_w.saturating_sub(filled + 1)), t.dim()),
        Span::raw("  "),
        Span::styled(dur, t.dim()),
    ])
    .render(line2, buf);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let buf = f.buffer_mut();
    let (prompt, input) = match &app.mode {
        Mode::Command(i) => (":", Some(i)),
        Mode::Search(i) => ("/", Some(i)),
        _ => ("", None),
    };
    if let Some(input) = input {
        let line = format!("{prompt}{}", input.text());
        buf.set_stringn(
            area.x,
            area.y,
            &line,
            area.width as usize,
            Style::new().fg(t.fg),
        );
        let before: String = input.text().chars().take(input.cursor()).collect();
        let cx = area.x + (prompt.width() + before.width()) as u16;
        f.set_cursor_position((cx.min(area.right().saturating_sub(1)), area.y));
        return;
    }

    let (badge, badge_style) = match app.mode {
        Mode::Visual { .. } => (" VISUAL ", Style::new().fg(t.selection_fg).bg(t.playing)),
        _ => (" NORMAL ", Style::new().fg(t.selection_fg).bg(t.accent)),
    };
    buf.set_string(
        area.x,
        area.y,
        badge,
        badge_style.add_modifier(Modifier::BOLD),
    );
    let x = area.x + badge.len() as u16 + 1;

    // Right side: pending keys (showcmd) and position in list.
    let len = app.list_len();
    let pos = if app.view == View::Visualizer || len == 0 {
        String::new()
    } else {
        format!("{}/{len}", app.list().selected + 1)
    };
    let right = format!("{}   {pos} ", app.keymap.pending_display());
    let rx = area.right().saturating_sub(right.width() as u16);
    buf.set_string(rx, area.y, &right, t.dim());

    if let Some(m) = &app.message {
        let style = if m.error {
            Style::new().fg(t.error)
        } else {
            Style::new().fg(t.fg)
        };
        let w = rx.saturating_sub(x + 1) as usize;
        buf.set_stringn(x, area.y, truncate(&m.text, w), w, style);
    }
}

/// Returns the (clamped) scroll offset.
fn draw_help(f: &mut Frame, area: Rect, app: &App, scroll: usize) -> usize {
    let t = &app.theme;
    let w = area.width.saturating_sub(8).min(90);
    let h = area.height.saturating_sub(4);
    let popup = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + 2,
        width: w,
        height: h,
    };

    let mut lines: Vec<Line> = Vec::new();
    let section =
        |name: &str| Line::styled(name.to_string(), t.accent().add_modifier(Modifier::BOLD));
    for (mode, title) in [
        (KeyMode::Normal, "normal mode"),
        (KeyMode::Visual, "visual mode (plus everything in normal)"),
    ] {
        lines.push(section(title));
        // Group keys per action: "j <Down>  cursor down"
        let mut grouped: Vec<(Action, Vec<String>)> = Vec::new();
        for (keys, action) in app.keymap.bindings(mode) {
            match grouped.iter_mut().find(|(a, _)| *a == action) {
                Some((_, ks)) => ks.push(keys),
                None => grouped.push((action, vec![keys])),
            }
        }
        for (action, keys) in grouped {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<22}", keys.join(" ")), Style::new().fg(t.fg)),
                Span::styled(action.help(), t.dim()),
            ]));
        }
        lines.push(Line::raw(""));
    }
    lines.push(section("commands"));
    for (cmd, help) in COMMANDS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {cmd:<28}"), Style::new().fg(t.fg)),
            Span::styled(*help, t.dim()),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  counts work like vim: 5j, 3dd, 10G, 2>",
        t.dim(),
    ));

    let inner_h = h.saturating_sub(2) as usize;
    let scroll = scroll.min(lines.len().saturating_sub(inner_h));
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.dim())
        .title(Span::styled(" help ", t.accent()))
        .title_bottom(Span::styled(" j/k scroll · any other key closes ", t.dim()));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll as u16, 0))
            .style(t.base()),
        popup,
    );
    scroll
}

pub fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn rpad(s: &str, w: u16) -> String {
    format!("{s:>width$}", width = w as usize)
}

/// Truncates to a display width, ending with `…`.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Truncates from the left (for paths: the end is the interesting part).
fn truncate_left(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut w = 1;
    let mut start = chars.len();
    while start > 0 {
        let cw = unicode_width::UnicodeWidthChar::width(chars[start - 1]).unwrap_or(0);
        if w + cw > max {
            break;
        }
        w += cw;
        start -= 1;
    }
    format!("…{}", chars[start..].iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_display_width() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("日本語の曲", 5), "日本…");
        assert_eq!(truncate_left("C:/Music/Album", 8), "…c/Album");
    }

    #[test]
    fn columns_fit() {
        for w in [40u16, 80, 200] {
            let c = queue_columns(w, 3);
            let (x, cw) = c[5];
            assert!(x + cw <= w, "width {w}: {c:?}");
        }
    }
}
