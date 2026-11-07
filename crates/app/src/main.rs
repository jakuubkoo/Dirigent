// Windows: a GUI app (no console window); `win::attach_console` lends it the caller's console for CLI messages.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod agent;
mod assets;
mod blur;
mod changes;
mod cli;
mod commit_box;
mod diff_syntax;
mod editor;
mod file_tree;
mod files_panel;
mod git_ops;
mod git_panel;
mod history;
mod keys;
mod layout;
mod line_edit;
mod notify;
mod perf;
mod persist;
mod pr;
mod projects;
mod quick_open;
mod scrollbar;
mod search_panel;
mod settings;
mod settings_view;
#[cfg(windows)]
mod shell;
mod terminal_element;
mod terminal_view;
mod text_area;
mod theme;
mod ui;
mod window_chrome;
mod workspace;

use gpui::{
    App, Bounds, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions, point, prelude::*, px,
    size,
};
use workspace::*;

use std::path::PathBuf;

fn main() {
    #[cfg(windows)]
    win::attach_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        // `DIRIGENT_BUILD` ("<hash> <date>") is set by scripts/install.sh.
        println!("Dirigent {} ({})", env!("CARGO_PKG_VERSION"), option_env!("DIRIGENT_BUILD").unwrap_or("dev build"));
        return;
    }
    #[cfg(target_os = "linux")]
    if let (Some("--desktop-entry"), Some(exec)) = (args.first().map(String::as_str), args.get(1)) {
        // For packaging (scripts/build-deb.sh): the launcher entry for the app at `exec`.
        print!("{}", desktop_text(exec));
        return;
    }
    #[cfg(target_os = "linux")]
    if let Some(flag @ ("--install-desktop" | "--uninstall-desktop")) = args.first().map(String::as_str) {
        if let Err(e) = desktop_entry(flag == "--install-desktop") {
            eprintln!("dirigent: {e}");
            std::process::exit(1);
        }
        return;
    }
    let fail = |code, msg: String| -> ! {
        eprintln!("dirigent: {msg}");
        std::process::exit(code)
    };
    persist::migrate_old_state();
    let cmd = cli::parse(&args).unwrap_or_else(|e| fail(2, format!("{e}\n\n{}", cli::USAGE)));
    if cmd.help {
        print!("{}", cli::USAGE);
        return;
    }
    // The caller's activation token is for raising our window, once: our sessions must not inherit it.
    let token = ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|t| !t.is_empty()));
    // SAFETY: still single-threaded (nothing spawned yet).
    unsafe {
        std::env::remove_var("XDG_ACTIVATION_TOKEN");
        std::env::remove_var("DESKTOP_STARTUP_ID");
    }
    let cwd = std::env::current_dir().unwrap_or_else(|e| fail(1, format!("no current directory: {e}")));
    let req = cli::request(&cmd, &cwd, token).unwrap_or_else(|e| fail(1, e));
    let state_path = persist::path();
    let override_state = std::env::var_os("DIRIGENT_STATE").map(PathBuf::from);
    let sock = cli::socket_path(&agent::runtime_parent(), override_state.as_deref());
    // Already running: hand it the request and leave.
    let send = |req: &cli::Request| match cli::send(&sock, req) {
        Ok(cli::Sent::Done) => std::process::exit(0),
        Ok(cli::Sent::NotRunning) => {}
        Err(e) => fail(1, e),
    };
    send(&req);
    // We are the app (not a request to a running one): two installs side by side get a hint, in the terminal and
    // the log.
    if let (Ok(exe), Some(home)) = (std::env::current_exe(), std::env::var_os("HOME").map(PathBuf::from)) {
        let exists = |p: &std::path::Path| p.exists();
        if let Some(hint) =
            cli::install_hint(&exe, &home, exists(&cli::local_exe(&home)), exists(cli::SYSTEM_EXE.as_ref()))
        {
            eprintln!("{hint}");
        }
    }
    #[cfg(unix)]
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        // Started from a terminal: the app runs on its own (own process group, output to a log), the shell returns.
        detach(&state_path);
    }
    // A GUI app returns to the terminal at once anyway; its output (incl. DIRIGENT_PERF) goes to the log.
    #[cfg(windows)]
    win::log_to(&state_path);
    let listener = match cli::bind(&sock) {
        Ok(l) => Some(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            send(&req); // another instance started meanwhile
            None
        }
        Err(e) => {
            eprintln!("dirigent: no single-instance socket ({}): {e}", sock.display());
            None
        }
    };
    let launch = cli::Launch { req, listener, sock };
    perf::process_started();
    // The only synchronous file IO: a small file, read before any window exists.
    let state_path = persist::path();
    let saved = persist::load(&state_path);
    // Before the state is saved again: a first start takes the preferences over from the old state file.
    let settings_path = settings::path();
    let mut loaded = settings::load(&settings_path, &state_path);
    // Dock widths moved from the state file into settings.json: taken over once.
    if settings::adopt_widths(
        &mut loaded.parsed.settings,
        loaded.text.as_deref(),
        saved.sidebar.width,
        saved.git_panel.width,
    ) {
        let text = loaded.parsed.settings.to_json();
        match settings::save(&settings_path, &text, false) {
            Ok(()) => loaded.text = Some(text),
            Err(e) => eprintln!("dirigent: couldn't write {}: {e}", settings_path.display()),
        }
    }
    let translucent = blur::decide(std::env::var("DIRIGENT_BLUR").ok().as_deref(), blur::wayland_globals);
    let t = theme::get();
    let rgb8 = |c: gpui::Rgba| [(c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8];
    session::set_reported_colors(rgb8(t.terminal.fg), rgb8(t.terminal.bg));
    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(assets::fonts()) {
            eprintln!("dirigent: bundled fonts failed to load: {e}");
        }
        keys::bind(cx);
        // Wayland doesn't let clients place windows: only size + maximized/fullscreen take effect there.
        let window_bounds = match saved.window {
            Some(w) => {
                let b = Bounds { origin: point(px(w.x), px(w.y)), size: size(px(w.w.max(400.)), px(w.h.max(300.))) }; // unscaled: the compositor's window size
                match w.mode {
                    persist::WindowMode::Windowed => WindowBounds::Windowed(b),
                    persist::WindowMode::Maximized => WindowBounds::Maximized(b),
                    persist::WindowMode::Fullscreen => WindowBounds::Fullscreen(b),
                }
            }
            None => WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(750.)), cx)), // unscaled: window size
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
                app_id: Some("dirigent".into()),
                // X11 only; on Wayland the dock takes the icon from dirigent.desktop (`--install-desktop`).
                icon: image::load_from_memory(assets::APP_ICON_PNG).ok().map(|i| std::sync::Arc::new(i.into_rgba8())),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    Workspace::new(
                        saved.clone(),
                        state_path.clone(),
                        (settings_path, loaded),
                        translucent,
                        launch,
                        window,
                        cx,
                    )
                })
            },
        )
        .expect("open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

/// Re-runs this binary detached (own process group: Ctrl+C or closing the terminal don't reach it), stdout/stderr
/// appended to `dirigent.log` next to the state file (moved to `.log.old` past 1 MB), then exits.
#[cfg(unix)]
fn detach(state_path: &std::path::Path) -> ! {
    use std::os::unix::process::CommandExt;
    let started = open_log(state_path).and_then(|out| {
        std::process::Command::new(std::env::current_exe()?)
            .args(std::env::args_os().skip(1))
            .stdin(std::process::Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .process_group(0)
            .spawn()
    });
    match started {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            eprintln!("dirigent: couldn't start in the background: {e}");
            std::process::exit(1)
        }
    }
}

/// `dirigent.log` next to the state file, appended to (moved to `.log.old` past 1 MB).
fn open_log(state_path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let dir = state_path.parent().unwrap_or(std::path::Path::new("."));
    let log = dir.join("dirigent.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&log, dir.join("dirigent.log.old"));
    }
    std::fs::create_dir_all(dir)?;
    std::fs::OpenOptions::new().create(true).append(true).open(&log)
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };

    /// Started from a console (`dirigent <dir>` in a terminal): our messages (usage, errors) go there.
    pub fn attach_console() {
        // SAFETY: no arguments to get wrong; fails harmlessly when there is no parent console.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    /// From now on stdout/stderr go to `dirigent.log` (std looks the handles up on every write).
    pub fn log_to(state_path: &std::path::Path) {
        match super::open_log(state_path) {
            Ok(f) => {
                let h = f.into_raw_handle(); // kept open for the process's lifetime
                // SAFETY: a valid file handle we own and never close.
                unsafe {
                    SetStdHandle(STD_OUTPUT_HANDLE, h);
                    SetStdHandle(STD_ERROR_HANDLE, h);
                }
            }
            Err(e) => eprintln!("dirigent: no log file: {e}"),
        }
    }
}

/// The launcher entry starting the app at `exec` (`--install-desktop`, and the .deb via `--desktop-entry`).
#[cfg(target_os = "linux")]
fn desktop_text(exec: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Dirigent\nComment=Many Claude Code sessions in one window\n\
         Exec=\"{exec}\"\nIcon=dirigent\nTerminal=false\nCategories=Development;\nStartupWMClass=dirigent\n"
    )
}

/// `--install-desktop`: `~/.local/share/applications/dirigent.desktop` (launching this binary) and the icon, so
/// GNOME names notifications and shows the dock icon; `--uninstall-desktop` removes both. Nothing else ever writes
/// to your home directory.
#[cfg(target_os = "linux")]
fn desktop_entry(install: bool) -> std::io::Result<()> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| std::io::Error::other("no HOME"))?;
    let entry = data.join("applications/dirigent.desktop");
    let icon = data.join("icons/hicolor/scalable/apps/dirigent.svg");
    if !install {
        for p in [&entry, &icon] {
            match std::fs::remove_file(p) {
                Ok(()) => println!("removed {}", p.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        return Ok(());
    }
    let body = desktop_text(&std::env::current_exe()?.display().to_string());
    for (path, bytes) in [(&entry, body.as_bytes()), (&icon, assets::APP_ICON_SVG)] {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, bytes)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
