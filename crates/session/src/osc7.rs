//! OSC 7 (`ESC ] 7 ; file://host/path ST`): the shell reporting its current directory. vte's `ansi` handler
//! doesn't handle it (only logs it), so the reader scans raw PTY output for it before parsing.

use std::path::PathBuf;

const MAX: usize = 4096;

/// Finds OSC 7 sequences in a byte stream that may split them across reads.
#[derive(Default)]
pub struct Scanner {
    /// Bytes of an OSC 7 sequence started in an earlier chunk (after `ESC ] 7 ;`).
    partial: Option<Vec<u8>>,
}

impl Scanner {
    /// Returns the payload (`file://host/path`) of the last complete OSC 7 sequence in `chunk`, if any.
    pub fn scan(&mut self, chunk: &[u8]) -> Option<Vec<u8>> {
        let mut found = None;
        let mut i = 0;
        if let Some(mut buf) = self.partial.take() {
            if buf.last() == Some(&0x1b) && !chunk.is_empty() {
                // The previous read ended on the ESC of a terminator.
                buf.pop();
                found = Some(buf);
                i = (chunk[0] == b'\\') as usize;
            } else {
                match terminator(chunk) {
                    Some((end, skip)) => {
                        buf.extend_from_slice(&chunk[..end]);
                        found = Some(buf);
                        i = end + skip;
                    }
                    None if buf.len() + chunk.len() <= MAX => {
                        buf.extend_from_slice(chunk);
                        self.partial = Some(buf);
                        return None;
                    }
                    None => return None, // too long: not a real cwd report, drop it
                }
            }
        }
        while let Some(off) = chunk[i..].iter().position(|&b| b == 0x1b) {
            let start = i + off;
            let rest = &chunk[start + 1..];
            if rest.len() < 3 {
                // ponytail: a read boundary inside the 4-byte introducer misses that one report (the next prompt
                // sends it again). Carry `rest` over if that ever shows up in practice.
                break;
            }
            if &rest[..3] != b"]7;" {
                i = start + 1;
                continue;
            }
            let body = &rest[3..];
            match terminator(body) {
                Some((end, skip)) => {
                    found = Some(body[..end].to_vec());
                    i = start + 4 + end + skip;
                }
                None => {
                    if body.len() <= MAX {
                        self.partial = Some(body.to_vec());
                    }
                    break;
                }
            }
        }
        found
    }
}

/// End of an OSC body: BEL, or ST (`ESC \`). Returns (body length, terminator length).
fn terminator(b: &[u8]) -> Option<(usize, usize)> {
    let pos = b.iter().position(|&c| c == 0x07 || c == 0x1b)?;
    if b[pos] == 0x07 {
        return Some((pos, 1));
    }
    // ESC without a following byte yet: treat as unterminated (the next chunk completes it).
    match b.get(pos + 1) {
        Some(b'\\') => Some((pos, 2)),
        Some(_) => Some((pos, 1)), // any other ESC aborts the string (as in vte); the payload so far still counts
        None => None,
    }
}

/// `file://host/path` → path, only if `host` is this machine (empty, `localhost` or our hostname): an OSC 7 from
/// a remote shell (ssh) describes a directory that doesn't exist here.
pub fn parse(payload: &[u8], hostname: &str) -> Option<PathBuf> {
    let rest = payload.strip_prefix(b"file://")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let host = std::str::from_utf8(&rest[..slash]).ok()?;
    if !(host.is_empty() || host == "localhost" || host.eq_ignore_ascii_case(hostname)) {
        return None;
    }
    Some(to_path(decode(&rest[slash..])))
}

#[cfg(unix)]
fn to_path(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

/// `/C:/Users/x` (or cmd's raw `/C:\Users\x`) → `C:\Users\x`.
#[cfg(windows)]
fn to_path(bytes: Vec<u8>) -> PathBuf {
    let s = String::from_utf8_lossy(&bytes);
    let s = s.strip_prefix('/').filter(|r| r.as_bytes().get(1) == Some(&b':')).unwrap_or(&s);
    PathBuf::from(s.replace('/', "\\"))
}

fn decode(b: &[u8]) -> Vec<u8> {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c)))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// This machine's hostname (what zsh's `$HOST` holds).
#[cfg(unix)]
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_default()
}

/// This machine's name (what our PowerShell prompt hook puts in OSC 7).
#[cfg(windows)]
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut s = Scanner::default();
        chunks.iter().filter_map(|c| s.scan(c)).collect()
    }

    #[test]
    fn finds_bel_and_st_terminated() {
        assert_eq!(scan_all(&[b"ab\x1b]7;file://h/x\x07cd"]), vec![b"file://h/x".to_vec()]);
        assert_eq!(scan_all(&[b"\x1b[0m\x1b]7;file://h/y\x1b\\$ "]), vec![b"file://h/y".to_vec()]);
        // Several in one chunk: the last one wins.
        assert_eq!(scan_all(&[b"\x1b]7;file:///a\x07\x1b]7;file:///b\x07"]), vec![b"file:///b".to_vec()]);
        // Other OSCs are not ours.
        assert!(scan_all(&[b"\x1b]0;title\x07\x1b]8;;http://x\x07"]).is_empty());
    }

    #[test]
    fn split_across_reads() {
        assert_eq!(
            scan_all(&[b"x\x1b]7;file://h/ho", b"me/j", b"ak\x1b", b"\\rest"]),
            vec![b"file://h/home/jak".to_vec()]
        );
        assert_eq!(scan_all(&[b"\x1b]7;", b"file:///t\x07"]), vec![b"file:///t".to_vec()]);
        // An unterminated, endless one is dropped instead of growing forever.
        let big = vec![b'a'; MAX + 10];
        assert!(scan_all(&[b"\x1b]7;", &big, b"\x07"]).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn parses_local_paths_only() {
        assert_eq!(parse(b"file://box/home/j/my%20dir", "box"), Some("/home/j/my dir".into()));
        assert_eq!(parse(b"file:///tmp", "box"), Some("/tmp".into()));
        assert_eq!(parse(b"file://localhost/tmp/100%25", "box"), Some("/tmp/100%".into()));
        assert_eq!(parse(b"file://other/etc", "box"), None, "remote shell");
        assert_eq!(parse(b"http://box/etc", "box"), None);
        assert_eq!(parse(b"file://box/a%zzb", "box"), Some("/a%zzb".into()), "bad escape kept as-is");
    }

    #[test]
    #[cfg(windows)]
    fn parses_windows_paths() {
        let p = |s: &[u8]| parse(s, "BOX").map(|p| p.to_string_lossy().into_owned());
        assert_eq!(p(b"file://BOX/C:/Users/j/my%20dir").as_deref(), Some(r"C:\Users\j\my dir"), "PowerShell hook");
        assert_eq!(p(b"file://box/D:/").as_deref(), Some(r"D:\"), "host case-insensitive");
        assert_eq!(p(b"file://localhost/C:\\Code\\a b").as_deref(), Some(r"C:\Code\a b"), "cmd's raw $P");
        assert_eq!(p(b"file://other/C:/x"), None, "remote shell");
    }
}
