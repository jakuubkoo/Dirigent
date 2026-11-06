//! Design tokens. Every color, radius, font and fixed size in the UI comes from here; nothing else in `src/` may
//! contain a color literal (a test checks). A light theme later is just another `Theme` static.

use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{Pixels, Rgba, px};

/// `0xRRGGBB`.
pub const fn hex(v: u32) -> Rgba {
    Rgba { r: ((v >> 16) & 0xff) as f32 / 255., g: ((v >> 8) & 0xff) as f32 / 255., b: (v & 0xff) as f32 / 255., a: 1. }
}

/// `0xRRGGBBAA`.
pub const fn hexa(v: u32) -> Rgba {
    let mut c = hex(v >> 8);
    c.a = (v & 0xff) as f32 / 255.;
    c
}

pub struct Bg {
    /// Terminal, main area, diff/preview body.
    pub base: Rgba,
    /// Sidebar, right panel, title bar, status bar.
    pub panel: Rgba,
    /// Sidebar/title strip when the compositor blurs what's behind the window.
    pub panel_translucent: Rgba,
    /// Slim headers, modals, tooltips, banners.
    pub elevated: Rgba,
    pub hover: Rgba,
    pub selected: Rgba,
    /// Selected row while its list has keyboard focus.
    pub selected_focus: Rgba,
    pub input: Rgba,
    /// Dims everything behind a modal.
    pub scrim: Rgba,
}

pub struct Border {
    pub subtle: Rgba,
    pub strong: Rgba,
}

pub struct Text {
    pub primary: Rgba,
    pub muted: Rgba,
    pub dim: Rgba,
    pub faint: Rgba,
    /// Text on an accent-filled button.
    pub on_accent: Rgba,
}

pub struct States {
    pub working: Rgba,
    pub needs_you: Rgba,
    pub done: Rgba,
    pub done_seen: Rgba,
    pub error: Rgba,
}

pub struct Git {
    pub added: Rgba,
    pub modified: Rgba,
    pub deleted: Rgba,
    pub renamed: Rgba,
    pub conflict: Rgba,
}

pub struct DiffColors {
    pub add_bg: Rgba,
    pub add_fg: Rgba,
    pub del_bg: Rgba,
    pub del_fg: Rgba,
    /// Changed words inside a paired removed/added line.
    pub add_word_bg: Rgba,
    pub del_word_bg: Rgba,
    pub hunk: Rgba,
    pub context: Rgba,
    /// Split view: the empty side of a one-sided row.
    pub filler: Rgba,
    /// Clean split style: removed/added text and numbers, and the faint background under changed tokens.
    pub clean_del: Rgba,
    pub clean_add: Rgba,
    pub clean_del_bg: Rgba,
    pub clean_add_bg: Rgba,
    /// Structural diff: moved code (gutter number, bar, hint) and its token background.
    pub moved: Rgba,
    pub moved_bg: Rgba,
    /// Line numbers.
    pub gutter: Rgba,
}

pub struct Scrollbar {
    pub thumb: Rgba,
    pub active: Rgba,
}

pub struct Meter {
    pub track: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    pub crit: Rgba,
}

pub struct Terminal {
    pub bg: Rgba,
    pub fg: Rgba,
    pub cursor: Rgba,
    /// Background of selected text (the text keeps its color).
    pub selection: Rgba,
    pub ansi: [Rgba; 16],
}

/// Syntax colors, one per `syntax::Class`.
pub struct Syntax {
    pub keyword: Rgba,
    pub string: Rgba,
    pub number: Rgba,
    pub comment: Rgba,
    pub function: Rgba,
    pub r#type: Rgba,
    pub variable: Rgba,
    pub constant: Rgba,
    pub property: Rgba,
    pub punctuation: Rgba,
    pub operator: Rgba,
    pub tag: Rgba,
    pub attribute: Rgba,
}

impl Syntax {
    /// The color of a `syntax::Class` id (`Class as u8`).
    pub fn color(&self, class: u8) -> Option<Rgba> {
        use syntax::Class::*;
        Some(match syntax::Class::from_u8(class)? {
            Keyword => self.keyword,
            String => self.string,
            Number => self.number,
            Comment => self.comment,
            Function => self.function,
            Type => self.r#type,
            Variable => self.variable,
            Constant => self.constant,
            Property => self.property,
            Punctuation => self.punctuation,
            Operator => self.operator,
            Tag => self.tag,
            Attribute => self.attribute,
        })
    }
}

