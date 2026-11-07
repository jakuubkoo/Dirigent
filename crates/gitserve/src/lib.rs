//! One long-lived process that runs commands for another: the app runs git inside a WSL distro through it (the
//! Linux `dirigent-hook git-server`) instead of starting `wsl.exe` (~130 ms) for every call.
//!
//! Frames both ways over the server's stdin/stdout: `u32 LE len | u64 LE id | u8 kind | payload`, `len` counting
//! from the id. Payload fields are `u32 LE len | bytes`. Requests: `Run` (cwd, argv, env, stdin) and `Cancel`.
//! Answers: `Hello` once at start, then per request any number of `Out` chunks and one `Exit` or `Error`. Requests run
//! concurrently and their output is chunked, so a big diff never holds up a status.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bumped when the frames change: the app's client refuses a server that answers with another.
pub const VERSION: u32 = 1;
/// Largest `Out` chunk.
pub const CHUNK: usize = 64 * 1024;
/// Default cap on the output queued for one request and not yet taken by its caller.
pub const CAP: usize = 64 << 20;
const MAX_FRAME: usize = 256 << 20;
#[cfg_attr(not(unix), allow(dead_code))] // the server side
const MAX_STDERR: u64 = 1 << 20;

const RUN: u8 = 1;
const CANCEL: u8 = 2;
const HELLO: u8 = 3;
const OUT: u8 = 4;
const EXIT: u8 = 5;
const ERROR: u8 = 6;

/// A command to run: `argv[0]` found on the server's PATH, in `cwd`; `env` sets (`Some`) or removes (`None`) on top
/// of the server's own environment; `stdin` is written to it and closed (empty: no stdin).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Run {
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, Option<String>)>,
    pub stdin: Vec<u8>,
}

/// A finished request. `code`: the exit code, or minus the signal that killed it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Done {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// What a request gives, in order: `Out`s, then `Exit`.
#[derive(Debug)]
pub enum Event {
    Out(Vec<u8>),
    Exit { code: i32, stderr: Vec<u8> },
}

fn put(b: &mut Vec<u8>, field: &[u8]) {
    b.extend((field.len() as u32).to_le_bytes());
    b.extend(field);
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("git server: bad {what}"))
}

struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let (len, rest) = self.0.split_first_chunk::<4>().ok_or_else(|| bad("field"))?;
        let len = u32::from_le_bytes(*len) as usize;
        let (field, rest) = rest.split_at_checked(len).ok_or_else(|| bad("field"))?;
        self.0 = rest;
        Ok(field)
    }
    fn string(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| bad("string"))
    }
    fn count(&mut self) -> io::Result<usize> {
        Ok(u32::from_le_bytes(self.bytes()?.try_into().map_err(|_| bad("count"))?) as usize)
    }
}

impl Run {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64 + self.stdin.len());
        put(&mut b, self.cwd.as_bytes());
        put(&mut b, &(self.argv.len() as u32).to_le_bytes());
        for a in &self.argv {
            put(&mut b, a.as_bytes());
        }
        put(&mut b, &(self.env.len() as u32).to_le_bytes());
        for (k, v) in &self.env {
            put(&mut b, k.as_bytes());
            put(&mut b, &[v.is_some() as u8]);
            put(&mut b, v.as_deref().unwrap_or("").as_bytes());
        }
        put(&mut b, &self.stdin);
        b
    }

    pub fn decode(payload: &[u8]) -> io::Result<Self> {
        let mut f = Fields(payload);
        let cwd = f.string()?;
        let argv = (0..f.count()?).map(|_| f.string()).collect::<io::Result<Vec<_>>>()?;
        let env = (0..f.count()?)
            .map(|_| {
                let k = f.string()?;
                let set = f.bytes()? == [1];
                let v = f.string()?;
                Ok((k, set.then_some(v)))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let stdin = f.bytes()?.to_vec();
        if argv.is_empty() {
            return Err(bad("argv"));
        }
        Ok(Self { cwd, argv, env, stdin })
    }
}

/// Writes one frame (a single `write_all`, so frames from several threads under one lock never interleave).
pub fn write_frame(w: &mut impl Write, id: u64, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut b = Vec::with_capacity(13 + payload.len());
    b.extend(((9 + payload.len()) as u32).to_le_bytes());
    b.extend(id.to_le_bytes());
    b.push(kind);
    b.extend(payload);
    w.write_all(&b)?;
    w.flush()
}

/// The next frame: `(id, kind, payload)`; None at a clean end of the stream.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<(u64, u8, Vec<u8>)>> {
    let mut len = [0; 4];
    match r.read_exact(&mut len) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        r => r?,
    }
    let len = u32::from_le_bytes(len) as usize;
    if !(9..=MAX_FRAME).contains(&len) {
        return Err(bad("frame length"));
    }
    let mut b = vec![0; len];
    r.read_exact(&mut b)?;
    let id = u64::from_le_bytes(b[..8].try_into().unwrap());
    let kind = b[8];
    b.drain(..9);
    Ok(Some((id, kind, b)))
}

