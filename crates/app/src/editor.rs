//! The file editor's state, pure (no gpui): a `ropey` buffer with cursor and selection (char indices), moves,
//! auto-indent, Tab/Shift+Tab, undo/redo (rope snapshots: a clone shares chunks, O(1)), dirty tracking, the strict
//! load/save byte round trip, and the decisions on disk changes. `FileView` renders it and maps keys and clicks on it.
//! No IME: text comes from `key_char`, like `text_area`.

use std::ops::Range;

use ropey::Rope;

/// Larger files open read-only (the full re-highlight per edit stays well under a second).
pub const MAX_BYTES: usize = 2 << 20;
pub const MAX_LINES: usize = 50_000;
/// Longer lines would be cut by the display (`git::text::MAX_LINE_CHARS`): such a file opens read-only.
pub const MAX_LINE_CHARS: usize = git::text::MAX_LINE_CHARS;
const MAX_UNDO: usize = 1000;
const TAB: usize = 4;

/// Syntax spans of one line: raw byte ranges in the line (no newline), `syntax::Class` ids.
pub type Spans = Vec<(Range<u32>, u8)>;

/// Why a file can't be edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnly {
    Binary,
    NotUtf8,
    TooLarge,
    LongLines,
    MixedEol,
}

impl ReadOnly {
    pub fn label(self) -> &'static str {
        match self {
            Self::Binary => "Read-only: binary",
            Self::NotUtf8 => "Read-only: not UTF-8",
            Self::TooLarge => "Read-only: too large to edit",
            Self::LongLines => "Read-only: lines too long to edit",
            Self::MixedEol => "Read-only: mixed line endings",
        }
    }
}

/// What the bytes on disk looked like, to write them back the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Format {
    pub crlf: bool,
    pub bom: bool,
}

/// Strict read: the text (LF only) and its format, or why it stays read-only.
pub fn load(bytes: &[u8]) -> Result<(Rope, Format), ReadOnly> {
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(ReadOnly::Binary);
    }
    if bytes.len() > MAX_BYTES {
        return Err(ReadOnly::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReadOnly::NotUtf8)?;
    let (text, bom) = match text.strip_prefix('\u{feff}') {
        Some(t) => (t, true),
        None => (text, false),
    };
    let crlfs = text.matches("\r\n").count();
    let lfs = text.matches('\n').count();
    let crs = text.matches('\r').count();
    if crs != crlfs || (crlfs > 0 && crlfs != lfs) {
        return Err(ReadOnly::MixedEol);
    }
    if lfs >= MAX_LINES {
        return Err(ReadOnly::TooLarge);
    }
    let crlf = crlfs > 0;
    let text = if crlf { std::borrow::Cow::Owned(text.replace("\r\n", "\n")) } else { text.into() };
    if text.split('\n').any(|l| l.len() > MAX_LINE_CHARS && l.chars().count() > MAX_LINE_CHARS) {
        return Err(ReadOnly::LongLines);
    }
    Ok((Rope::from_str(&text), Format { crlf, bom }))
}

/// The bytes to write: the format `load` saw.
pub fn save_bytes(rope: &Rope, f: Format) -> Vec<u8> {
    let mut out = Vec::with_capacity(rope.len_bytes() + 3);
    if f.bom {
        out.extend_from_slice("\u{feff}".as_bytes());
    }
    for chunk in rope.chunks() {
        if f.crlf {
            out.extend_from_slice(chunk.replace('\n', "\r\n").as_bytes());
        } else {
            out.extend_from_slice(chunk.as_bytes());
        }
    }
    out
}

