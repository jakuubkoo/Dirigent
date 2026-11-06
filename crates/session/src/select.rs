//! Text selection on the terminal grid (alacritty's `Selection`, kept in `Term::selection`, which output scrolling
//! rotates). Word and line selections expand through alacritty's semantic / wrapped-line search.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};

/// The grid point under viewport cell (`col`, `row`) (0 = top visible row), clamped into the grid.
pub fn grid_point<T>(term: &Term<T>, col: usize, row: usize) -> Point {
    let row = row.min(term.screen_lines().saturating_sub(1));
    let col = col.min(term.columns().saturating_sub(1));
    Point::new(Line(row as i32 - term.grid().display_offset() as i32), Column(col))
}

/// A new selection at `point`: 1 click = characters, 2 = a word, 3 = the (wrapped) line.
pub fn start<T>(term: &mut Term<T>, clicks: usize, point: Point, side: Side) {
    let ty = match clicks {
        0 | 1 => SelectionType::Simple,
        2 => SelectionType::Semantic,
        _ => SelectionType::Lines,
    };
    term.selection = Some(Selection::new(ty, point, side));
}

/// Moves the selection's free end (drag, Shift+click).
pub fn extend<T>(term: &mut Term<T>, point: Point, side: Side) -> bool {
    let Some(sel) = term.selection.as_mut() else { return false };
    sel.update(point, side);
    true
}

/// Mouse up: a click that selected nothing clears it. Returns whether something is selected.
pub fn finish<T>(term: &mut Term<T>) -> bool {
    if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
        term.selection = None;
    }
    term.selection.is_some()
}

/// Everything: scrollback and screen.
pub fn select_all<T>(term: &mut Term<T>) {
    let mut sel = Selection::new(SelectionType::Lines, Point::new(term.topmost_line(), Column(0)), Side::Left);
    sel.update(Point::new(term.bottommost_line(), term.last_column()), Side::Right);
    term.selection = Some(sel);
}

/// The selected text (None when nothing is selected).
pub fn text<T>(term: &Term<T>) -> Option<String> {
    term.selection_to_string().filter(|s| !s.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    use crate::Size;

    /// A term of `cols` × `rows` with `bytes` fed through the parser.
    pub(crate) fn term(cols: u16, rows: u16, bytes: &str) -> Term<VoidListener> {
        let size = Size { cols, rows, cell_width: 8, cell_height: 16 };
        let mut t = Term::new(Config::default(), &size, VoidListener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut t, bytes.as_bytes());
        t
    }

    fn word_at(t: &mut Term<VoidListener>, col: usize, row: usize) -> String {
        let p = grid_point(t, col, row);
        start(t, 2, p, Side::Left);
        text(t).unwrap()
    }

    #[test]
    fn drag_selects_characters_and_a_click_selects_nothing() {
        let mut t = term(20, 3, "hello world\r\nsecond");
        let p = grid_point(&t, 0, 0);
        start(&mut t, 1, p, Side::Left);
        assert!(!finish(&mut t), "a plain click leaves nothing selected");
        let p = grid_point(&t, 6, 0);
        start(&mut t, 1, p, Side::Left);
        let p = grid_point(&t, 2, 1);
        extend(&mut t, p, Side::Right);
        assert!(finish(&mut t));
        assert_eq!(text(&t).unwrap(), "world\nsec");
        // Shift+click moves the same end further.
        let p = grid_point(&t, 5, 1);
        extend(&mut t, p, Side::Right);
        assert_eq!(text(&t).unwrap(), "world\nsecond");
    }

    #[test]
    fn double_click_word_boundaries() {
        let mut t = term(60, 2, "see src/main.rs:42 and foo-bar \"quoted\" (paren)");
        // Paths stay whole up to the colon (':' separates words, like in alacritty).
        assert_eq!(word_at(&mut t, 6, 0), "src/main.rs");
        assert_eq!(word_at(&mut t, 24, 0), "foo-bar");
        assert_eq!(word_at(&mut t, 33, 0), "quoted");
        assert_eq!(word_at(&mut t, 42, 0), "paren");
    }

    #[test]
    fn triple_click_takes_the_wrapped_line() {
        // 10 columns: the first line wraps onto row 1.
        let mut t = term(10, 4, "0123456789abcde\r\nnext");
        let p = grid_point(&t, 3, 1);
        start(&mut t, 3, p, Side::Left);
        assert_eq!(text(&t).unwrap(), "0123456789abcde\n", "one logical line, with its line break");
    }

    #[test]
    fn selection_in_scrollback_and_select_all() {
        let lines: String = (0..10).map(|i| format!("line{i}\r\n")).collect();
        let mut t = term(10, 3, &lines);
        t.scroll_display(Scroll::Delta(4));
        // Viewport rows now show history.
        let shown: Vec<String> = (0..3).map(|r| {
            let p = grid_point(&t, 0, r);
            start(&mut t, 2, p, Side::Left);
            text(&t).unwrap()
        }).collect();
        assert_eq!(shown, ["line4", "line5", "line6"]);
        select_all(&mut t);
        let all = text(&t).unwrap();
        assert!(all.starts_with("line0\n") && all.contains("line9"), "{all:?}");
    }

    #[test]
    fn wide_characters() {
        let mut t = term(20, 2, "ab日本cd");
        let p = grid_point(&t, 1, 0);
        start(&mut t, 1, p, Side::Left);
        let p = grid_point(&t, 6, 0);
        extend(&mut t, p, Side::Left);
        assert_eq!(text(&t).unwrap(), "b日本");
    }
}
