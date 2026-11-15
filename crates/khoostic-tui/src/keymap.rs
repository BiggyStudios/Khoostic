//! Vim-style key handling: multi-key sequences (`gg`, `dd`), count prefixes (`5j`), and
//! vim key notation (`<C-d>`, `<S-Tab>`, `<Space>`, `<lt>`).
//!
//! Visual mode looks up its own table first and falls back to normal mode, so playback and
//! motion keys work everywhere without duplicating bindings.

use std::collections::{HashMap, HashSet};
use std::fmt;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match code {
            KeyCode::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            c => c,
        };
        // Shift is already encoded in the character itself ('G', '>', ...).
        if matches!(code, KeyCode::Char(_)) {
            mods.remove(KeyModifiers::SHIFT);
        }
        Self { code, mods }
    }

    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn digit(&self) -> Option<u32> {
        match (self.code, self.mods.is_empty()) {
            (KeyCode::Char(c), true) => c.to_digit(10),
            _ => None,
        }
    }
}

impl From<KeyEvent> for Chord {
    fn from(e: KeyEvent) -> Self {
        Self::new(e.code, e.modifiers)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char('<') => "lt".to_string(),
            KeyCode::Char(c) if self.mods.is_empty() => return write!(f, "{c}"),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "CR".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Backspace => "BS".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::PageUp => "PageUp".into(),
            KeyCode::PageDown => "PageDown".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::Delete => "Del".into(),
            KeyCode::Insert => "Insert".into(),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}"),
        };
        let mut prefix = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            prefix.push_str("C-");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            prefix.push_str("A-");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            prefix.push_str("S-");
        }
        write!(f, "<{prefix}{name}>")
    }
}

pub fn format_keys(keys: &[Chord]) -> String {
    keys.iter().map(ToString::to_string).collect()
}

/// Parses vim key notation: `"gg"`, `"<C-d>"`, `"<S-Tab>"`, `"<lt>"`, `"<Space>"`.
pub fn parse_keys(s: &str) -> Result<Vec<Chord>, String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>').filter(|&e| e > 1)
        {
            out.push(parse_special(&rest[1..end])?);
            rest = &rest[end + 1..];
            continue;
        }
        out.push(Chord::char(c));
        rest = &rest[c.len_utf8()..];
    }
    if out.is_empty() {
        return Err("empty key sequence".into());
    }
    Ok(out)
}

fn parse_special(inner: &str) -> Result<Chord, String> {
    let mut mods = KeyModifiers::NONE;
    let mut name = inner;
    loop {
        let lower = name.to_ascii_lowercase();
        let m = match lower.get(..2) {
            Some("c-") => KeyModifiers::CONTROL,
            Some("a-") | Some("m-") => KeyModifiers::ALT,
            Some("s-") => KeyModifiers::SHIFT,
            _ => break,
        };
        if name.len() == 2 {
            break; // e.g. "<c->" — treat literally below
        }
        mods |= m;
        name = &name[2..];
    }
    let code = match name.to_ascii_lowercase().as_str() {
        "space" => KeyCode::Char(' '),
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        "bar" => KeyCode::Char('|'),
        "cr" | "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "bs" | "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "del" | "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        f if f.starts_with('f') && f.len() > 1 => {
            KeyCode::F(f[1..].parse().map_err(|_| format!("bad key <{inner}>"))?)
        }
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                // Ctrl/Alt letters are case-insensitive, like in vim.
                (Some(c), None) if !mods.is_empty() => KeyCode::Char(c.to_ascii_lowercase()),
                _ => return Err(format!("unknown key <{inner}>")),
            }
        }
    };
    Ok(Chord::new(code, mods))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyMode {
    Normal,
    Visual,
}

impl KeyMode {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "normal" | "n" => Some(Self::Normal),
            "visual" | "v" => Some(Self::Visual),
            _ => None,
        }
    }
}

#[derive(Default, Clone)]
struct Table {
    exact: HashMap<Vec<Chord>, Action>,
    prefixes: HashSet<Vec<Chord>>,
}

impl Table {
    fn bind(&mut self, keys: Vec<Chord>, action: Option<Action>) {
        match action {
            Some(a) => {
                self.exact.insert(keys, a);
            }
            None => {
                self.exact.remove(&keys);
            }
        }
        self.prefixes = self
            .exact
            .keys()
            .flat_map(|k| (1..k.len()).map(move |n| k[..n].to_vec()))
            .collect();
    }

