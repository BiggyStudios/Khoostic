//! Rendering. Minimal by design: no boxes around the main views, dim chrome, one accent color.

use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::audio::PlaybackState;
use crate::model::{QueueEntry, RepeatMode, TrackLocator};
use crate::player::PlayerSnapshot;
use crate::player::browse::EntryKind;
use crate::tui::action::Action;
use crate::tui::app::{App, ListView, Mode};
use crate::tui::command::{COMMANDS, View};
use crate::tui::keymap::KeyMode;
use crate::tui::visualizer;

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
        View::Albums => draw_albums(f.buffer_mut(), main, app, &snap),
        View::Songs => draw_songs(f.buffer_mut(), main, app, &snap),
        View::Playlists => draw_playlists(f.buffer_mut(), main, app, &snap),
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
    let sp = app.pl.status_line();
    let right = Line::from(vec![
        Span::styled(
            if sp.is_empty() {
                String::new()
            } else {
                format!("{sp}   ")
            },
            t.dim(),
        ),
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

fn draw_now_playing(buf: &mut Buffer, area: Rect, app: &mut App, snap: &PlayerSnapshot) {
    // Cover art as 6x3 half-block cells (6x6 pixels) when the terminal is wide enough.
    let area = if area.width > 60 {
        let cover = snap.current.as_ref().and_then(|e| e.track.cover.clone());
        let cell_area = Rect {
            width: 6,
            height: 3,
            ..area
        };
        if let Some(cells) = app.lib.cover_cells(&app.library, cover.as_ref(), 6, 3) {
            paint_cells(buf, cell_area, cells);
        } else {
            buf.set_style(cell_area, Style::new().bg(app.theme.visual_bg));
        }
        Rect {
            x: area.x + 8,
            width: area.width - 8,
            ..area
        }
    } else {
        area
    };
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

/// Paints half-block cover cells (`▀`: fg = top pixel, bg = bottom pixel).
fn paint_cells(
    buf: &mut Buffer,
    area: Rect,
    cells: &[(ratatui::style::Color, ratatui::style::Color)],
) {
    for (i, (top, bottom)) in cells.iter().enumerate() {
        let (x, y) = (i as u16 % area.width, i as u16 / area.width);
        if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
            cell.set_symbol("▀").set_fg(*top).set_bg(*bottom);
        }
    }
}

/// Row style without borrowing the whole app (so track lists can borrow library data).
fn list_row_style(
    theme: &crate::tui::theme::Theme,
    mode: &Mode,
    index: usize,
    view: &ListView,
    playing: bool,
) -> Style {
    let in_visual = match mode {
        Mode::Visual { anchor } => {
            let (a, b) = ((*anchor).min(view.selected), (*anchor).max(view.selected));
            (a..=b).contains(&index)
        }
        _ => false,
    };
    if index == view.selected {
        theme.selected()
    } else if in_visual {
        Style::new().bg(theme.visual_bg).fg(theme.fg)
    } else if playing {
        Style::new().fg(theme.playing)
    } else {
        Style::new().fg(theme.fg)
    }
}

/// A track table: number, title, artist, (album), time. `numbers` uses tag track numbers.
#[allow(clippy::too_many_arguments)]
fn draw_tracks(
    buf: &mut Buffer,
    area: Rect,
    theme: &crate::tui::theme::Theme,
    mode: &Mode,
    tracks: &[crate::model::Track],
    view: &ListView,
    playing: Option<&crate::model::TrackLocator>,
    numbers: bool,
    album_col: bool,
) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let num_w = 4u16;
    let flex = area.width.saturating_sub(num_w + 2 + 8 + 6);
    let (title_w, artist_w) = if album_col {
        (flex * 45 / 100, flex * 25 / 100)
    } else {
        (flex * 60 / 100, flex * 40 / 100)
    };
    let album_w = if album_col {
        flex - title_w - artist_w
    } else {
        0
    };
    let xs = [
        0,
        num_w + 2,
        num_w + 2 + title_w + 2,
        num_w + 2 + title_w + artist_w + 4,
    ];
    let dim = theme.dim();
    buf.set_string(head.x, head.y, rpad("#", num_w), dim);
    buf.set_string(head.x + xs[1], head.y, "title", dim);
    buf.set_string(head.x + xs[2], head.y, "artist", dim);
    if album_col {
        buf.set_string(head.x + xs[3], head.y, "album", dim);
    }
    buf.set_string(head.right().saturating_sub(4), head.y, "time", dim);

    let rows = tracks
        .iter()
        .enumerate()
        .skip(view.offset)
        .take(body.height as usize);
    for (row, (i, t)) in rows.enumerate() {
        let y = body.y + row as u16;
        let is_playing = playing == Some(&t.locator);
        let style = list_row_style(theme, mode, i, view, is_playing);
        buf.set_style(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            style,
        );
        let sub = if i == view.selected {
            style
        } else {
            style.patch(Style::new().fg(theme.dim))
        };
        let num = if numbers {
            t.track_number.map(|n| n.to_string()).unwrap_or_default()
        } else {
            (i + 1).to_string()
        };
        let marker = if is_playing { "▶".to_string() } else { num };
        buf.set_string(body.x, y, rpad(&marker, num_w), sub);
        buf.set_stringn(
            body.x + xs[1],
            y,
            truncate(&t.title, title_w as usize),
            title_w as usize,
            style,
        );
        let artist = truncate(&t.artist_line(), artist_w as usize);
        buf.set_stringn(body.x + xs[2], y, artist, artist_w as usize, sub);
        if album_col {
            let album = truncate(t.album.as_deref().unwrap_or(""), album_w as usize);
            buf.set_stringn(body.x + xs[3], y, album, album_w as usize, sub);
        }
        if let Some(d) = t.duration {
            let s = fmt_time(d);
            buf.set_string(body.right().saturating_sub(s.len() as u16), y, s, sub);
        }
    }
}

fn draw_albums(buf: &mut Buffer, area: Rect, app: &mut App, snap: &PlayerSnapshot) {
    let playing = snap.current.as_ref().map(|e| e.track.locator.clone());
    if let Some((al, _)) = app.lib.open.clone() {
        // Album page: half-block cover + details, then the tracks.
        let cover_w = 16u16.min(area.width / 3);
        let header_h = cover_w / 2 + 1;
        let cells_area = Rect {
            width: cover_w,
            height: cover_w / 2,
            ..area
        };
        if let Some(cells) =
            app.lib
                .cover_cells(&app.library, al.cover.as_ref(), cover_w, cover_w / 2)
        {
            paint_cells(buf, cells_area, cells);
        } else {
            buf.set_style(cells_area, Style::new().bg(app.theme.visual_bg));
        }
        let x = area.x + cover_w + 2;
        let w = area.width.saturating_sub(cover_w + 2) as usize;
        let t = &app.theme;
        buf.set_stringn(x, area.y + 1, "ALBUM", w, t.dim());
        let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
        buf.set_stringn(x, area.y + 2, truncate(&al.name, w), w, bold);
        let mut meta = Vec::new();
        if let Some(a) = &al.artist {
            meta.push(a.clone());
        }
        if let Some(y) = al.year {
            meta.push(y.to_string());
        }
        meta.push(format!("{} songs", al.tracks));
        buf.set_stringn(x, area.y + 3, truncate(&meta.join(" · "), w), w, t.dim());
        buf.set_stringn(
            x,
            area.y + 5,
            "<CR> play · a add · A play next · h back",
            w,
            t.dim(),
        );

        let list = Rect {
            y: area.y + header_h + 1,
            height: area.height.saturating_sub(header_h + 1),
            ..area
        };
        let len = app.lib.open.as_ref().map_or(0, |(_, t)| t.len());
        app.lib.album_view.height = list.height.saturating_sub(1) as usize;
        app.lib.album_view.scroll_into_view(len);
        let tracks = &app.lib.open.as_ref().expect("open album").1;
        let view = &app.lib.album_view;
        draw_tracks(
            buf,
            list,
            &app.theme,
            &app.mode,
            tracks,
            view,
            playing.as_ref(),
            true,
            false,
        );
        return;
    }

    let len = app.lib.albums.len();
    if len == 0 {
        let msg = if app.library.is_scanning() {
            "scanning your music…"
        } else {
            "no albums yet: set [library] music_dirs, or browse files (gb)"
        };
        buf.set_stringn(
            area.x,
            area.y + 1,
            msg,
            area.width as usize,
            app.theme.dim(),
        );
        return;
    }
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    app.lib.albums_view.height = body.height as usize;
    app.lib.albums_view.scroll_into_view(len);
    let t = &app.theme;
    let w = area.width as usize;
    let (name_w, artist_w) = (w * 45 / 100, w * 35 / 100);
    buf.set_string(head.x + 2, head.y, "album", t.dim());
    buf.set_string(head.x + 2 + name_w as u16, head.y, "artist", t.dim());
    buf.set_string(
        head.right().saturating_sub(11),
        head.y,
        "year  songs",
        t.dim(),
    );
    let view = &app.lib.albums_view;
    let playing_album = snap.current.as_ref().and_then(|e| e.track.album.clone());
    let rows = app
        .lib
        .albums
        .iter()
        .enumerate()
        .skip(view.offset)
        .take(body.height as usize);
    for (row, (i, al)) in rows.enumerate() {
        let y = body.y + row as u16;
        let is_playing = playing_album.as_deref() == Some(al.name.as_str());
        let style = list_row_style(t, &app.mode, i, view, is_playing);
        buf.set_style(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            style,
        );
        let sub = if i == view.selected {
            style
        } else {
            style.patch(Style::new().fg(t.dim))
        };
        buf.set_string(body.x, y, if is_playing { "▶ " } else { "  " }, style);
        buf.set_stringn(
            body.x + 2,
            y,
            truncate(&al.name, name_w - 2),
            name_w - 2,
            style,
        );
        let artist = truncate(al.artist.as_deref().unwrap_or(""), artist_w - 2);
        buf.set_stringn(body.x + 2 + name_w as u16, y, artist, artist_w - 2, sub);
        let year = al
            .year
            .map(|y| y.to_string())
            .unwrap_or_else(|| "    ".into());
        let right = format!("{year}  {:>5}", al.tracks);
        buf.set_string(
            body.right().saturating_sub(right.width() as u16),
            y,
            right,
            sub,
        );
    }
}

fn draw_songs(buf: &mut Buffer, area: Rect, app: &mut App, snap: &PlayerSnapshot) {
    let len = app.lib.songs.len();
    if len == 0 {
        let msg = if app.library.is_scanning() {
            "scanning your music…"
        } else {
            "no songs yet"
        };
        buf.set_stringn(
            area.x,
            area.y + 1,
            msg,
            area.width as usize,
            app.theme.dim(),
        );
        return;
    }
    app.lib.songs_view.height = area.height.saturating_sub(1) as usize;
    app.lib.songs_view.scroll_into_view(len);
    let playing = snap.current.as_ref().map(|e| e.track.locator.clone());
    let view = &app.lib.songs_view;
    draw_tracks(
        buf,
        area,
        &app.theme,
        &app.mode,
        &app.lib.songs,
        view,
        playing.as_ref(),
        false,
        true,
    );
}

fn draw_playlists(buf: &mut Buffer, area: Rect, app: &mut App, snap: &PlayerSnapshot) {
    let playing = snap.current.as_ref().map(|e| e.track.locator.clone());
    if let Some((title, _)) = app.pl.open.clone() {
        let t = &app.theme;
        let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
        buf.set_stringn(
            area.x,
            area.y,
            truncate(&title, area.width as usize),
            area.width as usize,
            bold,
        );
        let hint = "<CR> play from here · a add · :addto <playlist> · h back";
        buf.set_stringn(area.x, area.y + 1, hint, area.width as usize, t.dim());
        let list = Rect {
            y: area.y + 3,
            height: area.height.saturating_sub(3),
            ..area
        };
        let len = app.pl.open.as_ref().map_or(0, |(_, t)| t.len());
        app.pl.tracks_view.height = list.height.saturating_sub(1) as usize;
        app.pl.tracks_view.scroll_into_view(len);
        let tracks = &app.pl.open.as_ref().expect("open playlist").1;
        let view = &app.pl.tracks_view;
        draw_tracks(
            buf,
            list,
            &app.theme,
            &app.mode,
            tracks,
            view,
            playing.as_ref(),
            false,
            true,
        );
        return;
    }

    let entries = app.pl.entries();
    if entries.is_empty() {
        let msg = if app.pl.spotify.is_some() && !app.pl.connected() {
            "no playlists yet: :newpl <name> creates one, :spotify login connects Spotify"
        } else {
            "no playlists yet: :newpl <name> creates one"
        };
        buf.set_stringn(
            area.x,
            area.y + 1,
            msg,
            area.width as usize,
            app.theme.dim(),
        );
        return;
    }
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    app.pl.root_view.height = body.height as usize;
    app.pl.root_view.scroll_into_view(entries.len());
    let t = &app.theme;
    let name_w = (area.width as usize * 60 / 100).max(10);
    buf.set_string(head.x + 2, head.y, "playlist", t.dim());
    buf.set_string(head.x + 2 + name_w as u16, head.y, "where", t.dim());
    let view = &app.pl.root_view;
    let rows = entries
        .iter()
        .enumerate()
        .skip(view.offset)
        .take(body.height as usize);
    for (row, (i, e)) in rows.enumerate() {
        let y = body.y + row as u16;
        let style = list_row_style(t, &app.mode, i, view, false);
        buf.set_style(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            style,
        );
        let sub = if i == view.selected {
            style
        } else {
            style.patch(Style::new().fg(t.dim))
        };
        let icon = match e {
            crate::tui::playlists::Entry::Local { .. } => "▪ ",
            _ => "♫ ",
        };
        buf.set_string(body.x, y, icon, sub);
        buf.set_stringn(
            body.x + 2,
            y,
            truncate(&e.title(), name_w - 2),
            name_w - 2,
            style,
        );
        let rest = body.width.saturating_sub(2 + name_w as u16) as usize;
        buf.set_stringn(
            body.x + 2 + name_w as u16,
            y,
            truncate(&e.detail(), rest),
            rest,
            sub,
        );
    }
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
