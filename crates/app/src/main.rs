mod agent;
mod assets;
mod blur;
mod file_tree;
mod files_panel;
mod git_panel;
mod line_edit;
mod perf;
mod persist;
mod scrollbar;
mod terminal_element;
mod theme;
mod ui;
mod terminal_view;
mod window_chrome;
mod workspace;

use gpui::{
    App, Bounds, KeyBinding, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions, point, prelude::*,
    px, size,
};
use workspace::*;

fn main() {
    perf::process_started();
    // The only synchronous file IO: a small file, read before any window exists.
    let state_path = persist::path();
    let saved = persist::load(&state_path);
    let translucent = blur::decide(std::env::var("AGENT_IDE_BLUR").ok().as_deref(), blur::wayland_globals);
    let t = theme::get();
    let rgb8 = |c: gpui::Rgba| [(c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8];
    session::set_reported_colors(rgb8(t.terminal.fg), rgb8(t.terminal.bg));
    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(assets::fonts()) {
            eprintln!("agent-ide: bundled fonts failed to load: {e}");
        }
        let ctx = Some("Workspace");
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-t", NewSession, ctx),
            KeyBinding::new("ctrl-shift-w", CloseSession, ctx),
            KeyBinding::new("ctrl-tab", NextSession, ctx),
            KeyBinding::new("ctrl-shift-tab", PrevSession, ctx),
            KeyBinding::new("f2", Rename, ctx),
            KeyBinding::new("ctrl-shift-b", ToggleSidebar, ctx),
            KeyBinding::new("ctrl-shift-j", NextNeedsYou, ctx),
            KeyBinding::new("ctrl-shift-r", ResumeAll, ctx),
            KeyBinding::new("ctrl-shift-g", ShowChanges, ctx),
            KeyBinding::new("ctrl-shift-e", ShowFiles, ctx),
            KeyBinding::new("ctrl-shift-d", ToggleDiffMode, ctx),
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
        // Wayland doesn't let clients place windows: only size + maximized/fullscreen take effect there.
        let window_bounds = match saved.window {
            Some(w) => {
                let b = Bounds { origin: point(px(w.x), px(w.y)), size: size(px(w.w.max(400.)), px(w.h.max(300.))) };
                match w.mode {
                    persist::WindowMode::Windowed => WindowBounds::Windowed(b),
                    persist::WindowMode::Maximized => WindowBounds::Maximized(b),
                    persist::WindowMode::Fullscreen => WindowBounds::Fullscreen(b),
                }
            }
            None => WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(750.)), cx)),
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                // Server-side where the platform has them (X11, KDE); GNOME Wayland falls back to client-side and
                // the workspace draws its own title bar (window_chrome.rs).
                window_decorations: Some(WindowDecorations::Server),
                window_background: if translucent {
                    WindowBackgroundAppearance::Blurred
                } else {
                    WindowBackgroundAppearance::Opaque
                },
                app_id: Some("agent-ide".into()),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Workspace::new(saved.clone(), state_path.clone(), translucent, window, cx)),
        )
        .expect("open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
