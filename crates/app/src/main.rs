mod agent;
mod line_edit;
mod perf;
mod terminal_element;
mod terminal_view;
mod window_chrome;
mod workspace;

use gpui::{App, Bounds, KeyBinding, WindowBounds, WindowDecorations, WindowOptions, prelude::*, px, size};
use workspace::*;

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let ctx = Some("Workspace");
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-t", NewSession, ctx),
            KeyBinding::new("ctrl-shift-w", CloseSession, ctx),
            KeyBinding::new("ctrl-tab", NextSession, ctx),
            KeyBinding::new("ctrl-shift-tab", PrevSession, ctx),
            KeyBinding::new("f2", Rename, ctx),
            KeyBinding::new("ctrl-shift-b", ToggleSidebar, ctx),
            KeyBinding::new("ctrl-shift-j", NextNeedsYou, ctx),
            KeyBinding::new("ctrl-1", Activate1, ctx),
            KeyBinding::new("ctrl-2", Activate2, ctx),
            KeyBinding::new("ctrl-3", Activate3, ctx),
            KeyBinding::new("ctrl-4", Activate4, ctx),
            KeyBinding::new("ctrl-5", Activate5, ctx),
            KeyBinding::new("ctrl-6", Activate6, ctx),
            KeyBinding::new("ctrl-7", Activate7, ctx),
            KeyBinding::new("ctrl-8", Activate8, ctx),
            KeyBinding::new("ctrl-9", Activate9, ctx),
        ]);
        let bounds = Bounds::centered(None, size(px(1200.), px(750.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Server-side where the platform has them (X11, KDE); GNOME Wayland falls back to client-side and
                // the workspace draws its own title bar (window_chrome.rs).
                window_decorations: Some(WindowDecorations::Server),
                app_id: Some("agent-ide".into()),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Workspace::new(window, cx)),
        )
        .expect("open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
