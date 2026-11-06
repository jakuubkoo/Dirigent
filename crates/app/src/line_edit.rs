//! A single-line text field: `text_area` (selection, word moves, undo, copy/cut) kept on one line, plus what the
//! last frame drew (`ui::text_input`) so mouse clicks map to offsets and the field scrolls to the cursor. No IME;
//! text comes from `key_char`.

use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;

use gpui::{App, ClipboardItem, EntityId, Keystroke, Pixels, ShapedLine};

use crate::text_area::{Key, TextArea};

#[derive(Debug, PartialEq)]
pub enum Edit {
    Changed,
    /// Only the cursor or the selection moved (also a copy).
    Moved,
    Submit,
    Cancel,
    Ignored,
}

#[derive(Default)]
pub struct LineEdit {
    area: TextArea,
    view: Rc<RefCell<View>>,
}

/// Shared with the mouse listeners of the frame that drew it.
#[derive(Default)]
pub struct View {
    /// The shaped text (None: empty) and the x of its first char at scroll 0.
    pub line: Option<ShapedLine>,
    pub left: Pixels,
    /// Horizontal scroll that keeps the cursor visible (sticky: changes only when the cursor would leave the field).
    pub scroll: Pixels,
    pub text: String,
    /// (anchor, cursor) as drawn; a mouse press or drag sets it and `pending` until the next key applies it.
    pub sel: (usize, usize),
    pub pending: bool,
    pub dragging: bool,
    /// The view that drew it: a mouse change notifies only it (never `window.refresh()`: it would repaint the
    /// cached terminal).
    pub owner: Option<EntityId>,
}

impl View {
    /// The byte offset under window x.
    pub fn index_at(&self, x: Pixels) -> usize {
        self.line.as_ref().map_or(0, |l| l.closest_index_for_x(x - self.left + self.scroll))
    }
}

impl Deref for LineEdit {
    type Target = TextArea;
    fn deref(&self) -> &TextArea {
        &self.area
    }
}

impl LineEdit {
    pub fn new(text: impl Into<String>) -> Self {
        let mut area = TextArea::default();
        area.text = single_line(&text.into());
        area.click(area.text.len(), false);
        Self { area, view: Default::default() }
    }

    pub fn view(&self) -> Rc<RefCell<View>> {
        self.view.clone()
    }

    /// (anchor, cursor) including a mouse selection not applied yet.
    pub fn sel(&self) -> (usize, usize) {
        let v = self.view.borrow();
        if v.pending {
            return v.sel;
        }
        let r = self.area.selection();
        if self.area.cursor() == r.start { (r.end, r.start) } else { (r.start, r.end) }
    }

    fn sync(&mut self) {
        let mut v = self.view.borrow_mut();
        if std::mem::take(&mut v.pending) {
            self.area.select(v.sel.0, v.sel.1);
        }
    }

    /// Typed or pasted text replaces the selection; control chars (newlines too) are dropped.
    pub fn insert(&mut self, s: &str) {
        self.sync();
        self.area.insert(&single_line(s), false);
    }

    /// Clipboard text: its lines joined with spaces, trimmed.
    pub fn paste(&mut self, s: &str) {
        let joined: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        self.insert(&joined.join(" "));
    }

    /// A key press with the clipboard (Ctrl/Cmd+C, X, V).
    pub fn key(&mut self, ks: &Keystroke, cx: &mut App) -> Edit {
        let ctrl = ks.modifiers.control || ks.modifiers.platform;
        if ctrl && ks.key == "v" {
            let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return Edit::Ignored };
            self.paste(&text);
            return Edit::Changed;
        }
        let (edit, copied) = self.key_with(&ks.key, ks.key_char.as_deref(), ctrl, ks.modifiers.shift);
        if let Some(text) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        edit
    }

    /// `key` without the clipboard: gpui's key name, the typed text, Ctrl/Cmd, Shift; copied text comes back.
    pub fn key_with(&mut self, key: &str, ch: Option<&str>, ctrl: bool, shift: bool) -> (Edit, Option<String>) {
        self.sync();
        match key {
            "enter" => return (Edit::Submit, None),
            "escape" => return (Edit::Cancel, None),
            // Owners use them (lists, next field); on one line they would only jump to the ends.
            "up" | "down" | "tab" => return (Edit::Ignored, None),
            "u" if ctrl => {
                let end = self.area.cursor();
                self.area.select(0, end);
                return self.key_with("backspace", None, false, false);
            }
            _ => {}
        }
        let len = self.area.text.len();
        match self.area.key(key, ch, ctrl, shift) {
            Key::Changed => (Edit::Changed, None),
            Key::Moved => (Edit::Moved, None),
            Key::Copy(t) if self.area.text.len() != len => (Edit::Changed, Some(t)),
            Key::Copy(t) => (Edit::Moved, Some(t)),
            Key::Ignored => (Edit::Ignored, None),
        }
    }
}

fn single_line(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(e: &mut LineEdit, k: &str, ch: Option<&str>, ctrl: bool) -> Edit {
        e.key_with(k, ch, ctrl, false).0
    }

    #[test]
    fn editing() {
        let mut e = LineEdit::new("~/Cöde");
        assert_eq!(key(&mut e, "backspace", None, false), Edit::Changed);
        assert_eq!(e.text, "~/Cöd");
        assert_eq!(key(&mut e, "left", None, false), Edit::Moved);
        key(&mut e, "left", None, false);
        key(&mut e, "x", Some("x"), false);
        assert_eq!(e.text, "~/Cxöd");
        key(&mut e, "delete", None, false);
        assert_eq!(e.text, "~/Cxd");
        key(&mut e, "home", None, false);
        assert_eq!(key(&mut e, "backspace", None, false), Edit::Ignored, "at start: no-op");
        key(&mut e, "end", None, false);
        key(&mut e, "u", Some("u"), true);
        assert_eq!(e.text, "");
        assert_eq!(key(&mut e, "q", Some("q"), true), Edit::Ignored, "other Ctrl keys stay with the owner");
        e.insert("a\nb");
        assert_eq!(e.text, "ab");
        assert_eq!(key(&mut e, "enter", None, false), Edit::Submit);
        assert_eq!(key(&mut e, "escape", None, false), Edit::Cancel);
        assert_eq!(key(&mut e, "up", None, false), Edit::Ignored);
    }

    #[test]
    fn selection_clipboard_and_undo() {
        let mut e = LineEdit::new("fix login bug");
        e.key_with("left", None, true, true);
        assert_eq!(e.key_with("c", None, true, false), (Edit::Moved, Some("bug".into())));
        assert_eq!(e.key_with("x", None, true, false), (Edit::Changed, Some("bug".into())));
        assert_eq!(e.text, "fix login ");
        e.paste("  issue\n  42 \n");
        assert_eq!(e.text, "fix login issue 42");
        key(&mut e, "z", None, true);
        assert_eq!(e.text, "fix login ");
        key(&mut e, "z", None, true);
        assert_eq!(e.text, "fix login bug");
        key(&mut e, "a", None, true);
        key(&mut e, "x", Some("y"), false);
        assert_eq!(e.text, "y", "typing replaces the selection");
    }

    #[test]
    fn mouse_selection_applies_before_the_next_key() {
        let mut e = LineEdit::new("hello world");
        {
            let mut v = e.view.borrow_mut();
            v.sel = (6, 11);
            v.pending = true;
        }
        assert_eq!(e.sel(), (6, 11), "drawn before any key");
        key(&mut e, "x", Some("W"), false);
        assert_eq!(e.text, "hello W");
        assert_eq!(e.sel(), (7, 7));
    }
}
