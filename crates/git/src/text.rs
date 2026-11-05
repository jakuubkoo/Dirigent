//! Turning file or diff text into display lines.

use std::io::Read;
use std::ops::Range;
use std::path::Path;

/// Longer lines are cut (display only).
pub const MAX_LINE_CHARS: usize = 2000;
/// A preview reads at most this much of a file.
pub const MAX_FILE_BYTES: usize = 10 << 20;
pub const MAX_FILE_LINES: usize = 200_000;

/// Tabs → spaces to the next multiple of 4, counted after `marker` leading columns (1 for a diff's +/-/space,
/// 0 for a plain file); cut at `MAX_LINE_CHARS` with "…".
pub fn display_line(line: &str, marker: usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for ch in line.chars() {
        if col >= MAX_LINE_CHARS {
            out.push('…');
            break;
        }
        if ch == '\t' {
            let width = 4 - (col.saturating_sub(marker) % 4);
            out.extend(std::iter::repeat_n(' ', width));
            col += width;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum FileText {
    /// A NUL byte in the first 8 KB.
    Binary,
    /// `truncated`: only the first `MAX_FILE_BYTES` / `MAX_FILE_LINES` are here. `raw`: the text as read (for
    /// syntax highlighting).
    Lines { lines: Vec<String>, truncated: bool, raw: String },
}

/// Reads a file for the preview. Blocking: run off the UI thread.
pub fn read_file(path: &Path) -> std::io::Result<FileText> {
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut buf)?;
    if buf[..buf.len().min(8192)].contains(&0) {
        return Ok(FileText::Binary);
    }
    let cut = buf.len() > MAX_FILE_BYTES;
    buf.truncate(MAX_FILE_BYTES);
    let mut truncated = cut;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = Vec::new();
    for line in text.lines() {
        if lines.len() == MAX_FILE_LINES {
            truncated = true;
            break;
        }
        lines.push(display_line(line, 0));
    }
    if cut {
        lines.pop(); // the last one is cut mid-line
    }
    Ok(FileText::Lines { lines, truncated, raw: text })
}

/// Maps byte spans of a raw line onto `display_line(line, 0)`: a tab's span covers its spaces, anything past the
/// cut is dropped. Spans must lie on char boundaries.
pub fn display_spans<C: Copy>(line: &str, spans: &[(Range<u32>, C)]) -> Vec<(Range<u32>, C)> {
    if spans.is_empty() {
        return Vec::new();
    }
    // (raw offset, display offset) at every char start, plus the end.
    let mut map: Vec<(u32, u32)> = Vec::with_capacity(line.len() + 1);
    let (mut col, mut disp, mut end) = (0usize, 0usize, line.len());
    for (i, ch) in line.char_indices() {
        if col >= MAX_LINE_CHARS {
            end = i;
            break;
        }
        map.push((i as u32, disp as u32));
        let w = if ch == '\t' { 4 - col % 4 } else { 1 };
        col += w;
        disp += if ch == '\t' { w } else { ch.len_utf8() };
    }
    map.push((end as u32, disp as u32));
    let at = |raw: u32| map[map.partition_point(|&(r, _)| r <= raw.min(end as u32)) - 1].1;
    spans
        .iter()
        .filter_map(|(r, c)| {
            let (a, b) = (at(r.start), at(r.end));
            (b > a).then_some((a..b, *c))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_and_long_lines() {
        assert_eq!(display_line("\tx", 0), "    x");
        assert_eq!(display_line("ab\tx", 0), "ab  x");
        assert_eq!(display_line("+\tx", 1), "+    x");
        assert_eq!(display_line(&"y".repeat(5000), 0).chars().count(), MAX_LINE_CHARS + 1);
    }

    #[test]
    fn spans_follow_tabs_multibyte_and_the_cut() {
        // "\tč = 1": tab → 4 spaces, `č` is 2 bytes.
        let line = "\tč = 1";
        let shown = display_line(line, 0);
        let got = display_spans(line, &[(0..1, 'w'), (1..3, 'v'), (6..7, 'n')]);
        let texts: Vec<_> = got.iter().map(|(r, c)| (&shown[r.start as usize..r.end as usize], *c)).collect();
        assert_eq!(texts, [("    ", 'w'), ("č", 'v'), ("1", 'n')]);
        let mid = "ab\tc";
        assert_eq!(display_spans(mid, &[(3..4, 'c')]), [(4..5, 'c')], "tab after 2 cols is 2 wide");
        let long = "y".repeat(MAX_LINE_CHARS + 50);
        assert_eq!(display_spans(&long, &[(1990..2040, 's'), (2010..2020, 'x')]), [(1990..2000, 's')]);
    }

    #[test]
    fn preview_caps() {
        let d = std::env::temp_dir().join(format!("dirigent-text-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let w = |name: &str, body: &[u8]| {
            std::fs::write(d.join(name), body).unwrap();
            read_file(&d.join(name)).unwrap()
        };
        assert_eq!(
            w("a", b"one\n\ttwo"),
            FileText::Lines { lines: vec!["one".into(), "    two".into()], truncated: false, raw: "one\n\ttwo".into() }
        );
        assert_eq!(w("b", b"PNG\0\x01\x02"), FileText::Binary);
        let many = "x\n".repeat(MAX_FILE_LINES + 10);
        let FileText::Lines { lines, truncated, .. } = w("c", many.as_bytes()) else { panic!() };
        assert!(truncated && lines.len() == MAX_FILE_LINES);
        let big = "0123456789abcdef\n".repeat(MAX_FILE_BYTES / 17 + 100);
        let FileText::Lines { truncated, .. } = w("d", big.as_bytes()) else { panic!() };
        assert!(truncated);
        let _ = std::fs::remove_dir_all(&d);
    }
}
