//! Links under the pointer: OSC 8 hyperlinks, plain URLs, and file paths like `src/main.rs:42`. Scans only the
//! (wrapped) line under the point, so a lookup costs microseconds; no regex.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Point;
use alacritty_terminal::term::cell::Flags;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An http(s) / mailto / file URL (OSC 8 or typed out).
    Url(String),
    /// A file path as printed (relative to the shell's directory, `~/…` or absolute), with an optional line/column.
    Path { path: String, line: Option<u32>, col: Option<u32> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// First and last cell (inclusive, grid points).
    pub start: Point,
    pub end: Point,
    pub target: Target,
}

/// Longest line we look at (characters).
const MAX_LINE: usize = 4096;

/// The link under `point`: an OSC 8 hyperlink, else a URL, else (only with `paths`) a file path.
pub fn at<T>(term: &Term<T>, point: Point, paths: bool) -> Option<Link> {
    let grid = term.grid();
    if point.line < term.topmost_line() || point.line > term.bottommost_line() {
        return None;
    }
    // The (wrapped) line as chars, each with its cell.
    let (first, last) = (term.line_search_left(point), term.line_search_right(point));
    let (mut text, mut cells) = (Vec::new(), Vec::new());
    let mut p = first;
    let cols = term.columns();
    while p <= last && text.len() < MAX_LINE {
        let cell = &grid[p];
        if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            text.push(cell.c);
            cells.push(p);
        }
        p = if p.column.0 + 1 < cols { Point::new(p.line, p.column + 1) } else { Point::new(p.line + 1, Default::default()) };
    }
    // A wide char's spacer belongs to the char before it.
    let k = cells.iter().rposition(|c| *c <= point)?;

    if let Some(link) = grid[cells[k]].hyperlink() {
        let same = |i: &usize| grid[cells[*i]].hyperlink().as_ref() == Some(&link);
        let s = (0..=k).rev().take_while(same).last().unwrap_or(k);
        let e = (k..cells.len()).take_while(same).last().unwrap_or(k);
        return Some(Link { start: cells[s], end: cells[e], target: Target::Url(link.uri().to_string()) });
    }
    let (s, e, target) = find_url(&text, k).or_else(|| if paths { find_path(&text, k) } else { None })?;
    Some(Link { start: cells[s], end: cells[e], target })
}

/// The run of non-separator chars around `k` (inclusive bounds), or None on a separator.
fn token(text: &[char], k: usize) -> Option<(usize, usize)> {
    let sep = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '│' | '|');
    if sep(*text.get(k)?) {
        return None;
    }
    let s = (0..=k).rev().take_while(|&i| !sep(text[i])).last()?;
    let e = (k..text.len()).take_while(|&i| !sep(text[i])).last()?;
    Some((s, e))
}

/// Drops trailing punctuation and closing brackets that have no opening one inside.
fn trim_end(text: &[char], s: usize, mut e: usize) -> usize {
    while e > s {
        let c = text[e];
        let unbalanced = |open: char, close: char| {
            c == close && text[s..=e].iter().filter(|&&x| x == open).count() < text[s..=e].iter().filter(|&&x| x == close).count()
        };
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?') || unbalanced('(', ')') || unbalanced('[', ']') || unbalanced('{', '}') {
            e -= 1;
        } else {
            break;
        }
    }
    e
}

/// A URL (http, https, file, mailto) containing char `k`: (start, end inclusive, target).
pub fn find_url(text: &[char], k: usize) -> Option<(usize, usize, Target)> {
    let (ts, te) = token(text, k)?;
    let tok: String = text[ts..=te].iter().collect();
    // The last scheme starting at or before `k`.
    let start = ["https://", "http://", "file://", "mailto:"]
        .iter()
        .flat_map(|scheme| tok.match_indices(scheme).map(|(b, _)| tok[..b].chars().count() + ts))
        .filter(|&s| s <= k)
        .max()?;
    let e = trim_end(text, start, te);
    let url: String = text[start..=e].iter().collect();
    let scheme_len = url.find(':')? + if url.starts_with("mailto:") { 1 } else { 3 };
    (k <= e && url.len() > scheme_len).then_some((start, e, Target::Url(url)))
}

