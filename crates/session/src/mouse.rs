//! Mouse reports for apps that enable xterm mouse modes (1000 click, 1002 drag, 1003 any motion; encodings SGR
//! 1006, UTF-8 1005, else X10) and the bytes a paste sends.

use alacritty_terminal::term::TermMode;

use crate::keys::Mods;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held (mode 1003).
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Press,
    Release,
    Motion,
}

/// The report for an event at the 0-based cell (`col`, `line`), or None when the app's mode doesn't ask for it or
/// the cell can't be encoded (X10 stops at column/row 223).
pub fn report(button: Button, kind: Kind, col: usize, line: usize, mods: Mods, mode: TermMode) -> Option<Vec<u8>> {
    let wanted = match kind {
        Kind::Press | Kind::Release => mode.intersects(TermMode::MOUSE_MODE),
        Kind::Motion => {
            mode.contains(TermMode::MOUSE_MOTION) || (mode.contains(TermMode::MOUSE_DRAG) && button != Button::None)
        }
    };
    let wheel = matches!(button, Button::WheelUp | Button::WheelDown);
    if !wanted || (wheel && kind != Kind::Press) {
        return None;
    }
    let mut code: u32 = match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    if kind == Kind::Motion {
        code += 32;
    }
    code += 4 * mods.shift as u32 + 8 * mods.alt as u32 + 16 * mods.ctrl as u32;
    let (x, y) = (col as u32 + 1, line as u32 + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if kind == Kind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    if kind == Kind::Release {
        code |= 3; // the old encodings don't say which button was released
    }
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    let mut out = b"\x1b[M".to_vec();
    for v in [code, x, y] {
        let v = v + 32;
        if utf8 {
            let c = char::from_u32(v).filter(|_| v < 2048)?;
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            out.push(u8::try_from(v).ok()?);
        }
    }
    Some(out)
}

/// What a paste writes: wrapped in bracketed-paste markers when the app enabled them (ESC is dropped from the text,
/// so pasted text can't end the bracket and inject keys); otherwise line breaks become Enter (CR).
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut out = b"\x1b[200~".to_vec();
        out.extend(text.bytes().filter(|&b| b != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };

    fn sgr(mode: TermMode) -> TermMode {
        mode | TermMode::SGR_MOUSE
    }

    #[test]
    fn sgr_reports() {
        let click = sgr(TermMode::MOUSE_REPORT_CLICK);
        assert_eq!(report(Button::Left, Kind::Press, 0, 0, NONE, click).unwrap(), b"\x1b[<0;1;1M");
        assert_eq!(report(Button::Left, Kind::Release, 9, 4, NONE, click).unwrap(), b"\x1b[<0;10;5m");
        assert_eq!(report(Button::Right, Kind::Press, 300, 2, NONE, click).unwrap(), b"\x1b[<2;301;3M");
        // Modifiers: shift 4, alt 8, ctrl 16.
        let all = Mods { ctrl: true, alt: true, shift: true };
        assert_eq!(report(Button::Middle, Kind::Press, 0, 0, all, click).unwrap(), b"\x1b[<29;1;1M");
        // Wheel: press only.
        assert_eq!(report(Button::WheelUp, Kind::Press, 1, 1, NONE, click).unwrap(), b"\x1b[<64;2;2M");
        assert_eq!(report(Button::WheelDown, Kind::Press, 1, 1, NONE, click).unwrap(), b"\x1b[<65;2;2M");
        assert_eq!(report(Button::WheelUp, Kind::Release, 1, 1, NONE, click), None);
    }

    #[test]
    fn motion_follows_the_mode() {
        let click = sgr(TermMode::MOUSE_REPORT_CLICK);
        let drag = sgr(TermMode::MOUSE_DRAG);
        let any = sgr(TermMode::MOUSE_MOTION);
        // 1000: no motion at all.
        assert_eq!(report(Button::Left, Kind::Motion, 1, 1, NONE, click), None);
        // 1002: motion with a button held.
        assert_eq!(report(Button::Left, Kind::Motion, 1, 1, NONE, drag).unwrap(), b"\x1b[<32;2;2M");
        assert_eq!(report(Button::None, Kind::Motion, 1, 1, NONE, drag), None);
        // 1003: any motion; no button = 3 + 32.
        assert_eq!(report(Button::None, Kind::Motion, 1, 1, NONE, any).unwrap(), b"\x1b[<35;2;2M");
        // No mouse mode: nothing.
        assert_eq!(report(Button::Left, Kind::Press, 1, 1, NONE, TermMode::SGR_MOUSE), None);
    }

    #[test]
    fn x10_and_utf8_encodings() {
        let x10 = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(report(Button::Left, Kind::Press, 0, 0, NONE, x10).unwrap(), b"\x1b[M\x20\x21\x21");
        // Release: button bits 3.
        assert_eq!(report(Button::Right, Kind::Release, 0, 0, NONE, x10).unwrap(), b"\x1b[M\x23\x21\x21");
        // Column 223 (0-based 222) is the last one X10 can say.
        assert_eq!(report(Button::Left, Kind::Press, 222, 0, NONE, x10).unwrap(), b"\x1b[M\x20\xff\x21");
        assert_eq!(report(Button::Left, Kind::Press, 223, 0, NONE, x10), None);
        // UTF-8 (1005) goes further: 300 + 1 + 32 = 333 = U+014D.
        let utf8 = x10 | TermMode::UTF8_MOUSE;
        assert_eq!(report(Button::Left, Kind::Press, 300, 0, NONE, utf8).unwrap(), "\x1b[M\u{20}\u{14d}\u{21}".as_bytes());
    }

    #[test]
    fn paste_bytes() {
        assert_eq!(paste("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        // A pasted end marker can't close the bracket early.
        assert_eq!(paste("x\x1b[201~rm -rf ~\n", true), b"\x1b[200~x[201~rm -rf ~\n\x1b[201~");
    }
}
