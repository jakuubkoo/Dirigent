//! One agent session: a PTY running a shell plus its terminal state. No UI dependencies.

pub mod expect;
pub mod find;
pub mod keys;
pub mod links;
pub mod mouse;
pub mod osc7;
pub mod select;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::vte::ansi::{Processor, Rgb};
use async_channel::{Receiver, TrySendError};
pub use portable_pty::CommandBuilder;
use portable_pty::{Child, MasterPty, PtySize, native_pty_system};

pub use alacritty_terminal;

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// Grid size plus cell size in pixels (the latter only for TIOCGWINSZ / size reports).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl Size {
    fn pty(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: self.cols * self.cell_width,
            pixel_height: self.rows * self.cell_height,
        }
    }
}

/// Answers terminal queries (device status, colors, size) by writing back to the PTY.
pub struct Listener {
    writer: Writer,
    size: Arc<Mutex<Size>>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let reply = match event {
            Event::PtyWrite(s) => s,
            Event::TextAreaSizeRequest(f) => {
                let s = *self.size.lock().unwrap();
                f(WindowSize {
                    num_lines: s.rows,
                    num_cols: s.cols,
                    cell_width: s.cell_width,
                    cell_height: s.cell_height,
                })
            }
            // OSC 11 (background) and OSC 10 (foreground) queries; other indices get the foreground too.
            Event::ColorRequest(idx, f) => {
                let (fg, bg) = *REPORTED.lock().unwrap();
                f(if idx == 257 { bg } else { fg })
            }
            Event::Title(t) => {
                *self.title.lock().unwrap() = Some(t);
                return;
            }
            Event::ResetTitle => {
                *self.title.lock().unwrap() = None;
                return;
            }
            _ => return,
        };
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(reply.as_bytes()).and_then(|_| w.flush());
    }
}

static REPORTED: std::sync::Mutex<(Rgb, Rgb)> =
    std::sync::Mutex::new((Rgb { r: 0xd8, g: 0xd8, b: 0xd8 }, Rgb { r: 0x1e, g: 0x1e, b: 0x24 }));

/// Reader thread: feeds the pending `answer_once`; on a match the reply goes out from a short-lived thread (the
/// pauses must not stall reading).
fn answer(flags: &Flags, writer: &Writer, chunk: &[u8]) {
    let mut slot = flags.expect.lock().unwrap();
    let Some(e) = slot.as_mut() else { return };
    let expired = std::time::Instant::now() > e.deadline;
    let hit = !expired && e.feed(chunk);
    if hit || expired {
        let done = slot.take();
        flags.expecting.store(false, Ordering::Release);
        if let (true, Some(e)) = (hit, done) {
            let writer = writer.clone();
            let _ = thread::Builder::new().name("pty-answer".into()).spawn(move || {
                for part in &e.reply {
                    // The TUI drops keys sent right as it draws (claude 2.1.318's trust dialog: ~0.5 s): wait each time.
                    thread::sleep(std::time::Duration::from_millis(400));
                    let mut w = writer.lock().unwrap();
                    let _ = w.write_all(part).and_then(|_| w.flush());
                }
            });
        }
    }
}

/// The foreground/background the terminal reports to apps that ask (OSC 10/11): the UI theme's terminal colors,
/// set at startup and on every theme switch, so e.g. Claude Code picks its light/dark styling to match (an app that
/// already asked keeps its answer).
pub fn set_reported_colors(fg: [u8; 3], bg: [u8; 3]) {
    let rgb = |c: [u8; 3]| Rgb { r: c[0], g: c[1], b: c[2] };
    *REPORTED.lock().unwrap() = (rgb(fg), rgb(bg));
}

pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    /// Visible session: one message per batch of new output (bounded(1): pending wakeups coalesce).
    /// Hidden session: one message when it first gets unread output, then silence until shown again.
    /// Closed when the shell exits.
    pub wakeups: Receiver<()>,
    pub title: Arc<Mutex<Option<String>>>,
    flags: Arc<Flags>,
    writer: Writer,
    /// `None` once a Windows pseudoconsole was closed after its shell exited (`watch_exit`).
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    /// Taken by whoever ends the process: the reader thread on EOF (reaps), or Drop (kills + reaps).
    child: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
    size: Arc<Mutex<Size>>,
}

