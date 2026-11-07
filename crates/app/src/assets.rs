//! Fonts and icons compiled into the binary, so the app looks the same everywhere and reads nothing from disk.
//! Licenses: Inter and JetBrains Mono under the SIL OFL 1.1, Lucide icons under ISC (texts in `assets/`).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*]
    };
}

/// The app icon (original; see assets/icon): notifications, `--install-desktop`.
#[cfg(target_os = "linux")]
pub const APP_ICON_SVG: &[u8] = include_bytes!("../assets/icon/dirigent.svg");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon/dirigent-256.png");

static ICONS: &[(&str, &[u8])] = icons![
    "check",
    "trash-2",
    "git-pull-request",
    "external-link",
    "chevron-right",
    "chevron-down",
    "chevron-up",
    "plus",
    "x",
    "minus",
    "square",
    "copy",
    "panel-left-close",
    "panel-right-close",
    "list",
    "files",
    "git-compare",
    "circle-check",
    "loader-circle",
    "moon",
    "triangle-alert",
    "git-branch",
    "refresh-cw",
    "rotate-ccw",
    "eye",
    "eye-off",
    "folder",
    "folder-open",
    "file",
    "file-code",
    "file-text",
    "file-json",
    "file-cog",
    "file-lock",
    "file-image",
    "file-terminal",
    "file-archive",
    "terminal",
    "users",
    "square-terminal",
    "sparkles",
    "pin",
    "ellipsis",
    "settings",
    "search",
];

static FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(p, _)| *p == path).map(|(_, b)| Cow::Borrowed(*b)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| (*p).into()).collect())
    }
}

/// For `cx.text_system().add_fonts(..)` before the first window opens.
pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    FONTS.iter().map(|b| Cow::Borrowed(*b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_and_font_loads() {
        for (path, bytes) in ICONS {
            assert!(bytes.starts_with(b"<!--") || bytes.starts_with(b"<svg"), "{path}");
            assert!(Assets.load(path).unwrap().is_some());
        }
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        for f in FONTS {
            assert_eq!(&f[..4], &[0, 1, 0, 0], "TrueType header");
        }
        // Every icon the UI names exists.
        for name in crate::ui::ALL_ICONS {
            assert!(Assets.load(name).unwrap().is_some(), "{name}");
        }
    }
}
