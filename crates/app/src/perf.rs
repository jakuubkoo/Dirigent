//! `DIRIGENT_PERF=1`: once per second of activity prints terminal paints, root renders, PTY resizes and the
//! average terminal render time; also every session switch's action → first paint latency. No-op when unset.

use std::cell::Cell;
use std::sync::LazyLock;
use std::time::Instant;

static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("DIRIGENT_PERF").is_some());
/// `DIRIGENT_PERF=2` also logs every single event.
static VERBOSE: LazyLock<bool> = LazyLock::new(|| std::env::var("DIRIGENT_PERF").is_ok_and(|v| v == "2"));
static START: LazyLock<Instant> = LazyLock::new(Instant::now);

fn verbose(what: &str) {
    if *VERBOSE {
        eprintln!("[perf] {:8.1} ms {what}", START.elapsed().as_secs_f64() * 1e3);
    }
}

#[derive(Default, Clone, Copy)]
struct Counters {
    term_paints: u64,
    term_nanos: u64,
    root_renders: u64,
    resizes: u64,
}

thread_local! {
    static C: Cell<Counters> = Cell::default();
    static SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
    static SWITCH: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn bump(event: &str, f: impl FnOnce(&mut Counters)) {
    if !*ENABLED {
        return;
    }
    verbose(event);
    let mut c = C.get();
    f(&mut c);
    C.set(c);
    let since = SINCE.get().unwrap_or_else(Instant::now);
    SINCE.set(Some(since));
    if since.elapsed().as_secs_f32() >= 1. {
        eprintln!(
            "[perf] term paints {}, root renders {}, pty resizes {}, avg term render {:.2} ms",
            c.term_paints,
            c.root_renders,
            c.resizes,
            c.term_nanos as f64 / c.term_paints.max(1) as f64 / 1e6
        );
        C.set(Counters::default());
        SINCE.set(Some(Instant::now()));
    }
}

/// End of a terminal paint whose prepaint began at `started`.
static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static FIRST_FRAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Call first thing in `main`.
pub fn process_started() {
    PROCESS_START.get_or_init(Instant::now);
}

pub fn term_paint(started: Instant) {
    if *ENABLED
        && !FIRST_FRAME.swap(true, std::sync::atomic::Ordering::Relaxed)
        && let Some(t) = PROCESS_START.get()
    {
        eprintln!("[perf] startup → first frame {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    }
    bump("term paint", |c| {
        c.term_paints += 1;
        c.term_nanos += started.elapsed().as_nanos() as u64;
    });
    if let Some(t) = SWITCH.take() {
        eprintln!(
            "[perf] switch → first paint {:.2} ms (render {:.2} ms)",
            t.elapsed().as_secs_f64() * 1e3,
            started.elapsed().as_secs_f64() * 1e3
        );
    }
}

pub fn root_render() {
    bump("root render", |c| c.root_renders += 1);
}

pub fn pty_resizes(n: u64) {
    bump("pty resizes", |c| c.resizes += n);
}

pub fn switch_started() {
    if *ENABLED {
        SWITCH.set(Some(Instant::now()));
    }
}

/// Verbose-only marker for cache diagnostics.
pub fn event(what: &str) {
    verbose(what);
}

/// Verbose-only event whose text is built lazily (safe to call from any thread).
pub fn event_with(f: impl FnOnce() -> String) {
    if *VERBOSE {
        verbose(&f());
    }
}

thread_local! {
    static KEY: Cell<Option<Instant>> = const { Cell::new(None) };
    static KEY_MS: std::cell::RefCell<Vec<f64>> = const { std::cell::RefCell::new(Vec::new()) };
    static EDITOR_RENDERS: Cell<u64> = const { Cell::new(0) };
}

/// The editor handled a key (stamped for `key_painted`).
pub fn key_pressed() {
    if *ENABLED {
        KEY.set(Some(Instant::now()));
    }
}

/// The editor painted its caret: closes the key → paint measurement, if one is open.
pub fn key_painted() {
    if let Some(t) = KEY.take() {
        let ms = t.elapsed().as_secs_f64() * 1e3;
        verbose(&format!("key → paint {ms:.2} ms"));
        KEY_MS.with_borrow_mut(|v| v.push(ms));
    }
}

/// Key → paint latencies since the last call (typing harness).
pub fn take_key_latencies() -> Vec<f64> {
    KEY_MS.with_borrow_mut(std::mem::take)
}

pub fn editor_render() {
    if *ENABLED {
        EDITOR_RENDERS.set(EDITOR_RENDERS.get() + 1);
        verbose("file view render");
    }
}

/// File view renders so far (typing harness).
pub fn editor_renders() -> u64 {
    EDITOR_RENDERS.get()
}
