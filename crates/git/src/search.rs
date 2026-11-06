//! Content search over an index's files: plain text or regex, case and whole-word options, several worker
//! threads, results per file as they are found, cancellable. Binary and big files are skipped.

use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use regex::bytes::{Regex, RegexBuilder};

use crate::index::Entry;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case: bool,
    pub word: bool,
}

impl Query {
    /// The matcher, or the regex error to show.
    pub fn compile(&self) -> Result<Regex, String> {
        let pat = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        let pat = if self.word { format!(r"\b(?:{pat})\b") } else { pat };
        RegexBuilder::new(&pat)
            .case_insensitive(!self.case)
            .multi_line(true)
            .size_limit(1 << 22)
            .build()
            .map_err(|e| e.to_string())
    }
}

/// One matching line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 1-based.
    pub line: u32,
    /// The line (cut around the match when long), and the match within it (bytes).
    pub text: String,
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHits {
    pub rel: String,
    pub hits: Vec<Hit>,
}

/// Files bigger than this are skipped.
pub const MAX_FILE: u64 = 2 << 20;
/// Stop after this many matching lines / files.
pub const MAX_HITS: usize = 10_000;
pub const MAX_FILES: usize = 2_000;
/// Shown line length (bytes, roughly): longer lines are cut around the match.
const SHOWN: usize = 300;

/// Searches `files` (relative to `root`) on `threads` threads; `found` gets each file with matches (from any
/// thread). Returns whether a cap stopped it. `cancel` stops it early.
pub fn search(
    root: &Path,
    files: &[Entry],
    re: &Regex,
    threads: usize,
    cancel: &AtomicBool,
    found: &(dyn Fn(FileHits) + Sync),
) -> bool {
    let next = AtomicUsize::new(0);
    let (hits, nfiles) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let full = AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                while !cancel.load(Ordering::Relaxed) && !full.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = files.get(i) else { break };
                    let Some(h) = search_file(&root.join(&entry.rel), re, cancel) else { continue };
                    let total = hits.fetch_add(h.len(), Ordering::Relaxed) + h.len();
                    if total >= MAX_HITS || nfiles.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_FILES {
                        full.store(true, Ordering::Relaxed);
                    }
                    found(FileHits { rel: entry.rel.clone(), hits: h });
                }
            });
        }
    });
    full.load(Ordering::Relaxed)
}

