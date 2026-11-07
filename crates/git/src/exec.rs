//! Where a built git command runs: as built (natively, or per call through `wsl.exe`), or, for a command
//! `wsl::command` built while its distro has a git server, as a request to that server (`wsl::server`).

use std::io::{self, Read, Write};
use std::process::{Child, ChildStdout, Command, ExitStatus, Output};
use std::time::{Duration, Instant};

use crate::wsl;

/// Reads (the `git()` builder): a hung git is stopped after this (server path).
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Writes run the user's hooks (pre-commit can lint for a while).
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(300);

/// The request for `c` (`wsl.exe -d D --cd L -e prog args…`) and the server of D, if it has one.
fn request(c: &Command, stdin: Option<&[u8]>) -> Option<(String, gitserve::Client, gitserve::Run)> {
    if c.get_program() != "wsl.exe" {
        return None;
    }
    let args = c.get_args().map(|a| a.to_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?;
    let [d, distro, cd, cwd, e, argv @ ..] = args.as_slice() else { return None };
    if (d.as_str(), cd.as_str(), e.as_str()) != ("-d", "--cd", "-e") || argv.is_empty() {
        return None;
    }
    let server = wsl::server(distro)?;
    // What `wsl.exe` would pass in: the names in WSLENV; the git variables never cross either way.
    let mut env: Vec<(String, Option<String>)> = c
        .get_envs()
        .filter_map(|(k, v)| {
            let k = k.to_str()?;
            wsl::ENV.split(':').any(|n| n == k).then(|| (k.to_owned(), v.and_then(|v| v.to_str()).map(str::to_owned)))
        })
        .collect();
    env.extend(["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"].map(|k| (k.to_owned(), None)));
    let run = gitserve::Run { cwd: cwd.clone(), argv: argv.to_vec(), env, stdin: stdin.unwrap_or_default().to_vec() };
    Some((distro.clone(), server, run))
}

fn status(code: i32) -> ExitStatus {
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt as _;
        ExitStatus::from_raw(code as u32)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        ExitStatus::from_raw(if code >= 0 { (code & 0xff) << 8 } else { -code })
    }
}

/// Runs `c` to the end with `stdin` written to it (the command's stdin must be piped then). `retry`: a read-only
/// command whose server died meanwhile is sent once more to the restarted one; `timeout` applies on the server.
pub(crate) fn output(c: &mut Command, stdin: Option<&[u8]>, timeout: Duration, retry: bool) -> io::Result<Output> {
    for attempt in 0..1 + retry as usize {
        let Some((_, server, run)) = request(c, stdin) else { break };
        match server.request(&run).and_then(|p| p.output(timeout)) {
            Ok(d) => return Ok(Output { status: status(d.code), stdout: d.stdout, stderr: d.stderr }),
            Err(e) if gitserve::is_stopped(&e) && attempt == 0 && retry => {}
            Err(e) => return Err(e),
        }
    }
    if c.get_program() == "wsl.exe" {
        wsl::count_spawn();
    }
    let Some(input) = stdin else { return c.output() };
    let mut child = c.spawn()?;
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(input)?;
    }
    child.wait_with_output()
}

/// A command's stdout as it comes (`diff`). Dropped before its end, the command is stopped.
pub(crate) enum Stream {
    Child {
        child: Child,
        stdout: ChildStdout,
        ended: bool,
    },
    Server {
        distro: String,
        server: gitserve::Client,
        run: gitserve::Run,
        pending: gitserve::Pending,
        /// Output was handed out, or the one retry was used: a server death now ends the stream.
        no_retry: bool,
        end: Instant,
    },
}

pub(crate) enum Chunk {
    Data(Vec<u8>),
    End,
    Cancelled,
    /// The server went away and no new one took the request.
    Failed,
}

/// Starts read-only `c` with its stdout piped.
pub(crate) fn stream(c: &mut Command) -> io::Result<Stream> {
    if let Some((distro, server, run)) = request(c, None) {
        let pending = server.request(&run)?;
        let end = Instant::now() + READ_TIMEOUT;
        return Ok(Stream::Server { distro, server, run, pending, no_retry: false, end });
    }
    if c.get_program() == "wsl.exe" {
        wsl::count_spawn();
    }
    let mut child = c.stdout(std::process::Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
    Ok(Stream::Child { child, stdout, ended: false })
}

impl Stream {
    /// The next piece of output. `cancelled` is asked before every read, and every 100 ms while a server request
    /// waits (a stalled git can't hold up a cancel); past `READ_TIMEOUT` a server request counts as cancelled.
    pub(crate) fn next(&mut self, cancelled: &dyn Fn() -> bool) -> Chunk {
        match self {
            Stream::Child { stdout, ended, .. } => {
                if cancelled() {
                    return Chunk::Cancelled;
                }
                let mut buf = vec![0; gitserve::CHUNK];
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        *ended = true;
                        Chunk::End
                    }
                    Ok(n) => {
                        buf.truncate(n);
                        Chunk::Data(buf)
                    }
                }
            }
            Stream::Server { distro, server, run, pending, no_retry, end } => loop {
                if cancelled() || Instant::now() > *end {
                    return Chunk::Cancelled;
                }
                match pending.next(Duration::from_millis(100)) {
                    Ok(Some(gitserve::Event::Out(b))) => {
                        *no_retry = true;
                        return Chunk::Data(b);
                    }
                    Ok(Some(gitserve::Event::Exit { .. })) => return Chunk::End,
                    Ok(None) => {}
                    // Died before anything was handed out: once more on the restarted server.
                    Err(e) if gitserve::is_stopped(&e) && !*no_retry => {
                        *no_retry = true;
                        let Some(restarted) = wsl::server(distro).and_then(|s| s.request(run).ok().map(|p| (s, p)))
                        else {
                            return Chunk::Failed;
                        };
                        (*server, *pending) = restarted;
                    }
                    Err(_) => return Chunk::Failed,
                }
            },
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Stream::Child { child, ended, .. } = self {
            if !*ended {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
