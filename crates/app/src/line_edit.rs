//! Minimal single-line text field state (path entry, rename). No IME; text comes from `key_char`.

#[derive(Debug, PartialEq)]
pub enum Edit {
    Changed,
    Submit,
    Cancel,
    Ignored,
}

#[derive(Default)]
pub struct LineEdit {
    pub text: String,
    cursor: usize, // byte offset, always on a char boundary
}

impl LineEdit {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self { cursor: text.len(), text }
    }

    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    pub fn after_cursor(&self) -> &str {
        &self.text[self.cursor..]
    }

    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    fn prev(&self) -> usize {
        self.before_cursor().char_indices().last().map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.after_cursor().chars().next().map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    /// `key` is the gpui key name, `ch` the typed text, `ctrl`/`platform` the modifiers.
    pub fn key(&mut self, key: &str, ch: Option<&str>, ctrl: bool) -> Edit {
        match key {
            "enter" => return Edit::Submit,
            "escape" => return Edit::Cancel,
            "backspace" if self.cursor > 0 => {
                let p = self.prev();
                self.text.replace_range(p..self.cursor, "");
                self.cursor = p;
            }
            "delete" if self.cursor < self.text.len() => {
                let n = self.next();
                self.text.replace_range(self.cursor..n, "");
            }
            "left" => self.cursor = self.prev(),
            "right" => self.cursor = self.next(),
            "home" => self.cursor = 0,
            "end" => self.cursor = self.text.len(),
            "u" if ctrl => {
                self.text.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            _ if ctrl => return Edit::Ignored,
            _ => match ch {
                Some(s) if !s.is_empty() && !s.chars().any(char::is_control) => self.insert(s),
                _ => return Edit::Ignored,
            },
        }
        Edit::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut e = LineEdit::new("~/Cöde");
        assert_eq!(e.key("backspace", None, false), Edit::Changed);
        assert_eq!(e.text, "~/Cöd");
        e.key("left", None, false);
        e.key("left", None, false);
        e.key("x", Some("x"), false);
        assert_eq!((e.before_cursor(), e.after_cursor()), ("~/Cx", "öd"));
        e.key("delete", None, false);
        assert_eq!(e.text, "~/Cxd");
        e.key("home", None, false);
        e.key("backspace", None, false); // at start: no-op
        assert_eq!(e.text, "~/Cxd");
        e.key("end", None, false);
        e.key("u", Some("u"), true);
        assert_eq!(e.text, "");
        assert_eq!(e.key("c", Some("c"), true), Edit::Ignored);
        e.insert("a\nb");
        assert_eq!(e.text, "ab");
        assert_eq!(e.key("enter", None, false), Edit::Submit);
        assert_eq!(e.key("escape", None, false), Edit::Cancel);
    }
}
