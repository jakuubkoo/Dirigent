//! `dirigent [--new] [--claude] [PATH]`: argument parsing, path resolution and the single-instance socket. A
//! running app listens on `$XDG_RUNTIME_DIR/dirigent.sock` (Windows: a named pipe derived from that path); a new
//! process sends it one JSON line (the request) and exits on the `ok` reply. No socket or a stale one (crashed
//! instance) → the new process becomes the app. No gpui.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener as Listener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const USAGE: &str = "\
Usage: dirigent [--new] [--claude] [PATH]

Opens a session in PATH (a directory, or a file's directory with the file shown in the preview) in the running
dirigent, or starts it. Without PATH it just starts or focuses the app.

Options:
  --new             a new session even if one is already in that directory
  --claude          start claude in the session (with dirigent's hooks)
  -V, --version     print the version
  -h, --help        print this help
  --install-desktop / --uninstall-desktop
                    add / remove the GNOME launcher entry and icon
";

/// Parsed command line (the path not yet checked).
#[derive(Debug, Default, PartialEq)]
pub struct Cmd {
    pub help: bool,
    pub new: bool,
    pub claude: bool,
    pub path: Option<String>,
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let mut cmd = Cmd::default();
    let mut only_paths = false;
    for a in args {
        match a.as_str() {
            "--" if !only_paths => only_paths = true,
            "-h" | "--help" if !only_paths => cmd.help = true,
            "--new" if !only_paths => cmd.new = true,
            "--claude" if !only_paths => cmd.claude = true,
            f if f.starts_with('-') && f.len() > 1 && !only_paths => return Err(format!("unknown option: {f}")),
            p if cmd.path.is_none() => cmd.path = Some(p.to_string()),
            p => return Err(format!("only one path, got another: {p}")),
        }
    }
    Ok(cmd)
}

/// What the running app is asked to do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// The session's directory (canonical); None: just start / focus.
    pub dir: Option<PathBuf>,
    /// The same directory as typed (absolute, symlinks kept): a tab's cwd comes from `$PWD`, which keeps them.
    pub alt_dir: Option<PathBuf>,
    /// A file to show in the preview.
    pub open: Option<PathBuf>,
    pub new: bool,
    pub claude: bool,
    /// The caller's activation token (Wayland `XDG_ACTIVATION_TOKEN`, X11 `DESKTOP_STARTUP_ID`).
    pub token: Option<String>,
}

/// Checks the path (relative to `cwd`) and builds the request; a missing path fails here, before anything starts.
pub fn request(cmd: &Cmd, cwd: &Path, token: Option<String>) -> Result<Request, String> {
    let mut req = Request { new: cmd.new, claude: cmd.claude, token, ..Default::default() };
    let Some(p) = &cmd.path else {
        if cmd.claude || cmd.new {
            // A session needs a directory: the current one.
            req.dir = Some(cwd.canonicalize().map_err(|e| format!("{}: {e}", cwd.display()))?);
            req.alt_dir = Some(cwd.to_path_buf());
        }
        return Ok(req);
    };
    let typed = cwd.join(p);
    let real = typed.canonicalize().map_err(|e| match e.kind() {
        ErrorKind::NotFound => format!("no such file or directory: {p}"),
        _ => format!("{p}: {e}"),
    })?;
    if real.is_dir() {
        req.dir = Some(real);
        req.alt_dir = Some(normalize(&typed));
    } else {
        req.dir = real.parent().map(Path::to_path_buf);
        req.alt_dir = normalize(&typed).parent().map(Path::to_path_buf);
        req.open = Some(real);
    }
    Ok(req)
}

/// `a/./b/` → `a/b` and `a/b/..` → `a` lexically (symlinks kept, unlike `canonicalize`).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// The socket: one per state file, so test runs (`DIRIGENT_STATE`) never reach your real app.
pub fn socket_path(runtime: &Path, state_override: Option<&Path>) -> PathBuf {
    match state_override {
        None => runtime.join("dirigent.sock"),
        Some(state) => runtime.join(format!("dirigent.{:016x}.sock", fnv(state))),
    }
}

/// FNV-1a: stable across Rust versions (an old running app and a new client agree).
fn fnv(p: &Path) -> u64 {
    p.as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Windows has no unix sockets: the pipe stands for the socket path (per user, since the path is in your temp
/// dir, and per state file like the socket).
#[cfg(windows)]
fn pipe_name(sock: &Path) -> String {
    format!("dirigent-{:016x}", fnv(sock))
}

#[cfg(windows)]
pub type Listener = hook::pipe::Server;

#[derive(Debug)]
pub enum Sent {
    /// The running app did it.
    Done,
    /// Nobody listens (no socket, or a stale one that was removed): start the app.
    NotRunning,
}

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST: u64 = 64 * 1024;

/// Hands the request to a running app.
pub fn send(sock: &Path, req: &Request) -> Result<Sent, String> {
    let Some(mut s) = connect(sock)? else { return Ok(Sent::NotRunning) };
    let mut line = serde_json::to_string(req).map_err(|e| e.to_string())?;
    line.push('\n');
    s.write_all(line.as_bytes()).map_err(|e| format!("sending to the running dirigent: {e}"))?;
    let reply = match read_reply(s) {
        Ok(r) => r,
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            return Err("the running dirigent didn't answer within 5 s".into());
        }
        Err(e) => return Err(format!("reading the reply: {e}")),
    };
    match reply.trim_end() {
        "ok" => Ok(Sent::Done),
        r => Err(r
            .strip_prefix("error: ")
            .unwrap_or(if r.is_empty() { "the running dirigent closed the connection" } else { r })
            .to_string()),
    }
}

/// The running app's socket; None when nobody listens (start the app).
#[cfg(unix)]
fn connect(sock: &Path) -> Result<Option<UnixStream>, String> {
    match UnixStream::connect(sock) {
        Ok(s) => {
            s.set_read_timeout(Some(REPLY_TIMEOUT)).map_err(|e| e.to_string())?;
            Ok(Some(s))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
            // Left by an instance that crashed (or was killed): nobody accepts on it.
            std::fs::remove_file(sock).map_err(|e| format!("can't remove stale socket {}: {e}", sock.display()))?;
            eprintln!("dirigent: removed a stale socket from a crashed instance ({})", sock.display());
            Ok(None)
        }
        Err(e) => Err(format!("{}: {e}", sock.display())),
    }
}

/// The running app's pipe; None when nobody serves it (a pipe vanishes with its process: never stale).
#[cfg(windows)]
fn connect(sock: &Path) -> Result<Option<std::fs::File>, String> {
    match hook::pipe::connect(&pipe_name(sock), REPLY_TIMEOUT) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", hook::pipe::path(&pipe_name(sock)))),
    }
}

#[cfg(unix)]
fn read_reply(s: UnixStream) -> std::io::Result<String> {
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).map(|_| reply)
}

