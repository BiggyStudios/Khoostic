//! `:` commands.

use std::path::PathBuf;
use std::time::Duration;

use khoostic_core::RepeatMode;

use crate::action::Action;
use crate::keymap::KeyMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Queue,
    Albums,
    Songs,
    Browse,
    Visualizer,
}

impl View {
    pub const ALL: [View; 5] = [
        View::Queue,
        View::Albums,
        View::Songs,
        View::Browse,
        View::Visualizer,
    ];

    pub fn title(self) -> &'static str {
        match self {
            View::Queue => "Queue",
            View::Albums => "Albums",
            View::Songs => "Songs",
            View::Browse => "Browse",
            View::Visualizer => "Visualizer",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Adjust {
    Set(f32),
    By(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SeekArg {
    To(Duration),
    By(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Quit,
    Run(Action),
    Add(PathBuf),
    Browse(Option<PathBuf>),
    Clear,
    Volume(Adjust),
    Seek(SeekArg),
    Shuffle(Option<bool>),
    Repeat(Option<RepeatMode>),
    Goto(usize),
    Map {
        mode: KeyMode,
        keys: String,
        action: String,
    },
    Unmap {
        mode: KeyMode,
        keys: String,
    },
    View(View),
    Help,
    Config {
        init: bool,
    },
}

pub const COMMANDS: &[(&str, &str)] = &[
    (":q", "quit"),
    (":add <path>", "add file or folder to the queue"),
    (":e [path]", "browse a folder (default: music folder)"),
    (":clear", "empty the queue"),
    (":vol <n|+n|-n>", "set or change volume (0-100)"),
    (":seek <[+-]m:ss|secs>", "seek absolute or relative"),
    (":shuffle [on|off]", "toggle or set shuffle"),
    (":repeat [off|all|one]", "cycle or set repeat"),
    (":<n>", "go to line n"),
    (
        ":map [n|v] <keys> <action>",
        "bind keys (session only; put in config to persist)",
    ),
    (":unmap [n|v] <keys>", "remove a binding"),
    (":view <queue|albums|songs|browse|vis>", "switch view"),
    (
        ":config [init]",
        "show config path / write a default config",
    ),
    (":<action>", "run any action by name, e.g. :next_track"),
];

pub fn parse(line: &str) -> Result<Cmd, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty command".into());
    }
    if let Ok(n) = line.parse::<usize>() {
        return Ok(Cmd::Goto(n));
    }
    let (name, rest) = match line.split_once(char::is_whitespace) {
        Some((n, r)) => (n, r.trim()),
        None => (line, ""),
    };
    let arg = (!rest.is_empty()).then_some(rest);

    Ok(match name {
        "q" | "q!" | "quit" | "qa" | "qa!" | "wq" | "x" => Cmd::Quit,
        "add" | "a" => Cmd::Add(expand_path(arg.ok_or("usage: :add <path>")?)),
        "e" | "edit" | "browse" | "cd" => Cmd::Browse(arg.map(expand_path)),
        "clear" => Cmd::Clear,
        "vol" | "volume" => Cmd::Volume(parse_volume(arg.ok_or("usage: :vol <n|+n|-n>")?)?),
        "seek" => Cmd::Seek(parse_seek(arg.ok_or("usage: :seek <[+-]m:ss>")?)?),
        "shuffle" => Cmd::Shuffle(match arg {
            None | Some("toggle") => None,
            Some(a) => Some(parse_bool(a)?),
        }),
        "repeat" => Cmd::Repeat(match arg {
            None => None,
            Some("off" | "none") => Some(RepeatMode::Off),
            Some("all" | "on") => Some(RepeatMode::All),
            Some("one" | "single") => Some(RepeatMode::One),
            Some(a) => return Err(format!("repeat: expected off, all or one, got {a:?}")),
        }),
        "map" | "nmap" | "vmap" | "unmap" | "nunmap" | "vunmap" => parse_map(name, arg)?,
        "view" => Cmd::View(match arg {
            Some("queue" | "q") => View::Queue,
            Some("albums" | "library" | "l") => View::Albums,
            Some("songs" | "s") => View::Songs,
            Some("browse" | "b" | "files") => View::Browse,
            Some("vis" | "visualizer" | "v") => View::Visualizer,
            _ => return Err("usage: :view <queue|albums|songs|browse|vis>".into()),
        }),
        "h" | "help" => Cmd::Help,
        "config" => Cmd::Config {
            init: arg == Some("init"),
        },
        "play" => Cmd::Run(Action::TogglePause),
        "pause" | "toggle" => Cmd::Run(Action::TogglePause),
        "stop" => Cmd::Run(Action::Stop),
        "next" | "n" => Cmd::Run(Action::NextTrack),
        "prev" | "previous" | "N" => Cmd::Run(Action::PrevTrack),
        other => match Action::from_name(other) {
            Some(a) => Cmd::Run(a),
            None => return Err(format!("not a command: {other}")),
        },
    })
}

fn parse_map(name: &str, arg: Option<&str>) -> Result<Cmd, String> {
    let unmap = name.ends_with("unmap");
    let mut parts: Vec<&str> = arg.unwrap_or("").split_whitespace().collect();
    let mode = match name.chars().next() {
        Some('v') => KeyMode::Visual,
        Some('n') => KeyMode::Normal,
        _ => match parts.first().and_then(|p| KeyMode::from_name(p)) {
            Some(m) if parts.len() > if unmap { 1 } else { 2 } => {
                parts.remove(0);
                m
            }
            _ => KeyMode::Normal,
        },
    };
    match (unmap, parts.as_slice()) {
        (false, [keys, action]) => Ok(Cmd::Map {
            mode,
            keys: keys.to_string(),
            action: action.to_string(),
        }),
        (true, [keys]) => Ok(Cmd::Unmap {
            mode,
            keys: keys.to_string(),
        }),
        (false, _) => Err("usage: :map [n|v] <keys> <action>".into()),
        (true, _) => Err("usage: :unmap [n|v] <keys>".into()),
    }
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => Err(format!("expected on or off, got {s:?}")),
    }
}

fn parse_volume(s: &str) -> Result<Adjust, String> {
    let s = s.trim_end_matches('%');
    let num = |t: &str| t.parse::<f32>().map_err(|_| format!("bad volume {s:?}"));
    Ok(match s.as_bytes().first() {
        Some(b'+') => Adjust::By(num(&s[1..])? / 100.0),
        Some(b'-') => Adjust::By(-num(&s[1..])? / 100.0),
        _ => Adjust::Set((num(s)? / 100.0).clamp(0.0, 1.0)),
    })
}

fn parse_seek(s: &str) -> Result<SeekArg, String> {
    let (sign, body) = match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => (0, s),
    };
    let d = parse_time(body).ok_or_else(|| format!("bad time {s:?} (use m:ss or seconds)"))?;
    Ok(match sign {
        0 => SeekArg::To(d),
        sign => SeekArg::By(sign * d.as_millis() as i64),
    })
}

/// `90`, `1:30`, `1:02:03`, `12.5`.
pub fn parse_time(s: &str) -> Option<Duration> {
    let mut secs = 0.0f64;
    for part in s.split(':') {
        let v: f64 = part.parse().ok()?;
        if !v.is_finite() || v < 0.0 {
            return None;
        }
        secs = secs * 60.0 + v;
    }
    Some(Duration::from_secs_f64(secs))
}

fn expand_path(s: &str) -> PathBuf {
    let s = s.trim_matches(|c| c == '"' || c == '\'');
    if let Some(rest) = s.strip_prefix('~')
        && let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
    {
        return home.join(rest.trim_start_matches(['/', '\\']));
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(parse("q").unwrap(), Cmd::Quit);
        assert_eq!(parse("  42 ").unwrap(), Cmd::Goto(42));
        assert_eq!(parse("next_track").unwrap(), Cmd::Run(Action::NextTrack));
        assert!(parse("frobnicate").is_err());
        assert_eq!(
            parse("add \"C:/My Music\"").unwrap(),
            Cmd::Add("C:/My Music".into())
        );
        assert_eq!(parse("e").unwrap(), Cmd::Browse(None));
    }

    #[test]
    fn volume_and_seek() {
        assert_eq!(parse("vol 50").unwrap(), Cmd::Volume(Adjust::Set(0.5)));
        assert_eq!(parse("vol +10").unwrap(), Cmd::Volume(Adjust::By(0.1)));
        assert_eq!(parse("vol -5%").unwrap(), Cmd::Volume(Adjust::By(-0.05)));
        assert_eq!(
            parse("seek 1:30").unwrap(),
            Cmd::Seek(SeekArg::To(Duration::from_secs(90)))
        );
        assert_eq!(parse("seek -10").unwrap(), Cmd::Seek(SeekArg::By(-10_000)));
        assert_eq!(
            parse("seek +1:00:00").unwrap(),
            Cmd::Seek(SeekArg::By(3_600_000))
        );
        assert!(parse("seek abc").is_err());
        assert!(parse("seek -nan").is_err());
    }

    #[test]
    fn modes() {
        assert_eq!(
            parse("repeat one").unwrap(),
            Cmd::Repeat(Some(RepeatMode::One))
        );
        assert_eq!(parse("shuffle").unwrap(), Cmd::Shuffle(None));
        assert_eq!(parse("shuffle off").unwrap(), Cmd::Shuffle(Some(false)));
    }

    #[test]
    fn mapping() {
        assert_eq!(
            parse("map <C-n> next_track").unwrap(),
            Cmd::Map {
                mode: KeyMode::Normal,
                keys: "<C-n>".into(),
                action: "next_track".into()
            }
        );
        assert_eq!(
            parse("map v D delete").unwrap(),
            Cmd::Map {
                mode: KeyMode::Visual,
                keys: "D".into(),
                action: "delete".into()
            }
        );
        assert_eq!(
            parse("vmap D delete").unwrap(),
            Cmd::Map {
                mode: KeyMode::Visual,
                keys: "D".into(),
                action: "delete".into()
            }
        );
        // A key literally called "v" in normal mode still works.
        assert_eq!(
            parse("map v visual").unwrap(),
            Cmd::Map {
                mode: KeyMode::Normal,
                keys: "v".into(),
                action: "visual".into()
            }
        );
        assert_eq!(
            parse("unmap q").unwrap(),
            Cmd::Unmap {
                mode: KeyMode::Normal,
                keys: "q".into()
            }
        );
    }
}
