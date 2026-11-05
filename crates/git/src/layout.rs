//! A parsed diff turned into display rows: line numbers, removed/added lines paired by similarity, word-level
//! changed ranges for each pair, and side-by-side rows. Pure and blocking: build it off the UI thread, once per
//! diff version (`hash` tells versions apart).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use imara_diff::{Algorithm, InternedInput};

use crate::{Diff, LineKind};

/// Pairs scoring below this (token Dice coefficient) are not paired; pairs whose changed bytes exceed
/// `1 - MIN_SIMILARITY` of the pair get no word highlights either.
const MIN_SIMILARITY: f32 = 0.5;
const MAX_UNCHANGED_SHARE_FOR_PLAIN: f32 = 0.4;
/// Lines longer than this (bytes or tokens) are never word-diffed.
const MAX_WORD_LINE_BYTES: usize = 1000;
const MAX_WORD_LINE_TOKENS: usize = 400;
/// Change blocks with more candidate pairs than this are paired by position instead of aligned.
const MAX_ALIGN_CELLS: usize = 4096;
/// Word-diffed pairs per file; later pairs stay plain.
const MAX_PAIRS: usize = 50_000;

#[derive(Clone, Debug, PartialEq)]
pub struct DLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// Without the +/-/space marker. Hunk rows: only git's section context (`⋯ fn main()`).
    pub text: Arc<str>,
    /// Changed byte ranges of `text` (on token boundaries); empty = plain line highlight.
    pub words: Vec<Range<u32>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitRow {
    /// A hunk header or "\ No newline at end of file": one row across both sides.
    Full(u32),
    /// Old side, new side (indices into `lines`); None = filler.
    Pair(Option<u32>, Option<u32>),
}

#[derive(Debug, Default, PartialEq)]
pub struct Layout {
    /// Unified rows, in git's order (file header dropped).
    pub lines: Vec<DLine>,
    pub split: Vec<SplitRow>,
    pub truncated: bool,
    pub max_old: u32,
    pub max_new: u32,
    pub hash: u64,
}

/// Identifies a diff version: equal hash → the cached layout still applies.
pub fn hash(diff: &Diff) -> u64 {
    let mut h = DefaultHasher::new();
    diff.truncated.hash(&mut h);
    for l in &diff.lines {
        (l.kind as u8).hash(&mut h);
        l.text.hash(&mut h);
    }
    h.finish()
}

pub fn layout(diff: &Diff) -> Layout {
    let mut out = Layout { truncated: diff.truncated, hash: hash(diff), ..Default::default() };
    let (mut old, mut new) = (0u32, 0u32);
    for l in &diff.lines {
        let (o, n, text) = match l.kind {
            LineKind::Header => continue,
            LineKind::Hunk => {
                (old, new) = hunk_starts(&l.text);
                (None, None, hunk_context(&l.text))
            }
            LineKind::Meta => (None, None, l.text.clone()),
            LineKind::Context => {
                (old, new) = (old + 1, new + 1);
                (Some(old - 1), Some(new - 1), strip_marker(&l.text))
            }
            LineKind::Del => {
                old += 1;
                (Some(old - 1), None, strip_marker(&l.text))
            }
            LineKind::Add => {
                new += 1;
                (None, Some(new - 1), strip_marker(&l.text))
            }
        };
        out.max_old = out.max_old.max(o.unwrap_or(0));
        out.max_new = out.max_new.max(n.unwrap_or(0));
        out.lines.push(DLine { kind: l.kind, old: o, new: n, text: text.into(), words: Vec::new() });
    }
    pair_blocks(&mut out);
    out
}

fn strip_marker(text: &str) -> String {
    text.get(1..).unwrap_or("").to_string()
}

/// `@@ -12,3 +14,5 @@ …` → (12, 14). A count of 0 means the start names the line *before* the (empty) range,
/// so the first real line would be start + 1; that only happens with no lines on that side, so it never shows.
fn hunk_starts(line: &str) -> (u32, u32) {
    let mut it = line.split(' ').skip(1);
    let start = |s: Option<&str>, sign: char| {
        s.and_then(|s| s.strip_prefix(sign)).and_then(|s| s.split(',').next()).and_then(|s| s.parse().ok()).unwrap_or(1)
    };
    let o = start(it.next(), '-');
    let n = start(it.next(), '+');
    (o, n)
}

