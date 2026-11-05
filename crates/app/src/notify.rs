//! Desktop notifications for agent states, behind a small trait (Linux: freedesktop notifications over D-Bus;
//! Windows toasts can be another impl later).
//!
//! Everything D-Bus runs off the UI thread and only after the first notification: a command thread owns the
//! session connection (Notify / CloseNotification), a signal thread blocks on its socket for clicks. No timers,
//! no polling. No session bus or no daemon: one log line, then notifications are silently off.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use crate::agent::State;
use crate::persist::Notifications;

pub struct Notice {
    pub tab: u32,
    pub title: String,
    pub body: String,
    /// Needs you (vs. done): higher urgency.
    pub urgent: bool,
    pub sound: bool,
}

/// The notification of `tab` was clicked; `token` = the daemon's xdg-activation token, if it sent one.
pub struct Click {
    pub tab: u32,
    pub token: Option<String>,
}

pub trait Notifier {
    /// Shows `n`, replacing the tab's notification still on screen (never stacks).
    fn show(&self, n: Notice);
    /// Removes the tab's notification, if any (its session was opened).
    fn close(&self, tab: u32);
}

/// Whether a state change of a session is worth a notification. The session in front of you (focused window,
/// active tab) never is; "done" only when the window isn't focused at all.
pub fn should_notify(state: State, window_active: bool, active_tab: bool, settings: Notifications) -> bool {
    match state {
        State::NeedsYou => settings.needs_you && !(window_active && active_tab),
        State::Done => settings.done && !window_active,
        _ => false,
    }
}

/// The platform notifier and the clicks it reports. `icon`: an image file for the notification.
pub fn start(icon: PathBuf) -> (Box<dyn Notifier>, async_channel::Receiver<Click>) {
    let (clicks, rx) = async_channel::unbounded();
    #[cfg(target_os = "linux")]
    let n: Box<dyn Notifier> = Box::new(FreeDesktop { cmds: Mutex::new(None), icon, clicks });
    #[cfg(not(target_os = "linux"))]
    let n: Box<dyn Notifier> = {
        let _ = (icon, clicks);
        Box::new(Silent)
    };
    (n, rx)
}

#[cfg(not(target_os = "linux"))]
struct Silent;

#[cfg(not(target_os = "linux"))]
impl Notifier for Silent {
    fn show(&self, _: Notice) {}
    fn close(&self, _: u32) {}
}

enum Cmd {
    Show(Notice),
    Close(u32),
}

/// Which daemon notification belongs to which tab, and the activation token of a clicked one.
#[derive(Default)]
struct Ids {
    by_tab: HashMap<u32, u32>,
    tokens: HashMap<u32, String>,
}

impl Ids {
    fn tab_of(&self, id: u32) -> Option<u32> {
        self.by_tab.iter().find(|&(_, &v)| v == id).map(|(&t, _)| t)
    }

    fn forget(&mut self, id: u32) {
        self.by_tab.retain(|_, v| *v != id);
        self.tokens.remove(&id);
    }
}

struct FreeDesktop {
    /// None until the first notification starts the threads.
    cmds: Mutex<Option<mpsc::Sender<Cmd>>>,
    icon: PathBuf,
    clicks: async_channel::Sender<Click>,
}

