//! Editor harnesses (with `DIRIGENT_PERF=1` and `DIRIGENT_PERF_OPEN=<file>`; scratch copies only, files get written):
//! - `DIRIGENT_PERF_TYPE=<text>` (`\n` = Enter): types one char per 50 ms through the editor's key path into the
//!   opened file, then into `DIRIGENT_PERF_TYPE_ALSO=<file>` if set; logs renders per key, key → paint latency and
//!   renders in 2 s idle. Never saves.
//! - `DIRIGENT_PERF_CHECK=<file>`: type, Ctrl+S, an external write (silent reload + notice), Ctrl+Z (must not revert
//!   it), an external write over unsaved edits (banner), Ctrl+S (conflict), Overwrite. Logs `[check] ✓/✗ …`.
//! - `DIRIGENT_PERF_GUARD=<dir>` (needs 2+ sessions, e.g. `DIRIGENT_RUN=$':\n:'`; the dir holds `plain.txt`,
//!   `crlf.txt` (CRLF), `bin.dat` (binary), `huge.rs` (over the limit)): read-only files, a CRLF edit + save keeping
//!   CRLF, and the unsaved-edits dialog on Ctrl+Tab, closing another session and the window's x (Cancel keeps the
//!   edits, Discard goes on).

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AsyncWindowContext, WeakEntity};

use super::{Modal, Overlay, Then, Workspace};
use crate::editor::ReadOnly;
use crate::files_panel::FileView;

async fn sleep(cx: &mut AsyncWindowContext, ms: u64) {
    cx.background_executor().timer(Duration::from_millis(ms)).await;
}

fn with_file<R>(
    this: &WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
    f: impl FnOnce(&mut FileView, &mut gpui::Context<FileView>) -> R,
) -> Option<R> {
    this.update(cx, |w, cx| match &w.overlay {
        Some(Overlay::File(v)) => Some(v.update(cx, f)),
        _ => None,
    })
    .ok()
    .flatten()
}

/// Opens `path` in the editor (dropping the open one's edits) and waits for it.
async fn open(this: &WeakEntity<Workspace>, cx: &mut AsyncWindowContext, path: &Path) -> bool {
    let p = path.to_path_buf();
    let _ = this.update_in(cx, |w, window, cx| {
        if let Some(Overlay::File(f)) = &w.overlay {
            f.update(cx, |f, _| f.forget_edits());
        }
        w.open_file(p, false, window, cx);
    });
    for _ in 0..100 {
        sleep(cx, 50).await;
        if with_file(this, cx, |f, _| f.editable() && f.path == path) == Some(true) {
            sleep(cx, 1000).await; // colors, first frames
            return true;
        }
    }
    eprintln!("[perf] {} didn't open in the editor", path.display());
    false
}

async fn type_and_measure(this: &WeakEntity<Workspace>, cx: &mut AsyncWindowContext, text: &str, label: &str) {
    crate::perf::take_key_latencies();
    let renders = crate::perf::editor_renders();
    let n = text.chars().count();
    for ch in text.chars() {
        with_file(this, cx, |f, cx| f.perf_type(ch, cx));
        sleep(cx, 50).await;
    }
    let typed = crate::perf::editor_renders() - renders;
    let mut ms = crate::perf::take_key_latencies();
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms.get(((ms.len() as f64 - 1.) * q).round() as usize).copied().unwrap_or(0.);
    eprintln!(
        "[perf] {label}: {n} keys, {typed} renders ({:.2}/key); key → paint median {:.2} ms, p95 {:.2} ms, max {:.2} ms ({} measured)",
        typed as f64 / n.max(1) as f64,
        at(0.5),
        at(0.95),
        at(1.),
        ms.len()
    );
    let idle = crate::perf::editor_renders();
    sleep(cx, 2000).await;
    eprintln!("[perf] {label}: idle 2 s after typing: {} renders", crate::perf::editor_renders() - idle);
}