/// Extras for `spawn_command_with`; the defaults are `spawn_command`'s behavior.
#[derive(Default)]
pub struct Opts {
    /// Accept a cwd report (OSC 7) naming any host: the shell runs on this machine under another hostname (WSL).
    pub any_host: bool,
    /// Runs on the reader thread at every cwd report, i.e. every prompt, changed or not.
    pub on_prompt: Option<Box<dyn Fn() + Send>>,
}

#[derive(Default)]
struct Flags {
    visible: AtomicBool,
    unread: AtomicBool,
    exited: AtomicBool,
    exit_code: Mutex<Option<u32>>,
    /// Last directory the shell reported (OSC 7); `cwd_changed` until the UI takes it.
    cwd: Mutex<Option<PathBuf>>,
    cwd_changed: AtomicBool,
    /// A prompt to answer once (`answer_once`); checked per chunk only while set.
    expect: Mutex<Option<expect::Expect>>,
    expecting: AtomicBool,
    /// Bumped per chunk of output: lets the UI tell whether the app went quiet (`output_seq`).
    output: AtomicU64,
}

impl Session {
    /// Spawns the user's default shell (as a login shell) in `cwd` with extra environment variables.
    pub fn spawn(cwd: &Path, size: Size, env: &[(String, String)]) -> std::io::Result<Self> {
        let mut cmd = CommandBuilder::new_default_prog();
        for (k, v) in env {
            cmd.env(k, v);
        }
        Self::spawn_command(cmd, cwd, size)
    }

    pub fn spawn_command(cmd: CommandBuilder, cwd: &Path, size: Size) -> std::io::Result<Self> {
        Self::spawn_command_with(cmd, cwd, size, Opts::default())
    }