impl Notifier for FreeDesktop {
    fn show(&self, n: Notice) {
        let mut cmds = self.cmds.lock().unwrap();
        let tx = cmds.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let (icon, clicks) = (self.icon.to_string_lossy().into_owned(), self.clicks.clone());
            let _ = std::thread::Builder::new().name("notify".into()).spawn(move || run(rx, icon, clicks));
            tx
        });
        // A failed send means the thread gave up (no daemon): stay quiet.
        let _ = tx.send(Cmd::Show(n));
    }

    fn close(&self, tab: u32) {
        if let Some(tx) = self.cmds.lock().unwrap().as_ref() {
            let _ = tx.send(Cmd::Close(tab));
        }
    }
}

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// The command thread. Returns (dropping the receiver, so later commands are no-ops) on the first D-Bus error.
fn run(rx: mpsc::Receiver<Cmd>, icon: String, clicks: async_channel::Sender<Click>) {
    // The app icon for the daemon to show (written once, into the runtime dir removed on quit).
    if let Some(dir) = std::path::Path::new(&icon).parent() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&icon, crate::assets::APP_ICON_SVG);
    }
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => c,
        Err(e) => return eprintln!("dirigent: desktop notifications off (no session bus: {e})"),
    };
    let ids = Arc::new(Mutex::new(Ids::default()));
    let (conn2, ids2) = (conn.clone(), ids.clone());
    let _ = std::thread::Builder::new().name("notify-signals".into()).spawn(move || listen(conn2, ids2, clicks));
    for cmd in rx {
        match cmd {
            Cmd::Show(n) => {
                let replaces = ids.lock().unwrap().by_tab.get(&n.tab).copied().unwrap_or(0);
                match notify(&conn, &icon, replaces, &n) {
                    Ok(id) => {
                        ids.lock().unwrap().by_tab.insert(n.tab, id);
                    }
                    Err(e) => return eprintln!("dirigent: desktop notifications off ({e})"),
                }
            }
            Cmd::Close(tab) => {
                let id = ids.lock().unwrap().by_tab.remove(&tab);
                if let Some(id) = id {
                    let _ = conn.call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(id,));
                }
            }
        }
    }
}

fn notify(conn: &zbus::blocking::Connection, icon: &str, replaces: u32, n: &Notice) -> zbus::Result<u32> {
    use zbus::zvariant::Value;
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from("dirigent"));
    hints.insert("urgency", Value::from(if n.urgent { 1u8 } else { 0u8 }));
    if n.sound {
        hints.insert("sound-name", Value::from("message-new-instant"));
    } else {
        hints.insert("suppress-sound", Value::from(true));
    }
    // GNOME renders body markup: keep the text literal.
    let body = n.body.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let actions = ["default", "Open"];
    let reply = conn.call_method(
        Some(DEST),
        PATH,
        Some(DEST),
        "Notify",
        &("Dirigent", replaces, icon, n.title.as_str(), body.as_str(), &actions[..], hints, -1i32),
    )?;
    reply.body().deserialize::<u32>()
}

/// The signal thread: clicks (with the activation token GNOME sends right before) and closes.
fn listen(conn: zbus::blocking::Connection, ids: Arc<Mutex<Ids>>, clicks: async_channel::Sender<Click>) {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(DEST)
        .and_then(|b| b.path(PATH))
        .map(|b| b.build());
    let Ok(rule) = rule else { return };
    let Ok(iter) = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, None) else { return };
    for msg in iter.flatten() {
        let header = msg.header();
        let body = msg.body();
        match header.member().map(|m| m.as_str()) {
            Some("ActivationToken") => {
                if let Ok((id, token)) = body.deserialize::<(u32, String)>() {
                    ids.lock().unwrap().tokens.insert(id, token);
                }
            }
            Some("ActionInvoked") => {
                let Ok((id, key)) = body.deserialize::<(u32, String)>() else { continue };
                let mut ids = ids.lock().unwrap();
                if let (Some(tab), true) = (ids.tab_of(id), key == "default") {
                    let token = ids.tokens.remove(&id);
                    ids.forget(id);
                    if clicks.send_blocking(Click { tab, token }).is_err() {
                        return;
                    }
                }
            }
            Some("NotificationClosed") => {
                if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>() {
                    ids.lock().unwrap().forget(id);
                }
            }
            _ => {}
        }
    }
}