pub(super) async fn run(this: WeakEntity<Workspace>, cx: &mut AsyncWindowContext) {
    if let Ok(text) = std::env::var("DIRIGENT_PERF_TYPE") {
        let text = text.replace("\\n", "\n");
        let first = with_file(&this, cx, |f, _| f.path.clone());
        let also = std::env::var("DIRIGENT_PERF_TYPE_ALSO").ok().map(PathBuf::from);
        for path in first.into_iter().chain(also) {
            if open(&this, cx, &path).await {
                let lines = with_file(&this, cx, |f, _| f.loaded_rows()).unwrap_or(0);
                let label = format!("{} ({lines} lines)", path.file_name().unwrap_or_default().to_string_lossy());
                type_and_measure(&this, cx, &text, &label).await;
            }
        }
    }
    if let Some(path) = std::env::var_os("DIRIGENT_PERF_CHECK").map(PathBuf::from) {
        check(&this, cx, &path).await;
    }
    if let Some(dir) = std::env::var_os("DIRIGENT_PERF_GUARD").map(PathBuf::from) {
        guard(&this, cx, &dir).await;
    }
}

fn ok(cond: bool, what: &str) {
    eprintln!("[check] {} {what}", if cond { "✓" } else { "✗" });
}

/// Opens a file that must stay read-only (`open` waits for an editor, so this waits for the content instead).
async fn read_only(this: &WeakEntity<Workspace>, cx: &mut AsyncWindowContext, path: &Path) -> Option<ReadOnly> {
    let p = path.to_path_buf();
    let _ = this.update_in(cx, |w, window, cx| w.open_file(p, false, window, cx));
    sleep(cx, 1500).await;
    with_file(this, cx, |f, _| (!f.editable() && f.path == path).then(|| f.perf_read_only()).flatten()).flatten()
}

/// The unsaved-edits dialog is up, the editor and the session didn't change.
fn asked(w: &Workspace, cx: &gpui::App, active: usize, tabs: usize) -> bool {
    matches!(w.modal, Some(Modal::Confirm { then: Then::Leave(_), .. }))
        && matches!(&w.overlay, Some(Overlay::File(f)) if f.read(cx).dirty())
        && w.active == active
        && w.tabs.len() == tabs
}

async fn guard(this: &WeakEntity<Workspace>, cx: &mut AsyncWindowContext, dir: &Path) {
    // Read-only files.
    ok(read_only(this, cx, &dir.join("bin.dat")).await == Some(ReadOnly::Binary), "binary file opens read-only");
    ok(
        read_only(this, cx, &dir.join("huge.rs")).await == Some(ReadOnly::TooLarge),
        "file over the limit opens read-only",
    );

    // CRLF: edit + Ctrl+S keeps CRLF everywhere.
    let crlf = dir.join("crlf.txt");
    std::fs::write(&crlf, "one\r\ntwo\r\nthree\r\n").unwrap();
    if open(this, cx, &crlf).await {
        with_file(this, cx, |f, cx| {
            f.perf_key("end", true, cx);
            for ch in "four\n".chars() {
                f.perf_type(ch, cx);
            }
            f.perf_key("s", true, cx);
        });
        sleep(cx, 500).await;
        let bytes = std::fs::read(&crlf).unwrap_or_default();
        ok(bytes == b"one\r\ntwo\r\nthree\r\nfour\r\n", "CRLF file: edit + Ctrl+S keeps CRLF line endings");
    }

    // The unsaved-edits dialog on every other way out.
    if !open(this, cx, &dir.join("plain.txt")).await {
        return;
    }
    with_file(this, cx, |f, cx| f.perf_type('z', cx));
    let Ok((active, tabs)) = this.update(cx, |w, _| (w.active, w.tabs.len())) else { return };
    if tabs < 2 {
        return eprintln!("[check] ✗ needs 2+ sessions (DIRIGENT_RUN)");
    }
    let other = (active + 1) % tabs;
    let cancel = |w: &mut Workspace, window: &mut gpui::Window, cx: &mut gpui::Context<Workspace>| {
        w.close_modal(window, cx);
        matches!(&w.overlay, Some(Overlay::File(f)) if f.read(cx).dirty())
    };
    let r = this.update_in(cx, |w, window, cx| {
        w.cycle(1, window, cx); // Ctrl+Tab
        let a = asked(w, cx, active, tabs);
        (a, cancel(w, window, cx))
    });
    ok(r.ok() == Some((true, true)), "Ctrl+Tab with unsaved edits asks; Cancel keeps the edits");
    let r = this.update_in(cx, |w, window, cx| {
        w.close_session(other, false, window, cx); // middle click / x on another session
        let a = asked(w, cx, active, tabs);
        (a, cancel(w, window, cx))
    });
    ok(r.ok() == Some((true, true)), "closing another session with unsaved edits asks; Cancel keeps both");
    let r = this.update_in(cx, |w, window, cx| {
        let held = w.ask_before_quit(window, cx); // the title-bar x and the compositor's close
        let a = held && asked(w, cx, active, tabs);
        (a, cancel(w, window, cx))
    });
    ok(r.ok() == Some((true, true)), "the window's x with unsaved edits asks; Cancel keeps the window");
    let r = this.update_in(cx, |w, window, cx| {
        w.cycle(1, window, cx);
        w.confirm_ok(window, cx); // Discard changes
        w.overlay.is_none() && w.active != active && w.modal.is_none()
    });
    ok(r.ok() == Some(true), "Discard changes: the session switch goes on, the editor closes");
    let r = this.update_in(cx, |w, window, cx| !w.ask_before_quit(window, cx) && w.modal.is_none());
    ok(r.ok() == Some(true), "nothing unsaved: the window's x closes without asking");
    eprintln!("[check] guard done");
}