/// Pipes have no read timeout: the read runs on a thread we stop waiting for.
#[cfg(windows)]
fn read_reply(s: std::fs::File) -> std::io::Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reply = String::new();
        let _ = tx.send(BufReader::new(s).read_line(&mut reply).map(|_| reply));
    });
    rx.recv_timeout(REPLY_TIMEOUT).unwrap_or_else(|_| Err(ErrorKind::TimedOut.into()))
}

/// What `main` hands the workspace: the request this process started with, and the bound socket (None: another
/// instance owns it, or binding failed).
pub struct Launch {
    pub req: Request,
    pub listener: Option<Listener>,
    pub sock: PathBuf,
}

/// A request from another process, waiting for the UI's answer.
pub struct Incoming {
    pub req: Request,
    reply: std::sync::mpsc::Sender<Result<(), String>>,
}

impl Incoming {
    pub fn reply(self, r: Result<(), String>) {
        let _ = self.reply.send(r);
    }
}

/// Binds the socket; `AddrInUse` means another instance is (just) listening there.
#[cfg(unix)]
pub fn bind(sock: &Path) -> std::io::Result<Listener> {
    if let Some(dir) = sock.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Listener::bind(sock)
}

/// Creates the pipe; `AddrInUse` means another instance serves it.
#[cfg(windows)]
pub fn bind(sock: &Path) -> std::io::Result<Listener> {
    Listener::bind(&pipe_name(sock), true)
}