pub struct Theme {
    pub bg: Bg,
    pub border: Border,
    pub text: Text,
    pub accent: Rgba,
    pub state: States,
    pub git: Git,
    pub diff: DiffColors,
    pub scrollbar: Scrollbar,
    pub meter: Meter,
    pub terminal: Terminal,
    pub syntax: Syntax,
    /// The window close button's hover background.
    pub danger_hover: Rgba,
    /// A search match (Search panel results, the matched text in the preview).
    pub search_match: Rgba,
}

pub static DARK: Theme = Theme {
    bg: Bg {
        base: hex(0x0c0c0e),
        panel: hex(0x141417),
        panel_translucent: hexa(0x141417b8),
        elevated: hex(0x1b1b1f),
        hover: hex(0x1f1f24),
        selected: hex(0x26262c),
        selected_focus: hex(0x24304d),
        input: hex(0x0f0f12),
        scrim: hexa(0x00000080),
    },
    border: Border { subtle: hexa(0xffffff0f), strong: hexa(0xffffff1f) },
    text: Text {
        primary: hex(0xececf1),
        muted: hex(0xa1a1aa),
        dim: hex(0x6b6b76),
        faint: hex(0x45454d),
        on_accent: hex(0xffffff),
    },
    accent: hex(0x7c8aff),
    state: States {
        working: hex(0x60a5fa),
        needs_you: hex(0xf5a524),
        done: hex(0x4ade80),
        done_seen: hex(0x6b6b76),
        error: hex(0xf87171),
    },
    git: Git {
        added: hex(0x73c991),
        modified: hex(0xe2c08d),
        deleted: hex(0xf07178),
        renamed: hex(0x7aa2f7),
        conflict: hex(0xf5a524),
    },
    diff: DiffColors {
        add_bg: hex(0x11251a),
        add_fg: hex(0xb5e3c0),
        del_bg: hex(0x2a1417),
        del_fg: hex(0xf2b8bd),
        add_word_bg: hex(0x1f5132),
        del_word_bg: hex(0x6b2a31),
        hunk: hex(0x5f6b8a),
        context: hex(0xc8c8ce),
        filler: hex(0x101013),
        clean_del: hex(0xf87171),
        clean_add: hex(0x4ade80),
        clean_del_bg: hexa(0xf8717129),
        clean_add_bg: hexa(0x4ade8029),
        moved: hex(0x60a5fa),
        moved_bg: hexa(0x60a5fa29),
        gutter: hex(0x45454d),
    },
    scrollbar: Scrollbar { thumb: hexa(0xffffff1f), active: hexa(0xffffff47) },
    meter: Meter { track: hexa(0xffffff14), ok: hex(0x7c8aff), warn: hex(0xf5a524), crit: hex(0xf87171) },
    terminal: Terminal {
        bg: hex(0x0c0c0e),
        fg: hex(0xd6d6dc),
        cursor: hex(0xececf1),
        selection: hex(0x2b3b63),
        ansi: [
            hex(0x24242a),
            hex(0xf0707a),
            hex(0x8fd18a),
            hex(0xe9c46a),
            hex(0x6aa8f7),
            hex(0xc792ea),
            hex(0x5ccfe6),
            hex(0xc8c8ce),
            hex(0x5a5a66),
            hex(0xff8a93),
            hex(0xa8e6a3),
            hex(0xf5d58a),
            hex(0x8bbcff),
            hex(0xd7aaf5),
            hex(0x7fdcf0),
            hex(0xf4f4f6),
        ],
    },
    syntax: Syntax {
        keyword: hex(0xc4a1ff),
        string: hex(0x9ece6a),
        number: hex(0xff9e64),
        comment: hex(0x6b6b76),
        function: hex(0x7aa2f7),
        r#type: hex(0x5ccfe6),
        variable: hex(0xd6d6dc),
        constant: hex(0xf5a97f),
        property: hex(0xa9b8f5),
        punctuation: hex(0x8b8b96),
        operator: hex(0x89ddff),
        tag: hex(0xf7768e),
        attribute: hex(0xe0af68),
    },
    danger_hover: hex(0xc4314b),
    search_match: hex(0x5c4a14),
};

pub fn get() -> &'static Theme {
    &DARK
}

