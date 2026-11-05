//! Turning file or diff text into display lines.

use std::io::Read;
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
    /// `truncated`: only the first `MAX_FILE_BYTES` / `MAX_FILE_LINES` are here.
    Lines { lines: Vec<String>, truncated: bool },
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
    let text = String::from_utf8_lossy(&buf);
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
    Ok(FileText::Lines { lines, truncated })
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
    fn preview_caps() {
        let d = std::env::temp_dir().join(format!("agent-ide-text-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let w = |name: &str, body: &[u8]| {
            std::fs::write(d.join(name), body).unwrap();
            read_file(&d.join(name)).unwrap()
        };
        assert_eq!(w("a", b"one\n\ttwo"), FileText::Lines { lines: vec!["one".into(), "    two".into()], truncated: false });
        assert_eq!(w("b", b"PNG\0\x01\x02"), FileText::Binary);
        let many = "x\n".repeat(MAX_FILE_LINES + 10);
        let FileText::Lines { lines, truncated } = w("c", many.as_bytes()) else { panic!() };
        assert!(truncated && lines.len() == MAX_FILE_LINES);
        let big = "0123456789abcdef\n".repeat(MAX_FILE_BYTES / 17 + 100);
        let FileText::Lines { truncated, .. } = w("d", big.as_bytes()) else { panic!() };
        assert!(truncated);
        let _ = std::fs::remove_dir_all(&d);
    }
}
