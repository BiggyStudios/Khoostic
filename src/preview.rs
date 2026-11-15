//! `khoostic play`: a deliberately small live view used to exercise the engine end to end
//! before the real TUI exists. Keys are vim-flavored to match what the TUI will use.

use std::io::{Write, stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, SetAttribute, SetForegroundColor};
use crossterm::{cursor, execute, queue, terminal};
use khoostic_audio::{AnalysisFrame, BANDS, EngineConfig, PlaybackState};
use khoostic_core::{RepeatMode, Track};
use khoostic_player::{Player, PlayerCommand as Cmd, PlayerEvent, PlayerSnapshot, Resolvers};

const FRAME: Duration = Duration::from_millis(16);
const SPECTRUM_ROWS: usize = 8;
const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub fn run(config: EngineConfig, tracks: Vec<Track>, shuffle: bool) -> anyhow::Result<()> {
    let player = Player::start(config, Resolvers::with_defaults())?;
    let count = tracks.len();
    player.send(Cmd::Enqueue(tracks));
    if shuffle {
        player.send(Cmd::ToggleShuffle);
    }
    player.send(Cmd::Next);

    let _term = TerminalGuard::enter()?;
    let mut last_error: Option<String> = None;
    let mut finished = false;

    loop {
        while let Ok(ev) = player.events().try_recv() {
            match ev {
                PlayerEvent::Error(e) => last_error = Some(e),
                PlayerEvent::StateChanged(PlaybackState::Stopped) => finished = true,
                PlayerEvent::StateChanged(_) => finished = false,
                PlayerEvent::TrackChanged(_) => {}
            }
        }

        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if ctrl => break,
                KeyCode::Char(' ') => player.send(Cmd::TogglePause),
                KeyCode::Char('n') | KeyCode::Char('j') | KeyCode::Down => player.send(Cmd::Next),
                KeyCode::Char('p') | KeyCode::Char('k') | KeyCode::Up => player.send(Cmd::Prev),
                KeyCode::Char('l') | KeyCode::Right => player.send(Cmd::SeekBy(5_000)),
                KeyCode::Char('h') | KeyCode::Left => player.send(Cmd::SeekBy(-5_000)),
                KeyCode::Char('L') => player.send(Cmd::SeekBy(30_000)),
                KeyCode::Char('H') => player.send(Cmd::SeekBy(-30_000)),
                KeyCode::Char('0') | KeyCode::Char('^') => player.send(Cmd::Seek(Duration::ZERO)),
                KeyCode::Char('+') | KeyCode::Char('=') => player.send(Cmd::VolumeBy(0.05)),
                KeyCode::Char('-') => player.send(Cmd::VolumeBy(-0.05)),
                KeyCode::Char('s') => player.send(Cmd::ToggleShuffle),
                KeyCode::Char('r') => player.send(Cmd::CycleRepeat),
                _ => {}
            }
        }

        let snap = player.snapshot();
        let index = player.queue().read().current_index();
        draw(
            &snap,
            &player.analysis(),
            index,
            count,
            last_error.as_deref(),
            finished,
        )?;
    }
    Ok(())
}