/// Color of a git status letter (M/A/D/R/C/T/U/?).
pub fn git_kind(kind: char) -> Rgba {
    let g = &get().git;
    match kind {
        'A' | '?' => g.added,
        'D' => g.deleted,
        'R' | 'C' => g.renamed,
        'U' => g.conflict,
        _ => g.modified,
    }
}

/// The UI zoom (Settings → General, Ctrl+= / Ctrl+- / Ctrl+0) as f32 bits; set only by `Workspace::apply_settings`.
static ZOOM: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

pub fn set_zoom(factor: f32) {
    ZOOM.store(factor.to_bits(), Ordering::Relaxed);
}

/// Every UI size goes through here: `v` px at 100 %, scaled by the UI zoom. A raw `px(..)` outside this file
/// fails `no_unscaled_sizes_outside_theme` unless its line says `// unscaled: <why>`.
pub fn z(v: f32) -> Pixels {
    px(v * zoom())
}

pub const UI_FONT: &str = "Inter";
pub const MONO_FONT: &str = "JetBrains Mono";
pub const TERMINAL_FONT_SIZE: f32 = 13.5;

/// UI font sizes (px).
pub mod text_size {
    pub const XS: f32 = 11.;
    pub const SM: f32 = 12.;
    pub const MD: f32 = 13.;
}

pub mod radius {
    pub const SM: f32 = 4.;
    pub const MD: f32 = 6.;
    pub const LG: f32 = 8.;
}

/// Spacing scale (px).
pub mod space {
    pub const XXS: f32 = 2.;
    pub const XS: f32 = 4.;
    pub const SM: f32 = 6.;
    pub const MD: f32 = 8.;
    pub const LG: f32 = 12.;
    pub const XL: f32 = 16.;
}

/// Fixed heights (px).
pub mod height {
    pub const TITLE_BAR: f32 = 30.;
    pub const STATUS_BAR: f32 = 24.;
    pub const HEADER: f32 = 28.;
    pub const SESSION_ROW: f32 = 44.;
    pub const GROUP_HEADER: f32 = 26.;
    pub const LIST_ROW: f32 = 24.;
    pub const TAB_BAR: f32 = 32.;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(hex(0xff0080), Rgba { r: 1., g: 0., b: 128. / 255., a: 1. });
        assert_eq!(hexa(0xffffff0f).a, 15. / 255.);
    }

    /// Colors live only here: no other source file may contain a color literal (hex color or a gpui
    /// `rgb(..)`/`rgba(..)`/`hsla(..)` call).
    #[test]
    fn no_color_literals_outside_theme() {
        fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") && p.file_name().unwrap() != "theme.rs" {
                    out.push(p);
                }
            }
        }
        let call = |line: &str, name: &str| {
            line.match_indices(&format!("{name}("))
                .any(|(p, _)| p == 0 || !line[..p].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
        };
        let mut paths = Vec::new();
        files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut paths);
        let mut offenders = Vec::new();
        for path in paths {
            for (i, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                let has_hex = line.match_indices("0x").any(|(p, _)| {
                    let digits = line[p + 2..].chars().take_while(|c| c.is_ascii_hexdigit()).count();
                    digits == 6 || digits == 8
                });
                if has_hex || ["rgb", "rgba", "hsla"].iter().any(|n| call(line, n)) {
                    offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(offenders.is_empty(), "color literals outside theme.rs:\n{}", offenders.join("\n"));
    }

    /// Sizes scale with the UI zoom: a `px(..)` call (not the `.px(..)` padding method) outside this file must go
    /// through `z(..)` or say why it is exempt (`// unscaled: …`, e.g. math on values that are already scaled).
    #[test]
    fn no_unscaled_sizes_outside_theme() {
        fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") && p.file_name().unwrap() != "theme.rs" {
                    out.push(p);
                }
            }
        }
        let mut paths = Vec::new();
        files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut paths);
        let mut offenders = Vec::new();
        for path in paths {
            for (i, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                let raw = code.match_indices("px(").any(|(p, _)| {
                    !code[..p].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
                });
                if raw && !line.contains("// unscaled") {
                    offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(offenders.is_empty(), "unscaled px(..) outside theme.rs (use theme::z):\n{}", offenders.join("\n"));
    }

    #[test]
    fn z_scales_with_the_zoom() {
        assert_eq!(z(10.), px(10.));
        set_zoom(1.5);
        assert_eq!(z(10.), px(15.));
        set_zoom(1.);
    }
}