/// A file path containing char `k`: `src/main.rs:42`, `./a/b.rs:3:7`, `~/x.md`, `/abs/p.rs`, `name.ext:12`.
/// Needs a `/` plus an extension, a line number or an explicit `/ ~/ ./ ../` start, or an extension plus a line
/// number, so prose (`and/or`, `e.g.`, `1.5`, `v0.2.2`) doesn't qualify.
pub fn find_path(text: &[char], k: usize) -> Option<(usize, usize, Target)> {
    let (mut s, te) = token(text, k)?;
    while s < te && matches!(text[s], '(' | '[' | '{') {
        s += 1;
    }
    let e = trim_end(text, s, te);
    if k < s || k > e {
        return None;
    }
    let tok: String = text[s..=e].iter().collect();
    if tok.contains("://") {
        return None;
    }
    // Up to two trailing `:<digits>`: line, then column.
    let mut nums = Vec::new();
    let mut path = tok.as_str();
    while nums.len() < 2
        && let Some((head, n)) = path.rsplit_once(':')
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
    {
        nums.push(n.parse::<u32>().ok()?);
        path = head;
    }
    nums.reverse();
    let ok_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~' | '+' | '@' | '#');
    if path.is_empty() || !path.chars().all(ok_char) {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').filter(|(stem, ext)| {
        !stem.is_empty() && (1..=10).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    let rooted = ["/", "~/", "./", "../"].iter().any(|p| path.starts_with(p)) && path.len() > 2;
    let slash = path.contains('/') && !path.ends_with('/');
    let line = !nums.is_empty();
    if !((slash && (ext.is_some() || line || rooted)) || (ext.is_some() && line)) {
        return None;
    }
    let target = Target::Path { path: path.to_string(), line: nums.first().copied(), col: nums.get(1).copied() };
    Some((s, e, target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::tests::term;
    use alacritty_terminal::index::{Column, Line};

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// The URL / path found at the first char of `needle` in `line`.
    fn url(line: &str, needle: &str) -> Option<String> {
        let k = line.find(needle).map(|b| line[..b].chars().count())?;
        find_url(&chars(line), k).map(|(s, e, t)| {
            let Target::Url(u) = t else { panic!() };
            assert_eq!(u, chars(line)[s..=e].iter().collect::<String>());
            u
        })
    }

    fn path(line: &str, needle: &str) -> Option<(String, Option<u32>, Option<u32>)> {
        let k = line.find(needle).map(|b| line[..b].chars().count())?;
        find_path(&chars(line), k).map(|(_, _, t)| match t {
            Target::Path { path, line, col } => (path, line, col),
            Target::Url(_) => panic!(),
        })
    }

    #[test]
    fn urls() {
        assert_eq!(url("see https://github.com/a/b/pull/1.", "github").unwrap(), "https://github.com/a/b/pull/1");
        assert_eq!(url("(https://example.com/x)", "example").unwrap(), "https://example.com/x");
        assert_eq!(url("https://en.wikipedia.org/wiki/Rust_(language), ok", "wiki").unwrap(), "https://en.wikipedia.org/wiki/Rust_(language)");
        assert_eq!(url("\"http://a.b/c?d=1&e=2\"", "a.b").unwrap(), "http://a.b/c?d=1&e=2");
        assert_eq!(url("mail mailto:me@x.org!", "me@").unwrap(), "mailto:me@x.org");
        assert_eq!(url("plain words here", "words"), None);
        assert_eq!(url("https:// nothing", "https"), None);
        // The pointer before the scheme in the same token is not on the URL.
        assert_eq!(url("x=https://a.b", "x="), None);
    }

    #[test]
    fn paths() {
        let p = |s: &str, n: &str| path(s, n);
        assert_eq!(p("error at src/main.rs:42", "src"), Some(("src/main.rs".into(), Some(42), None)));
        assert_eq!(p("  ./a/b.rs:3:7: warning", "a/b"), Some(("./a/b.rs".into(), Some(3), Some(7))));
        assert_eq!(p("edit ~/x.md.", "~/x"), Some(("~/x.md".into(), None, None)));
        assert_eq!(p("(/abs/p.rs)", "abs"), Some(("/abs/p.rs".into(), None, None)));
        assert_eq!(p("main.rs:10", "main"), Some(("main.rs".into(), Some(10), None)));
        assert_eq!(p("Cargo.toml", "Cargo"), None, "a bare name without a line");
        for prose in ["version 1.5 out", "v0.2.2", "e.g. this", "and/or that", "at a/b.", "x: 12"] {
            let first = prose.split(' ').find(|w| w.chars().any(|c| c.is_alphanumeric() || c == '/')).unwrap();
            assert_eq!(p(prose, first), None, "{prose}");
        }
        assert_eq!(p("https://a.b/c.rs", "a.b"), None, "URLs are not paths");
    }

    #[test]
    fn links_on_the_grid() {
        // A URL wrapping from row 0 onto row 1 (12 columns).
        let mut t = term(12, 3, "go https://ab.cd/ef ok");
        let link = at(&t, Point::new(Line(1), Column(2)), false).unwrap();
        assert_eq!(link.target, Target::Url("https://ab.cd/ef".into()));
        assert_eq!((link.start, link.end), (Point::new(Line(0), Column(3)), Point::new(Line(1), Column(6))));
        // Paths only when asked.
        t = term(40, 2, "see src/lib.rs:7 now");
        let at_path = Point::new(Line(0), Column(6));
        assert_eq!(at(&t, at_path, false), None);
        assert_eq!(at(&t, at_path, true).unwrap().target, Target::Path { path: "src/lib.rs".into(), line: Some(7), col: None });
        // OSC 8: the visible text differs from the target; the whole link run is one link.
        t = term(40, 2, "PR \x1b]8;;https://github.com/o/r/pull/1\x1b\\#1 link\x1b]8;;\x1b\\ end");
        let link = at(&t, Point::new(Line(0), Column(5)), false).unwrap();
        assert_eq!(link.target, Target::Url("https://github.com/o/r/pull/1".into()));
        assert_eq!((link.start.column.0, link.end.column.0), (3, 9));
        assert_eq!(at(&t, Point::new(Line(0), Column(12)), false), None);
    }
}