/// `@@ -1,2 +1,3 @@ fn main()` → `⋯ fn main()`; just `⋯` when git found no section context.
fn hunk_context(line: &str) -> String {
    match line.splitn(3, "@@").nth(2).map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => format!("⋯ {c}"),
        None => "⋯".into(),
    }
}

/// Splits into word runs (alphanumeric/`_`), whitespace runs, and single other chars; returns byte ranges.
fn tokens(s: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq, Clone, Copy)]
    enum C {
        Word,
        Space,
        Other,
    }
    let class = |c: char| if c.is_alphanumeric() || c == '_' { C::Word } else if c.is_whitespace() { C::Space } else { C::Other };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut prev = None;
    for (i, ch) in s.char_indices() {
        let c = class(ch);
        match out.last_mut() {
            Some(r) if prev == Some(c) && c != C::Other => r.end = i + ch.len_utf8(),
            _ => out.push(i..i + ch.len_utf8()),
        }
        prev = Some(c);
    }
    out
}

/// Sorted hashes of the non-whitespace tokens, for the similarity score.
fn signature(s: &str) -> Option<Vec<u64>> {
    if s.len() > MAX_WORD_LINE_BYTES {
        return None;
    }
    let toks = tokens(s);
    if toks.len() > MAX_WORD_LINE_TOKENS {
        return None;
    }
    let mut v: Vec<u64> = toks
        .into_iter()
        .map(|r| &s[r])
        .filter(|t| !t.starts_with(char::is_whitespace))
        .map(|t| {
            let mut h = DefaultHasher::new();
            t.hash(&mut h);
            h.finish()
        })
        .collect();
    v.sort_unstable();
    Some(v)
}

/// Dice coefficient of two sorted multisets; 0 when either is missing or empty.
fn similarity(a: &Option<Vec<u64>>, b: &Option<Vec<u64>>) -> f32 {
    let (Some(a), Some(b)) = (a, b) else { return 0. };
    if a.is_empty() || b.is_empty() {
        return 0.;
    }
    let (mut i, mut j, mut common) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
        }
    }
    2. * common as f32 / (a.len() + b.len()) as f32
}