#[cfg_attr(not(unix), allow(dead_code))] // the server side
fn exit_payload(code: i32, stderr: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + stderr.len());
    put(&mut b, &code.to_le_bytes());
    put(&mut b, stderr);
    b
}

#[cfg_attr(not(unix), allow(dead_code))] // the server side
/// `Error` payload: whether the program wasn't found (git missing), then the message.
fn error_payload(e: &io::Error) -> Vec<u8> {
    let mut b = Vec::new();
    put(&mut b, &[(e.kind() == io::ErrorKind::NotFound) as u8]);
    put(&mut b, e.to_string().as_bytes());
    b
}

/// The server: reads requests from stdin until it closes, runs each on its own thread in its own process group,
/// answers on stdout. At the end (the app closed stdin or went away) every request still running is killed.
#[cfg(unix)]
pub fn serve() -> i32 {
    let out = Arc::new(Mutex::new(io::stdout()));
    let groups: Arc<Mutex<HashMap<u64, i32>>> = Arc::default();
    if write_frame(&mut *out.lock().unwrap(), 0, HELLO, &VERSION.to_le_bytes()).is_err() {
        return 1;
    }
    let mut stdin = io::stdin().lock();
    while let Ok(Some((id, kind, payload))) = read_frame(&mut stdin) {
        match kind {
            RUN => {
                let (out, groups) = (out.clone(), groups.clone());
                std::thread::spawn(move || {
                    let answer = match Run::decode(&payload) {
                        Ok(run) => serve_one(id, run, &out, &groups),
                        Err(e) => (ERROR, error_payload(&e)),
                    };
                    let _ = write_frame(&mut *out.lock().unwrap(), id, answer.0, &answer.1);
                });
            }
            CANCEL => {
                if let Some(&pg) = groups.lock().unwrap().get(&id) {
                    // SAFETY: plain kill(2) on a group we started.
                    unsafe { libc::kill(-pg, libc::SIGKILL) };
                }
            }
            _ => {}
        }
    }
    for &pg in groups.lock().unwrap().values() {
        // SAFETY: as above.
        unsafe { libc::kill(-pg, libc::SIGKILL) };
    }
    0
}

