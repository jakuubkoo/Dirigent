//! Paints the visible terminal grid. The term lock is held only while copying the grid into
//! segments/rects; shaping and painting happen after it is released.

use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, Bounds, Element, ElementId, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, ShapedLine, SharedString, StrikethroughStyle, Style, TextAlign, TextRun,
    UnderlineStyle, Window, fill, font, point, px, relative, size,
};
use session::alacritty_terminal::grid::Dimensions;
use session::alacritty_terminal::term::cell::Flags;
use session::alacritty_terminal::term::color::Colors;
use session::alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use session::Session;

pub mod palette {
    use gpui::Rgba;
    const fn rgb(hex: u32) -> Rgba {
        Rgba {
            r: ((hex >> 16) & 0xff) as f32 / 255.,
            g: ((hex >> 8) & 0xff) as f32 / 255.,
            b: (hex & 0xff) as f32 / 255.,
            a: 1.,
        }
    }
    pub const BG: Rgba = rgb(0x1e1e24);
    pub const FG: Rgba = rgb(0xd8d8d8);
    pub const CURSOR: Rgba = rgb(0xe8e8e8);
    pub const ANSI: [Rgba; 16] = [
        rgb(0x2a2a31), rgb(0xe0626c), rgb(0x8cc265), rgb(0xd9b25f),
        rgb(0x5fa2e6), rgb(0xc27adf), rgb(0x4fb8c8), rgb(0xc8c8cc),
        rgb(0x5c5c66), rgb(0xf07882), rgb(0xa5d97f), rgb(0xf0ca78),
        rgb(0x7ab8f5), rgb(0xd696ef), rgb(0x6cd0de), rgb(0xf4f4f6),
    ];
}

pub const FONT_FAMILY: &str = "DejaVu Sans Mono"; // ponytail: hard-coded; make it a setting when settings exist
pub const FONT_SIZE: f32 = 14.;

#[derive(Clone)]
pub struct Metrics {
    font: Font,
    font_size: Pixels,
    pub cell_width: Pixels,
    pub line_height: Pixels,
}

impl Metrics {
    pub fn new(window: &mut Window) -> Self {
        let font = font(FONT_FAMILY);
        let font_size = px(FONT_SIZE);
        let ts = window.text_system();
        let id = ts.resolve_font(&font);
        let cell_width = ts.advance(id, font_size, 'm').map(|s| s.width).unwrap_or(px(FONT_SIZE * 0.6));
        Self { font, font_size, cell_width, line_height: px((FONT_SIZE * 1.2).round()) }
    }
}

fn rgb_hsla(r: u8, g: u8, b: u8) -> Hsla {
    gpui::Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: 1. }.into()
}