async fn check(this: &WeakEntity<Workspace>, cx: &mut AsyncWindowContext, path: &Path) {
    if !open(this, cx, path).await {
        return;
    }
    let state = |cx: &mut AsyncWindowContext| with_file(this, cx, |f, _| f.perf_state()).flatten();
    let disk = || std::fs::read_to_string(path).unwrap_or_default();
    let key = |cx: &mut AsyncWindowContext, k: &str, ctrl: bool| with_file(this, cx, |f, cx| f.perf_key(k, ctrl, cx));
    let typed = |cx: &mut AsyncWindowContext, s: &str| {
        for ch in s.chars() {
            with_file(this, cx, |f, cx| f.perf_type(ch, cx));
        }
    };

    key(cx, "end", true);
    typed(cx, "mine\n");
    ok(state(cx).is_some_and(|s| s.1), "typing makes it dirty");
    key(cx, "s", true);
    sleep(cx, 400).await;
    let s = state(cx).unwrap_or_default();
    ok(!s.1 && s.0 == disk(), "Ctrl+S writes my text, clean again");

    let agent = format!("{}agent line\n", disk());
    std::fs::write(path, &agent).unwrap();
    sleep(cx, 700).await;
    let s = state(cx).unwrap_or_default();
    ok(s.0 == agent && !s.1, "external write, no unsaved edits: silently reloaded");
    ok(s.2 == "notice", "\"Reloaded\" notice shown");
    eprintln!("[check] notice on screen now");
    sleep(cx, 1500).await;
    key(cx, "z", true);
    sleep(cx, 100).await;
    let s = state(cx).unwrap_or_default();
    ok(s.0 == agent && !s.1, "Ctrl+Z doesn't revert the agent's change");
    sleep(cx, 3000).await;
    ok(state(cx).is_some_and(|s| s.2 == "none"), "notice gone by itself");

    typed(cx, "x");
    let agent2 = format!("{agent}agent again\n");
    std::fs::write(path, &agent2).unwrap();
    sleep(cx, 700).await;
    let s = state(cx).unwrap_or_default();
    ok(s.1 && s.0 != agent2 && s.2 == "changed", "external write over unsaved edits: banner, my text kept");
    key(cx, "s", true);
    sleep(cx, 400).await;
    let s = state(cx).unwrap_or_default();
    ok(s.2 == "conflict" && disk() == agent2, "Ctrl+S after the external write: conflict banner, nothing written");
    eprintln!("[check] conflict banner on screen now");
    sleep(cx, 2000).await;
    with_file(this, cx, |f, cx| f.perf_overwrite(cx));
    sleep(cx, 400).await;
    let s = state(cx).unwrap_or_default();
    ok(!s.1 && s.0 == disk() && s.2 == "none", "Overwrite writes my text");
    eprintln!("[check] done");
}