/// The matching lines of one file (None: no match, unreadable, binary or too big).
fn search_file(path: &Path, re: &Regex, cancel: &AtomicBool) -> Option<Vec<Hit>> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    if data[..data.len().min(8192)].contains(&0) {
        return None;
    }
    let mut out = Vec::new();
    let (mut line, mut counted) = (1u32, 0usize);
    let mut last_line_start = usize::MAX;
    for (n, m) in re.find_iter(&data).enumerate() {
        if n % 1000 == 999 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        line += memchr::memchr_iter(b'\n', &data[counted..m.start()]).count() as u32;
        counted = m.start();
        let start = memchr::memrchr(b'\n', &data[..m.start()]).map_or(0, |i| i + 1);
        if start == last_line_start || m.start() == m.end() {
            continue; // one row per line; empty matches (`^`, `\b`) show nothing
        }
        last_line_start = start;
        let end = memchr::memchr(b'\n', &data[m.end()..]).map_or(data.len(), |i| m.end() + i);
        out.push(cut(&data[start..end], m.start() - start..m.end().min(end) - start, line));
        if out.len() >= 200 {
            break; // per file
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The shown line: whole, or ~`SHOWN` bytes around the match with `…` marks; tabs as spaces; trailing CR dropped.
fn cut(line: &[u8], m: Range<usize>, n: u32) -> Hit {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let (mut a, mut b) = (0, line.len());
    if line.len() > SHOWN {
        a = m.start.saturating_sub(SHOWN / 3);
        b = (a + SHOWN).max(m.end).min(line.len());
    }
    let (pre, mid, post) = (&line[a..m.start], &line[m.start..m.end.min(line.len())], &line[m.end.min(line.len())..b]);
    let lossy = |s: &[u8]| String::from_utf8_lossy(s).replace('\t', "    ");
    let mut text = if a > 0 { "…".to_string() } else { String::new() };
    text.push_str(&lossy(pre));
    let start = text.len();
    text.push_str(&lossy(mid));
    let end = text.len();
    text.push_str(&lossy(post));
    if b < line.len() {
        text.push('…');
    }
    Hit { line: n, text, range: start..end }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn tmp(name: &str, files: &[(&str, &[u8])]) -> (std::path::PathBuf, Vec<Entry>) {
        let d = std::env::temp_dir().join(format!("dirigent-search-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (rel, body) in files {
            let p = d.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let entries = files.iter().map(|(rel, _)| Entry::new(rel.to_string())).collect();
        (d, entries)
    }

    fn run(d: &Path, files: &[Entry], q: Query) -> Vec<FileHits> {
        let out = Mutex::new(Vec::new());
        search(d, files, &q.compile().unwrap(), 3, &AtomicBool::new(false), &|f| out.lock().unwrap().push(f));
        let mut v = out.into_inner().unwrap();
        v.sort_by(|a, b| a.rel.cmp(&b.rel));
        v
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Default::default() }
    }

    #[test]
    fn plain_case_word_regex() {
        let (d, files) = tmp(
            "modes",
            &[
                ("a.rs", b"fn main() {\n    let foo = 1; // Foo\n}\nfoobar\n"),
                ("b/c.rs", b"no match here\nfoo.bar()\n"),
            ],
        );
        // Plain text, case-insensitive by default; one row per line; a `.` is literal.
        let r = run(&d, &files, q("foo"));
        assert_eq!(
            r.iter().map(|f| (f.rel.as_str(), f.hits.iter().map(|h| h.line).collect::<Vec<_>>())).collect::<Vec<_>>(),
            [("a.rs", vec![2, 4]), ("b/c.rs", vec![2])]
        );
        let h = &r[0].hits[0];
        assert_eq!((h.text.as_str(), &h.text[h.range.clone()]), ("    let foo = 1; // Foo", "foo"));
        assert_eq!(run(&d, &files, q("o.b")).len(), 1, "plain: the dot is literal");
        // Case sensitive.
        let r = run(&d, &files, Query { case: true, ..q("Foo") });
        assert_eq!(r[0].hits.iter().map(|h| h.line).collect::<Vec<_>>(), [2]);
        // Whole word: `foobar` doesn't count.
        let r = run(&d, &files, Query { word: true, ..q("foo") });
        assert_eq!(r[0].hits.iter().map(|h| h.line).collect::<Vec<_>>(), [2]);
        // Regex.
        let r = run(&d, &files, Query { regex: true, ..q(r"fn \w+\(") });
        assert_eq!((r.len(), r[0].hits[0].line), (1, 1));
        assert!(Query { regex: true, ..q("(") }.compile().is_err());
    }

    #[test]
    fn skips_binary_and_big_files_and_cuts_long_lines() {
        let big = vec![b'x'; MAX_FILE as usize + 1];
        let long = format!("{}needle{}\n", "a".repeat(1000), "b".repeat(1000));
        let (d, files) = tmp(
            "skip",
            &[("bin", b"needle\0\x01"), ("big", &big), ("long.txt", long.as_bytes()), ("crlf.txt", b"x\r\nneedle\r\n")],
        );
        let r = run(&d, &files, q("needle"));
        assert_eq!(r.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(), ["crlf.txt", "long.txt"]);
        assert_eq!((r[0].hits[0].line, r[0].hits[0].text.as_str()), (2, "needle"));
        let h = &r[1].hits[0];
        assert!(h.text.starts_with('…') && h.text.ends_with('…') && h.text.len() < 400, "{}", h.text.len());
        assert_eq!(&h.text[h.range.clone()], "needle");
    }

    #[test]
    fn cancel_stops_it() {
        let files: Vec<(String, Vec<u8>)> = (0..200).map(|i| (format!("f{i}.txt"), b"hit\n".to_vec())).collect();
        let refs: Vec<(&str, &[u8])> = files.iter().map(|(a, b)| (a.as_str(), b.as_slice())).collect();
        let (d, entries) = tmp("cancel", &refs);
        let cancel = AtomicBool::new(true);
        let n = AtomicUsize::new(0);
        search(&d, &entries, &q("hit").compile().unwrap(), 2, &cancel, &|_| {
            n.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(n.load(Ordering::Relaxed), 0);
    }
}
