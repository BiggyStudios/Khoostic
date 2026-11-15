//! A tiny readline for the `:` and `/` prompts (emacs-style keys, like vim's cmdline).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default, Clone)]
pub struct LineInput {
    text: String,
    /// Cursor position in chars.
    cursor: usize,
    history_pos: Option<usize>,
}

pub enum InputResult {
    /// Text changed or cursor moved.
    Edited,
    Submit(String),
    Cancel,
    Ignored,
}

impl LineInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(b, _)| b)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn set(&mut self, s: &str) {
        self.text = s.to_owned();
        self.cursor = self.len();
    }

    pub fn handle(&mut self, key: KeyEvent, history: &[String]) -> InputResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return InputResult::Cancel,
            KeyCode::Char('c') | KeyCode::Char('[') if ctrl => return InputResult::Cancel,
            KeyCode::Enter => return InputResult::Submit(self.text.clone()),
            // Backspace on an empty line leaves the prompt, like vim.
            KeyCode::Backspace if self.text.is_empty() => return InputResult::Cancel,
            KeyCode::Backspace | KeyCode::Char('h') if ctrl || key.code == KeyCode::Backspace => {
                if self.cursor > 0 {
                    let b = self.byte_at(self.cursor - 1);
                    self.text.remove(b);
                    self.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.len() {
                    let b = self.byte_at(self.cursor);
                    self.text.remove(b);
                }
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut start = self.cursor;
                while start > 0 && chars[start - 1] == ' ' {
                    start -= 1;
                }
                while start > 0 && chars[start - 1] != ' ' {
                    start -= 1;
                }
                let (a, b) = (self.byte_at(start), self.byte_at(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor = start;
            }
            KeyCode::Char('u') if ctrl => {
                let b = self.byte_at(self.cursor);
                self.text.replace_range(..b, "");
                self.cursor = 0;
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home | KeyCode::Char('a') if ctrl || key.code == KeyCode::Home => {
                self.cursor = 0
            }
            KeyCode::End | KeyCode::Char('e') if ctrl || key.code == KeyCode::End => {
                self.cursor = self.len()
            }
            KeyCode::Up | KeyCode::Char('p') if ctrl || key.code == KeyCode::Up => {
                if history.is_empty() {
                    return InputResult::Ignored;
                }
                let pos = self
                    .history_pos
                    .map_or(history.len() - 1, |p| p.saturating_sub(1));
                self.history_pos = Some(pos);
                self.set(&history[pos]);
            }
            KeyCode::Down | KeyCode::Char('n') if ctrl || key.code == KeyCode::Down => {
                match self.history_pos {
                    Some(p) if p + 1 < history.len() => {
                        self.history_pos = Some(p + 1);
                        self.set(&history[p + 1]);
                    }
                    Some(_) => {
                        self.history_pos = None;
                        self.set("");
                    }
                    None => return InputResult::Ignored,
                }
            }
            KeyCode::Char(c) if !ctrl => {
                let b = self.byte_at(self.cursor);
                self.text.insert(b, c);
                self.cursor += 1;
            }
            _ => return InputResult::Ignored,
        }
        InputResult::Edited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn editing() {
        let mut l = LineInput::default();
        for c in "add ~/Müsic".chars() {
            l.handle(key(KeyCode::Char(c)), &[]);
        }
        l.handle(key(KeyCode::Left), &[]);
        l.handle(key(KeyCode::Backspace), &[]);
        assert_eq!(l.text(), "add ~/Müsc");
        l.handle(ctrl('w'), &[]);
        assert_eq!(l.text(), "add c");
        l.handle(ctrl('u'), &[]);
        assert_eq!(l.text(), "c");
    }

    #[test]
    fn history() {
        let hist = vec!["vol 10".to_string(), "q".to_string()];
        let mut l = LineInput::default();
        l.handle(key(KeyCode::Up), &hist);
        assert_eq!(l.text(), "q");
        l.handle(key(KeyCode::Up), &hist);
        assert_eq!(l.text(), "vol 10");
        l.handle(key(KeyCode::Down), &hist);
        l.handle(key(KeyCode::Down), &hist);
        assert_eq!(l.text(), "");
        assert!(matches!(
            l.handle(key(KeyCode::Backspace), &hist),
            InputResult::Cancel
        ));
    }
}
