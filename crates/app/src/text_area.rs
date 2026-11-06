//! A minimal multi-line text field's state: cursor and selection (byte offsets on char boundaries), moves by
//! char/word/line, undo/redo with typing runs coalesced. Pure (no gpui): the commit box renders it and maps keys
//! and clicks onto it. No IME (text comes from `key_char`), like `line_edit`.

#[derive(Clone, Default, Debug, PartialEq)]
pub struct TextArea {
    pub text: String,
    cursor: usize,
    /// The other end of the selection (== cursor: none).
    anchor: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The last edit was typing at this cursor position: the next char joins its undo step.
    typing_at: Option<usize>,
    /// Column kept across up/down moves.
    goal_col: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    text: String,
    cursor: usize,
}

const MAX_UNDO: usize = 200;

/// What a key did.
#[derive(Debug, PartialEq)]
pub enum Key {
    Changed,
    Moved,
    /// Text to put on the clipboard (copy/cut); a cut also changed the text.
    Copy(String),
    Ignored,
}

impl TextArea {
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected byte range (empty when nothing is selected).
    pub fn selection(&self) -> std::ops::Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }

    pub fn set_text(&mut self, text: &str) {
        self.snapshot();
        self.text = text.to_string();
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        self.typing_at = None;
    }

    /// (line, byte column) of a byte offset.
    pub fn line_col(&self, at: usize) -> (usize, usize) {
        let before = &self.text[..at];
        let line = before.matches('\n').count();
        let col = at - before.rfind('\n').map_or(0, |i| i + 1);
        (line, col)
    }

    /// Byte offset of (line, byte column), clamped to the line and to a char boundary.
    pub fn offset(&self, line: usize, col: usize) -> usize {
        let start: usize = self.text.split('\n').take(line).map(|l| l.len() + 1).sum::<usize>().min(self.text.len());
        let len = self.text[start..].find('\n').unwrap_or(self.text.len() - start);
        let mut at = start + col.min(len);
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    /// Mouse: place the cursor (`extend`: drag/shift-click keeps the anchor).
    pub fn click(&mut self, at: usize, extend: bool) {
        self.cursor = at.min(self.text.len());
        if !extend {
            self.anchor = self.cursor;
        }
        self.typing_at = None;
        self.goal_col = None;
    }

    /// Mouse: select `anchor..cursor` (clamped to the text; offsets on char boundaries).
    pub fn select(&mut self, anchor: usize, cursor: usize) {
        self.anchor = anchor.min(self.text.len());
        self.click(cursor, true);
    }

    fn snapshot(&mut self) {
        self.undo.push(Snapshot { text: self.text.clone(), cursor: self.cursor });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Replaces the selection with `s` (typing joins the previous typing step for undo).
    pub fn insert(&mut self, s: &str, typing: bool) {
        let s: String = s.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
        if s.is_empty() && self.selection().is_empty() {
            return;
        }
        let r = self.selection();
        if !(typing && self.typing_at == Some(self.cursor) && r.is_empty()) {
            self.snapshot();
        }
        self.text.replace_range(r.clone(), &s);
        self.cursor = r.start + s.len();
        self.anchor = self.cursor;
        self.typing_at = (typing && !s.contains('\n')).then_some(self.cursor);
        self.goal_col = None;
    }

    fn delete(&mut self, r: std::ops::Range<usize>) {
        if r.is_empty() {
            return;
        }
        self.snapshot();
        self.text.replace_range(r.clone(), "");
        self.cursor = r.start;
        self.anchor = self.cursor;
        self.typing_at = None;
        self.goal_col = None;
    }

    fn prev_char(&self, at: usize) -> usize {
        self.text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    fn next_char(&self, at: usize) -> usize {
        self.text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
    }

    fn word_left(&self, at: usize) -> usize {
        let before = &self.text[..at];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
        trimmed.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_').len()
    }

    fn word_right(&self, at: usize) -> usize {
        let after = &self.text[at..];
        let skip = after.len() - after.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_').len();
        let rest = &after[skip..];
        at + skip + (rest.len() - rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_').len())
    }

    fn move_to(&mut self, at: usize, extend: bool) {
        self.cursor = at;
        if !extend {
            self.anchor = at;
        }
        self.typing_at = None;
    }

    fn vertical(&mut self, down: bool, extend: bool) {
        let (line, col) = self.line_col(self.cursor);
        let goal = *self.goal_col.get_or_insert(col);
        let lines = self.text.matches('\n').count();
        let at = match (down, line) {
            (false, 0) => 0,
            (true, l) if l >= lines => self.text.len(),
            (false, l) => self.offset(l - 1, goal),
            (true, l) => self.offset(l + 1, goal),
        };
        let goal_col = self.goal_col;
        self.move_to(at, extend);
        self.goal_col = goal_col;
    }

    fn restore(&mut self, from_undo: bool) -> bool {
        let (from, to) = if from_undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        let Some(s) = from.pop() else { return false };
        to.push(Snapshot { text: std::mem::replace(&mut self.text, s.text), cursor: self.cursor });
        self.cursor = s.cursor.min(self.text.len());
        self.anchor = self.cursor;
        self.typing_at = None;
        true
    }

    /// A key press: gpui's key name, its typed char, Ctrl / Shift.
    pub fn key(&mut self, key: &str, ch: Option<&str>, ctrl: bool, shift: bool) -> Key {
        let sel = self.selection();
        let line_start = |s: &Self, at: usize| s.text[..at].rfind('\n').map_or(0, |i| i + 1);
        let line_end = |s: &Self, at: usize| s.text[at..].find('\n').map_or(s.text.len(), |i| at + i);
        let moved = |s: &mut Self, at: usize| {
            s.move_to(at, shift);
            s.goal_col = None;
            Key::Moved
        };
        match (key, ctrl) {
            ("left", _) if !shift && !sel.is_empty() && !ctrl => moved(self, sel.start),
            ("right", _) if !shift && !sel.is_empty() && !ctrl => moved(self, sel.end),
            ("left", false) => moved(self, self.prev_char(self.cursor)),
            ("right", false) => moved(self, self.next_char(self.cursor)),
            ("left", true) => moved(self, self.word_left(self.cursor)),
            ("right", true) => moved(self, self.word_right(self.cursor)),
            ("up", false) => {
                self.vertical(false, shift);
                Key::Moved
            }
            ("down", false) => {
                self.vertical(true, shift);
                Key::Moved
            }
            ("home", false) => moved(self, line_start(self, self.cursor)),
            ("end", false) => moved(self, line_end(self, self.cursor)),
            ("home", true) => moved(self, 0),
            ("end", true) => moved(self, self.text.len()),
            ("a", true) => {
                self.anchor = 0;
                self.cursor = self.text.len();
                Key::Moved
            }
            ("c", true) if !sel.is_empty() => Key::Copy(self.text[sel].to_string()),
            ("x", true) if !sel.is_empty() => {
                let cut = self.text[sel.clone()].to_string();
                self.delete(sel);
                Key::Copy(cut)
            }
            ("z", true) if !shift => {
                if self.restore(true) {
                    Key::Changed
                } else {
                    Key::Ignored
                }
            }
            ("z", true) | ("y", true) => {
                if self.restore(false) {
                    Key::Changed
                } else {
                    Key::Ignored
                }
            }
            ("backspace", _) => {
                let r = if !sel.is_empty() {
                    sel
                } else if ctrl {
                    self.word_left(self.cursor)..self.cursor
                } else {
                    self.prev_char(self.cursor)..self.cursor
                };
                if r.is_empty() {
                    Key::Ignored
                } else {
                    self.delete(r);
                    Key::Changed
                }
            }
            ("delete", _) => {
                let r = if !sel.is_empty() {
                    sel
                } else if ctrl {
                    self.cursor..self.word_right(self.cursor)
                } else {
                    self.cursor..self.next_char(self.cursor)
                };
                if r.is_empty() {
                    Key::Ignored
                } else {
                    self.delete(r);
                    Key::Changed
                }
            }
            ("enter", false) => {
                self.insert("\n", false);
                Key::Changed
            }
            (_, false) => match ch {
                Some(c) if !c.is_empty() && !c.chars().any(char::is_control) => {
                    self.insert(c, true);
                    Key::Changed
                }
                _ => Key::Ignored,
            },
            _ => Key::Ignored,
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Double click: the word around byte offset `at` (on a char boundary), else the one char there.
pub fn word_at(text: &str, at: usize) -> std::ops::Range<usize> {
    let at = at.min(text.len());
    let (before, after) = text.split_at(at);
    let start = before.trim_end_matches(is_word).len();
    let end = at + after.len() - after.trim_start_matches(is_word).len();
    if start < end { start..end } else { at..after.chars().next().map_or(at, |c| at + c.len_utf8()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_at_double_click() {
        assert_eq!(word_at("fix login_bug!", 6), 4..13);
        assert_eq!(word_at("fix login", 3), 0..3, "right after a word: that word");
        assert_eq!(word_at("a  b", 2), 2..3, "between spaces: one char");
        assert_eq!(word_at("čeština", 2), 0.."čeština".len());
        assert_eq!(word_at("", 0), 0..0);
    }

    fn typed(s: &str) -> TextArea {
        let mut t = TextArea::default();
        for c in s.chars() {
            if c == '\n' {
                t.key("enter", None, false, false);
            } else {
                t.key("x", Some(&c.to_string()), false, false);
            }
        }
        t
    }

    #[test]
    fn typing_undo_coalesces_and_redo() {
        let mut t = typed("Fix bug\nmore");
        assert_eq!(t.text, "Fix bug\nmore");
        t.key("z", None, true, false);
        assert_eq!(t.text, "Fix bug\n", "one undo removes the typed word run");
        t.key("z", None, true, false);
        assert_eq!(t.text, "Fix bug", "the newline is its own step");
        t.key("z", None, true, true);
        assert_eq!(t.text, "Fix bug\n");
        t.key("z", None, true, false);
        t.key("z", None, true, false);
        assert_eq!(t.text, "");
    }

    #[test]
    fn moves_and_selection() {
        let mut t = typed("hello world\nab");
        assert_eq!(t.line_col(t.cursor()), (1, 2));
        t.key("up", None, false, false);
        assert_eq!(t.line_col(t.cursor()), (0, 2), "column kept");
        t.key("end", None, false, false);
        t.key("left", None, true, true);
        assert_eq!(&t.text[t.selection()], "world");
        assert_eq!(t.key("c", None, true, false), Key::Copy("world".into()));
        t.key("x", Some("W"), false, false);
        assert_eq!(t.text, "hello W\nab", "typing replaces the selection");
        t.key("home", None, true, false);
        t.key("down", None, false, true);
        assert_eq!(&t.text[t.selection()], "hello W\n");
        assert_eq!(t.key("x", None, true, false), Key::Copy("hello W\n".into()));
        assert_eq!(t.text, "ab");
        t.key("a", None, true, false);
        t.key("backspace", None, false, false);
        assert_eq!(t.text, "");
    }

    #[test]
    fn multibyte_and_words() {
        let mut t = typed("čeština je fajn");
        t.key("backspace", None, true, false);
        assert_eq!(t.text, "čeština je ");
        t.key("home", None, false, false);
        t.key("right", None, false, false);
        assert_eq!(t.cursor(), 'č'.len_utf8());
        t.key("right", None, true, false);
        assert_eq!(&t.text[..t.cursor()], "čeština");
        assert_eq!(t.offset(0, 1), 0, "never inside a char");
    }

    #[test]
    fn paste_keeps_newlines_and_drops_controls() {
        let mut t = TextArea::default();
        t.insert("a\tb\nc\u{7}", false);
        assert_eq!(t.text, "ab\nc");
    }
}
