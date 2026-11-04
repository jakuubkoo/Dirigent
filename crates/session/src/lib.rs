//! One agent session: a PTY running a shell plus its terminal state. No UI dependencies.

pub mod keys;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::Term;
use async_channel::{Receiver, TrySendError};
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::vte::ansi::{Processor, Rgb};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

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
            // ponytail: OSC 10/11/4 queries answer with a fixed dark fg/bg; wire the UI palette here when themes exist.
            Event::ColorRequest(idx, f) => f(match idx {
                257 => Rgb { r: 0x1e, g: 0x1e, b: 0x24 },
                _ => Rgb { r: 0xd8, g: 0xd8, b: 0xd8 },
            }),
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

pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    /// Visible session: one message per batch of new output (bounded(1): pending wakeups coalesce).
    /// Hidden session: one message when it first gets unread output, then silence until shown again.
    /// Closed when the shell exits.
    pub wakeups: Receiver<()>,
    pub title: Arc<Mutex<Option<String>>>,
    pub cwd: PathBuf,
    flags: Arc<Flags>,
    writer: Writer,
    master: Box<dyn MasterPty + Send>,
    /// Taken by whoever ends the process: the reader thread on EOF (reaps), or Drop (kills + reaps).
    child: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
    size: Arc<Mutex<Size>>,
}

#[derive(Default)]
struct Flags {
    visible: AtomicBool,
    unread: AtomicBool,
    exited: AtomicBool,
    exit_code: Mutex<Option<u32>>,
}

impl Session {
    /// Spawns the user's default shell in `cwd`.
    pub fn spawn(cwd: &Path, size: Size) -> std::io::Result<Self> {
        Self::spawn_command(CommandBuilder::new_default_prog(), cwd, size)
    }

    // ponytail: one reader thread per session; fine for tens of sessions, switch to one poll(2) thread for hundreds.
    pub fn spawn_command(mut cmd: CommandBuilder, cwd: &Path, size: Size) -> std::io::Result<Self> {
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
        let (term2, flags2, child2) = (term.clone(), flags.clone(), child.clone());
        thread::Builder::new().name("pty-reader".into()).spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF / EIO when the shell exits
                    Ok(n) => n,
                };
                // Lock held only while parsing this chunk, never while the UI shapes/paints.
                parser.advance(&mut *term2.lock(), &buf[..n]);
                // Hidden sessions wake the UI only on the unread flip, so background floods cost the UI nothing.
                if flags2.visible.load(Ordering::Acquire) || !flags2.unread.swap(true, Ordering::AcqRel) {
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

        Ok(Self {
            term,
            wakeups,
            title,
            cwd: cwd.to_path_buf(),
            flags,
            writer,
            master: pair.master,
            child,
            size: size_shared,
        })
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
        let _ = self.master.resize(new.pty());
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

    fn sh(script: &str) -> CommandBuilder {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
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
    fn exit_is_reported_with_code() {
        let s = Session::spawn_command(sh("exit 3"), Path::new("/"), SIZE).unwrap();
        let pid = s.pid().unwrap();
        wait_until("exit", || s.is_exited() && s.wakeups.is_closed());
        assert_eq!(s.exit_code(), Some(3));
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "exited shell must be reaped, not a zombie");
    }

    #[test]
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
}