    fn lookup(&self, keys: &[Chord]) -> Lookup {
        match (self.exact.get(keys), self.prefixes.contains(keys)) {
            (Some(&a), false) => Lookup::Exact(a),
            (Some(&a), true) => Lookup::Ambiguous(a),
            (None, true) => Lookup::Prefix,
            (None, false) => Lookup::None,
        }
    }
}

enum Lookup {
    Exact(Action),
    /// Matches, but a longer binding also starts this way (resolved on timeout).
    Ambiguous(Action),
    Prefix,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Action { action: Action, count: Option<u32> },
    Pending,
    Unbound,
}

#[derive(Clone)]
pub struct Keymap {
    normal: Table,
    visual: Table,
    count: Option<u32>,
    pending: Vec<Chord>,
}

const DEFAULT_NORMAL: &[(&str, Action)] = {
    use Action::*;
    &[
        ("j", MoveDown),
        ("<Down>", MoveDown),
        ("k", MoveUp),
        ("<Up>", MoveUp),
        ("gg", Top),
        ("<Home>", Top),
        ("G", Bottom),
        ("<End>", Bottom),
        ("<C-d>", HalfPageDown),
        ("<C-u>", HalfPageUp),
        ("<C-f>", PageDown),
        ("<PageDown>", PageDown),
        ("<C-b>", PageUp),
        ("<PageUp>", PageUp),
        ("o", GotoPlaying),
        ("<CR>", Activate),
        ("h", Parent),
        ("<BS>", Parent),
        ("l", Open),
        ("a", Append),
        ("A", InsertNext),
        ("dd", Delete),
        ("x", Delete),
        ("<Del>", Delete),
        ("yy", Yank),
        ("p", PasteAfter),
        ("P", PasteBefore),
        ("J", MoveItemDown),
        ("K", MoveItemUp),
        ("V", VisualMode),
        ("v", VisualMode),
        ("<Esc>", Escape),
        ("<Space>", TogglePause),
        ("S", Stop),
        (">", NextTrack),
        ("<lt>", PrevTrack),
        ("f", SeekForward),
        ("<Right>", SeekForward),
        ("b", SeekBackward),
        ("<Left>", SeekBackward),
        ("F", SeekForwardBig),
        ("B", SeekBackwardBig),
        ("0", SeekStart),
        ("+", VolumeUp),
        ("=", VolumeUp),
        ("-", VolumeDown),
        ("s", ToggleShuffle),
        ("r", CycleRepeat),
        (":", CommandMode),
        ("/", Search),
        ("n", SearchNext),
        ("N", SearchPrev),
        ("<Tab>", NextView),
        ("<S-Tab>", PrevView),
        ("gq", ViewQueue),
        ("gb", ViewBrowse),
        ("gv", ViewVisualizer),
        ("<F1>", ViewQueue),
        ("<F2>", ViewBrowse),
        ("<F3>", ViewVisualizer),
        ("c", CycleVisualizer),
        ("?", Help),
        ("q", Quit),
    ]
};

const DEFAULT_VISUAL: &[(&str, Action)] = {
    use Action::*;
    &[
        ("d", Delete),
        ("x", Delete),
        ("y", Yank),
        ("V", VisualMode),
        ("v", VisualMode),
    ]
};

impl Default for Keymap {
    fn default() -> Self {
        let mut km = Self {
            normal: Table::default(),
            visual: Table::default(),
            count: None,
            pending: Vec::new(),
        };
        for &(keys, action) in DEFAULT_NORMAL {
            km.normal
                .bind(parse_keys(keys).expect("valid default"), Some(action));
        }
        for &(keys, action) in DEFAULT_VISUAL {
            km.visual
                .bind(parse_keys(keys).expect("valid default"), Some(action));
        }
        km
    }
}

