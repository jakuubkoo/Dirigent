//! One-shot answer to a prompt in the PTY output (e.g. Claude Code's folder trust dialog): the reader thread feeds
//! every chunk through `Expect::feed`, which strips escape sequences and whitespace (TUIs move the cursor instead
//! of printing spaces) and reports when the needle shows up. Until the deadline only; then it is dropped.

use std::time::Instant;

pub struct Expect {
    /// Without whitespace.
    needle: String,
    pub reply: Vec<Vec<u8>>,
    pub deadline: Instant,
    /// The stripped tail seen so far (bounded).
    seen: String,
    state: State,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Text,
    Esc,
    /// CSI: until a final byte 0x40..=0x7e.
    Csi,
    /// OSC / DCS / APC …: until BEL or ESC \.
    Str,
    StrEsc,
}

impl Expect {
    /// `reply`: written in order, each part after a pause (a TUI reads them as separate keys).
    pub fn new(needle: &str, reply: Vec<Vec<u8>>, deadline: Instant) -> Self {
        let needle: String = needle.chars().filter(|c| !c.is_whitespace()).collect();
        Self { needle, reply, deadline, seen: String::new(), state: State::Text }
    }

    /// True once the needle has appeared.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut text = Vec::with_capacity(bytes.len());
        for &b in bytes {
            self.state = match (self.state, b) {
                (State::Text, 0x1b) => State::Esc,
                (State::Text, b) => {
                    if !b.is_ascii_whitespace() && b >= 0x20 {
                        text.push(b);
                    }
                    State::Text
                }
                (State::Esc, b'[') => State::Csi,
                (State::Esc, b']' | b'P' | b'_' | b'^' | b'X') => State::Str,
                (State::Esc, _) => State::Text, // two-byte sequences (ESC = , ESC ( B is close enough)
                (State::Csi, 0x40..=0x7e) => State::Text,
                (State::Csi, _) => State::Csi,
                (State::Str, 0x07) => State::Text,
                (State::Str, 0x1b) => State::StrEsc,
                (State::Str, _) => State::Str,
                (State::StrEsc, b'\\') => State::Text,
                (State::StrEsc, _) => State::Str,
            };
        }
        self.seen.push_str(&String::from_utf8_lossy(&text));
        if self.seen.contains(&self.needle) {
            return true;
        }
        // Keep only what a needle split across chunks still needs.
        let keep = self.needle.len();
        if self.seen.len() > keep * 4 + 4096 {
            let mut cut = self.seen.len() - keep;
            while !self.seen.is_char_boundary(cut) {
                cut += 1;
            }
            self.seen.drain(..cut);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_text_drawn_with_cursor_moves_across_chunks() {
        // The shape claude 2.1.318 draws the trust dialog in: words separated by cursor-forward, colors around.
        let raw = b"\x1b[2m\xe2\x9d\xaf\x1b[22mNo,\x1b[1Cexit\r\n\x1b[38;2;1;2;3mYes,\x1b[1CI\x1b[1Ctrust\x1b[1Cthis\x1b[1Cfol";
        let mut e = Expect::new("Yes, I trust this folder", vec![b"\r".to_vec()], Instant::now());
        assert!(!e.feed(raw));
        assert!(e.feed(b"der\x1b[39m\r\n\x1b]0;title\x07"));
        let mut e = Expect::new("trust", vec![], Instant::now());
        assert!(!e.feed(b"\x1b]0;trust in a title\x07plain"), "OSC payloads aren't text");
    }
}