fn draw(
    s: &PlayerSnapshot,
    a: &AnalysisFrame,
    index: Option<usize>,
    count: usize,
    error: Option<&str>,
    finished: bool,
) -> std::io::Result<()> {
    let mut out = stdout().lock();
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let width = cols as usize;
    let dim = SetForegroundColor(Color::DarkGrey);
    let accent = SetForegroundColor(Color::Cyan);
    let reset = SetAttribute(Attribute::Reset);

    queue!(
        out,
        cursor::MoveTo(0, 0),
        terminal::Clear(terminal::ClearType::All)
    )?;

    let icon = match s.state {
        PlaybackState::Playing => "▶",
        PlaybackState::Paused => "⏸",
        PlaybackState::Stopped => "■",
    };
    let title = s.current.as_ref().map_or("—", |e| e.track.title.as_str());
    let pos = index.map_or_else(|| "-".into(), |i| (i + 1).to_string());
    queue!(
        out,
        cursor::MoveTo(2, 1),
        accent,
        Print(icon),
        reset,
        SetAttribute(Attribute::Bold),
        Print(format!("  {}", truncate(title, width.saturating_sub(20)))),
        reset,
        dim,
        Print(format!("   {pos}/{count}")),
        reset,
    )?;

    // Progress line.
    let dur = s.duration.unwrap_or_default();
    let bar_w = width.saturating_sub(32).clamp(10, 80);
    let frac = if dur.is_zero() {
        0.0
    } else {
        s.position.as_secs_f64() / dur.as_secs_f64()
    };
    let filled = ((frac.clamp(0.0, 1.0)) * bar_w as f64) as usize;
    queue!(
        out,
        cursor::MoveTo(2, 3),
        Print(fmt_time(s.position)),
        Print("  "),
        accent,
        Print("━".repeat(filled)),
        Print("●"),
        dim,
        Print("─".repeat(bar_w.saturating_sub(filled))),
        reset,
        Print("  "),
        Print(fmt_time(dur)),
    )?;

    let repeat = match s.repeat {
        RepeatMode::Off => "off",
        RepeatMode::All => "all",
        RepeatMode::One => "one",
    };
    queue!(
        out,
        cursor::MoveTo(2, 4),
        dim,
        Print(format!(
            "vol {:>3}%   shuffle {}   repeat {}   {} Hz   buffer {:>4} ms   {}",
            (s.volume * 100.0).round(),
            if s.shuffle { "on " } else { "off" },
            repeat,
            s.sample_rate,
            s.buffered.as_millis(),
            truncate(&s.device, 32),
        )),
        reset,
    )?;

    // Spectrum: one column per band, scaled to fit the terminal.
    let bands = BANDS.min(width.saturating_sub(4));
    for row in 0..SPECTRUM_ROWS {
        queue!(out, cursor::MoveTo(2, (6 + row) as u16))?;
        let level_floor = (SPECTRUM_ROWS - 1 - row) as f32;
        let mut line = String::with_capacity(bands * 3);
        for b in 0..bands {
            let v = a.bands[b * BANDS / bands] * SPECTRUM_ROWS as f32;
            let cell = ((v - level_floor) * 8.0).clamp(0.0, 8.0) as usize;
            line.push(BLOCKS[cell]);
        }
        let color = match row {
            0..=1 => Color::Magenta,
            2..=4 => Color::Blue,
            _ => Color::Cyan,
        };
        queue!(out, SetForegroundColor(color), Print(line), reset)?;
    }
    let pulse = if a.beat > 0.5 {
        "●"
    } else if a.beat > 0.15 {
        "•"
    } else {
        "·"
    };
    queue!(
        out,
        cursor::MoveTo(2, (7 + SPECTRUM_ROWS) as u16),
        accent,
        Print(pulse),
        dim,
        Print(format!(
            " beats {:<5} bass {:>3.0}%  mid {:>3.0}%  treble {:>3.0}%",
            a.beat_count,
            a.bass * 100.0,
            a.mid * 100.0,
            a.treble * 100.0
        )),
        reset,
    )?;

    let mut y = (9 + SPECTRUM_ROWS) as u16;
    if finished {
        queue!(
            out,
            cursor::MoveTo(2, y),
            dim,
            Print("end of queue — space to restart"),
            reset
        )?;
        y += 1;
    }
    if let Some(e) = error {
        queue!(
            out,
            cursor::MoveTo(2, y),
            SetForegroundColor(Color::Red),
            Print(truncate(e, width.saturating_sub(4))),
            reset
        )?;
        y += 1;
    }
    queue!(
        out,
        cursor::MoveTo(2, y + 1),
        dim,
        Print(
            "space pause · j/k next/prev · h/l ±5s · H/L ±30s · +/- vol · s shuffle · r repeat · q quit"
        ),
        reset,
    )?;
    out.flush()
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Restores the terminal even if we bail out with an error or panic.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(stdout(), cursor::Show, terminal::LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
            hook(info);
        }));
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