impl Keymap {
    /// Applies user overrides: `mode -> keys -> action name` (`"none"` unbinds).
    /// Returns human-readable problems instead of failing, so one typo doesn't block startup.
    pub fn with_overrides<'a, M, B>(mut self, overrides: M) -> (Self, Vec<String>)
    where
        M: IntoIterator<Item = (&'a String, B)>,
        B: IntoIterator<Item = (&'a String, &'a String)>,
    {
        let mut problems = Vec::new();
        for (mode, binds) in overrides {
            let Some(mode) = KeyMode::from_name(mode) else {
                problems.push(format!("unknown key mode {mode:?} (use normal or visual)"));
                continue;
            };
            for (keys, action) in binds {
                if let Err(e) = self.bind_str(mode, keys, action) {
                    problems.push(e);
                }
            }
        }
        (self, problems)
    }

    pub fn bind_str(&mut self, mode: KeyMode, keys: &str, action: &str) -> Result<(), String> {
        let chords = parse_keys(keys)?;
        let action = match action {
            "none" | "nop" | "<nop>" => None,
            name => {
                Some(Action::from_name(name).ok_or_else(|| format!("unknown action {name:?}"))?)
            }
        };
        self.table_mut(mode).bind(chords, action);
        Ok(())
    }

    fn table_mut(&mut self, mode: KeyMode) -> &mut Table {
        match mode {
            KeyMode::Normal => &mut self.normal,
            KeyMode::Visual => &mut self.visual,
        }
    }

    /// Bindings for the help screen, sorted by action then key.
    pub fn bindings(&self, mode: KeyMode) -> Vec<(String, Action)> {
        let table = match mode {
            KeyMode::Normal => &self.normal,
            KeyMode::Visual => &self.visual,
        };
        let mut v: Vec<_> = table
            .exact
            .iter()
            .map(|(k, &a)| (format_keys(k), a))
            .collect();
        v.sort_by_key(|(k, a)| (Action::ALL.iter().position(|x| x == a), k.len(), k.clone()));
        v
    }

    /// What has been typed so far (for the status line, like vim's `showcmd`).
    pub fn pending_display(&self) -> String {
        let mut s = self.count.map(|c| c.to_string()).unwrap_or_default();
        s.push_str(&format_keys(&self.pending));
        s
    }

    pub fn is_pending(&self) -> bool {
        self.count.is_some() || !self.pending.is_empty()
    }

    pub fn reset(&mut self) {
        self.count = None;
        self.pending.clear();
    }

    pub fn feed(&mut self, mode: KeyMode, key: Chord) -> Outcome {
        if self.pending.is_empty()
            && let Some(d) = key.digit()
        {
            let bound_alone = self.lookup(mode, &[key]).is_some();
            // `0` is a key of its own unless a count is being typed (vim does the same).
            if self.count.is_some() || (d != 0 && !bound_alone) {
                self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d));
                return Outcome::Pending;
            }
        }
        self.pending.push(key);
        let keys = self.pending.clone();
        match self.lookup(mode, &keys) {
            Some(Lookup::Exact(action)) => self.finish(action),
            Some(Lookup::Prefix) | Some(Lookup::Ambiguous(_)) => Outcome::Pending,
            Some(Lookup::None) | None => {
                self.reset();
                Outcome::Unbound
            }
        }
    }

    /// Called when the user stops typing mid-sequence: fires an ambiguous exact match
    /// (e.g. both `g` and `gg` bound and only `g` was typed), otherwise abandons it.
    pub fn timeout(&mut self, mode: KeyMode) -> Option<Outcome> {
        if self.pending.is_empty() {
            return None;
        }
        let keys = self.pending.clone();
        match self.lookup(mode, &keys) {
            Some(Lookup::Ambiguous(action)) => Some(self.finish(action)),
            _ => {
                self.reset();
                Some(Outcome::Unbound)
            }
        }
    }

    fn finish(&mut self, action: Action) -> Outcome {
        let count = self.count.take();
        self.pending.clear();
        Outcome::Action { action, count }
    }

    fn lookup(&self, mode: KeyMode, keys: &[Chord]) -> Option<Lookup> {
        if mode == KeyMode::Visual {
            match self.visual.lookup(keys) {
                Lookup::None => {}
                hit => return Some(hit),
            }
        }
        match self.normal.lookup(keys) {
            Lookup::None => None,
            hit => Some(hit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_str(km: &mut Keymap, mode: KeyMode, s: &str) -> Vec<Outcome> {
        parse_keys(s)
            .unwrap()
            .into_iter()
            .map(|c| km.feed(mode, c))
            .collect()
    }

    fn last(km: &mut Keymap, mode: KeyMode, s: &str) -> Outcome {
        feed_str(km, mode, s).pop().unwrap()
    }

    #[test]
    fn notation_parses() {
        let k = parse_keys("<C-d>gg<S-Tab><lt><Space>x<F5><c-W>").unwrap();
        assert_eq!(k[0], Chord::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(k[1], Chord::char('g'));
        assert_eq!(k[3], Chord::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(k[4], Chord::char('<'));
        assert_eq!(k[5], Chord::char(' '));
        assert_eq!(k[7], Chord::new(KeyCode::F(5), KeyModifiers::NONE));
        assert_eq!(k[8], Chord::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(format_keys(&k), "<C-d>gg<S-Tab><lt><Space>x<F5><C-w>");
        assert!(parse_keys("<Bogus>").is_err());
        // A lone '<' that isn't notation is just the character.
        assert_eq!(parse_keys("<").unwrap(), vec![Chord::char('<')]);
    }

    #[test]
    fn crossterm_events_normalize() {
        let shifted = Chord::from(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(shifted, Chord::char('G'));
        let backtab = Chord::from(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(backtab, parse_keys("<S-Tab>").unwrap()[0]);
    }

    #[test]
    fn sequences_and_counts() {
        let mut km = Keymap::default();
        let n = KeyMode::Normal;
        assert_eq!(
            last(&mut km, n, "j"),
            Outcome::Action {
                action: Action::MoveDown,
                count: None
            }
        );
        assert_eq!(feed_str(&mut km, n, "gg")[0], Outcome::Pending);
        assert_eq!(
            last(&mut km, n, "gg"),
            Outcome::Action {
                action: Action::Top,
                count: None
            }
        );
        assert_eq!(
            last(&mut km, n, "12j"),
            Outcome::Action {
                action: Action::MoveDown,
                count: Some(12)
            }
        );
        assert_eq!(km.pending_display(), "");
        assert_eq!(
            last(&mut km, n, "3dd"),
            Outcome::Action {
                action: Action::Delete,
                count: Some(3)
            }
        );
        // `0` alone is bound (restart track), but inside a count it's a digit.
        assert_eq!(
            last(&mut km, n, "0"),
            Outcome::Action {
                action: Action::SeekStart,
                count: None
            }
        );
        assert_eq!(
            last(&mut km, n, "10G"),
            Outcome::Action {
                action: Action::Bottom,
                count: Some(10)
            }
        );
        assert_eq!(
            last(&mut km, n, "<lt>"),
            Outcome::Action {
                action: Action::PrevTrack,
                count: None
            }
        );
    }

    #[test]
    fn unbound_resets_state() {
        let mut km = Keymap::default();
        let n = KeyMode::Normal;
        assert_eq!(last(&mut km, n, "4gz"), Outcome::Unbound);
        assert!(!km.is_pending());
        assert_eq!(
            last(&mut km, n, "j"),
            Outcome::Action {
                action: Action::MoveDown,
                count: None
            }
        );
    }

    #[test]
    fn showcmd_display() {
        let mut km = Keymap::default();
        feed_str(&mut km, KeyMode::Normal, "5d");
        assert_eq!(km.pending_display(), "5d");
    }

    #[test]
    fn visual_overrides_then_falls_back() {
        let mut km = Keymap::default();
        let v = KeyMode::Visual;
        assert_eq!(
            last(&mut km, v, "d"),
            Outcome::Action {
                action: Action::Delete,
                count: None
            }
        );
        assert_eq!(
            last(&mut km, v, "j"),
            Outcome::Action {
                action: Action::MoveDown,
                count: None
            }
        );
        assert_eq!(
            last(&mut km, v, "gg"),
            Outcome::Action {
                action: Action::Top,
                count: None
            }
        );
    }

    #[test]
    fn overrides_and_ambiguity_timeout() {
        let overrides: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, String>,
        > = [(
            "normal".to_string(),
            [
                ("g".to_string(), "goto_playing".to_string()),
                ("q".to_string(), "none".to_string()),
                ("<C-q>".to_string(), "quit".to_string()),
                ("zz".to_string(), "bogus".to_string()),
            ]
            .into(),
        )]
        .into();
        let (mut km, problems) = Keymap::default().with_overrides(&overrides);
        assert_eq!(problems.len(), 1);
        let n = KeyMode::Normal;
        assert_eq!(last(&mut km, n, "q"), Outcome::Unbound);
        assert_eq!(
            last(&mut km, n, "<C-q>"),
            Outcome::Action {
                action: Action::Quit,
                count: None
            }
        );
        // `g` is both bound and a prefix of `gg`: waits, then fires on timeout.
        assert_eq!(last(&mut km, n, "g"), Outcome::Pending);
        assert_eq!(
            km.timeout(n),
            Some(Outcome::Action {
                action: Action::GotoPlaying,
                count: None
            })
        );
        assert_eq!(
            last(&mut km, n, "gg"),
            Outcome::Action {
                action: Action::Top,
                count: None
            }
        );
    }
}
