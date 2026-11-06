//! Whether the compositor will blur behind a transparent window. gpui requests blur through
//! `ext_background_effect_manager_v1` or `org_kde_kwin_blur_manager` and silently does nothing without them (and
//! X11 has no blur at all), so we check the Wayland globals ourselves. Without blur the sidebar stays opaque:
//! plain transparency hurts readability.

const BLUR_GLOBALS: [&str; 2] = ["ext_background_effect_manager_v1", "org_kde_kwin_blur_manager"];

/// `DIRIGENT_BLUR=force|off` overrides; otherwise blur iff one of the protocols is advertised.
pub fn decide(env: Option<&str>, globals: impl FnOnce() -> Vec<String>) -> bool {
    match env {
        Some("force") => true,
        Some("off") => false,
        _ => globals().iter().any(|g| BLUR_GLOBALS.contains(&g.as_str())),
    }
}

/// Interfaces the Wayland compositor advertises (empty on X11 or on error). One registry round trip (~1 ms).
pub fn wayland_globals() -> Vec<String> {
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_registry::{self, WlRegistry};
    use wayland_client::{Connection, Dispatch, QueueHandle};

    struct Probe;
    impl Dispatch<WlRegistry, GlobalListContents> for Probe {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    // gpui picks Wayland exactly when WAYLAND_DISPLAY is set and non-empty.
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|d| d.is_empty()) {
        return Vec::new();
    }
    let Ok(conn) = Connection::connect_to_env() else { return Vec::new() };
    let Ok((globals, _queue)) = registry_queue_init::<Probe>(&conn) else { return Vec::new() };
    globals.contents().clone_list().into_iter().map(|g| g.interface).collect()
}

#[cfg(test)]
mod tests {
    use super::decide;

    #[test]
    fn blur_decision() {
        let gnome = || vec!["wl_compositor".to_string(), "xdg_wm_base".into(), "zwp_linux_dmabuf_v1".into()];
        let kde = || vec!["wl_compositor".to_string(), "org_kde_kwin_blur_manager".into()];
        let newer = || vec!["ext_background_effect_manager_v1".to_string()];
        assert!(!decide(None, gnome));
        assert!(decide(None, kde));
        assert!(decide(None, newer));
        assert!(decide(Some("force"), gnome));
        assert!(!decide(Some("off"), kde));
    }
}
