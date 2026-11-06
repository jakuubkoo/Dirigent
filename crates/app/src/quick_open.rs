//! The Command Center's pure parts: what the typed prefix asks for, the `:line` suffix, and fuzzy file matching
//! over a project's index (our matcher, on precomputed lowercase bytes).

use git::index::Entry;

use crate::projects::fuzzy_core;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// No prefix: file names.
    Files,
    /// `>`: commands (the shortcut table).
    Commands,
    /// `#`: text inside files (results in the Search panel).
    Content,
    /// `:`: a line in the open preview.
    Line,
}

/// The mode and the rest of the input (leading spaces after a prefix dropped).
pub fn parse(input: &str) -> (Mode, &str) {
    let (mode, rest) = match input.chars().next() {
        Some('>') => (Mode::Commands, &input[1..]),
        Some('#') => (Mode::Content, &input[1..]),
        Some(':') => (Mode::Line, &input[1..]),
        _ => return (Mode::Files, input),
    };
    (mode, rest.trim_start())
}

/// `main.rs:42` → (`main.rs`, Some(42)); `main.rs:42:7` keeps the line only.
pub fn split_line(q: &str) -> (&str, Option<u32>) {
    let mut name = q;
    let mut nums = Vec::new();
    while nums.len() < 2
        && let Some((head, n)) = name.rsplit_once(':')
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
    {
        nums.push(n.parse::<u32>().ok());
        name = head;
    }
    (name, nums.last().copied().flatten())
}

/// One shown file: index into the list, score, matched byte offsets in `rel`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub ix: usize,
    pub score: i32,
    pub hl: Vec<usize>,
}

fn starts(t: &[u8], i: usize) -> bool {
    i == 0
        || matches!(t[i - 1], b'/' | b'-' | b'_' | b'.' | b' ')
        || (t[i - 1].is_ascii_lowercase() && t[i].is_ascii_uppercase())
}

