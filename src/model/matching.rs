//! Fuzzy "is this the same song?" for matching tracks across sources (a Last.fm suggestion to
//! a local file, a Spotify result to a song you already have).

use crate::model::Track;

/// A comparable form of a title: lowercase letters and digits only, with edition noise
/// ("(Remastered 2011)", "- Radio Edit", "[feat. X]") removed.
pub fn title_key(title: &str) -> String {
    let lower = title.to_lowercase();
    // Drop " - Remastered …", " - Radio Edit" style suffixes.
    let base = match lower.find(" - ") {
        Some(i) if is_noise(&lower[i + 3..]) => &lower[..i],
        _ => &lower,
    };
    let mut out = String::with_capacity(base.len());
    let mut depth = 0usize;
    let mut group = String::new();
    for c in base.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                group.clear();
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                // Keep meaningful parentheticals ("(Da Ba Dee)"), drop editions/features.
                if !is_noise(&group) {
                    out.extend(group.chars().filter(|c| c.is_alphanumeric()));
                }
                group.clear();
            }
            _ if depth > 0 => group.push(c),
            c if c.is_alphanumeric() => out.push(c),
            _ => {}
        }
    }
    if out.is_empty() {
        lower.chars().filter(|c| c.is_alphanumeric()).collect()
    } else {
        out
    }
}

fn is_noise(s: &str) -> bool {
    const WORDS: [&str; 12] = [
        "remaster",
        "radio edit",
        "feat",
        "ft.",
        "with ",
        "live",
        "version",
        "mono",
        "stereo",
        "edit",
        "explicit",
        "bonus",
    ];
    let s = s.trim();
    WORDS.iter().any(|w| s.starts_with(w) || s.contains(w))
}

/// Lowercase with accents folded (é → e, ß → ss), punctuation dropped and spaces collapsed:
/// for comparing what someone typed with names ("beyonce" finds "Beyoncé").
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            match fold(c) {
                Some(f) => out.push_str(f),
                None => out.push(c),
            }
        } else if c.is_whitespace() || matches!(c, '-' | '_' | '/' | '&' | ',' | '.') {
            space = true;
        }
    }
    out
}

/// The plain Latin letters for an accented one.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'ł' | 'ľ' | 'ĺ' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'œ' => "oe",
        'ř' | 'ŕ' => "r",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'ß' => "ss",
        'ť' | 'ţ' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'þ' => "th",
        _ => return None,
    })
}

/// A comparable form of an artist name (the first credited artist only).
pub fn artist_key(artist: &str) -> String {
    let lower = artist.to_lowercase();
    let first = lower
        .split([',', '&', ';', '/'])
        .next()
        .unwrap_or(&lower)
        .split(" feat")
        .next()
        .unwrap_or_default()
        .split(" ft.")
        .next()
        .unwrap_or_default()
        .split(" x ")
        .next()
        .unwrap_or_default();
    let first = first.strip_prefix("the ").unwrap_or(first);
    first.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// `(artist, title)` key for de-duplicating and matching.
pub fn song_key(artist: &str, title: &str) -> (String, String) {
    (artist_key(artist), title_key(title))
}

pub fn track_key(t: &Track) -> (String, String) {
    song_key(t.primary_artist().unwrap_or_default(), &t.title)
}

/// Whether `t` is the song `artist` – `title`.
pub fn is_song(t: &Track, artist: &str, title: &str) -> bool {
    let (a, ti) = song_key(artist, title);
    let (ta, tt) = track_key(t);
    !tt.is_empty() && tt == ti && (a.is_empty() || ta.is_empty() || ta == a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_ignore_editions_but_keep_real_parentheticals() {
        assert_eq!(title_key("Tsunami (Radio Edit)"), title_key("Tsunami"));
        assert_eq!(title_key("Bad - 2012 Remaster"), "bad");
        assert_eq!(title_key("Something (Remastered 2009)"), "something");
        assert_eq!(title_key("Blue (Da Ba Dee)"), "bluedabadee");
        assert_eq!(title_key("Suzume (feat. Toaka)"), "suzume");
        assert_eq!(title_key("STYX HELIX"), title_key("Styx Helix"));
        assert_eq!(title_key("(  )"), "");
    }

    #[test]
    fn artists_use_the_first_credit() {
        assert_eq!(artist_key("Eiffel 65, Flume"), "eiffel65");
        assert_eq!(artist_key("The Neighbourhood"), "neighbourhood");
        assert_eq!(artist_key("RADWIMPS feat. Toaka"), "radwimps");
        assert_eq!(artist_key("Juice WRLD & Nicki Minaj"), "juicewrld");
    }

    #[test]
    fn matches_tracks() {
        let t = Track {
            title: "Sweater Weather".into(),
            artists: vec!["The Neighbourhood".into()],
            ..Default::default()
        };
        assert!(is_song(
            &t,
            "the neighbourhood",
            "Sweater Weather - Remastered"
        ));
        assert!(!is_song(&t, "Someone Else", "Sweater Weather"));
    }

    #[test]
    fn normalize_folds_case_accents_and_punctuation() {
        assert_eq!(normalize("Beyoncé"), "beyonce");
        assert_eq!(normalize("  AC/DC  "), "ac dc");
        assert_eq!(normalize("Sigur Rós – Hoppípolla"), "sigur ros hoppipolla");
        assert_eq!(normalize("Straße"), "strasse");
        assert_eq!(normalize("米津玄師"), "米津玄師");
    }
}
