//! Find in the terminal: alacritty's regex search (wrapped lines, wide chars) over the screen and scrollback.
//! `search` walks the whole grid in short locked chunks (the reader thread interleaves); `visible` matches just the
//! viewport under the caller's lock, so highlights are exact even while output scrolls the grid.

use std::sync::atomic::{AtomicBool, Ordering};

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::RegexIter;
pub use alacritty_terminal::term::search::{Match, RegexSearch};

/// More matches than this are not collected (the count shows "10000+").
pub const CAP: usize = 10_000;
/// Lines searched per lock hold.
const CHUNK: i32 = 200;
/// A wrapped line is followed at most this many rows past a chunk's / the viewport's edge.
const MAX_WRAP: i32 = 500;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    /// Match case (off = case-insensitive, also for upper-case text: no smart case).
    pub case: bool,
    /// The text is a regex (off = literal).
    pub regex: bool,
}

impl Query {
    /// None for an empty query; Err for an invalid regex.
    pub fn build(&self) -> Result<Option<RegexSearch>, String> {
        if self.text.is_empty() {
            return Ok(None);
        }
        let body = if self.regex { self.text.clone() } else { regex_syntax::escape(&self.text) };
        // Inline flags beat alacritty's smart case (insensitive unless the text has an upper-case letter).
        let flags = if self.case { "(?-i)" } else { "(?i)" };
        RegexSearch::new(&format!("{flags}(?:{body})")).map(Some).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Found {
    /// Sorted, top (oldest) first.
    pub matches: Vec<Match>,
    pub capped: bool,
    /// Searched on the alternate screen (no scrollback there).
    pub alt: bool,
}

fn wraps<T>(t: &Term<T>, line: Line) -> bool {
    t.grid()[line][t.last_column()].flags.contains(Flags::WRAPLINE)
}

/// The last row of the logical line through `line` (bounded by `MAX_WRAP` and the grid's bottom).
fn line_end<T>(t: &Term<T>, mut line: Line) -> Line {
    let limit = (line + MAX_WRAP).min(t.bottommost_line());
    while line < limit && wraps(t, line) {
        line += 1;
    }
    line
}

/// The first row of the logical line through `line`.
fn line_start<T>(t: &Term<T>, mut line: Line) -> Line {
    let limit = (line - MAX_WRAP).max(t.topmost_line());
    while line > limit && wraps(t, line - 1) {
        line -= 1;
    }
    line
}

/// Every match in the screen and scrollback, top first. None when `cancel` was set. Matches that straddle a chunk
/// edge are found whole: chunks end where a logical line ends.
pub fn search<T>(term: &FairMutex<Term<T>>, re: &mut RegexSearch, cancel: &AtomicBool) -> Option<Found> {
    let mut found = Found::default();
    let mut line = term.lock().topmost_line();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let t = term.lock();
        found.alt = t.mode().contains(TermMode::ALT_SCREEN);
        line = line.max(t.topmost_line());
        let bottom = t.bottommost_line();
        if line > bottom {
            return Some(found);
        }
        let end = line_end(&t, (line + (CHUNK - 1)).min(bottom));
        let start = Point::new(line, Column(0));
        for m in RegexIter::new(start, Point::new(end, t.last_column()), Direction::Right, &t, re) {
            // A match can't start before the chunk, but the iterator's last search may wrap past its end.
            if *m.start() < start || m.end().line > end {
                break;
            }
            if found.matches.len() == CAP {
                found.capped = true;
                return Some(found);
            }
            found.matches.push(m);
        }
        line = end + 1;
    }
}

/// The matches touching the viewport (lines partly scrolled out included), top first.
pub fn visible<T>(t: &Term<T>, re: &mut RegexSearch) -> Vec<Match> {
    let top = Line(-(t.grid().display_offset() as i32));
    let bottom = top + (t.screen_lines() as i32 - 1);
    let start = Point::new(line_start(t, top), Column(0));
    let end = Point::new(line_end(t, bottom), t.last_column());
    RegexIter::new(start, end, Direction::Right, t, re)
        .take_while(|m| *m.start() >= start && *m.end() <= end)
        .take(CAP)
        .collect()
}

/// Scrolls so `m` is visible: centred when it's outside the viewport, nothing when it's already shown.
pub fn scroll_to<T: EventListener>(t: &mut Term<T>, m: &Match) {
    let offset = t.grid().display_offset() as i32;
    let rows = t.screen_lines() as i32;
    let (first, last) = (m.start().line.0, m.end().line.0);
    if first >= -offset && last < rows - offset {
        return;
    }
    let want = (rows / 2 - first).clamp(0, t.history_size() as i32);
    t.scroll_display(Scroll::Delta(want - offset));
}

/// The first current match: the last one starting at or above the viewport's bottom row, else the last one.
pub fn initial<T>(t: &Term<T>, matches: &[Match]) -> Option<usize> {
    let bottom = Point::new(Line(t.screen_lines() as i32 - 1 - t.grid().display_offset() as i32), t.last_column());
    match matches.partition_point(|m| *m.start() <= bottom) {
        0 => matches.len().checked_sub(1),
        n => Some(n - 1),
    }
}

/// After a re-search: the match nearest to the old current one's start.
pub fn nearest(matches: &[Match], old: Point) -> Option<usize> {
    let i = matches.partition_point(|m| *m.start() < old);
    let dist = |m: &Match| (m.start().line.0 - old.line.0).abs();
    match (i.checked_sub(1), (i < matches.len()).then_some(i)) {
        (Some(a), Some(b)) => Some(if dist(&matches[b]) <= dist(&matches[a]) { b } else { a }),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::tests::term;
    use alacritty_terminal::event::VoidListener;

    fn q(text: &str, case: bool, regex: bool) -> RegexSearch {
        Query { text: text.into(), case, regex }.build().unwrap().unwrap()
    }

    fn all(t: Term<VoidListener>, re: &mut RegexSearch) -> Vec<Match> {
        search(&FairMutex::new(t), re, &AtomicBool::new(false)).unwrap().matches
    }

    fn p(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    #[test]
    fn literal_case_and_regex() {
        let text = "Foo foo a.c abc FOO\r\n";
        assert_eq!(all(term(40, 3, text), &mut q("foo", false, false)).len(), 3);
        // Upper case in the query doesn't switch to case-sensitive (no smart case) ...
        assert_eq!(all(term(40, 3, text), &mut q("FOO", false, false)).len(), 3);
        // ... only the toggle does.
        assert_eq!(all(term(40, 3, text), &mut q("Foo", true, false)), [p(0, 0)..=p(0, 2)]);
        // Literal: the dot is a dot.
        assert_eq!(all(term(40, 3, text), &mut q("a.c", false, false)), [p(0, 8)..=p(0, 10)]);
        assert_eq!(all(term(40, 3, text), &mut q("a.c", false, true)).len(), 2);
        assert!(Query { text: "(".into(), case: false, regex: true }.build().is_err());
        assert!(Query::default().build().unwrap().is_none());
    }

    #[test]
    fn wrapped_lines_and_wide_chars() {
        // 10 columns: "needle" wraps from row 0 into row 1.
        let m = all(term(10, 3, "abcdeneedle xx\r\n"), &mut q("needle", false, false));
        assert_eq!(m, [p(0, 5)..=p(1, 0)]);
        let m = all(term(20, 2, "ab日本cd 日本"), &mut q("本c", false, false));
        assert_eq!(m, [p(0, 4)..=p(0, 6)]);
    }

    #[test]
    fn scrollback_in_chunks_and_viewport() {
        // 700 lines through a 5-row screen: most are history and span several chunks.
        let lines: String = (0..700).map(|i| format!("line {i}\r\n")).collect();
        let t = term(20, 5, &lines);
        let mut re = q("line 1", false, false);
        let m = all(term(20, 5, &lines), &mut re);
        // "line 1", "line 10".."line 19", "line 100".."line 199".
        assert_eq!(m.len(), 1 + 10 + 100);
        assert!(m.windows(2).all(|w| w[0].start() < w[1].start()), "sorted, no duplicates at chunk edges");
        // The viewport holds lines 696..699 plus the empty prompt row: no "line 1" there.
        assert!(visible(&t, &mut re).is_empty());
        let mut re = q("line 69", false, false);
        assert_eq!(visible(&t, &mut re).len(), 4);
    }

    #[test]
    fn a_match_wrapping_across_a_chunk_edge_is_found_once() {
        // Row 199 of the history ends mid-word; the word continues on row 200.
        let mut text: String = (0..199).map(|_| "x\r\n").collect();
        text.push_str("aaaaaaaaaaaaaaaaaneedle\r\n");
        text.push_str(&"y\r\n".repeat(50));
        let m = all(term(20, 5, &text), &mut q("needle", false, false));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start().column, Column(17));
        assert_eq!(m[0].end().column, Column(2));
    }

    #[test]
    fn cap_and_cancel() {
        let text = "ab ".repeat(4000);
        let f = search(&FairMutex::new(term(80, 200, &text)), &mut q("ab", false, false), &AtomicBool::new(false));
        let f = f.unwrap();
        assert!(!f.capped && f.matches.len() == 4000);
        let text = "a".repeat(12_000);
        let f = search(&FairMutex::new(term(100, 200, &text)), &mut q("a", false, false), &AtomicBool::new(false));
        assert!(f.unwrap().capped);
        assert!(search(&FairMutex::new(term(10, 2, "a")), &mut q("a", false, false), &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn alt_screen_searches_the_screen_only() {
        let lines: String = (0..20).map(|i| format!("old {i}\r\n")).collect();
        let f = search(
            &FairMutex::new(term(20, 3, &format!("{lines}\x1b[?1049hnew old\r\n"))),
            &mut q("old", false, false),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(f.alt);
        assert_eq!(f.matches.len(), 1, "{:?}", f.matches);
        assert_eq!(f.matches[0].start().column, Column(4));
    }

    #[test]
    fn scroll_to_centres_only_when_hidden() {
        let lines: String = (0..100).map(|i| format!("line {i}\r\n")).collect();
        let mut t = term(20, 10, &lines);
        let m = p(-50, 0)..=p(-50, 3);
        scroll_to(&mut t, &m);
        // Row = line + offset = the middle row.
        assert_eq!(t.grid().display_offset() as i32 - 50, 5);
        let near = p(-48, 0)..=p(-48, 3);
        scroll_to(&mut t, &near);
        assert_eq!(t.grid().display_offset(), 55, "already visible: no scroll");
        // Near the top of the history the offset clamps.
        scroll_to(&mut t, &(p(-91, 0)..=p(-91, 1)));
        assert_eq!(t.grid().display_offset(), t.history_size());
    }

    /// `cargo test --release -p session find_bench -- --ignored --nocapture`: a 100k-line scrollback.
    #[test]
    #[ignore]
    fn find_bench() {
        let lines: String =
            (0..100_000).map(|i| format!("{i:06} cargo build --release target/debug/foo.rs\r\n")).collect();
        let size = crate::Size { cols: 120, rows: 50, cell_width: 8, cell_height: 16 };
        let config = alacritty_terminal::term::Config { scrolling_history: 100_000, ..Default::default() };
        let mut t = Term::new(config, &size, VoidListener);
        alacritty_terminal::vte::ansi::Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new()
            .advance(&mut t, lines.as_bytes());
        let term = FairMutex::new(t);
        for (text, regex) in [("release", false), ("zzz", false), ("foo\\.rs$", true), ("0123", false)] {
            let mut re = q(text, false, regex);
            let at = std::time::Instant::now();
            let f = search(&term, &mut re, &AtomicBool::new(false)).unwrap();
            let all = at.elapsed();
            let at = std::time::Instant::now();
            let v = visible(&term.lock(), &mut re);
            println!(
                "{text:>10}: {} matches{} in {all:?} (~{:?}/chunk), viewport {} in {:?}",
                f.matches.len(),
                if f.capped { "+" } else { "" },
                all / 500,
                v.len(),
                at.elapsed()
            );
        }
    }

    #[test]
    fn initial_and_nearest() {
        let lines: String = (0..30).map(|i| format!("l{i}\r\n")).collect();
        let mut t = term(10, 5, &lines);
        let ms = [p(-20, 0)..=p(-20, 0), p(-10, 0)..=p(-10, 0), p(2, 0)..=p(2, 0)];
        assert_eq!(initial(&t, &ms), Some(2), "at the bottom: the last match on screen");
        t.scroll_display(Scroll::Delta(12)); // rows show lines -12..=-8
        assert_eq!(initial(&t, &ms), Some(1), "the last one at or above the viewport's bottom");
        assert_eq!(initial(&t, &ms[2..]), Some(0), "none above: the last one");
        assert_eq!(initial(&t, &[]), None);
        assert_eq!(nearest(&ms, p(-12, 0)), Some(1));
        assert_eq!(nearest(&ms, p(-18, 0)), Some(0));
        assert_eq!(nearest(&ms, p(9, 0)), Some(2));
        assert_eq!(nearest(&[], p(0, 0)), None);
    }
}
