//! One agent session: a PTY running a shell plus its terminal state. No UI dependencies.

pub mod keys;

use std::io::{Read, Write};
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
    /// Receives one message per batch of new output (bounded(1): pending wakeups coalesce). Disconnects on exit.
    pub wakeups: Receiver<()>,
    pub title: Arc<Mutex<Option<String>>>,
    writer: Writer,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: Arc<Mutex<Size>>,
}

impl Session {
    /// Spawns the user's default shell.
    pub fn spawn(size: Size) -> std::io::Result<Self> {
        Self::spawn_command(CommandBuilder::new_default_prog(), size)
    }

    pub fn spawn_command(mut cmd: CommandBuilder, size: Size) -> std::io::Result<Self> {
        let pair = native_pty_system().openpty(size.pty()).map_err(io_err)?;
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        let child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        drop(pair.slave); // otherwise the reader never sees EOF
        let mut reader = pair.master.try_clone_reader().map_err(io_err)?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(io_err)?));

        let size_shared = Arc::new(Mutex::new(size));
        let title = Arc::new(Mutex::new(None));
        let listener = Listener { writer: writer.clone(), size: size_shared.clone(), title: title.clone() };
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &size, listener)));

        let (tx, wakeups) = async_channel::bounded(1);
        let term2 = term.clone();
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
                match tx.try_send(()) {
                    Ok(()) | Err(TrySendError::Full(())) => {}
                    Err(TrySendError::Closed(())) => break,
                }
            }
        })?;

        Ok(Self { term, wakeups, title, writer, master: pair.master, child, size: size_shared })
    }

    pub fn write(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(bytes).and_then(|_| w.flush());
    }

    /// Resizes PTY and grid; no-op when nothing changed.
    pub fn resize(&self, new: Size) {
        let mut cur = self.size.lock().unwrap();
        if *cur == new || new.cols == 0 || new.rows == 0 {
            return;
        }
        *cur = new;
        drop(cur);
        self.term.lock().resize(new);
        let _ = self.master.resize(new.pty());
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
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn grid_text(s: &Session) -> String {
        let term = s.term.lock();
        term.renderable_content().display_iter.map(|c| c.c).collect()
    }

    #[test]
    fn output_reaches_grid_and_resize_applies() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'hi-%s' \"$COLORTERM\"; sleep 2"]);
        let s = Session::spawn_command(cmd, Size { cols: 40, rows: 5, cell_width: 8, cell_height: 16 }).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !grid_text(&s).contains("hi-truecolor") {
            assert!(std::time::Instant::now() < deadline, "got: {:?}", grid_text(&s));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(s.wakeups.try_recv().is_ok(), "reader must signal a wakeup");
        s.resize(Size { cols: 20, rows: 3, cell_width: 8, cell_height: 16 });
        let term = s.term.lock();
        assert_eq!((term.columns(), term.screen_lines()), (20, 3));
    }
}