fn xterm256(i: u8) -> Hsla {
    match i {
        0..16 => palette::ANSI[i as usize].into(),
        16..232 => {
            let i = i - 16;
            let v = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
            rgb_hsla(v(i / 36), v((i / 6) % 6), v(i % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            rgb_hsla(g, g, g)
        }
    }
}

fn resolve(c: Color, colors: &Colors) -> Hsla {
    let over = |idx: usize| colors[idx].map(|c| rgb_hsla(c.r, c.g, c.b));
    match c {
        Color::Spec(c) => rgb_hsla(c.r, c.g, c.b),
        Color::Indexed(i) => over(i as usize).unwrap_or_else(|| xterm256(i)),
        Color::Named(n) => over(n as usize).unwrap_or_else(|| {
            let i = n as usize;
            match n {
                _ if i < 16 => palette::ANSI[i].into(),
                NamedColor::Background => palette::BG.into(),
                NamedColor::Cursor => palette::CURSOR.into(),
                NamedColor::DimForeground => palette::FG.into(),
                _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
                    palette::ANSI[i - NamedColor::DimBlack as usize].into()
                }
                _ => palette::FG.into(),
            }
        }),
    }
}

/// Text of one row starting at `col`, shaped with a forced per-glyph advance of one cell.
struct Segment {
    row: usize,
    col: usize,
    text: String,
    runs: Vec<TextRun>,
    cells: usize,
    wide: bool,
}

pub struct TerminalElement {
    session: Rc<Session>,
    m: Metrics,
}

impl TerminalElement {
    pub fn new(session: Rc<Session>, m: Metrics) -> Self {
        Self { session, m }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub struct Prepainted {
    rects: Vec<(usize, usize, usize, Hsla)>, // row, col, width in cells, color
    lines: Vec<(usize, usize, ShapedLine)>,  // row, col, shaped
    cursor: Option<Bounds<Pixels>>,
    started: Instant,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Prepainted {
        let started = Instant::now();
        let m = &self.m;
        let mut rects: Vec<(usize, usize, usize, Hsla)> = Vec::new();
        let mut segs: Vec<Segment> = Vec::new();
        let mut cursor = None;
        let default_bg: Hsla = palette::BG.into();
        {
            let term = self.session.term.lock();
            let content = term.renderable_content();
            let offset = content.display_offset as i32;
            let screen_lines = term.screen_lines();
            let cur = content.cursor;
            let cur_row = cur.point.line.0 + offset;
            let cur_visible = cur.shape != CursorShape::Hidden && (0..screen_lines as i32).contains(&cur_row);
            let cur_at = (cur_row as usize, cur.point.column.0);

            for cell in content.display_iter {
                let flags = cell.flags;
                if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let row = (cell.point.line.0 + offset) as usize;
                let col = cell.point.column.0;
                let mut fg = resolve(cell.fg, content.colors);
                let mut bg = resolve(cell.bg, content.colors);
                if flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cur_visible && cur.shape == CursorShape::Block && (row, col) == cur_at {
                    (fg, bg) = (bg, palette::CURSOR.into());
                }
                if flags.contains(Flags::DIM) {
                    fg.a *= 0.66;
                }
                if flags.contains(Flags::HIDDEN) {
                    fg = bg;
                }
                let wide = flags.contains(Flags::WIDE_CHAR);
                let width = if wide { 2 } else { 1 };

                if bg != default_bg {
                    match rects.last_mut() {
                        Some((r, c, w, color)) if *r == row && *c + *w == col && *color == bg => *w += width,
                        _ => rects.push((row, col, width, bg)),
                    }
                }
                if cell.c == ' ' && cell.zerowidth().is_none() && !flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
                    continue; // blanks are not shaped; a segment ends at the gap
                }

                let mut run_font = m.font.clone();
                if flags.intersects(Flags::BOLD) {
                    run_font.weight = FontWeight::BOLD;
                }
                if flags.intersects(Flags::ITALIC) {
                    run_font.style = FontStyle::Italic;
                }
                let underline = flags.intersects(Flags::ALL_UNDERLINES).then(|| UnderlineStyle {
                    color: Some(fg),
                    thickness: px(1.),
                    wavy: flags.contains(Flags::UNDERCURL),
                });
                let strikethrough =
                    flags.contains(Flags::STRIKEOUT).then(|| StrikethroughStyle { color: Some(fg), thickness: px(1.) });

                // Wide glyphs get their own segment so the forced one-cell advance doesn't misplace them.
                let extend = !wide && segs.last().is_some_and(|s| !s.wide && s.row == row && s.col + s.cells == col);
                if !extend {
                    segs.push(Segment { row, col, text: String::new(), runs: Vec::new(), cells: 0, wide });
                }
                let seg = segs.last_mut().unwrap();
                let start = seg.text.len();
                seg.text.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    seg.text.extend(zw);
                }
                seg.cells += width;
                let len = seg.text.len() - start;
                match seg.runs.last_mut() {
                    Some(r) if r.color == fg && r.font == run_font && r.underline == underline && r.strikethrough == strikethrough => r.len += len,
                    _ => seg.runs.push(TextRun { len, font: run_font, color: fg, background_color: None, underline, strikethrough }),
                }
            }

            if cur_visible && cur.shape != CursorShape::Block {
                let x = bounds.origin.x + m.cell_width * cur_at.1 as f32;
                let y = bounds.origin.y + m.line_height * cur_at.0 as f32;
                let (w, h, dy) = match cur.shape {
                    CursorShape::Beam => (px(2.), m.line_height, px(0.)),
                    CursorShape::Underline => (m.cell_width, px(2.), m.line_height - px(2.)),
                    _ => (m.cell_width, m.line_height, px(0.)), // HollowBlock: drawn as outline in paint
                };
                cursor = Some(Bounds::new(point(x, y + dy), size(w, h)));
            }
        } // term lock released before shaping

        let ts = window.text_system();
        let lines = segs
            .into_iter()
            .map(|s| {
                let force = (!s.wide).then_some(m.cell_width);
                (s.row, s.col, ts.shape_line(SharedString::from(s.text), m.font_size, &s.runs, force))
            })
            .collect();
        Prepainted { rects, lines, cursor, started }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        p: &mut Prepainted,
        window: &mut Window,
        cx: &mut App,
    ) {
        let m = &self.m;
        let o = bounds.origin;
        for &(row, col, w, color) in &p.rects {
            let x = (o.x + m.cell_width * col as f32).floor();
            let x2 = (o.x + m.cell_width * (col + w) as f32).ceil();
            let y = o.y + m.line_height * row as f32;
            window.paint_quad(fill(Bounds::new(point(x, y), size(x2 - x, m.line_height)), color));
        }
        for (row, col, line) in &p.lines {
            let origin = point(o.x + m.cell_width * *col as f32, o.y + m.line_height * *row as f32);
            let _ = line.paint(origin, m.line_height, TextAlign::Left, None, window, cx);
        }
        if let Some(c) = p.cursor {
            window.paint_quad(fill(c, palette::CURSOR));
        }
        crate::perf::term_paint(p.started);
    }
}