/// One file against the query (lowercase bytes, no spaces): the name counts more than the whole path.
pub fn score(q: &[u8], e: &Entry) -> Option<(i32, Vec<usize>)> {
    // Cheap reject first: the query must be a subsequence of the path.
    let mut it = e.lower.iter();
    if !q.iter().all(|c| it.any(|x| x == c)) {
        return None;
    }
    let rel = e.rel.as_bytes();
    let name = &rel[e.name..];
    let on_name = fuzzy_core(q, &e.lower[e.name..], |i| starts(name, i))
        .map(|m| (m.score + 10, m.chars.iter().map(|c| c + e.name).collect()));
    let on_path = fuzzy_core(q, &e.lower, |i| starts(rel, i)).map(|m| (m.score, m.chars));
    match (on_name, on_path) {
        (Some(a), Some(b)) => Some(if a.0 >= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// The best `limit` files for `query` (best first; ties: shorter path, then alphabetical). Empty query: `recent`
/// (relative paths, most recent first) that still exist, then the rest in order.
pub fn filter(query: &str, files: &[Entry], recent: &[String], limit: usize) -> Vec<Hit> {
    let q: Vec<u8> = query.bytes().filter(|b| !b.is_ascii_whitespace()).map(|b| b.to_ascii_lowercase()).collect();
    if q.is_empty() {
        let mut out: Vec<Hit> = recent
            .iter()
            .filter_map(|r| files.binary_search_by(|e| e.rel.as_str().cmp(r)).ok())
            .map(|ix| Hit { ix, score: 0, hl: Vec::new() })
            .take(limit)
            .collect();
        let shown: Vec<usize> = out.iter().map(|h| h.ix).collect();
        out.extend((0..files.len()).filter(|i| !shown.contains(i)).take(limit - out.len()).map(|ix| Hit {
            ix,
            score: 0,
            hl: Vec::new(),
        }));
        return out;
    }
    let mut hits: Vec<Hit> =
        files.iter().enumerate().filter_map(|(ix, e)| score(&q, e).map(|(score, hl)| Hit { ix, score, hl })).collect();
    let order = |a: &Hit, b: &Hit| {
        b.score.cmp(&a.score).then(files[a.ix].rel.len().cmp(&files[b.ix].rel.len())).then(a.ix.cmp(&b.ix))
    };
    if hits.len() > limit {
        hits.select_nth_unstable_by(limit, order);
        hits.truncate(limit);
    }
    hits.sort_unstable_by(order);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(rels: &[&str]) -> Vec<Entry> {
        let mut v: Vec<Entry> = rels.iter().map(|r| Entry::new(r.to_string())).collect();
        v.sort_by(|a, b| a.rel.cmp(&b.rel));
        v
    }

    fn top<'a>(q: &str, f: &'a [Entry]) -> Vec<&'a str> {
        filter(q, f, &[], 10).into_iter().map(|h| f[h.ix].rel.as_str()).collect()
    }

    #[test]
    fn prefixes_and_lines() {
        assert_eq!(parse("main"), (Mode::Files, "main"));
        assert_eq!(parse("> zoom"), (Mode::Commands, "zoom"));
        assert_eq!(parse("#foo bar"), (Mode::Content, "foo bar"));
        assert_eq!(parse(":42"), (Mode::Line, "42"));
        assert_eq!(split_line("main.rs:42"), ("main.rs", Some(42)));
        assert_eq!(split_line("main.rs:42:7"), ("main.rs", Some(42)));
        assert_eq!(split_line("main.rs"), ("main.rs", None));
        assert_eq!(split_line("a:b"), ("a:b", None));
    }

    #[test]
    fn names_beat_paths_and_highlights_point_into_the_path() {
        let f = files(&[
            "crates/app/src/main.rs",
            "crates/main_stuff/lib.rs",
            "docs/maintenance.md",
            "src/workspace/view.rs",
        ]);
        assert_eq!(top("main.rs", &f)[0], "crates/app/src/main.rs");
        assert_eq!(top("wsview", &f)[0], "src/workspace/view.rs");
        // Highlights are byte offsets in the relative path: the name part here.
        let h = &filter("main", &f, &[], 10)[0];
        assert_eq!(f[h.ix].rel, "crates/app/src/main.rs");
        assert_eq!(h.hl, vec![15, 16, 17, 18]);
        // Not a subsequence: nothing.
        assert!(top("zzz", &f).is_empty());
        // Spaces are ignored; case doesn't matter.
        assert_eq!(top("MAIN rs", &f)[0], "crates/app/src/main.rs");
    }

    #[test]
    fn empty_query_shows_recent_first() {
        let f = files(&["a.rs", "b.rs", "c.rs"]);
        let recent = ["c.rs".to_string(), "gone.rs".to_string(), "a.rs".to_string()];
        let shown: Vec<&str> = filter("", &f, &recent, 10).into_iter().map(|h| f[h.ix].rel.as_str()).collect();
        assert_eq!(shown, ["c.rs", "a.rs", "b.rs"]);
        assert_eq!(filter("", &f, &[], 2).len(), 2);
    }

    /// `QUICK_OPEN_BENCH=<dir> cargo test --release -p dirigent quick_open_bench -- --ignored --nocapture`: the
    /// dir's files (gitignore rules) repeated under prefixes up to 100k, then per-keystroke filter times.
    #[test]
    #[ignore]
    fn quick_open_bench() {
        let dir = std::env::var("QUICK_OPEN_BENCH").expect("QUICK_OPEN_BENCH=<dir>");
        let t = std::time::Instant::now();
        let ix = git::index::Index::build(std::path::Path::new(&dir), false, git::index::CAP);
        println!("index: {} files in {:?}", ix.files.len(), t.elapsed());
        let mut rels: Vec<String> = Vec::new();
        for copy in 0.. {
            for e in ix.files.iter() {
                rels.push(if copy == 0 { e.rel.clone() } else { format!("copy{copy}/{}", e.rel) });
            }
            if rels.len() >= 100_000 {
                break;
            }
        }
        rels.truncate(100_000);
        rels.sort();
        let files: Vec<Entry> = rels.into_iter().map(Entry::new).collect();
        for q in ["w", "wo", "wor", "work", "works", "worksp", "workspace", "workspace.rs", "edtests", "zzzz"] {
            let t = std::time::Instant::now();
            let n = filter(q, &files, &[], 100).len();
            println!("{q:>14}: {:>6.2} ms ({n} shown)", t.elapsed().as_secs_f64() * 1000.);
        }
    }
}