/// Runs one request, streaming its stdout as `Out` frames; returns its last frame (`Exit` or `Error`).
#[cfg(unix)]
fn serve_one(id: u64, run: Run, out: &Mutex<io::Stdout>, groups: &Mutex<HashMap<u64, i32>>) -> (u8, Vec<u8>) {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut c = Command::new(&run.argv[0]);
    c.args(&run.argv[1..])
        .current_dir(&run.cwd)
        .process_group(0)
        .stdin(if run.stdin.is_empty() { Stdio::null() } else { Stdio::piped() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &run.env {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    let mut child = match c.spawn() {
        Ok(child) => child,
        Err(e) => return (ERROR, error_payload(&e)),
    };
    groups.lock().unwrap().insert(id, child.id() as i32);
    if let Some(mut w) = child.stdin.take() {
        let input = run.stdin;
        std::thread::spawn(move || {
            let _ = w.write_all(&input);
        });
    }
    let mut err = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err.by_ref().take(MAX_STDERR).read_to_end(&mut v);
        let _ = io::copy(&mut err, &mut io::sink());
        v
    });
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = vec![0; CHUNK];
    loop {
        match stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if write_frame(&mut *out.lock().unwrap(), id, OUT, &buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    drop(stdout);
    let status = child.wait();
    groups.lock().unwrap().remove(&id);
    let code = match status {
        Ok(s) => s.code().unwrap_or_else(|| -s.signal().unwrap_or(0)),
        Err(_) => -1,
    };
    (EXIT, exit_payload(code, &stderr.join().unwrap_or_default()))
}

enum Msg {
    Event(Event),
    Failed(io::Error),
}

struct Slot {
    tx: Sender<Msg>,
    queued: Arc<AtomicUsize>,
}

struct Shared {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    pending: Mutex<HashMap<u64, Slot>>,
    /// The server's stdout ended: no answer will come any more.
    dead: AtomicBool,
    next: AtomicU64,
    cap: usize,
}

impl Shared {
    fn cancel(&self, id: u64) {
        if let Some(w) = self.stdin.lock().unwrap().as_mut() {
            let _ = write_frame(w, id, CANCEL, &[]);
        }
    }
}

impl Drop for Shared {
    /// Closing stdin ends the server (it kills what still runs); give it 2 s, then kill it.
    fn drop(&mut self) {
        self.stdin.lock().unwrap().take();
        let child = self.child.get_mut().unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The error every request still waiting gets when the server goes away.
pub fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the git server stopped")
}

/// Whether a request failed because its server went away (and so may be retried on a new one if it only reads).
pub fn is_stopped(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
}

/// A connection to one server process. Cheap to clone; the process ends when the last clone is dropped (requests
/// still in flight then fail with `stopped`).
#[derive(Clone)]
pub struct Client(Arc<Shared>);

impl Client {
    /// Starts the server with `cmd` (its stdio is taken over) and waits up to `hello` for it to answer.
    /// `cap`: the most output one request may have queued and not yet taken (`CAP`).
    pub fn start(mut cmd: Command, hello: Duration, cap: usize) -> io::Result<Self> {
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let (stdin, mut stdout) = (child.stdin.take(), child.stdout.take().unwrap());
        let shared = Arc::new(Shared {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending: Mutex::default(),
            dead: AtomicBool::new(false),
            next: AtomicU64::new(1),
            cap,
        });
        let (hello_tx, hello_rx) = mpsc::channel();
        let weak = Arc::downgrade(&shared);
        std::thread::spawn(move || {
            let greeting = match read_frame(&mut stdout) {
                Ok(Some((0, HELLO, v))) if v == VERSION.to_le_bytes() => Ok(()),
                Ok(Some((0, HELLO, v))) => Err(format!("a server of another version ({v:?})")),
                Ok(_) => Err("no greeting".to_string()),
                Err(e) => Err(e.to_string()),
            };
            let ok = greeting.is_ok();
            let _ = hello_tx.send(greeting);
            if ok {
                read_answers(&mut stdout, &weak);
            }
            if let Some(s) = weak.upgrade() {
                let mut pending = s.pending.lock().unwrap();
                s.dead.store(true, Ordering::SeqCst);
                for (_, slot) in pending.drain() {
                    let _ = slot.tx.send(Msg::Failed(stopped()));
                }
            }
        });
        match hello_rx.recv_timeout(hello) {
            Ok(Ok(())) => Ok(Self(shared)),
            Ok(Err(why)) => Err(io::Error::other(why)),
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "the git server didn't answer")),
        }
    }

    /// The server process (tests).
    pub fn pid(&self) -> u32 {
        self.0.child.lock().unwrap().id()
    }

    /// Whether the server went away (a new one is needed).
    pub fn is_dead(&self) -> bool {
        self.0.dead.load(Ordering::SeqCst)
    }

    /// Sends a request; its answers come through the returned `Pending`.
    pub fn request(&self, run: &Run) -> io::Result<Pending> {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        {
            // Registered under the lock the reader takes to mark the server dead: never left waiting forever.
            let mut pending = self.0.pending.lock().unwrap();
            if self.is_dead() {
                return Err(stopped());
            }
            pending.insert(id, Slot { tx, queued: queued.clone() });
        }
        let sent = match self.0.stdin.lock().unwrap().as_mut() {
            Some(w) => write_frame(w, id, RUN, &run.encode()),
            None => Err(stopped()),
        };
        if let Err(e) = sent {
            self.0.pending.lock().unwrap().remove(&id);
            return Err(if e.kind() == io::ErrorKind::BrokenPipe { stopped() } else { e });
        }
        Ok(Pending { shared: Arc::downgrade(&self.0), id, rx, queued, done: false })
    }
}

/// The reader thread: hands every answer to its request, never waiting on one.
fn read_answers(stdout: &mut impl Read, shared: &std::sync::Weak<Shared>) {
    while let Ok(Some((id, kind, payload))) = read_frame(stdout) {
        let Some(s) = shared.upgrade() else { return };
        let mut pending = s.pending.lock().unwrap();
        let Some(slot) = pending.get(&id) else { continue }; // cancelled, over its cap, or gone
        let msg = match kind {
            OUT => {
                if slot.queued.fetch_add(payload.len(), Ordering::SeqCst) + payload.len() > s.cap {
                    let slot = pending.remove(&id).unwrap();
                    drop(pending);
                    let mb = s.cap >> 20;
                    let _ = slot.tx.send(Msg::Failed(io::Error::other(format!("more than {mb} MiB of output"))));
                    s.cancel(id);
                    continue;
                }
                Msg::Event(Event::Out(payload))
            }
            EXIT | ERROR => {
                let slot = pending.remove(&id).unwrap();
                let mut f = Fields(&payload);
                let msg = if kind == EXIT {
                    match (f.bytes(), f.bytes()) {
                        (Ok(code), Ok(stderr)) => Msg::Event(Event::Exit {
                            code: code.try_into().map_or(-1, i32::from_le_bytes),
                            stderr: stderr.to_vec(),
                        }),
                        _ => Msg::Failed(bad("exit")),
                    }
                } else {
                    let not_found = f.bytes().is_ok_and(|b| b == [1]);
                    let text = f.string().unwrap_or_default();
                    Msg::Failed(io::Error::new(
                        if not_found { io::ErrorKind::NotFound } else { io::ErrorKind::Other },
                        text,
                    ))
                };
                let _ = slot.tx.send(msg);
                continue;
            }
            _ => continue,
        };
        let _ = slot.tx.send(msg);
    }
}

/// A request in flight. Dropping it before its end cancels it (the server kills its process group).
pub struct Pending {
    shared: std::sync::Weak<Shared>,
    id: u64,
    rx: Receiver<Msg>,
    queued: Arc<AtomicUsize>,
    done: bool,
}

impl Pending {
    /// The next answer, waiting up to `wait`: None when nothing came in that time.
    pub fn next(&mut self, wait: Duration) -> io::Result<Option<Event>> {
        if self.done {
            return Err(io::Error::other("request already finished"));
        }
        match self.rx.recv_timeout(wait) {
            Ok(Msg::Event(Event::Out(b))) => {
                self.queued.fetch_sub(b.len(), Ordering::SeqCst);
                Ok(Some(Event::Out(b)))
            }
            Ok(Msg::Event(exit)) => {
                self.done = true;
                Ok(Some(exit))
            }
            Ok(Msg::Failed(e)) => {
                self.done = true;
                Err(e)
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                self.done = true;
                Err(stopped())
            }
        }
    }

    /// Everything, waiting up to `timeout` in all; past it the request is cancelled and `TimedOut` returned.
    pub fn output(mut self, timeout: Duration) -> io::Result<Done> {
        let end = Instant::now() + timeout;
        let mut stdout = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.next(left)? {
                Some(Event::Out(b)) => stdout.extend(b),
                Some(Event::Exit { code, stderr }) => return Ok(Done { code, stdout, stderr }),
                None if left.is_zero() => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "git took too long and was stopped"));
                }
                None => {}
            }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(s) = self.shared.upgrade()
            && !self.done
            && s.pending.lock().unwrap().remove(&self.id).is_some()
        {
            s.cancel(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        for run in [
            Run { cwd: "/".into(), argv: vec!["true".into()], ..Default::default() },
            Run {
                cwd: "/home/a b".into(),
                argv: vec!["git".into(), "".into(), "ünï".into()],
                env: vec![("LC_ALL".into(), Some("C".into())), ("GIT_DIR".into(), None), ("E".into(), Some("".into()))],
                stdin: (0..=255).cycle().take(1 << 20).collect(),
            },
        ] {
            assert_eq!(Run::decode(&run.encode()).unwrap(), run);
        }
        assert!(Run::decode(&Run::default().encode()).is_err(), "no program");
        assert!(Run::decode(&[1, 2]).is_err(), "truncated");
    }

    #[test]
    fn frames_round_trip() {
        let mut b = Vec::new();
        write_frame(&mut b, 7, OUT, b"abc").unwrap();
        write_frame(&mut b, u64::MAX, EXIT, &exit_payload(-9, b"")).unwrap();
        write_frame(&mut b, 1, CANCEL, &[]).unwrap();
        let mut r = b.as_slice();
        assert_eq!(read_frame(&mut r).unwrap(), Some((7, OUT, b"abc".to_vec())));
        assert_eq!(read_frame(&mut r).unwrap(), Some((u64::MAX, EXIT, exit_payload(-9, b""))));
        assert_eq!(read_frame(&mut r).unwrap(), Some((1, CANCEL, vec![])));
        assert_eq!(read_frame(&mut r).unwrap(), None, "clean end");
        assert!(read_frame(&mut &[3, 0, 0, 0, 1, 2, 3][..]).is_err(), "too short for an id");
    }
}
