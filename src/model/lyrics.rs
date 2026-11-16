//! Song lyrics: lines, optionally time-synced, from whichever provider had them. Includes the
//! LRC format (`[mm:ss.xx] line`) that local files and LRCLIB use.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    /// Every line has a start time.
    pub synced: bool,
    /// Who provided them, for the credit line ("Spotify", "LRCLIB", "Local file").
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub start: Option<Duration>,
    pub text: String,
}

impl Lyrics {
    pub fn plain(text: &str, source: &str) -> Self {
        Self {
            lines: text
                .lines()
                .map(|l| LyricLine {
                    start: None,
                    text: l.trim_end().to_owned(),
                })
                .collect(),
            synced: false,
            source: source.to_owned(),
        }
    }

    /// Parses LRC. Lines without timestamps are dropped when others have them; metadata tags
    /// (`[ar:…]`, `[offset:…]`) are read or skipped. Falls back to plain text.
    pub fn from_lrc(text: &str, source: &str) -> Self {
        let mut lines = Vec::new();
        let mut offset_ms = 0i64;
        for raw in text.lines() {
            let mut rest = raw.trim();
            let mut times = Vec::new();
            while let Some(tag) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
                let (inside, after) = tag;
                if let Some(t) = parse_time(inside) {
                    times.push(t);
                } else if let Some(o) = inside.strip_prefix("offset:") {
                    offset_ms = o.trim().parse().unwrap_or(0);
                }
                rest = after;
            }
            let words = strip_word_times(rest.trim());
            for t in times {
                lines.push((t, words.clone()));
            }
        }
        if lines.is_empty() {
            return Self::plain(text, source);
        }
        lines.sort_by_key(|(t, _)| *t);
        Self {
            lines: lines
                .into_iter()
                .map(|(t, text)| LyricLine {
                    // A positive offset means the lyrics should show earlier.
                    start: Some(Duration::from_millis((t as i64 - offset_ms).max(0) as u64)),
                    text,
                })
                .collect(),
            synced: true,
            source: source.to_owned(),
        }
    }

    /// The line being sung at `pos` (the last one that has started).
    pub fn line_at(&self, pos: Duration) -> Option<usize> {
        if !self.synced {
            return None;
        }
        let i = self
            .lines
            .partition_point(|l| l.start.is_some_and(|s| s <= pos));
        i.checked_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.text.trim().is_empty())
    }
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx` → milliseconds.
fn parse_time(s: &str) -> Option<u64> {
    let (m, rest) = s.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let (sec, frac) = rest.split_once(['.', ':']).unwrap_or((rest, "0"));
    let sec: u64 = sec.trim().parse().ok()?;
    let frac = frac.trim();
    if !frac.chars().all(|c| c.is_ascii_digit()) || frac.is_empty() {
        return None;
    }
    let ms = match frac.len() {
        1 => frac.parse::<u64>().ok()? * 100,
        2 => frac.parse::<u64>().ok()? * 10,
        _ => frac[..3].parse::<u64>().ok()?,
    };
    Some(m * 60_000 + sec * 1000 + ms)
}

/// Enhanced LRC puts `<mm:ss.xx>` before words; the line is enough here.
fn strip_word_times(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        match rest[i..].find('>') {
            Some(j) if parse_time(&rest[i + 1..i + j]).is_some() => rest = &rest[i + j + 1..],
            _ => {
                out.push('<');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let l = Lyrics::from_lrc(
            "[ar:Daft Punk]\n[00:30.37]One more time\n[00:33.8][01:10.00]♪\n\n[00:45.980] <00:45.98>We're <00:46.50>gonna",
            "test",
        );
        assert!(l.synced);
        let texts: Vec<_> = l.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["One more time", "♪", "We're gonna", "♪"]);
        assert_eq!(l.lines[0].start, Some(Duration::from_millis(30_370)));
        assert_eq!(l.lines[1].start, Some(Duration::from_millis(33_800)));
        assert_eq!(l.lines[3].start, Some(Duration::from_secs(70)));
    }

    #[test]
    fn offset_and_plain_fallback() {
        let l = Lyrics::from_lrc("[offset:500]\n[00:10.00]a", "t");
        assert_eq!(l.lines[0].start, Some(Duration::from_millis(9_500)));
        let p = Lyrics::from_lrc("just words\nno times", "t");
        assert!(!p.synced);
        assert_eq!(p.lines.len(), 2);
    }

    #[test]
    fn current_line() {
        let l = Lyrics::from_lrc("[00:01.00]a\n[00:05.00]b\n[00:09.00]c", "t");
        assert_eq!(l.line_at(Duration::from_millis(500)), None);
        assert_eq!(l.line_at(Duration::from_secs(1)), Some(0));
        assert_eq!(l.line_at(Duration::from_secs(7)), Some(1));
        assert_eq!(l.line_at(Duration::from_secs(99)), Some(2));
    }
}