/// One thread blocked in `accept` (no wakeups while idle); each request goes to the UI, its answer back.
pub fn serve(listener: Listener) -> async_channel::Receiver<Incoming> {
    let (tx, rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new().name("cli-listener".into()).spawn(move || {
        #[cfg(unix)]
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
            if !answer(&conn, &tx) {
                return; // the UI is gone
            }
        }
        // ponytail: no read timeout on a pipe; a local client that connects and never writes blocks the next
        // request (only your own processes can connect).
        #[cfg(windows)]
        while let Ok(conn) = listener.accept() {
            if !answer(conn.file(), &tx) {
                return; // the UI is gone
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("dirigent: no cli listener: {e}");
    }
    rx
}

/// Reads one request, waits for the UI, writes `ok` / `error: …`. False when the UI side is gone.
fn answer(mut conn: impl Read + Write, tx: &async_channel::Sender<Incoming>) -> bool {
    let mut line = String::new();
    let read = BufReader::new(Read::by_ref(&mut conn).take(MAX_REQUEST)).read_line(&mut line);
    let result = match read
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str::<Request>(&line).map_err(|e| e.to_string()))
    {
        Err(e) => Err(format!("bad request: {e}")),
        Ok(req) => {
            let (reply, answer) = std::sync::mpsc::channel();
            if tx.send_blocking(Incoming { req, reply }).is_err() {
                return false;
            }
            answer.recv_timeout(REPLY_TIMEOUT).unwrap_or_else(|_| Err("the app didn't handle the request".into()))
        }
    };
    let text = match result {
        Ok(()) => "ok\n".to_string(),
        Err(e) => format!("error: {}\n", e.replace('\n', " ")),
    };
    let _ = conn.write_all(text.as_bytes());
    true
}

/// Which tab a request lands in (`tabs[i]` = (cwd, its agent runs), `order` = visual order): the active one first,
/// then the others; with `--claude` only a tab whose agent already runs (typing `claude` into a busy shell could hit
/// a running program). None: make a new one.
pub fn pick(req: &Request, tabs: &[(&Path, bool)], active: usize, order: &[usize]) -> Option<usize> {
    if req.new {
        return None;
    }
    let dir = req.dir.as_deref()?;
    let fits = |&i: &usize| {
        tabs.get(i).is_some_and(|(cwd, agent)| {
            (*cwd == dir || Some(*cwd) == req.alt_dir.as_deref()) && (!req.claude || *agent)
        })
    };
    std::iter::once(active).chain(order.iter().copied()).find(fits)
}

/// Where the apt package puts the app (`/usr/bin/dirigent` links here).
pub const SYSTEM_EXE: &str = "/usr/lib/dirigent/dirigent";

/// The user-local install (`scripts/install.sh`) under `home`.
pub fn local_exe(home: &Path) -> PathBuf {
    home.join(".local/lib/dirigent/dirigent")
}

/// Running one install while the other exists: which one a terminal's `dirigent` starts depends on PATH
/// (`~/.local/bin` comes first), and `apt upgrade` updates only the package. None when there's nothing to say.
pub fn install_hint(exe: &Path, home: &Path, local_exists: bool, system_exists: bool) -> Option<String> {
    let local = local_exe(home);
    let switch = "Run scripts/uninstall.sh (from the source checkout or the release tarball; your sessions and \
                  settings stay) to keep only the package.";
    if exe == local && system_exists {
        Some(format!(
            "dirigent: this is the user-local install ({}); the apt package in /usr/lib/dirigent is installed too, \
             but ~/.local/bin/dirigent shadows /usr/bin/dirigent, and `apt upgrade` doesn't update this copy. {switch}",
            local.display()
        ))
    } else if exe == Path::new(SYSTEM_EXE) && local_exists {
        Some(format!(
            "dirigent: a user-local install also exists ({}); in a terminal, `dirigent` may start that older copy \
             (~/.local/bin comes first on PATH). {switch}",
            local.display()
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)] // the two Linux installs
    fn install_hint_only_when_both_installs_exist() {
        let home = Path::new("/home/u");
        let (local, system) = (local_exe(home), PathBuf::from(SYSTEM_EXE));
        let hint = |exe: &Path, l, s| install_hint(exe, home, l, s);
        assert!(hint(&local, true, true).unwrap().contains("shadows /usr/bin/dirigent"));
        assert!(hint(&system, true, true).unwrap().contains("/home/u/.local/lib/dirigent/dirigent"));
        assert_eq!(hint(&local, true, false), None, "only the local install");
        assert_eq!(hint(&system, false, true), None, "only the package");
        assert_eq!(hint(Path::new("/src/target/release/dirigent"), true, true), None, "a dev build says nothing");
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flags_and_one_path() {
        assert_eq!(parse(&args(&[])).unwrap(), Cmd::default());
        let c = parse(&args(&["--claude", ".", "--new"])).unwrap();
        assert_eq!(c, Cmd { help: false, new: true, claude: true, path: Some(".".into()) });
        assert!(parse(&args(&["-h"])).unwrap().help);
        assert_eq!(parse(&args(&["--", "-odd"])).unwrap().path.as_deref(), Some("-odd"));
        assert!(parse(&args(&["--bogus"])).unwrap_err().contains("--bogus"));
        assert!(parse(&args(&["a", "b"])).unwrap_err().contains("only one path"));
    }

    /// Per process (like the other test helpers): two test runs at once (CI shards, a run while another is going)
    /// would otherwise delete and rebind each other's sockets.
    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/tmp"))
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("real/sub")).unwrap();
        std::fs::write(d.join("real/sub/f.rs"), "fn main() {}\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    #[cfg(unix)] // symlinks need a privilege on Windows
    fn resolves_dirs_files_symlinks_and_missing() {
        let d = tmp("cli-resolve");
        let cmd = |p: &str| Cmd { path: Some(p.into()), ..Default::default() };
        let r = request(&cmd("real/sub/."), &d, None).unwrap();
        assert_eq!(r.dir, Some(d.join("real/sub")));
        assert_eq!(r.open, None);
        let r = request(&cmd("link/sub/f.rs"), &d, Some("tok".into())).unwrap();
        assert_eq!(r.dir, Some(d.join("real/sub")), "canonical");
        assert_eq!(r.alt_dir, Some(d.join("link/sub")), "as typed");
        assert_eq!(r.open, Some(d.join("real/sub/f.rs")));
        assert_eq!(r.token.as_deref(), Some("tok"));
        assert_eq!(request(&cmd("nope"), &d, None).unwrap_err(), "no such file or directory: nope");
        let r = request(&Cmd::default(), &d, None).unwrap();
        assert_eq!(r.dir, None, "no path, no flags: only start/focus");
        let r = request(&Cmd { claude: true, ..Default::default() }, &d, None).unwrap();
        assert_eq!(r.dir, Some(d.clone()), "--claude alone: here");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn test_runs_get_their_own_socket() {
        let rt = Path::new("/run/user/1000");
        assert_eq!(socket_path(rt, None), rt.join("dirigent.sock"));
        let a = socket_path(rt, Some(Path::new("/tmp/a/state.json")));
        assert_ne!(a, socket_path(rt, None));
        assert_ne!(a, socket_path(rt, Some(Path::new("/tmp/b/state.json"))));
        assert_eq!(a, socket_path(rt, Some(Path::new("/tmp/a/state.json"))), "stable");
    }

    #[test]
    fn picks_active_then_order_and_claude_needs_an_agent() {
        let (a, b) = (Path::new("/p/a"), Path::new("/p/b"));
        let tabs = [(a, false), (b, false), (a, true)];
        let req = |dir: &Path, new, claude| Request { dir: Some(dir.into()), new, claude, ..Default::default() };
        assert_eq!(pick(&req(a, false, false), &tabs, 1, &[1, 2, 0]), Some(2), "visual order");
        assert_eq!(pick(&req(a, false, false), &tabs, 0, &[1, 2, 0]), Some(0), "the active one first");
        assert_eq!(pick(&req(a, true, false), &tabs, 0, &[0, 1, 2]), None, "--new");
        assert_eq!(pick(&req(a, false, true), &tabs, 0, &[0, 1, 2]), Some(2), "--claude: only a running agent");
        assert_eq!(pick(&req(b, false, true), &tabs, 0, &[0, 1, 2]), None);
        let alt = Request { dir: Some("/real/a".into()), alt_dir: Some(a.into()), ..Default::default() };
        assert_eq!(pick(&alt, &tabs, 1, &[1, 0, 2]), Some(0), "symlinked cwd matches the typed path");
    }

    /// A socket file nobody listens on, made by another process that then exits. Not bound and closed here: another
    /// test spawning a process at that moment hands the child a copy of the listener until the child's exec, and a
    /// connect in that window succeeds instead of being refused (it failed under load).
    #[cfg(unix)]
    fn stale_socket(path: &Path) {
        let made = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::tests::make_stale_socket", "--ignored", "--quiet"])
            .env("DIRIGENT_TEST_STALE_SOCKET", path)
            .output()
            .unwrap();
        assert!(made.status.success() && path.exists(), "{}", String::from_utf8_lossy(&made.stdout));
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "helper: run in its own process by socket_round_trip_stale_race_and_timeout"]
    fn make_stale_socket() {
        if let Some(p) = std::env::var_os("DIRIGENT_TEST_STALE_SOCKET") {
            drop(bind(Path::new(&p)).unwrap());
        }
    }

    #[test]
    fn socket_round_trip_stale_race_and_timeout() {
        let d = tmp("cli-socket");
        let sock = d.join("s.sock");
        let req = Request { dir: Some(d.clone()), claude: true, ..Default::default() };
        assert!(matches!(send(&sock, &req), Ok(Sent::NotRunning)), "no socket");

        // A live server: the request arrives, the UI's answer goes back.
        let rx = serve(bind(&sock).unwrap());
        let ui = std::thread::spawn(move || {
            let inc = rx.recv_blocking().unwrap();
            let got = inc.req.clone();
            inc.reply(Ok(()));
            let inc = rx.recv_blocking().unwrap();
            inc.reply(Err("nope".into()));
            got
        });
        assert!(matches!(send(&sock, &req), Ok(Sent::Done)));
        assert_eq!(send(&sock, &req).err().as_deref(), Some("nope"));
        assert_eq!(ui.join().unwrap(), req);
        // A second instance racing to start can't bind over it.
        assert_eq!(bind(&sock).unwrap_err().kind(), ErrorKind::AddrInUse);

        // Stale: the file stays, nobody accepts → removed, then binding works again. (A pipe dies with its
        // process: nothing is ever stale on Windows.)
        #[cfg(unix)]
        {
            let stale = d.join("stale.sock");
            stale_socket(&stale);
            assert!(matches!(send(&stale, &req), Ok(Sent::NotRunning)));
            assert!(!stale.exists());
            drop(bind(&stale).unwrap());
        }

        // A listener that never answers: the client gives up instead of hanging.
        let hang = d.join("hang.sock");
        let _l = bind(&hang).unwrap(); // accepted by the kernel backlog (Windows: the pipe instance), never read
        let t = std::time::Instant::now();
        assert!(send(&hang, &req).unwrap_err().contains("didn't answer"));
        assert!(t.elapsed() < Duration::from_secs(7));
        let _ = std::fs::remove_dir_all(&d);
    }
}