/// Order-preserving pairs (index into dels, index into adds) maximizing the summed similarity; pairs below
/// `MIN_SIMILARITY` never match. Big blocks fall back to pairing by position (linear; `MAX_PAIRS` bounds the
/// word diffs).
fn align(dels: &[Option<Vec<u64>>], adds: &[Option<Vec<u64>>]) -> Vec<(usize, usize)> {
    let (n, m) = (dels.len(), adds.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    if n * m > MAX_ALIGN_CELLS {
        return (0..n.min(m)).filter(|&i| similarity(&dels[i], &adds[i]) >= MIN_SIMILARITY).map(|i| (i, i)).collect();
    }
    // dp[i][j]: best score using dels[..i] and adds[..j].
    let w = m + 1;
    let mut dp = vec![0f32; (n + 1) * w];
    let mut score = vec![0f32; n * m];
    for i in 0..n {
        for j in 0..m {
            let s = similarity(&dels[i], &adds[j]);
            score[i * m + j] = s;
            let take = if s >= MIN_SIMILARITY { dp[i * w + j] + s } else { f32::MIN };
            dp[(i + 1) * w + j + 1] = take.max(dp[i * w + j + 1]).max(dp[(i + 1) * w + j]);
        }
    }
    let (mut i, mut j, mut pairs) = (n, m, Vec::new());
    while i > 0 && j > 0 {
        let s = score[(i - 1) * m + j - 1];
        if s >= MIN_SIMILARITY && dp[i * w + j] == dp[(i - 1) * w + j - 1] + s {
            pairs.push((i - 1, j - 1));
            (i, j) = (i - 1, j - 1);
        } else if dp[i * w + j] == dp[(i - 1) * w + j] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

/// Changed byte ranges on each side, or None when the pair is too different (or too long) to be worth it.
type Ranges = Vec<Range<u32>>;

fn word_ranges(a: &str, b: &str) -> Option<(Ranges, Ranges)> {
    if a.len() > MAX_WORD_LINE_BYTES || b.len() > MAX_WORD_LINE_BYTES {
        return None;
    }
    let (ta, tb) = (tokens(a), tokens(b));
    if ta.len() > MAX_WORD_LINE_TOKENS || tb.len() > MAX_WORD_LINE_TOKENS {
        return None;
    }
    let mut input = InternedInput::default();
    for r in &ta {
        let t = input.interner.intern(&a[r.clone()]);
        input.before.push(t);
    }
    for r in &tb {
        let t = input.interner.intern(&b[r.clone()]);
        input.after.push(t);
    }
    let diff = imara_diff::Diff::compute(Algorithm::Histogram, &input);
    let (mut ra, mut rb): (Vec<Range<usize>>, Vec<Range<usize>>) = (Vec::new(), Vec::new());
    for h in diff.hunks() {
        if !h.before.is_empty() {
            ra.push(h.before.start as usize..h.before.end as usize);
        }
        if !h.after.is_empty() {
            rb.push(h.after.start as usize..h.after.end as usize);
        }
    }
    let ra = to_bytes(&ta, a, ra);
    let rb = to_bytes(&tb, b, rb);
    let changed: usize = ra.iter().chain(&rb).map(|r| r.len()).sum();
    let total = a.len() + b.len();
    if total == 0 || (total - changed) as f32 / (total as f32) < MAX_UNCHANGED_SHARE_FOR_PLAIN {
        return None;
    }
    let cast = |v: Vec<Range<usize>>| v.into_iter().map(|r| r.start as u32..r.end as u32).collect();
    Some((cast(ra), cast(rb)))
}

/// Token-index ranges → byte ranges, merging two ranges separated by a single whitespace token.
fn to_bytes(toks: &[Range<usize>], s: &str, ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last_tok_end: Option<usize> = None;
    for r in ranges {
        let bytes = toks[r.start].start..toks[r.end - 1].end;
        if let (Some(prev), Some(end)) = (out.last_mut(), last_tok_end)
            && r.start == end + 1
            && s[toks[end].clone()].starts_with(char::is_whitespace)
        {
            prev.end = bytes.end;
        } else {
            out.push(bytes);
        }
        last_tok_end = Some(r.end);
    }
    out
}

/// Finds del/add blocks, pairs them, fills in word ranges, and builds the split rows.
fn pair_blocks(out: &mut Layout) {
    let mut budget = MAX_PAIRS;
    let mut split = Vec::with_capacity(out.lines.len());
    let lines = &mut out.lines;
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            LineKind::Hunk | LineKind::Meta | LineKind::Header => {
                split.push(SplitRow::Full(i as u32));
                i += 1;
            }
            LineKind::Context => {
                split.push(SplitRow::Pair(Some(i as u32), Some(i as u32)));
                i += 1;
            }
            LineKind::Del | LineKind::Add => {
                // A block: dels, then adds; "\ No newline" lines inside it are shown after it.
                let (mut dels, mut adds, mut metas) = (Vec::new(), Vec::new(), Vec::new());
                while i < lines.len() {
                    match lines[i].kind {
                        LineKind::Del if adds.is_empty() => dels.push(i),
                        LineKind::Add => adds.push(i),
                        LineKind::Meta => metas.push(i),
                        _ => break,
                    }
                    i += 1;
                }
                let sig = |ix: &[usize], lines: &[DLine]| -> Vec<Option<Vec<u64>>> {
                    ix.iter().map(|&k| signature(&lines[k].text)).collect()
                };
                let pairs = align(&sig(&dels, lines), &sig(&adds, lines));
                for &(d, a) in &pairs {
                    if budget == 0 {
                        break;
                    }
                    budget -= 1;
                    let (dl, al) = (dels[d], adds[a]);
                    if let Some((wd, wa)) = word_ranges(&lines[dl].text.clone(), &lines[al].text.clone()) {
                        lines[dl].words = wd;
                        lines[al].words = wa;
                    }
                }
                // Side by side: each pair on one row; the unpaired lines between pairs are zipped, filler for the rest.
                let (mut d0, mut a0) = (0, 0);
                for (d, a) in pairs.iter().copied().chain(std::iter::once((dels.len(), adds.len()))) {
                    let (gd, ga) = (d - d0, a - a0);
                    for k in 0..gd.max(ga) {
                        let l = (k < gd).then(|| dels[d0 + k] as u32);
                        let r = (k < ga).then(|| adds[a0 + k] as u32);
                        split.push(SplitRow::Pair(l, r));
                    }
                    if d < dels.len() {
                        split.push(SplitRow::Pair(Some(dels[d] as u32), Some(adds[a] as u32)));
                    }
                    (d0, a0) = (d + 1, a + 1);
                }
                split.extend(metas.into_iter().map(|k| SplitRow::Full(k as u32)));
            }
        }
    }
    out.split = split;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// `git diff --no-index` of two texts, parsed like the app does.
    fn raw_diff(name: &str, before: &str, after: &str) -> Diff {
        let d = std::env::temp_dir().join(format!("agent-ide-layout-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a"), before).unwrap();
        std::fs::write(d.join("b"), after).unwrap();
        let out = Command::new("git").args(["diff", "--no-index", "--no-color", "a", "b"]).current_dir(&d).output().unwrap();
        let _ = std::fs::remove_dir_all(&d);
        crate::parse_diff(&out.stdout)
    }

    fn real_diff(name: &str, before: &str, after: &str) -> Layout {
        layout(&raw_diff(name, before, after))
    }

    fn words(l: &DLine) -> Vec<&str> {
        l.words.iter().map(|r| &l.text[r.start as usize..r.end as usize]).collect()
    }

    fn find<'a>(lay: &'a Layout, kind: LineKind, text: &str) -> &'a DLine {
        lay.lines.iter().find(|l| l.kind == kind && &*l.text == text).unwrap_or_else(|| panic!("{kind:?} {text}"))
    }

    #[test]
    fn file_header_dropped_and_hunks_show_their_context() {
        let raw = Diff {
            lines: [
                (LineKind::Header, "diff --git a/x b/x"),
                (LineKind::Header, "index 1..2"),
                (LineKind::Header, "--- a/x"),
                (LineKind::Header, "+++ b/x"),
                (LineKind::Hunk, "@@ -1,2 +1,2 @@ fn main() {"),
                (LineKind::Del, "-a"),
                (LineKind::Hunk, "@@ -9 +9 @@"),
                (LineKind::Add, "+b"),
            ]
            .into_iter()
            .map(|(kind, t)| crate::Line { kind, text: t.into() })
            .collect(),
            truncated: false,
        };
        let lay = layout(&raw);
        let got: Vec<_> = lay.lines.iter().map(|l| (&*l.text, l.old, l.new)).collect();
        assert_eq!(got, vec![("⋯ fn main() {", None, None), ("a", Some(1), None), ("⋯", None, None), ("b", None, Some(9))]);
    }

    #[test]
    fn line_numbers_across_hunks() {
        let before: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let after = before.replace("line 3\n", "line three\n").replace("line 25\n", "line 25\nextra\n");
        let lay = real_diff("numbers", &before, &after);
        assert_eq!(lay.lines.iter().filter(|l| l.kind == LineKind::Hunk).count(), 2);
        assert_eq!(&*lay.lines[0].text, "⋯");
        let first = &lay.lines[1];
        assert_eq!((first.kind, first.old, first.new, &*first.text), (LineKind::Context, Some(1), Some(1), "line 1"));
        assert_eq!(find(&lay, LineKind::Del, "line 3").old, Some(3));
        assert_eq!(find(&lay, LineKind::Add, "line three").new, Some(3));
        let extra = find(&lay, LineKind::Add, "extra");
        assert_eq!((extra.old, extra.new), (None, Some(26)));
        assert_eq!(find(&lay, LineKind::Context, "line 26").old, Some(26));
        assert_eq!(find(&lay, LineKind::Context, "line 26").new, Some(27));
        assert_eq!((lay.max_old, lay.max_new), (28, 29));
    }

    #[test]
    fn new_and_deleted_files() {
        let new = real_diff("new", "", "a\nb\n");
        assert_eq!(new.lines.iter().map(|l| (l.old, l.new)).collect::<Vec<_>>(), vec![(None, None), (None, Some(1)), (None, Some(2))]);
        let gone = real_diff("gone", "a\nb\n", "");
        assert_eq!(gone.lines.iter().map(|l| (l.old, l.new)).collect::<Vec<_>>(), vec![(None, None), (Some(1), None), (Some(2), None)]);
        assert_eq!(gone.split[1..], [SplitRow::Pair(Some(1), None), SplitRow::Pair(Some(2), None)]);
    }

    #[test]
    fn renamed_variable_highlights_only_that_word() {
        let lay = real_diff("rename", "let count = total + 1;\n", "let amount = total + 1;\n");
        assert_eq!(words(find(&lay, LineKind::Del, "let count = total + 1;")), ["count"]);
        assert_eq!(words(find(&lay, LineKind::Add, "let amount = total + 1;")), ["amount"]);
    }

    #[test]
    fn number_and_whitespace_changes() {
        let lay = real_diff("num", "timeout(30, retries);\n", "timeout(45, retries);\n");
        assert_eq!(words(find(&lay, LineKind::Add, "timeout(45, retries);")), ["45"]);
        let ws = real_diff("ws", "a = b + c\n", "a =  b + c\n");
        assert_eq!(words(find(&ws, LineKind::Add, "a =  b + c")), ["  "]);
        assert_eq!(words(find(&ws, LineKind::Del, "a = b + c")), [" "]);
    }

    #[test]
    fn adjacent_changed_words_merge_across_one_space() {
        let lay = real_diff("merge", "call one two end\n", "call uno dos end\n");
        assert_eq!(words(find(&lay, LineKind::Add, "call uno dos end")), ["uno dos"]);
    }

    #[test]
    fn multibyte_ranges_sit_on_char_boundaries() {
        let lay = real_diff("utf8", "jméno = \"Čeněk\" 🦀 ok\n", "jméno = \"Šárka\" 🦀 ok\n");
        let add = find(&lay, LineKind::Add, "jméno = \"Šárka\" 🦀 ok");
        assert_eq!(words(add), ["Šárka"]);
        for r in &add.words {
            assert!(add.text.is_char_boundary(r.start as usize) && add.text.is_char_boundary(r.end as usize));
        }
    }

    #[test]
    fn too_different_or_too_long_pairs_stay_plain() {
        let lay = real_diff("different", "alpha beta gamma\n", "one two three four\n");
        assert!(lay.lines.iter().all(|l| l.words.is_empty()));
        let long_a = format!("x = {};\n", "a ".repeat(600));
        let long_b = format!("x = {};\n", "a ".repeat(599) + "b ");
        let lay = real_diff("long", &long_a, &long_b);
        assert!(lay.lines.iter().all(|l| l.words.is_empty()));
        // Mostly the same, but more than 60 % of the bytes changed → plain.
        let lay = real_diff("mostly", "f(a)\n", "f(a, bbbbbbbbbbbbbbbbbbbbbbbb)\n");
        assert!(lay.lines.iter().all(|l| l.words.is_empty()));
    }

    #[test]
    fn block_alignment_with_an_inserted_line() {
        let before = "fn a() { return first_value; }\nfn b() { return second_value; }\nfn c() { return third_value; }\n";
        let after = "fn a() { return first_value + 1; }\nlet unrelated = 42;\nfn b() { return second_value + 1; }\nfn c() { return third_value + 1; }\n";
        let lay = real_diff("align", before, after);
        let ix = |k, t| lay.lines.iter().position(|l| l.kind == k && &*l.text == t).unwrap() as u32;
        let (d0, d1, d2) = (
            ix(LineKind::Del, "fn a() { return first_value; }"),
            ix(LineKind::Del, "fn b() { return second_value; }"),
            ix(LineKind::Del, "fn c() { return third_value; }"),
        );
        let (a0, ax, a1, a2) = (
            ix(LineKind::Add, "fn a() { return first_value + 1; }"),
            ix(LineKind::Add, "let unrelated = 42;"),
            ix(LineKind::Add, "fn b() { return second_value + 1; }"),
            ix(LineKind::Add, "fn c() { return third_value + 1; }"),
        );
        use SplitRow::Pair;
        assert_eq!(lay.split[1..], [Pair(Some(d0), Some(a0)), Pair(None, Some(ax)), Pair(Some(d1), Some(a1)), Pair(Some(d2), Some(a2))]);
        assert!(lay.lines[ax as usize].words.is_empty(), "unpaired line has no word ranges");
        assert_eq!(words(&lay.lines[a1 as usize]), [" + 1"]);
    }

    #[test]
    fn unrelated_lines_are_zipped_but_not_paired() {
        let lay = real_diff("zip", "alpha beta\ngamma delta\n", "one two\n", );
        use SplitRow::Pair;
        assert_eq!(lay.split[1..], [Pair(Some(1), Some(3)), Pair(Some(2), None)]);
        assert!(lay.lines.iter().all(|l| l.words.is_empty()));
    }

    #[test]
    fn big_blocks_pair_by_position() {
        let before: String = (0..100).map(|i| format!("value_{i} = old_name + {i};\n")).collect();
        let after = before.replace("old_name", "new_name");
        let lay = real_diff("big", &before, &after);
        let del = find(&lay, LineKind::Del, "value_7 = old_name + 7;");
        assert_eq!(words(del), ["old_name"]);
        // 100 × 100 > MAX_ALIGN_CELLS: positional, so row k pairs del k with add k.
        assert!(lay.split[1..].iter().all(|r| matches!(r, SplitRow::Pair(Some(_), Some(_)))));
        assert_eq!(lay.split.len(), 101);
    }

    #[test]
    fn split_rows_keep_order_and_show_every_line_once() {
        let before: String = (0..60).map(|i| format!("row {i} {}\n", i * 7 % 13)).collect();
        let after: String = (0..60)
            .filter(|i| i % 5 != 0)
            .map(|i| if i % 3 == 0 { format!("row {i} changed\n") } else { format!("row {i} {}\n", i * 7 % 13) })
            .chain(["tail\n".to_string()])
            .collect();
        let lay = real_diff("order", &before, &after);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for r in &lay.split {
            match *r {
                SplitRow::Full(i) => {
                    left.push(i);
                    right.push(i);
                }
                SplitRow::Pair(l, r) => {
                    left.extend(l);
                    right.extend(r);
                }
            }
        }
        let expect = |keep: fn(LineKind) -> bool| -> Vec<u32> {
            lay.lines.iter().enumerate().filter(|(_, l)| keep(l.kind)).map(|(i, _)| i as u32).collect()
        };
        let mut l = left.clone();
        l.dedup();
        assert_eq!(l, expect(|k| k != LineKind::Add), "old side: every non-add line once, in order");
        let mut r = right.clone();
        r.dedup();
        assert_eq!(r, expect(|k| k != LineKind::Del), "new side: every non-del line once, in order");
    }

    #[test]
    fn missing_newline_meta_goes_after_the_block() {
        let lay = real_diff("eof", "a\nlast", "a\nlast\nmore\n");
        assert!(matches!(lay.split.last(), Some(SplitRow::Full(i)) if lay.lines[*i as usize].kind == LineKind::Meta));
    }

    #[test]
    fn same_diff_same_hash() {
        let a = real_diff("h1", "x\n", "y\n");
        let b = real_diff("h2", "x\n", "y\n");
        let c = real_diff("h3", "x\n", "z\n");
        assert_eq!(a.hash, b.hash);
        assert_ne!(a.hash, c.hash);
    }

    /// Timing harness (release): `cargo test --release -p git layout_perf -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn layout_perf() {
        let base: String = (0..10_000).map(|i| format!("    let value_{i} = compute(old_name, {i}) + offset;\n")).collect();
        let cases = [
            ("10k-line rewrite", base.lines().map(|l| format!("{}\n", l.replace("compute", "other").replace("offset", "zzz"))).collect::<String>()),
            ("rename on every line", base.replace("old_name", "new_name")),
            ("rename on every 3rd line", base.lines().enumerate().map(|(i, l)| if i % 3 == 0 { l.replace("old_name", "new_name") } else { l.to_string() } + "\n").collect()),
        ];
        for (name, after) in cases {
            let diff = raw_diff(&name.replace(' ', "-"), &base, &after);
            let t = std::time::Instant::now();
            let lay = layout(&diff);
            let took = t.elapsed();
            let paired = lay.lines.iter().filter(|l| !l.words.is_empty()).count();
            println!(
                "{name}: {} rows, {} split rows, {paired} lines with word ranges, layout {:.1} ms",
                lay.lines.len(),
                lay.split.len(),
                took.as_secs_f64() * 1e3
            );
        }
    }
}
