//! Design tokens. Every color, radius, font and fixed size in the UI comes from here; nothing else in `src/` may
//! contain a color literal (a test checks). Colors come from a theme: the built-ins are `crates/app/themes/*.json`
//! (each defines every token; `contrast` tests them all), user themes are `<settings dir>/themes/*.json` laid over
//! the default one token by token. `get()` is the live theme; `set()` switches it between frames.

use std::path::Path;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use gpui::{Pixels, Rgba, px};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Border {
    pub subtle: Rgba,
    pub strong: Rgba,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Text {
    pub primary: Rgba,
    pub muted: Rgba,
    pub dim: Rgba,
    pub faint: Rgba,
    /// Text on an accent-filled button.
    pub on_accent: Rgba,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct States {
    pub working: Rgba,
    pub needs_you: Rgba,
    pub done: Rgba,
    pub done_seen: Rgba,
    pub error: Rgba,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Git {
    pub added: Rgba,
    pub modified: Rgba,
    pub deleted: Rgba,
    pub renamed: Rgba,
    pub conflict: Rgba,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scrollbar {
    pub thumb: Rgba,
    pub active: Rgba,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meter {
    pub track: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    pub crit: Rgba,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Terminal {
    pub bg: Rgba,
    pub fg: Rgba,
    pub cursor: Rgba,
    /// Background of selected text (the text keeps its color).
    pub selection: Rgba,
    pub ansi: [Rgba; 16],
}

/// Syntax colors, one per `syntax::Class`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// Shown in Settings → Appearance.
    pub name: String,
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
    /// The current match of the terminal's find bar (the others use `search_match`).
    pub search_current: Rgba,
    /// Project colors (group header dot, session row branch icon), auto-assigned or picked per project.
    pub project: [Rgba; 8],
}

/// Built-in themes: (id, JSON); the first one is the default.
const BUILTIN: [(&str, &str); 6] = [
    ("slate", include_str!("../themes/slate.json")),
    ("neutral", include_str!("../themes/neutral.json")),
    ("recessed", include_str!("../themes/recessed.json")),
    ("ember", include_str!("../themes/ember.json")),
    ("contrast", include_str!("../themes/contrast.json")),
    ("paper", include_str!("../themes/paper.json")),
];

pub const DEFAULT: &str = BUILTIN[0].0;

/// Parsed once, never freed (`contrast` and `builtins_are_complete` keep them valid).
static BUILTINS: LazyLock<Vec<(&'static str, &'static Theme)>> = LazyLock::new(|| {
    BUILTIN
        .iter()
        .map(|&(id, json)| {
            let t: Theme = serde_json::from_str(json).unwrap_or_else(|e| panic!("built-in theme {id}: {e}"));
            (id, &*Box::leak(Box::new(t)))
        })
        .collect()
});

pub fn builtins() -> &'static [(&'static str, &'static Theme)] {
    &BUILTINS
}

fn default_theme() -> &'static Theme {
    BUILTINS[0].1
}

/// The live theme: null (= the default) or a theme from `builtins()` / `load_user`, all leaked, so never freed.
static CURRENT: AtomicPtr<Theme> = AtomicPtr::new(std::ptr::null_mut());

pub fn get() -> &'static Theme {
    let p = CURRENT.load(Ordering::Acquire);
    // SAFETY: CURRENT only ever holds null or a pointer `set` got from a `&'static Theme`.
    if p.is_null() { default_theme() } else { unsafe { &*p } }
}

/// Switches the live theme (startup, `Workspace::apply_settings` between frames; the caller re-renders every
/// view) and what terminals report as their colors.
pub fn set(t: &'static Theme) {
    CURRENT.store(std::ptr::from_ref(t).cast_mut(), Ordering::Release);
    let rgb8 = |c: Rgba| [(c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8];
    session::set_reported_colors(rgb8(t.terminal.fg), rgb8(t.terminal.bg));
}

/// A theme file from `<settings dir>/themes/`.
pub struct User {
    /// The file name without `.json`: what `appearance.theme` names.
    pub id: String,
    pub theme: &'static Theme,
    /// What was wrong with the file (missing, invalid or unknown tokens).
    pub notes: Vec<String>,
}

/// The theme `id` names, or the default plus a note.
pub fn resolve(id: &str, user: &[User]) -> (&'static Theme, Option<String>) {
    if let Some((_, t)) = BUILTINS.iter().find(|(i, _)| *i == id) {
        return (t, None);
    }
    match user.iter().find(|u| u.id == id) {
        Some(u) => (u.theme, None),
        None => (default_theme(), Some(format!("appearance.theme: no theme \"{id}\", using {}", default_theme().name))),
    }
}

/// What's wrong with the theme files, and with `appearance.theme` if it names no theme.
pub fn notes(user: &[User], id: &str) -> Vec<String> {
    let (_, missing) = resolve(id, user);
    user.iter().flat_map(|u| u.notes.iter().cloned()).chain(missing).collect()
}

/// Every `*.json` in `dir`, sorted by id. Blocking IO: before the window opens or off the UI thread.
// ponytail: every settings reload leaks the user themes again (~3 KB each); cache by file content if that matters.
pub fn load_user(dir: &Path) -> Vec<User> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<User> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let id = p.file_stem()?.to_str()?.to_owned();
            (p.extension()? == "json").then_some((id, p))
        })
        .map(|(id, p)| {
            let (theme, mut notes) = match std::fs::read_to_string(&p) {
                Ok(text) => parse_user(&id, &text),
                Err(e) => (default_theme().clone(), vec![format!("themes/{id}.json: {e}")]),
            };
            if BUILTINS.iter().any(|(b, _)| *b == id) {
                notes.push(format!("themes/{id}.json: named like a built-in theme, ignored (rename the file)"));
            }
            User { id, theme: Box::leak(Box::new(theme)), notes }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// A user theme: its tokens over the default theme's; missing, invalid and unknown tokens become notes.
pub fn parse_user(id: &str, text: &str) -> (Theme, Vec<String>) {
    let file = format!("themes/{id}.json");
    let base = default_theme();
    let mut out = serde_json::to_value(base).expect("a theme serializes");
    out["name"] = Value::String(id.to_owned());
    let mut notes = Vec::new();
    match serde_json::from_str::<Value>(text) {
        Ok(v @ Value::Object(_)) => {
            let mut missing = 0;
            overlay(&mut out, &v, "", &file, &mut notes, &mut missing);
            missing -= usize::from(v.get("name").is_none()); // the file name stands in
            if missing > 0 {
                notes.push(format!("{file}: {missing} tokens missing, taken from {}", base.name));
            }
        }
        Ok(_) => notes.push(format!("{file}: not a JSON object, using {}", base.name)),
        Err(e) => notes.push(format!("{file}: {e}; using {}", base.name)),
    }
    match serde_json::from_value(out) {
        Ok(t) => (t, notes),
        Err(e) => (base.clone(), vec![format!("{file}: {e}; using {}", base.name)]),
    }
}

/// Lays `user` over `base` leaf by leaf: only valid colors (and `name`) replace a default.
fn overlay(base: &mut Value, user: &Value, path: &str, file: &str, notes: &mut Vec<String>, missing: &mut usize) {
    fn leaves(v: &Value) -> usize {
        match v {
            Value::Object(m) => m.values().map(leaves).sum(),
            Value::Array(a) => a.iter().map(leaves).sum(),
            _ => 1,
        }
    }
    match base {
        Value::Object(b) => {
            let Some(u) = user.as_object() else {
                notes.push(format!("{file}: {path} needs an object of tokens; using the defaults"));
                return;
            };
            for k in u.keys().filter(|k| !b.contains_key(*k)) {
                let at = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                notes.push(format!("{file}: unknown token {at}, ignored"));
            }
            for (k, bv) in b.iter_mut() {
                let at = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                match u.get(k) {
                    Some(uv) => overlay(bv, uv, &at, file, notes, missing),
                    None => *missing += leaves(bv),
                }
            }
        }
        Value::Array(b) => match user.as_array() {
            Some(u) if u.len() == b.len() => {
                for (i, (bv, uv)) in b.iter_mut().zip(u).enumerate() {
                    overlay(bv, uv, &format!("{path}[{i}]"), file, notes, missing);
                }
            }
            _ => notes.push(format!("{file}: {path} needs {} colors; using the defaults", b.len())),
        },
        Value::String(_) if path == "name" => match user {
            Value::String(_) => *base = user.clone(),
            _ => notes.push(format!("{file}: name needs to be text")),
        },
        _ => match user.as_str().map(Rgba::try_from) {
            Some(Ok(_)) => *base = user.clone(),
            _ => notes.push(format!("{file}: {path}: {user} is not a color like \"#1e2129\"; using the default")),
        },
    }
}

/// A project's color: palette index `ix` (wraps), so it follows the theme.
pub fn project_color(ix: usize) -> Rgba {
    let p = &get().project;
    p[ix % p.len()]
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

    /// Every built-in theme parses strictly: all tokens present (no serde defaults), none unknown.
    #[test]
    fn builtins_are_complete() {
        for (id, json) in BUILTIN {
            let t: Theme = serde_json::from_str(json).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!t.name.is_empty(), "{id}: no name");
        }
    }

    /// WCAG 2 relative luminance contrast of `fg` (composited over `bg` when translucent) on an opaque `bg`.
    fn ratio(fg: Rgba, bg: Rgba) -> f32 {
        let mix = |f: f32, b: f32| f * fg.a + b * (1. - fg.a);
        let lum = |r: f32, g: f32, b: f32| {
            let c = |x: f32| if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
            0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b)
        };
        let (a, b) = (lum(mix(fg.r, bg.r), mix(fg.g, bg.g), mix(fg.b, bg.b)), lum(bg.r, bg.g, bg.b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// The same thresholds for every built-in theme: small text 4.5:1 (AA), `text.faint` (ignored files,
    /// placeholders, disabled controls), line numbers and non-text marks 3:1; the accent is text too (4.5:1). A theme
    /// that fails can't be merged.
    #[test]
    fn contrast() {
        let mut fails = Vec::new();
        for (id, t) in builtins() {
            let (bg, tx, d) = (&t.bg, &t.text, &t.diff);
            let ui = [bg.base, bg.panel, bg.elevated, bg.hover, bg.selected, bg.selected_focus];
            let mut rules: Vec<(String, Rgba, Rgba, f32)> = Vec::new();
            for (name, c) in [("text.primary", tx.primary), ("text.muted", tx.muted), ("text.dim", tx.dim)] {
                rules.extend(ui.iter().map(|&b| (name.into(), c, b, 4.5)));
            }
            rules.extend(ui.iter().chain([&bg.input]).map(|&b| ("text.faint".into(), tx.faint, b, 3.)));
            rules.extend([bg.base, d.add_bg, d.del_bg].map(|b| ("diff.gutter".into(), d.gutter, b, 3.)));
            rules.extend([
                ("diff.context".into(), d.context, bg.base, 4.5),
                ("diff.add_fg".into(), d.add_fg, d.add_bg, 4.5),
                ("diff.del_fg".into(), d.del_fg, d.del_bg, 4.5),
                ("diff.hunk".into(), d.hunk, bg.base, 3.),
                ("diff.clean_add".into(), d.clean_add, bg.base, 4.5),
                ("diff.clean_del".into(), d.clean_del, bg.base, 4.5),
                ("diff.moved".into(), d.moved, bg.base, 3.),
                ("text.on_accent".into(), tx.on_accent, t.accent, 4.5),
                ("text.on_accent on state.error".into(), tx.on_accent, t.state.error, 4.5),
                ("terminal.fg".into(), t.terminal.fg, t.terminal.bg, 4.5),
                ("text.primary on search_match".into(), tx.primary, t.search_match, 4.5),
            ]);
            let sx = &t.syntax;
            for (name, c) in [
                ("keyword", sx.keyword),
                ("string", sx.string),
                ("number", sx.number),
                ("comment", sx.comment),
                ("function", sx.function),
                ("type", sx.r#type),
                ("variable", sx.variable),
                ("constant", sx.constant),
                ("property", sx.property),
                ("punctuation", sx.punctuation),
                ("operator", sx.operator),
                ("tag", sx.tag),
                ("attribute", sx.attribute),
            ] {
                rules.push((format!("syntax.{name}"), c, bg.base, 4.5));
            }
            let g = &t.git;
            for (name, c) in [
                ("added", g.added),
                ("modified", g.modified),
                ("deleted", g.deleted),
                ("renamed", g.renamed),
                ("conflict", g.conflict),
            ] {
                rules.extend([bg.panel, bg.hover].map(|b| (format!("git.{name}"), c, b, 4.5)));
            }
            let st = &t.state;
            for (name, c) in [
                ("working", st.working),
                ("needs_you", st.needs_you),
                ("done", st.done),
                ("done_seen", st.done_seen),
                ("error", st.error),
            ] {
                rules.push((format!("state.{name}"), c, bg.panel, 3.));
            }
            // Warning and error colors are text too (notes, banners, "1 needs you").
            for (name, c) in [("state.needs_you", t.state.needs_you), ("state.error", t.state.error)] {
                rules.extend([bg.base, bg.panel, bg.elevated].map(|b| (format!("{name} (text)"), c, b, 4.5)));
            }
            // The accent is also text (search match highlights, links), on lists and selected rows.
            rules.extend(
                [bg.panel, bg.elevated, bg.hover, bg.selected, bg.selected_focus]
                    .map(|b| ("accent (text)".into(), t.accent, b, 4.5)),
            );
            rules.extend(t.project.iter().enumerate().map(|(i, &c)| (format!("project[{i}]"), c, bg.panel, 3.)));
            rules.push(("border.strong".into(), t.border.strong, bg.panel, 1.3));
            // Same-luminance hues pass any ratio: colors that must read as different states also differ in hue.
            let dist = |a: Rgba, b: Rgba| ((a.r - b.r).powi(2) + (a.g - b.g).powi(2) + (a.b - b.b).powi(2)).sqrt();
            for (name, a, b) in [
                ("accent vs state.needs_you", t.accent, st.needs_you),
                ("accent vs state.error", t.accent, st.error),
                ("state.needs_you vs state.error", st.needs_you, st.error),
                ("meter.ok vs meter.warn", t.meter.ok, t.meter.warn),
                ("meter.warn vs meter.crit", t.meter.warn, t.meter.crit),
            ] {
                if dist(a, b) < 0.3 {
                    fails.push(format!("{id}: {name} too alike ({:.2} < 0.3)", dist(a, b)));
                }
            }
            for (name, fg, b, min) in rules {
                let r = ratio(fg, b);
                if r < min {
                    fails.push(format!("{id}: {name} {r:.2} < {min}"));
                }
            }
        }
        assert!(fails.is_empty(), "contrast below the thresholds:\n{}", fails.join("\n"));
    }

    #[test]
    fn user_theme_falls_back_per_token() {
        let (t, notes) = parse_user(
            "mine",
            r##"{"bg": {"base": "#101010", "panel": "red"}, "accent": "#ff8800", "terminal": {"ansi": ["#000"]},
                 "sparkle": "#fff"}"##,
        );
        let d = default_theme();
        assert_eq!(t.name, "mine");
        assert_eq!(t.bg.base, Rgba::try_from("#101010").unwrap());
        assert_eq!(t.bg.panel, d.bg.panel, "invalid color → default");
        assert_eq!(t.accent, Rgba::try_from("#ff8800").unwrap());
        assert_eq!(t.terminal.ansi[3], d.terminal.ansi[3], "wrong length → defaults");
        let all = notes.join("\n");
        for want in [
            "bg.panel: \"red\" is not a color",
            "terminal.ansi needs 16 colors",
            "unknown token sparkle",
            "tokens missing",
        ] {
            assert!(all.contains(want), "missing note {want:?} in:\n{all}");
        }
        let (t, notes) = parse_user("bad", "{oops");
        assert_eq!((t.name.as_str(), t.bg.base), ("bad", d.bg.base), "broken file: the default's tokens");
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn resolve_unknown_falls_back() {
        assert!(std::ptr::eq(resolve("paper", &[]).0, builtins()[5].1));
        let (t, note) = resolve("nope", &[]);
        assert!(std::ptr::eq(t, default_theme()));
        assert!(note.unwrap().contains("no theme \"nope\""));
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
