//! Editor harnesses (with `DIRIGENT_PERF=1` and `DIRIGENT_PERF_OPEN=<file>`; scratch copies only, files get written):
//! - `DIRIGENT_PERF_TYPE=<text>` (`\n` = Enter): types one char per 50 ms through the editor's key path into the
//!   opened file, then into `DIRIGENT_PERF_TYPE_ALSO=<file>` if set; logs renders per key, key → paint latency and
//!   renders in 2 s idle. Never saves.
//! - `DIRIGENT_PERF_CHECK=<file>`: type, Ctrl+S, an external write (silent reload + notice), Ctrl+Z (must not revert
//!   it), an external write over unsaved edits (banner), Ctrl+S (conflict), Overwrite. Logs `[check] ✓/✗ …`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AsyncWindowContext, WeakEntity};

use super::{Overlay, Workspace};
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
}

async fn check(this: &WeakEntity<Workspace>, cx: &mut AsyncWindowContext, path: &Path) {
    if !open(this, cx, path).await {
        return;
    }
    let state = |cx: &mut AsyncWindowContext| with_file(this, cx, |f, _| f.perf_state()).flatten();
    let disk = || std::fs::read_to_string(path).unwrap_or_default();
    let ok = |cond: bool, what: &str| eprintln!("[check] {} {what}", if cond { "✓" } else { "✗" });
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