/// Brings the window to the front. On Wayland a token from the notification daemon (your click) lets Mutter
/// raise and focus it; gpui's own `activate_window` asks for a token without an input serial, which Mutter
/// refuses for an unfocused window ("Dirigent is ready"). gpui has no API for a foreign token, so the token goes
/// out over gpui's own `wl_display`. X11, or no token: `activate_window`.
pub fn raise(window: &gpui::Window, token: Option<&str>) {
    #[cfg(target_os = "linux")]
    if let Some(token) = token
        && wayland::activate(window, token).is_some()
    {
        return crate::perf::event("raise: xdg-activation with the daemon's token");
    }
    crate::perf::event("raise: activate_window");
    window.activate_window();
}

#[cfg(target_os = "linux")]
mod wayland {
    use std::cell::RefCell;

    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_registry::{self, WlRegistry};
    use wayland_client::protocol::wl_surface::WlSurface;
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
    use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::{self, XdgActivationV1};

    struct Act;

    impl Dispatch<WlRegistry, GlobalListContents> for Act {
        fn event(_: &mut Self, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<XdgActivationV1, ()> for Act {
        fn event(_: &mut Self, _: &XdgActivationV1, _: xdg_activation_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    thread_local! {
        /// Our queue on gpui's display and the bound activation global (UI thread only, bound once).
        static BOUND: RefCell<Option<(Connection, EventQueue<Act>, XdgActivationV1)>> = const { RefCell::new(None) };
    }

    /// `xdg_activation_v1.activate(token, <the window's surface>)`; None when not on Wayland or on any error.
    pub fn activate(window: &gpui::Window, token: &str) -> Option<()> {
        let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else { return None };
        let RawWindowHandle::Wayland(surface) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
        BOUND.with_borrow_mut(|bound| {
            if bound.is_none() {
                // Safety: gpui's wl_display lives as long as the app (and so as this thread-local's users).
                let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
                let conn = Connection::from_backend(backend);
                // One registry round trip on our own queue; gpui's queue isn't touched.
                let (globals, queue) = registry_queue_init::<Act>(&conn).ok()?;
                let activation: XdgActivationV1 = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
                *bound = Some((conn, queue, activation));
            }
            let (conn, _, activation) = bound.as_ref()?;
            // Safety: the surface belongs to the live window we were handed.
            let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast()) }.ok()?;
            let surface = WlSurface::from_id(conn, id).ok()?;
            activation.activate(token.to_string(), &surface);
            conn.flush().ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_rules() {
        let on = Notifications::default();
        // (state, window focused, active tab) → notify?
        let cases = [
            (State::NeedsYou, true, true, false),
            (State::NeedsYou, true, false, true),
            (State::NeedsYou, false, true, true),
            (State::NeedsYou, false, false, true),
            (State::Done, true, true, false),
            (State::Done, true, false, false),
            (State::Done, false, true, true),
            (State::Done, false, false, true),
            (State::Working, false, false, false),
        ];
        for (state, focused, active, want) in cases {
            assert_eq!(should_notify(state, focused, active, on), want, "{state:?} focused={focused} active={active}");
        }
        let off = Notifications { needs_you: false, done: false, sound: false };
        assert!(!should_notify(State::NeedsYou, false, false, off));
        assert!(!should_notify(State::Done, false, false, off));
    }

    /// No session bus: showing returns at once, nothing panics, later calls are no-ops.
    #[test]
    fn no_daemon_is_silent() {
        // SAFETY of set_var: this test binary doesn't read the variable elsewhere concurrently that matters; the
        // notifier thread reads it once at connect.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/dirigent-test") };
        let (n, clicks) = start(PathBuf::from("/nonexistent.svg"));
        let t = std::time::Instant::now();
        for tab in 0..3 {
            n.show(Notice { tab, title: "t".into(), body: "b".into(), urgent: true, sound: false });
            n.close(tab);
        }
        assert!(t.elapsed() < std::time::Duration::from_millis(100), "show/close never block the caller");
        n.show(Notice { tab: 9, title: "t".into(), body: "b".into(), urgent: false, sound: false });
        assert!(clicks.try_recv().is_err());
    }
}
