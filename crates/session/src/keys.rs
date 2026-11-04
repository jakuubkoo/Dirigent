//! Keystroke → bytes, xterm encoding (written from the xterm ctlseqs spec).

#[derive(Clone, Copy, Default, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// `key` uses gpui key names ("enter", "up", "f5", "a"); `text` is the produced character, if any.
/// `app_cursor` = DECCKM (TermMode::APP_CURSOR).
pub fn encode(key: &str, text: Option<&str>, m: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let param = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let csi_letter = |c: char, ss3_plain: bool| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[1;{param}{c}").into_bytes()
        } else if ss3_plain {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if param > 1 { format!("\x1b[{n};{param}~") } else { format!("\x1b[{n}~") }.into_bytes()
    };
    let alt_prefix = |b: &[u8]| if m.alt { [b"\x1b", b].concat() } else { b.to_vec() };

    Some(match key {
        // ESC CR = newline in Claude Code (and Alt+Enter in readline-style apps).
        "enter" => if m.shift || m.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() },
        "tab" => if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() },
        "backspace" => if m.ctrl { alt_prefix(b"\x08") } else { alt_prefix(b"\x7f") },
        "escape" => b"\x1b".to_vec(),
        "up" => csi_letter('A', app_cursor),
        "down" => csi_letter('B', app_cursor),
        "right" => csi_letter('C', app_cursor),
        "left" => csi_letter('D', app_cursor),
        "home" => csi_letter('H', app_cursor),
        "end" => csi_letter('F', app_cursor),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => csi_letter('P', true),
        "f2" => csi_letter('Q', true),
        "f3" => csi_letter('R', true),
        "f4" => csi_letter('S', true),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "space" if m.ctrl => alt_prefix(b"\x00"),
        k if m.ctrl && k.chars().count() == 1 => {
            let c = k.chars().next()?.to_ascii_lowercase();
            let b = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return text.map(|t| alt_prefix(t.as_bytes())),
            };
            alt_prefix(&[b])
        }
        _ => alt_prefix(text?.as_bytes()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };

    fn e(key: &str, text: Option<&str>, m: Mods, app: bool) -> Vec<u8> {
        encode(key, text, m, app).unwrap()
    }

    #[test]
    fn encodings() {
        let shift = Mods { shift: true, ..NONE };
        let ctrl = Mods { ctrl: true, ..NONE };
        let alt = Mods { alt: true, ..NONE };
        assert_eq!(e("enter", None, NONE, false), b"\r");
        assert_eq!(e("enter", None, shift, false), b"\x1b\r");
        assert_eq!(e("up", None, NONE, false), b"\x1b[A");
        assert_eq!(e("up", None, NONE, true), b"\x1bOA");
        assert_eq!(e("left", None, ctrl, true), b"\x1b[1;5D");
        assert_eq!(e("delete", None, NONE, false), b"\x1b[3~");
        assert_eq!(e("pageup", None, shift, false), b"\x1b[5;2~");
        assert_eq!(e("f1", None, NONE, false), b"\x1bOP");
        assert_eq!(e("f5", None, NONE, false), b"\x1b[15~");
        assert_eq!(e("c", Some("c"), ctrl, false), [3]);
        assert_eq!(e("d", Some("d"), Mods { ctrl: true, alt: true, ..NONE }, false), b"\x1b\x04");
        assert_eq!(e("b", Some("b"), alt, false), b"\x1bb");
        assert_eq!(e("tab", None, shift, false), b"\x1b[Z");
        assert_eq!(e("backspace", None, NONE, false), [0x7f]);
        assert_eq!(e("a", Some("A"), shift, false), b"A");
        assert_eq!(e("é", Some("é"), NONE, false), "é".as_bytes());
        assert_eq!(encode("shift", None, shift, false), None);
    }
}