/// Ctrl+S: writes `bytes` over `path` unless the file on disk is no longer the version the buffer was loaded or last
/// saved from (`base`), whatever the watcher reported: then Ok(None) and nothing is written. `force` (the conflict
/// banner's Overwrite) skips that check. Ok(hash of what was written). Blocking.
pub fn write_checked(path: &std::path::Path, bytes: &[u8], base: u64, force: bool) -> Result<Option<u64>, String> {
    if !force {
        let disk = match std::fs::read(path) {
            Ok(b) => Some(hash(&b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        if save_check(base, disk) == Save::Conflict {
            return Ok(None);
        }
    }
    // ponytail: written in place (keeps permissions, owner, a symlink's target, hard links; no temp file in the
    // repo), so a crash mid-write can leave the file cut. Temp file + rename (copying those) if that ever bites.
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(Some(hash(bytes)))
}

/// Content hash of file bytes (in-process only: what we loaded or saved vs what is on disk now).
pub fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// The open file changed on disk (`disk` = hash of its bytes now, `base` = what we loaded or last saved).
#[derive(Debug, PartialEq, Eq)]
pub enum DiskChange {
    /// Same bytes (our own save, a touch): nothing to do.
    Ignore,
    /// No unsaved edits: take the new text silently.
    Reload,
    /// Unsaved edits: ask (reload and lose mine / keep mine / show diff).
    Banner,
}

pub fn on_disk_change(dirty: bool, disk: u64, base: u64) -> DiskChange {
    match (disk == base, dirty) {
        (true, _) => DiskChange::Ignore,
        (false, false) => DiskChange::Reload,
        (false, true) => DiskChange::Banner,
    }
}

/// Ctrl+S: write only over the bytes we loaded or last saved, else confirm first.
#[derive(Debug, PartialEq, Eq)]
pub enum Save {
    Write,
    Conflict,
}

/// `disk`: hash of the file now, None if it's gone (writing recreates it).
pub fn save_check(base: u64, disk: Option<u64>) -> Save {
    match disk {
        Some(d) if d != base => Save::Conflict,
        _ => Save::Write,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    Tab,
    Spaces(usize),
}

impl Indent {
    /// Tabs if more lines start with a tab than with 2+ spaces; else the smallest space indent (2..=8), default 4.
    pub fn detect(rope: &Rope) -> Self {
        let (mut tabs, mut spaced, mut min) = (0, 0, usize::MAX);
        for line in rope.lines().take(2000) {
            match line.chars().next() {
                Some('\t') => tabs += 1,
                Some(' ') => {
                    let n = line.chars().take_while(|c| *c == ' ').count();
                    let next = line.chars().nth(n);
                    if n >= 2 && next.is_some_and(|c| !c.is_whitespace() && c != '*') {
                        spaced += 1;
                        min = min.min(n);
                    }
                }
                _ => {}
            }
        }
        if tabs > spaced { Self::Tab } else { Self::Spaces(if min <= 8 { min } else { TAB }) }
    }

    fn unit(self) -> String {
        match self {
            Self::Tab => "\t".into(),
            Self::Spaces(n) => " ".repeat(n),
        }
    }
}

/// An edit as lines and byte columns, for shifting the old syntax spans until fresh ones arrive.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// `start..end` (line, byte col) removed.
    Delete { start: (usize, u32), end: (usize, u32) },
    /// Inserted at `at`: `lines` newlines, `last` bytes after the last one (or the whole text without newlines).
    Insert { at: (usize, u32), lines: usize, last: u32 },
    /// Whole text replaced (undo, redo, reload): spans stay until the re-highlight.
    Reset,
}

/// Applies `e` to per-line spans so unchanged text keeps its colors (new text takes its left neighbour's color).
pub fn shift_spans(colors: &mut Vec<Spans>, e: &Edit) {
    let take = |colors: &mut Vec<Spans>, l: usize| colors.get_mut(l).map(std::mem::take).unwrap_or_default();
    match *e {
        Edit::Reset => {}
        Edit::Delete { start: (l0, c0), end: (l1, c1) } => {
            if l0 >= colors.len() {
                return;
            }
            let first = take(colors, l0);
            let last = if l1 == l0 { first.clone() } else { take(colors, l1) };
            let mut out: Spans =
                first.iter().filter(|(r, _)| r.start < c0).map(|(r, c)| (r.start..r.end.min(c0), *c)).collect();
            out.extend(
                last.iter().filter(|(r, _)| r.end > c1).map(|(r, c)| (r.start.max(c1) - c1 + c0..r.end - c1 + c0, *c)),
            );
            merge(&mut out);
            colors[l0] = out;
            colors.drain(l0 + 1..(l1 + 1).min(colors.len()));
        }
        Edit::Insert { at: (l, c), lines, last } => {
            if l >= colors.len() {
                return;
            }
            let line = take(colors, l);
            if lines == 0 {
                colors[l] = line
                    .into_iter()
                    .map(|(r, k)| {
                        let s = if r.start >= c { r.start + last } else { r.start };
                        let e = if r.end >= c { r.end + last } else { r.end };
                        (s..e, k)
                    })
                    .collect();
                return;
            }
            let head: Spans =
                line.iter().filter(|(r, _)| r.start < c).map(|(r, k)| (r.start..r.end.min(c), *k)).collect();
            let tail: Spans = line
                .iter()
                .filter(|(r, _)| r.end > c)
                .map(|(r, k)| (r.start.max(c) - c + last..r.end - c + last, *k))
                .collect();
            colors[l] = head;
            colors.splice(l + 1..l + 1, std::iter::repeat_n(Spans::new(), lines - 1).chain([tail]));
        }
    }
}

/// Joins touching spans of one class (a delete can bring them together).
fn merge(s: &mut Spans) {
    s.retain(|(r, _)| !r.is_empty());
    s.dedup_by(|b, a| {
        if a.1 == b.1 && a.0.end == b.0.start {
            a.0.end = b.0.end;
            true
        } else {
            false
        }
    });
}

#[derive(Clone)]
struct Snap {
    rope: Rope,
    cursor: usize,
    anchor: usize,
    version: u64,
}

/// What a key did.
#[derive(Debug, PartialEq)]
pub enum Key {
    Changed,
    Moved,
    /// Text for the clipboard (copy/cut; a cut also changed the text).
    Copy(String),
    Ignored,
}

pub struct Buffer {
    pub rope: Rope,
    cursor: usize,
    /// The other end of the selection (== cursor: none).
    anchor: usize,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// The last edit was typing that ended here: the next typed char joins its undo step.
    typing_at: Option<usize>,
    /// Display column kept across up/down moves.
    goal: Option<usize>,
    version: u64,
    last_version: u64,
    saved: u64,
    pub indent: Indent,
    edits: Vec<Edit>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Buffer {
    pub fn new(rope: Rope) -> Self {
        let indent = Indent::detect(&rope);
        Self {
            rope,
            cursor: 0,
            anchor: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            typing_at: None,
            goal: None,
            version: 0,
            last_version: 0,
            saved: 0,
            indent,
            edits: Vec::new(),
        }
    }

    pub fn dirty(&self) -> bool {
        self.version != self.saved
    }

    /// The text now is what the file holds (saved, or reloaded).
    pub fn mark_saved(&mut self) {
        self.saved = self.version;
    }

    /// `version` was written to disk (edits made while the save ran stay unsaved).
    pub fn mark_saved_at(&mut self, version: u64) {
        self.saved = version;
    }

    /// The cursor's display column (tabs expanded): for keeping it in view horizontally.
    pub fn cursor_disp_col(&self) -> usize {
        self.disp_col(self.cursor)
    }

    /// Changes with every edit, undo and redo: a result computed for an older version is stale.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn take_edits(&mut self) -> Vec<Edit> {
        std::mem::take(&mut self.edits)
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Selected char range (empty: none).
    pub fn selection(&self) -> Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }

    pub fn selected_text(&self) -> String {
        self.rope.slice(self.selection()).to_string()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Line `i` without its newline.
    pub fn line(&self, i: usize) -> String {
        let mut s = self.rope.line(i).to_string();
        if s.ends_with('\n') {
            s.pop();
        }
        s
    }

    /// (line, byte column) of a char index.
    pub fn line_col(&self, at: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(at);
        (line, self.rope.char_to_byte(at) - self.rope.line_to_byte(line))
    }

    /// Char index of (line, byte column), clamped to the text, the line and a char boundary.
    pub fn pos(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines() - 1);
        let start = self.rope.line_to_byte(line);
        let end = start + self.line_len_bytes(line);
        self.rope.byte_to_char((start + col).min(end))
    }

    fn line_len_bytes(&self, line: usize) -> usize {
        let l = self.rope.line(line);
        l.len_bytes() - (l.len_chars() > 0 && l.char(l.len_chars() - 1) == '\n') as usize
    }

    fn line_start(&self, at: usize) -> usize {
        self.rope.line_to_char(self.rope.char_to_line(at))
    }

    fn line_end(&self, at: usize) -> usize {
        let line = self.rope.char_to_line(at);
        self.rope.byte_to_char(self.rope.line_to_byte(line) + self.line_len_bytes(line))
    }

    /// Display column (tabs to the next multiple of 4) of a char index.
    fn disp_col(&self, at: usize) -> usize {
        let start = self.line_start(at);
        self.rope.slice(start..at).chars().fold(0, |col, c| if c == '\t' { col + TAB - col % TAB } else { col + 1 })
    }

    /// Char index on `line` nearest to display column `goal` (never past its end).
    fn at_disp_col(&self, line: usize, goal: usize) -> usize {
        let start = self.rope.line_to_char(line);
        let mut col = 0;
        for (i, c) in self.rope.line(line).chars().enumerate() {
            let w = if c == '\t' { TAB - col % TAB } else { 1 };
            if c == '\n' || col >= goal {
                return start + i;
            }
            if col + w > goal {
                return start + i + (goal - col > w / 2) as usize; // inside a tab: the nearer edge
            }
            col += w;
        }
        start + self.rope.line(line).len_chars()
    }

    fn char_at(&self, at: usize) -> Option<char> {
        (at < self.rope.len_chars()).then(|| self.rope.char(at))
    }

    fn word_left(&self, at: usize) -> usize {
        let mut it = self.rope.chars_at(at);
        let mut i = at;
        while let Some(c) = it.prev() {
            if is_word(c) {
                it.next();
                break;
            }
            i -= 1;
        }
        while let Some(c) = it.prev() {
            if !is_word(c) {
                break;
            }
            i -= 1;
        }
        i
    }

    fn word_right(&self, at: usize) -> usize {
        let mut i = at;
        let mut it = self.rope.chars_at(at).peekable();
        while it.next_if(|c| !is_word(*c)).is_some() {
            i += 1;
        }
        while it.next_if(|c| is_word(*c)).is_some() {
            i += 1;
        }
        i
    }

    /// Double click: the word around `at`, else the one char there.
    pub fn select_word(&mut self, at: usize) {
        let at = at.min(self.rope.len_chars());
        let mut s = at;
        while s > 0 && is_word(self.rope.char(s - 1)) {
            s -= 1;
        }
        let mut e = at;
        while self.char_at(e).is_some_and(is_word) {
            e += 1;
        }
        if s == e {
            e = (at + 1).min(self.line_end(at).max(at));
        }
        self.select(s, e);
    }

    /// Triple click: the whole line with its newline.
    pub fn select_line(&mut self, at: usize) {
        let line = self.rope.char_to_line(at.min(self.rope.len_chars()));
        let end =
            if line + 1 < self.rope.len_lines() { self.rope.line_to_char(line + 1) } else { self.rope.len_chars() };
        self.select(self.rope.line_to_char(line), end);
    }

    /// Mouse: place the cursor (`extend`: drag / Shift+click keeps the anchor).
    pub fn click(&mut self, at: usize, extend: bool) {
        self.move_to(at.min(self.rope.len_chars()), extend);
    }

    pub fn select(&mut self, anchor: usize, cursor: usize) {
        self.anchor = anchor.min(self.rope.len_chars());
        self.click(cursor, true);
    }

    fn move_to(&mut self, at: usize, extend: bool) {
        self.cursor = at;
        if !extend {
            self.anchor = at;
        }
        self.typing_at = None;
        self.goal = None;
    }

    fn snapshot(&mut self) {
        self.undo.push(Snap {
            rope: self.rope.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
            version: self.version,
        });
        if self.undo.len() > MAX_UNDO {
            self.undo.drain(..MAX_UNDO / 10);
        }
        self.redo.clear();
    }

    fn bump(&mut self) {
        self.last_version += 1;
        self.version = self.last_version;
    }

    fn remove(&mut self, r: Range<usize>) {
        if r.is_empty() {
            return;
        }
        let (a, b) = (self.line_col(r.start), self.line_col(r.end));
        self.edits.push(Edit::Delete { start: (a.0, a.1 as u32), end: (b.0, b.1 as u32) });
        self.rope.remove(r);
    }

    fn ins(&mut self, at: usize, s: &str) {
        if s.is_empty() {
            return;
        }
        let (l, c) = self.line_col(at);
        let lines = s.matches('\n').count();
        let last = s.rfind('\n').map_or(s.len(), |i| s.len() - i - 1) as u32;
        self.edits.push(Edit::Insert { at: (l, c as u32), lines, last });
        self.rope.insert(at, s);
    }

    /// Replaces the selection with `s` (typing joins the previous typing step for undo).
    pub fn insert(&mut self, s: &str, typing: bool) {
        let s: String = s.chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()).collect();
        let r = self.selection();
        if s.is_empty() && r.is_empty() {
            return;
        }
        if !(typing && self.typing_at == Some(self.cursor) && r.is_empty()) {
            self.snapshot();
        }
        self.remove(r.clone());
        self.ins(r.start, &s);
        self.cursor = r.start + s.chars().count();
        self.anchor = self.cursor;
        self.typing_at = (typing && !s.contains('\n')).then_some(self.cursor);
        self.goal = None;
        self.bump();
    }

    /// Clipboard text: CRLF / CR become LF.
    pub fn paste(&mut self, s: &str) {
        self.insert(&s.replace("\r\n", "\n").replace('\r', "\n"), false);
    }

    fn delete(&mut self, r: Range<usize>) -> Key {
        if r.is_empty() {
            return Key::Ignored;
        }
        self.snapshot();
        self.remove(r.clone());
        self.move_to(r.start, false);
        self.bump();
        Key::Changed
    }

    /// The file's text from disk replaces mine, keeping the cursor's line and column. Undo and redo start over:
    /// Ctrl+Z never crosses a reload (it would silently revert someone else's change). The text is saved now.
    pub fn reload(&mut self, rope: Rope) {
        let (l, c) = self.line_col(self.cursor);
        let (al, ac) = self.line_col(self.anchor);
        self.rope = rope;
        self.cursor = self.pos(l, c);
        self.anchor = self.pos(al, ac);
        self.undo.clear();
        self.redo.clear();
        self.typing_at = None;
        self.goal = None;
        self.edits.push(Edit::Reset);
        self.bump();
        self.mark_saved();
    }

    fn restore(&mut self, from_undo: bool) -> Key {
        let (from, to) = if from_undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        let Some(s) = from.pop() else { return Key::Ignored };
        to.push(Snap { rope: self.rope.clone(), cursor: self.cursor, anchor: self.anchor, version: self.version });
        self.rope = s.rope;
        self.cursor = s.cursor.min(self.rope.len_chars());
        self.anchor = s.anchor.min(self.rope.len_chars());
        self.version = s.version;
        self.typing_at = None;
        self.goal = None;
        self.edits.push(Edit::Reset);
        Key::Changed
    }

    fn vertical(&mut self, lines: isize, extend: bool) {
        let line = self.rope.char_to_line(self.cursor) as isize;
        let goal = self.goal.unwrap_or_else(|| self.disp_col(self.cursor));
        let target = line + lines;
        let at = if target < 0 {
            0
        } else if target as usize >= self.rope.len_lines() {
            self.rope.len_chars()
        } else {
            self.at_disp_col(target as usize, goal)
        };
        self.move_to(at, extend);
        self.goal = Some(goal);
    }

    /// Enter: the line's indent, one more after an opening bracket or a trailing `:`; between a bracket pair the
    /// closing one goes on its own line.
    fn newline(&mut self) {
        let start = self.line_start(self.cursor);
        let lead: String =
            self.rope.slice(start..self.cursor).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let before = self.rope.chars_at(self.cursor).reversed().take(self.cursor - start).find(|c| !c.is_whitespace());
        let after = self.char_at(self.selection().end);
        let unit = self.indent.unit();
        let pair = matches!((before, after), (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')')));
        let deeper = matches!(before, Some('{' | '[' | '(' | ':'));
        let first = format!("\n{lead}{}", if deeper { unit.as_str() } else { "" });
        if pair {
            self.insert(&format!("{first}\n{lead}"), false);
            self.cursor -= lead.chars().count() + 1;
            self.anchor = self.cursor;
        } else {
            self.insert(&first, false);
        }
    }

    /// Tab / Shift+Tab over every line the selection touches (one undo step); the lines stay selected.
    fn indent_lines(&mut self, out: bool) -> Key {
        let sel = self.selection();
        let first = self.rope.char_to_line(sel.start);
        let mut last = self.rope.char_to_line(sel.end);
        if last > first && sel.end == self.rope.line_to_char(last) {
            last -= 1; // a selection ending at a line start doesn't take that line
        }
        let unit = self.indent.unit();
        let redo = self.redo.clone(); // a no-op Shift+Tab mustn't lose the redo steps
        self.snapshot();
        let mut changed = false;
        for line in first..=last {
            let start = self.rope.line_to_char(line);
            if out {
                let n = match self.char_at(start) {
                    Some('\t') => 1,
                    _ => self.rope.line(line).chars().take(unit.len().max(1)).take_while(|c| *c == ' ').count(),
                };
                if n > 0 {
                    self.remove(start..start + n);
                    changed = true;
                }
            } else if self.line_len_bytes(line) > 0 {
                self.ins(start, &unit);
                changed = true;
            }
        }
        if !changed {
            self.undo.pop();
            self.redo = redo;
            return Key::Ignored;
        }
        let end = self.line_end(self.rope.line_to_char(last));
        self.anchor = self.rope.line_to_char(first);
        self.cursor = end;
        self.typing_at = None;
        self.goal = None;
        self.bump();
        Key::Changed
    }

    /// A key press: gpui's key name, its typed text, Ctrl / Shift; `page` = lines per PageUp/PageDown.
    pub fn key(&mut self, key: &str, ch: Option<&str>, ctrl: bool, shift: bool, page: usize) -> Key {
        let sel = self.selection();
        let moved = |s: &mut Self, at: usize| {
            s.move_to(at, shift);
            Key::Moved
        };
        let end = self.rope.len_chars();
        match (key, ctrl) {
            ("left", false) if !shift && !sel.is_empty() => moved(self, sel.start),
            ("right", false) if !shift && !sel.is_empty() => moved(self, sel.end),
            ("left", false) => moved(self, self.cursor.saturating_sub(1)),
            ("right", false) => moved(self, (self.cursor + 1).min(end)),
            ("left", true) => moved(self, self.word_left(self.cursor)),
            ("right", true) => moved(self, self.word_right(self.cursor)),
            ("up" | "down" | "pageup" | "pagedown", false) => {
                let n = if key.starts_with("page") { page.max(1) as isize } else { 1 };
                self.vertical(if key.ends_with("up") { -n } else { n }, shift);
                Key::Moved
            }
            ("home", false) => {
                let start = self.line_start(self.cursor);
                let text = self.rope.slice(start..self.line_end(self.cursor));
                let first = start + text.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                moved(self, if self.cursor == first { start } else { first })
            }
            ("end", false) => moved(self, self.line_end(self.cursor)),
            ("home", true) => moved(self, 0),
            ("end", true) => moved(self, end),
            ("a", true) => {
                self.select(0, end);
                Key::Moved
            }
            ("c", true) if !sel.is_empty() => Key::Copy(self.selected_text()),
            ("x", true) if !sel.is_empty() => {
                let cut = self.selected_text();
                self.delete(sel);
                Key::Copy(cut)
            }
            ("z", true) if !shift => self.restore(true),
            ("z", true) | ("y", true) => self.restore(false),
            ("backspace", _) => self.delete(match () {
                _ if !sel.is_empty() => sel,
                _ if ctrl => self.word_left(self.cursor)..self.cursor,
                _ => self.cursor.saturating_sub(1)..self.cursor,
            }),
            ("delete", _) => self.delete(match () {
                _ if !sel.is_empty() => sel,
                _ if ctrl => self.cursor..self.word_right(self.cursor),
                _ => self.cursor..(self.cursor + 1).min(end),
            }),
            ("enter", false) if !shift => {
                self.newline();
                Key::Changed
            }
            ("tab", false) if shift => self.indent_lines(true),
            ("tab", false) => {
                if self.rope.char_to_line(sel.start) != self.rope.char_to_line(sel.end) {
                    return self.indent_lines(false);
                }
                let unit = match self.indent {
                    Indent::Tab => "\t".to_string(),
                    Indent::Spaces(n) => " ".repeat(n - self.disp_col(sel.start) % n),
                };
                self.insert(&unit, false);
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

/// Display byte offset (tabs expanded as `git::text::display_line`) of raw byte column `raw` in `line`.
pub fn to_display(line: &str, raw: usize) -> usize {
    let mut col = 0;
    let mut disp = 0;
    for (i, ch) in line.char_indices() {
        if i >= raw {
            break;
        }
        let w = if ch == '\t' { TAB - col % TAB } else { 1 };
        col += w;
        disp += if ch == '\t' { w } else { ch.len_utf8() };
    }
    disp
}

/// Raw byte column in `line` of display byte offset `disp` (inside a tab: the nearer edge).
pub fn from_display(line: &str, disp: usize) -> usize {
    let (mut col, mut d) = (0, 0);
    for (i, ch) in line.char_indices() {
        let w = if ch == '\t' { TAB - col % TAB } else { 1 };
        let bytes = if ch == '\t' { w } else { ch.len_utf8() };
        if disp < d + bytes {
            return if ch == '\t' && disp - d > w / 2 { i + 1 } else { i };
        }
        col += w;
        d += bytes;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::new(Rope::from_str(s))
    }

    fn typed(b: &mut Buffer, s: &str) {
        for c in s.chars() {
            match c {
                '\n' => b.key("enter", None, false, false, 10),
                '\t' => b.key("tab", None, false, false, 10),
                c => b.key("x", Some(&c.to_string()), false, false, 10),
            };
        }
    }

    fn text(b: &Buffer) -> String {
        b.rope.to_string()
    }

    #[test]
    fn typing_undo_redo_and_dirty() {
        let mut b = buf("");
        typed(&mut b, "fn main");
        assert!(b.dirty());
        b.key("backspace", None, true, false, 10);
        assert_eq!(text(&b), "fn ");
        b.key("z", None, true, false, 10);
        assert_eq!(text(&b), "fn main");
        b.key("z", None, true, false, 10);
        assert_eq!(text(&b), "", "one typing run is one step");
        assert!(!b.dirty(), "undo back to the loaded text is clean");
        b.key("z", None, true, true, 10);
        assert_eq!(text(&b), "fn main");
        b.mark_saved();
        typed(&mut b, "!");
        assert!(b.dirty());
        b.key("z", None, true, false, 10);
        assert!(!b.dirty(), "undo to the saved version is clean");
        b.key("y", None, true, false, 10);
        assert_eq!(text(&b), "fn main!");
    }

    #[test]
    fn moves_words_lines_pages() {
        let mut b = buf("hello world\n\tšťáva x\nab");
        b.key("end", None, false, false, 10);
        b.key("left", None, true, true, 10);
        assert_eq!(b.selected_text(), "world");
        assert_eq!(b.key("c", None, true, false, 10), Key::Copy("world".into()));
        b.key("down", None, false, false, 10);
        assert_eq!(b.line_col(b.cursor()), (1, "\tšť".len()), "display column 6 across a tab");
        b.key("home", None, false, false, 10);
        assert_eq!(b.line_col(b.cursor()), (1, 1), "Home: first non-blank");
        b.key("home", None, false, false, 10);
        assert_eq!(b.line_col(b.cursor()), (1, 0), "again: column 0");
        b.key("right", None, true, false, 10);
        assert_eq!(b.line_col(b.cursor()), (1, "\tšťáva".len()));
        b.key("pagedown", None, false, false, 10);
        assert_eq!(b.cursor(), b.rope.len_chars());
        b.key("pageup", None, false, true, 10);
        assert_eq!(b.cursor(), 0);
        assert_eq!(b.selection(), 0..b.rope.len_chars());
        b.key("x", Some("Z"), false, false, 10);
        assert_eq!(text(&b), "Z");
    }

    #[test]
    fn mouse_word_and_line() {
        let mut b = buf("let foo_bar = 1;\nnext");
        b.select_word(6);
        assert_eq!(b.selected_text(), "foo_bar");
        b.select_line(2);
        assert_eq!(b.selected_text(), "let foo_bar = 1;\n");
        b.click(4, false);
        b.click(7, true);
        assert_eq!(b.selected_text(), "foo");
    }

    #[test]
    fn cut_paste_and_crlf_paste() {
        let mut b = buf("a b c");
        b.select(2, 3);
        assert_eq!(b.key("x", None, true, false, 10), Key::Copy("b".into()));
        assert_eq!(text(&b), "a  c");
        b.paste("x\r\ny");
        assert_eq!(text(&b), "a x\ny c");
        b.key("z", None, true, false, 10);
        assert_eq!(text(&b), "a  c");
    }

    #[test]
    fn auto_indent() {
        let mut b = buf("    if x {}");
        b.click(10, false);
        typed(&mut b, "\n");
        assert_eq!(text(&b), "    if x {\n        \n    }");
        typed(&mut b, "y");
        assert_eq!(b.line(1), "        y");
        let mut b = buf("\tdef f():");
        b.indent = Indent::Tab;
        b.key("end", None, false, false, 10);
        typed(&mut b, "\n");
        assert_eq!(text(&b), "\tdef f():\n\t\t");
        let mut b = buf("  plain");
        b.key("end", None, false, false, 10);
        typed(&mut b, "\n");
        assert_eq!(text(&b), "  plain\n  ");
    }

    #[test]
    fn tab_and_shift_tab() {
        let mut b = buf("a\nb\n\nc");
        assert_eq!(b.indent, Indent::Spaces(4));
        b.click(1, false);
        typed(&mut b, "\t");
        assert_eq!(b.line(0), "a   ", "to the next tab stop");
        b.select(0, b.rope.len_chars());
        b.key("tab", None, false, false, 10);
        assert_eq!(text(&b), "    a   \n    b\n\n    c", "empty lines stay empty");
        b.key("tab", None, false, true, 10);
        assert_eq!(text(&b), "a   \nb\n\nc");
        b.key("z", None, true, false, 10);
        assert_eq!(text(&b), "    a   \n    b\n\n    c", "one undo step");
        assert_eq!(Indent::detect(&Rope::from_str("x\n\ty\n\tz\n  w")), Indent::Tab);
        assert_eq!(Indent::detect(&Rope::from_str("x\n  y\n    z\n /* c\n   * d")), Indent::Spaces(2));
    }

    #[test]
    fn reload_keeps_cursor() {
        let mut b = buf("one\ntwo\nthree");
        b.click(b.pos(1, 2), false);
        b.reload(Rope::from_str("zero\none\ntwo\nthree"));
        assert_eq!(b.line_col(b.cursor()), (1, 2), "same line/column");
        assert!(!b.dirty(), "the reloaded text is what's on disk");
        b.click(b.pos(9, 9), false);
        b.reload(Rope::from_str("x"));
        assert_eq!(b.cursor(), 1, "clamped to the shorter text");
    }

    #[test]
    fn reload_clears_undo() {
        let mut b = buf("mine");
        b.key("end", None, false, false, 10);
        typed(&mut b, " edit");
        b.mark_saved();
        b.key("z", None, true, false, 10);
        b.key("y", None, true, false, 10); // something on both stacks
        b.reload(Rope::from_str("agent's version"));
        assert_eq!(b.key("z", None, true, false, 10), Key::Ignored, "Ctrl+Z never crosses a reload");
        assert_eq!(b.key("y", None, true, false, 10), Key::Ignored);
        assert_eq!(text(&b), "agent's version");
        assert!(!b.dirty());
        typed(&mut b, "!");
        b.key("z", None, true, false, 10);
        assert_eq!(text(&b), "agent's version", "undo stops at the reload");
        assert!(!b.dirty());
    }

    #[test]
    fn save_after_external_change_without_reload_conflicts() {
        let d = std::env::temp_dir().join(format!("dirigent-editor-save-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.txt");
        std::fs::write(&f, "loaded\n").unwrap();
        let base = hash(b"loaded\n");
        // Unchanged on disk: written.
        assert_eq!(write_checked(&f, b"mine 1\n", base, false), Ok(Some(hash(b"mine 1\n"))));
        let base = hash(b"mine 1\n");
        // An agent writes it; no watcher event has reached the editor (no reload): Ctrl+S must not overwrite.
        std::fs::write(&f, "agent\n").unwrap();
        assert_eq!(write_checked(&f, b"mine 2\n", base, false), Ok(None), "conflict");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "agent\n", "nothing written");
        // Overwrite (confirmed in the banner) writes.
        assert_eq!(write_checked(&f, b"mine 2\n", base, true), Ok(Some(hash(b"mine 2\n"))));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "mine 2\n");
        // Deleted meanwhile: Save recreates it.
        std::fs::remove_file(&f).unwrap();
        assert_eq!(write_checked(&f, b"back\n", base, false), Ok(Some(hash(b"back\n"))));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn display_columns() {
        let l = "\tab\tč";
        assert_eq!(git::text::display_line(l, 0), "    ab  č");
        assert_eq!(to_display(l, 1), 4);
        assert_eq!(to_display(l, 4), 8);
        assert_eq!(to_display(l, l.len()), 8 + 'č'.len_utf8());
        assert_eq!(from_display(l, 1), 0, "near the tab's start");
        assert_eq!(from_display(l, 3), 1, "near its end");
        assert_eq!(from_display(l, 5), 2);
        assert_eq!(from_display(l, 99), l.len());
    }

    #[test]
    fn load_save_round_trip() {
        for bytes in [&b"a\nb\n"[..], b"a\r\nb\r\n", b"no newline", "\u{feff}bom\n".as_bytes(), b""] {
            let (rope, f) = load(bytes).unwrap();
            assert!(!rope.to_string().contains('\r'));
            assert_eq!(save_bytes(&rope, f), bytes);
        }
        assert_eq!(load(b"a\0b").unwrap_err(), ReadOnly::Binary);
        assert_eq!(load(b"\xff\xfe").unwrap_err(), ReadOnly::NotUtf8);
        assert_eq!(load(b"a\r\nb\n").unwrap_err(), ReadOnly::MixedEol);
        assert_eq!(load(b"a\rb").unwrap_err(), ReadOnly::MixedEol);
        assert_eq!(load("x".repeat(MAX_LINE_CHARS + 1).as_bytes()).unwrap_err(), ReadOnly::LongLines);
        assert!(load("č".repeat(MAX_LINE_CHARS).as_bytes()).is_ok(), "the limit counts chars");
        assert_eq!(load(&vec![b'a'; MAX_BYTES + 1]).unwrap_err(), ReadOnly::TooLarge);
        assert_eq!(load("\n".repeat(MAX_LINES).as_bytes()).unwrap_err(), ReadOnly::TooLarge);
    }

    #[test]
    fn disk_change_and_save_decisions() {
        assert_eq!(on_disk_change(false, 1, 1), DiskChange::Ignore, "our own save");
        assert_eq!(on_disk_change(true, 1, 1), DiskChange::Ignore);
        assert_eq!(on_disk_change(false, 2, 1), DiskChange::Reload);
        assert_eq!(on_disk_change(true, 2, 1), DiskChange::Banner, "never overwrite my edits");
        assert_eq!(save_check(1, Some(1)), Save::Write);
        assert_eq!(save_check(1, Some(2)), Save::Conflict);
        assert_eq!(save_check(1, None), Save::Write, "deleted: recreate");
    }

    #[test]
    fn spans_follow_edits() {
        let k = |s: &[(u32, u32)]| s.iter().map(|&(a, b)| (a..b, 1u8)).collect::<Spans>();
        // "let foo = 1;" with `foo` colored; type "d" at 7 (end of foo) → the span grows.
        let mut b = buf("let foo = 1;\nx");
        let mut c = vec![k(&[(4, 7), (10, 11)]), k(&[(0, 1)])];
        b.click(7, false);
        typed(&mut b, "d");
        for e in b.take_edits() {
            shift_spans(&mut c, &e);
        }
        assert_eq!(c, vec![k(&[(4, 8), (11, 12)]), k(&[(0, 1)])]);
        // Enter after "food": the rest moves to a new line, the next line keeps its spans.
        typed(&mut b, "\n");
        for e in b.take_edits() {
            shift_spans(&mut c, &e);
        }
        assert_eq!(b.line(1), " = 1;");
        assert_eq!(c, vec![k(&[(4, 8)]), k(&[(3, 4)]), k(&[(0, 1)])]);
        // Join them again (backspace at line 1 start: the auto-indent was nothing here).
        b.key("backspace", None, false, false, 10);
        for e in b.take_edits() {
            shift_spans(&mut c, &e);
        }
        assert_eq!(b.line(0), "let food = 1;");
        assert_eq!(c, vec![k(&[(4, 8), (11, 12)]), k(&[(0, 1)])]);
        // Delete "food" (a span vanishes), and a multi-line delete drops the lines in between.
        b.select(4, 8);
        b.key("delete", None, false, false, 10);
        for e in b.take_edits() {
            shift_spans(&mut c, &e);
        }
        assert_eq!(c, vec![k(&[(7, 8)]), k(&[(0, 1)])]);
        b.select(0, b.rope.len_chars());
        b.key("backspace", None, false, false, 10);
        for e in b.take_edits() {
            shift_spans(&mut c, &e);
        }
        assert_eq!(c, vec![Spans::new()]);
    }
}