    // ponytail: one reader thread per session; fine for tens of sessions, switch to one poll(2) thread for hundreds.
    pub fn spawn_command_with(mut cmd: CommandBuilder, cwd: &Path, size: Size, opts: Opts) -> std::io::Result<Self> {
        let pair = native_pty_system().openpty(size.pty()).map_err(io_err)?;
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.cwd(cwd);
        let child = Arc::new(Mutex::new(Some(pair.slave.spawn_command(cmd).map_err(io_err)?)));
        drop(pair.slave); // otherwise the reader never sees EOF
        let mut reader = pair.master.try_clone_reader().map_err(io_err)?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(io_err)?));

        let size_shared = Arc::new(Mutex::new(size));
        let title = Arc::new(Mutex::new(None));
        let listener = Listener { writer: writer.clone(), size: size_shared.clone(), title: title.clone() };
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &size, listener)));
        let flags = Arc::new(Flags::default());

        let (tx, wakeups) = async_channel::bounded(1);
        let (term2, flags2, child2, writer2) = (term.clone(), flags.clone(), child.clone(), writer.clone());
        thread::Builder::new().name("pty-reader".into()).spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut buf = vec![0u8; 64 * 1024];
            let (mut scanner, host) = (osc7::Scanner::default(), osc7::hostname());
            let Opts { any_host, on_prompt } = opts;
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF / EIO when the shell exits
                    Ok(n) => n,
                };
                flags2.output.fetch_add(1, Ordering::Relaxed);
                // The shell's prompt hook reports its cwd every prompt; only a real change wakes the UI, hidden or not.
                let reported = scanner
                    .scan(&buf[..n])
                    .and_then(|p| if any_host { osc7::parse_any_host(&p) } else { osc7::parse(&p, &host) });
                if let (Some(_), Some(f)) = (&reported, &on_prompt) {
                    f();
                }
                let moved = reported.is_some_and(|dir| {
                    let mut cwd = flags2.cwd.lock().unwrap();
                    let changed = cwd.as_ref() != Some(&dir);
                    if changed {
                        *cwd = Some(dir);
                        flags2.cwd_changed.store(true, Ordering::Release);
                    }
                    changed
                });
                if flags2.expecting.load(Ordering::Acquire) {
                    answer(&flags2, &writer2, &buf[..n]);
                }
                // Lock held only while parsing this chunk, never while the UI shapes/paints.
                parser.advance(&mut *term2.lock(), &buf[..n]);
                // Hidden sessions wake the UI only on the unread flip, so background floods cost the UI nothing.
                let wake = flags2.visible.load(Ordering::Acquire) || !flags2.unread.swap(true, Ordering::AcqRel);
                if wake || moved {
                    match tx.try_send(()) {
                        Ok(()) | Err(TrySendError::Full(())) => {}
                        Err(TrySendError::Closed(())) => break,
                    }
                }
            }
            // Reap here so exited background sessions don't linger as zombies. Taken out of the lock first:
            // Drop must never wait on it.
            let child = child2.lock().unwrap().take();
            if let Some(mut child) = child {
                *flags2.exit_code.lock().unwrap() = child.wait().ok().map(|s| s.exit_code());
            }
            flags2.exited.store(true, Ordering::Release);
            // dropping `tx` closes the channel = exit notification
        })?;

        let master = Arc::new(Mutex::new(Some(pair.master)));
        #[cfg(windows)]
        watch_exit(&child, &master)?;
        Ok(Self { term, wakeups, title, flags, writer, master, child, size: size_shared })
    }

    /// Answers a prompt once: when `needle` (whitespace ignored, escapes stripped) appears in the output before
    /// `within` runs out, writes `reply`'s parts, each after a 400 ms pause. Replaces an earlier one.
    pub fn answer_once(&self, needle: &str, reply: Vec<Vec<u8>>, within: std::time::Duration) {
        *self.flags.expect.lock().unwrap() =
            Some(expect::Expect::new(needle, reply, std::time::Instant::now() + within));
        self.flags.expecting.store(true, Ordering::Release);
    }

    /// Scrollback limit in lines (alacritty's default is 10 000). Shrinking drops the oldest history lines now.
    pub fn set_scrollback(&self, lines: usize) {
        self.term.lock().set_options(Config { scrolling_history: lines, ..Config::default() });
    }

    pub fn write(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(bytes).and_then(|_| w.flush());
    }

    /// Resizes PTY and grid; returns false (no-op) when nothing changed.
    pub fn resize(&self, new: Size) -> bool {
        let mut cur = self.size.lock().unwrap();
        if *cur == new || new.cols == 0 || new.rows == 0 {
            return false;
        }
        *cur = new;
        drop(cur);
        self.term.lock().resize(new);
        if let Some(m) = self.master.lock().unwrap().as_ref() {
            let _ = m.resize(new.pty());
        }
        true
    }

    pub fn size(&self) -> Size {
        *self.size.lock().unwrap()
    }

    /// Scrollback: positive = up into history.
    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Showing a session clears its unread flag.
    pub fn set_visible(&self, visible: bool) {
        self.flags.visible.store(visible, Ordering::Release);
        if visible {
            self.flags.unread.store(false, Ordering::Release);
        }
    }

    pub fn is_visible(&self) -> bool {
        self.flags.visible.load(Ordering::Acquire)
    }

    /// Clears the unread flag without showing the session (the sidebar's "Mark as read").
    pub fn clear_unread(&self) {
        self.flags.unread.store(false, Ordering::Release);
    }

    pub fn is_unread(&self) -> bool {
        self.flags.unread.load(Ordering::Acquire)
    }

    pub fn is_exited(&self) -> bool {
        self.flags.exited.load(Ordering::Acquire)
    }

    /// Exit code once the shell has exited.
    pub fn exit_code(&self) -> Option<u32> {
        *self.flags.exit_code.lock().unwrap()
    }

    /// The shell's new directory if it reported a different one (OSC 7) since the last call.
    pub fn take_cwd_change(&self) -> Option<PathBuf> {
        if !self.flags.cwd_changed.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.flags.cwd.lock().unwrap().clone()
    }

    /// Changes whenever output arrived; equal across two reads = nothing was printed in between.
    pub fn output_seq(&self) -> u64 {
        self.flags.output.load(Ordering::Relaxed)
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.lock().unwrap().as_ref()?.process_id()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // kill() = SIGHUP + up to ~200 ms of polling + SIGKILL: never on the UI thread. wait() reaps the zombie.
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = thread::Builder::new().name("pty-reaper".into()).spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// ConPTY keeps the output pipe open after the shell exits, until the pseudoconsole is closed. A waiter thread
/// closes it when the process ends, while the reader thread is still reading (`ClosePseudoConsole` waits for the
/// output to be drained); the reader then sees EOF and runs the normal exit path.
#[cfg(windows)]
fn watch_exit(
    child: &Mutex<Option<Box<dyn Child + Send + Sync>>>,
    master: &Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
) -> std::io::Result<()> {
    use std::os::windows::io::{AsRawHandle, BorrowedHandle};
    use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
    let raw =
        child.lock().unwrap().as_ref().and_then(|c| c.as_raw_handle()).ok_or_else(|| io_err("no process handle"))?;
    // Our own handle: the reader thread may drop the child (and its handle) while we wait.
    // SAFETY: `raw` belongs to the child, which is alive and locked above while we duplicate it.
    let process = unsafe { BorrowedHandle::borrow_raw(raw) }.try_clone_to_owned()?;
    let master = master.clone();
    thread::Builder::new().name("pty-exit".into()).spawn(move || {
        // SAFETY: a valid process handle we own.
        unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) };
        drop(master.lock().unwrap().take());
    })?;
    Ok(())
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const SIZE: Size = Size { cols: 40, rows: 5, cell_width: 8, cell_height: 16 };

    fn grid_text(s: &Session) -> String {
        let term = s.term.lock();
        term.renderable_content().display_iter.map(|c| c.c).collect()
    }

    #[cfg(unix)]
    fn sh(script: &str) -> CommandBuilder {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        // PowerShell alone can take seconds to start on a cold CI runner.
        let deadline = Instant::now() + Duration::from_secs(if cfg!(windows) { 30 } else { 5 });
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[cfg(unix)]
    fn output_cwd_and_resize() {
        let dir = std::env::temp_dir();
        let s = Session::spawn_command(sh("printf '%s-%s' \"$COLORTERM\" \"$PWD\"; sleep 2"), &dir, SIZE).unwrap();
        s.set_visible(true);
        let want = format!("truecolor-{}", dir.display().to_string().trim_end_matches('/'));
        wait_until("output", || grid_text(&s).contains(&want[..want.len().min(38)]));
        assert!(s.wakeups.try_recv().is_ok(), "visible session must signal a wakeup");
        assert!(s.resize(Size { cols: 20, rows: 3, ..SIZE }));
        assert!(!s.resize(Size { cols: 20, rows: 3, ..SIZE }), "same size is a no-op");
        let term = s.term.lock();
        assert_eq!((term.columns(), term.screen_lines()), (20, 3));
    }

    #[test]
    #[cfg(unix)]
    fn scrollback_limit_applies_live() {
        let s = Session::spawn_command(sh("seq 1 500; sleep 2"), Path::new("/"), SIZE).unwrap();
        wait_until("output", || s.term.lock().grid().history_size() >= 400);
        s.set_scrollback(100);
        assert_eq!(s.term.lock().grid().history_size(), 100, "shrinking drops the oldest lines now");
        s.set_scrollback(0);
        assert_eq!(s.term.lock().grid().history_size(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn hidden_flood_wakes_once() {
        let s = Session::spawn_command(sh("sleep 0.2; exec yes"), Path::new("/"), SIZE).unwrap();
        wait_until("unread", || s.is_unread());
        assert!(s.wakeups.try_recv().is_ok());
        thread::sleep(Duration::from_millis(300)); // flood continues
        assert!(s.wakeups.try_recv().is_err(), "hidden session must not wake again");
        s.set_visible(true);
        assert!(!s.is_unread());
    }

    #[test]
    #[cfg(unix)]
    fn shell_cwd_reports_wake_once_per_change() {
        // Like the zsh precmd hook: the same dir reported twice (BEL, then ST), then a new one.
        let script = r#"printf '\033]7;file://%s/tmp\a' "$(cat /proc/sys/kernel/hostname)"; sleep 0.3
            printf '\033]7;file:///tmp\033\\'; sleep 0.3; printf '\033]7;file:///var\a'; sleep 2"#;
        let s = Session::spawn_command(sh(script), Path::new("/"), SIZE).unwrap();
        wait_until("first cwd", || s.wakeups.try_recv().is_ok() && s.flags.cwd.lock().unwrap().is_some());
        assert_eq!(s.take_cwd_change(), Some(PathBuf::from("/tmp")));
        wait_until("second cwd", || {
            let _ = s.wakeups.try_recv();
            s.flags.cwd.lock().unwrap().as_deref() == Some(Path::new("/var"))
        });
        assert_eq!(s.take_cwd_change(), Some(PathBuf::from("/var")), "the repeated /tmp was not a change");
        assert_eq!(s.take_cwd_change(), None);
    }

    #[test]
    #[cfg(unix)]
    fn exit_is_reported_with_code() {
        let s = Session::spawn_command(sh("exit 3"), Path::new("/"), SIZE).unwrap();
        let pid = s.pid().unwrap();
        wait_until("exit", || s.is_exited() && s.wakeups.is_closed());
        assert_eq!(s.exit_code(), Some(3));
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "exited shell must be reaped, not a zombie");
    }

    #[test]
    #[cfg(unix)]
    fn drop_kills_shell_and_foreground_child() {
        let s = Session::spawn_command(sh("sleep 1000; true"), Path::new("/"), SIZE).unwrap();
        let pid = s.pid().unwrap();
        let child_pid = || {
            std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default().trim().to_string()
        };
        wait_until("sleep child", || !child_pid().is_empty());
        let sleep_pid = child_pid();
        let started = Instant::now();
        drop(s);
        assert!(started.elapsed() < Duration::from_millis(20), "drop must not block");
        let gone = |p: &str| !Path::new(&format!("/proc/{p}")).exists();
        wait_until("shell reaped", || gone(&pid.to_string()));
        wait_until("sleep killed", || gone(&sleep_pid));
    }

    #[cfg(windows)]
    fn ps(script: &str) -> CommandBuilder {
        let mut cmd = CommandBuilder::new("powershell.exe");
        cmd.args(["-NoLogo", "-NoProfile", "-Command", script]);
        cmd
    }

    #[cfg(windows)]
    fn alive(pid: u32) -> bool {
        let out = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output();
        String::from_utf8_lossy(&out.unwrap().stdout).contains(&pid.to_string())
    }

    #[test]
    #[cfg(windows)]
    fn conpty_output_resize_and_exit_code() {
        let dir = std::env::temp_dir();
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/c", "echo out-%COLORTERM%-%CD%& ping -n 2 127.0.0.1 >nul & exit /b 3"]);
        let s = Session::spawn_command(cmd, &dir, SIZE).unwrap();
        s.set_visible(true);
        wait_until("output", || grid_text(&s).contains("out-truecolor-"));
        assert!(s.wakeups.try_recv().is_ok(), "visible session must signal a wakeup");
        assert!(s.resize(Size { cols: 20, rows: 3, ..SIZE }));
        // The exit must come through although ConPTY never closes the pipe on its own (`watch_exit`).
        wait_until("exit", || s.is_exited() && s.wakeups.is_closed());
        assert_eq!(s.exit_code(), Some(3));
    }

    #[test]
    #[cfg(windows)]
    fn conpty_passes_osc7_through() {
        let s = Session::spawn_command(
            ps("[Console]::Write([char]27 + ']7;file://localhost/C:/Windows' + [char]7); Start-Sleep 3"),
            &std::env::temp_dir(),
            SIZE,
        )
        .unwrap();
        wait_until("cwd", || s.flags.cwd.lock().unwrap().is_some());
        assert_eq!(s.take_cwd_change(), Some(PathBuf::from(r"C:\Windows")));
    }

    #[test]
    #[cfg(windows)]
    fn conpty_keys_reach_the_shell() {
        let s = Session::spawn_command(
            ps("$l = Read-Host; Write-Host \"got-$l\"; Start-Sleep 3"),
            &std::env::temp_dir(),
            SIZE,
        )
        .unwrap();
        s.set_visible(true);
        thread::sleep(Duration::from_millis(1500)); // powershell startup
        s.write(b"abc\r");
        wait_until("echo", || grid_text(&s).contains("got-abc"));
    }

    #[test]
    #[cfg(windows)]
    fn conpty_drop_kills_the_shell() {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/c", "ping -n 1000 127.0.0.1 >nul"]);
        let s = Session::spawn_command(cmd, &std::env::temp_dir(), SIZE).unwrap();
        let pid = s.pid().unwrap();
        assert!(alive(pid));
        let started = Instant::now();
        drop(s);
        assert!(started.elapsed() < Duration::from_millis(50), "drop must not block");
        wait_until("shell killed", || !alive(pid));
    }
}
