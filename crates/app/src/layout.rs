//! Which dock (left / right of the terminal) holds each panel, and the docks' widths. Pure: settings.json keeps it
//! (`layout` section), the workspace draws it.

use serde::{Deserialize, Serialize};

/// A panel. Serde names are the state file's old `PanelTab` names (+ `sessions`), so old files load.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    Sessions,
    Files,
    #[default]
    Changes,
    Search,
}

/// Order of the tabs inside a dock.
pub const PANELS: [Panel; 4] = [Panel::Sessions, Panel::Files, Panel::Changes, Panel::Search];

impl Panel {
    pub fn label(self) -> &'static str {
        match self {
            Panel::Sessions => "Sessions",
            Panel::Files => "Files",
            Panel::Changes => "Changes",
            Panel::Search => "Search",
        }
    }

    /// Narrowest a dock showing it may get (at 100 %).
    pub fn min_width(self) -> f32 {
        match self {
            Panel::Sessions => 180.,
            _ => crate::git_panel::MIN_WIDTH,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Dock {
    Left,
    Right,
}

impl Dock {
    pub fn ix(self) -> usize {
        self as usize
    }

    pub fn other(self) -> Dock {
        match self {
            Dock::Left => Dock::Right,
            Dock::Right => Dock::Left,
        }
    }
}

/// Dock widths settings.json accepts (at 100 %; drawn clamped to the panels' minimum).
pub const WIDTH: (u32, u32) = (100, 2000);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub sessions: Dock,
    pub files: Dock,
    pub changes: Dock,
    pub search: Dock,
    pub left_width: u32,
    pub right_width: u32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sessions: Dock::Left,
            files: Dock::Right,
            changes: Dock::Right,
            search: Dock::Right,
            left_width: 220,
            right_width: 340,
        }
    }
}

impl Layout {
    pub fn dock_of(&self, p: Panel) -> Dock {
        match p {
            Panel::Sessions => self.sessions,
            Panel::Files => self.files,
            Panel::Changes => self.changes,
            Panel::Search => self.search,
        }
    }

    pub fn set_dock(&mut self, p: Panel, d: Dock) {
        *match p {
            Panel::Sessions => &mut self.sessions,
            Panel::Files => &mut self.files,
            Panel::Changes => &mut self.changes,
            Panel::Search => &mut self.search,
        } = d;
    }

    pub fn panels(&self, d: Dock) -> impl Iterator<Item = Panel> + use<> {
        let l = *self;
        PANELS.into_iter().filter(move |p| l.dock_of(*p) == d)
    }

    pub fn width(&self, d: Dock) -> u32 {
        match d {
            Dock::Left => self.left_width,
            Dock::Right => self.right_width,
        }
    }

    pub fn set_width(&mut self, d: Dock, w: u32) {
        match d {
            Dock::Left => self.left_width = w,
            Dock::Right => self.right_width = w,
        }
    }

    /// The panel a dock shows: `wanted` if it lives there, else the dock's first one (None: empty dock).
    pub fn shown(&self, d: Dock, wanted: Panel) -> Option<Panel> {
        if self.dock_of(wanted) == d { Some(wanted) } else { self.panels(d).next() }
    }

    /// Narrowest the dock may be: the widest minimum of its panels.
    pub fn min_width(&self, d: Dock) -> f32 {
        self.panels(d).map(Panel::min_width).fold(0., f32::max)
    }

    /// Width the dock takes beside the terminal (at 100 %, divider included): open → its width (`live`, clamped
    /// to its minimum) + the divider; collapsed → the expand strip; empty → 0 (not drawn at all).
    pub fn taken(&self, d: Dock, open: bool, live: f32, divider: f32, strip: f32) -> f32 {
        if self.panels(d).next().is_none() {
            0.
        } else if open {
            live.max(self.min_width(d)) + divider
        } else {
            strip
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_docks_and_widths() {
        let mut l = Layout::default();
        assert_eq!(l.panels(Dock::Left).collect::<Vec<_>>(), [Panel::Sessions]);
        assert_eq!(l.panels(Dock::Right).collect::<Vec<_>>(), [Panel::Files, Panel::Changes, Panel::Search]);
        // Files on the left, Sessions on the right.
        l.set_dock(Panel::Files, Dock::Left);
        l.set_dock(Panel::Sessions, Dock::Right);
        assert_eq!(l.panels(Dock::Left).collect::<Vec<_>>(), [Panel::Files]);
        assert_eq!(l.panels(Dock::Right).collect::<Vec<_>>(), [Panel::Sessions, Panel::Changes, Panel::Search]);
        // The active panel moved away: the dock's first one; an empty dock shows nothing.
        assert_eq!(l.shown(Dock::Left, Panel::Sessions), Some(Panel::Files));
        assert_eq!(l.shown(Dock::Right, Panel::Search), Some(Panel::Search));
        for p in PANELS {
            l.set_dock(p, Dock::Right);
        }
        assert_eq!(l.shown(Dock::Left, Panel::Files), None);
        // Minimum: the widest of its panels.
        let d = Layout::default();
        assert_eq!(d.min_width(Dock::Left), 180.);
        assert_eq!(d.min_width(Dock::Right), crate::git_panel::MIN_WIDTH);
        // What the docks take beside the terminal.
        assert_eq!(d.taken(Dock::Left, true, 220., 4., 10.), 224.);
        assert_eq!(d.taken(Dock::Left, true, 50., 4., 10.), 184., "clamped to the minimum");
        assert_eq!(d.taken(Dock::Left, false, 220., 4., 10.), 10., "collapsed Sessions: the strip");
        assert_eq!(d.taken(Dock::Right, false, 340., 4., 10.), 10., "every collapsed dock keeps its strip");
        assert_eq!(l.taken(Dock::Left, true, 220., 4., 10.), 0., "empty dock: nothing");
        assert_eq!(l.taken(Dock::Right, false, 340., 4., 10.), 10.);
    }

    #[test]
    fn old_state_names_still_parse() {
        for (name, p) in [("\"changes\"", Panel::Changes), ("\"files\"", Panel::Files), ("\"search\"", Panel::Search)] {
            assert_eq!(serde_json::from_str::<Panel>(name).unwrap(), p);
        }
    }
}
