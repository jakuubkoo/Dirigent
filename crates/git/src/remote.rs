//! Network git (fetch, fast-forward, push, publish) and the remote's host: which forge it is and the URLs we open
//! in the browser. Blocking: run off the UI thread, under the repo's write lock.
//!
//! Credentials are the user's own setup (ssh agent, credential helper, askpass). We never ask for or store
//! passwords: commands run without a terminal (`GIT_TERMINAL_PROMPT=0`, own session = no controlling tty, so ssh
//! can't sit on a passphrase prompt), and git's own error is shown when auth fails.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::write::{GitError, run};

type Result<T> = std::result::Result<T, GitError>;

/// Set it to stop a running network command (the child's whole process group is killed).
pub type Cancel = Arc<AtomicBool>;

/// A network command that hangs this long is killed.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// stdout and stderr of a finished tool run.
#[derive(Debug, Default)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

/// Runs `program args` in `dir` with no terminal: stdin null, its own session (no controlling tty), prompts off.
/// Killed (with its children, e.g. ssh) on `cancel` or after `timeout`. Non-zero exit → `Failed` with the output.
pub fn run_tool(program: &str, dir: &Path, args: &[&str], cancel: &Cancel, timeout: Duration) -> Result<Output> {
    use std::os::unix::process::CommandExt as _;
    let mut c = Command::new(program);
    c.args(args)
        .current_dir(dir)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and touches no memory of ours.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.spawn().map_err(GitError::Io)?;
    let pid = child.id() as i32;
    let reader = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = reader(Box::new(child.stdout.take().expect("piped")));
    let err = reader(Box::new(child.stderr.take().expect("piped")));
    let started = Instant::now();
    // ponytail: 50 ms wait loop on a background thread (no pidfd wait in std); only while a command runs.
    let status = loop {
        if let Some(s) = child.try_wait().map_err(GitError::Io)? {
            break Some(s);
        }
        if cancel.load(Ordering::Relaxed) || started.elapsed() > timeout {
            // SAFETY: the child leads its own process group (setsid): this kills it and e.g. its ssh.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let o = Output { stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() };
    match status {
        None if cancel.load(Ordering::Relaxed) => Err(GitError::Cancelled),
        None => Err(GitError::Failed(format!("{program} took longer than {} s and was stopped", timeout.as_secs()))),
        Some(s) if s.success() => Ok(o),
        Some(_) => Err(classify(o)),
    }
}

/// Auth failures get a line saying where to fix them (the user's git setup), above git's own message.
fn classify(o: Output) -> GitError {
    let text = format!("{}{}", o.stdout, o.stderr).trim().to_string();
    let auth = [
        "terminal prompts disabled",
        "Permission denied (publickey",
        "Authentication failed",
        "could not read Username",
        "could not read Password",
        "Host key verification failed",
    ];
    if text.contains("Not possible to fast-forward") {
        return GitError::NotFastForward;
    }
    if auth.iter().any(|a| text.contains(a)) {
        return GitError::Failed(format!(
            "Authentication failed. Dirigent uses your git setup (ssh agent, credential helper): fix it in a terminal, then retry.\n\n{text}"
        ));
    }
    GitError::Failed(text)
}

fn git(root: &Path, args: &[&str], cancel: &Cancel) -> Result<Output> {
    run_tool("git", root, args, cancel, TIMEOUT)
}

/// `git fetch --prune` (the upstream's remote, else origin: git's own default).
pub fn fetch(root: &Path, cancel: &Cancel) -> Result<()> {
    git(root, &["fetch", "--prune"], cancel).map(drop)
}

/// Fast-forwards the branch to its upstream (`merge --ff-only @{u}`, so no pull.rebase config applies). Local
/// changes that the update would overwrite → git refuses, nothing changes. Diverged → `NotFastForward`.
pub fn fast_forward(root: &Path, cancel: &Cancel) -> Result<()> {
    git(root, &["merge", "--ff-only", "@{u}"], cancel).map(drop)
}

/// `git push` with the user's push.default and upstream. Returns git's stderr (the remote's messages).
pub fn push(root: &Path, cancel: &Cancel) -> Result<String> {
    git(root, &["push"], cancel).map(|o| o.stderr)
}

/// `git push [-u] [-o opt]... <remote> HEAD`: publishes the branch under its own name (`-u` sets the upstream);
/// push options (GitLab merge requests) only take effect when the push sends something. Returns git's stderr.
pub fn push_to(root: &Path, remote: &str, set_upstream: bool, options: &[String], cancel: &Cancel) -> Result<String> {
    let mut args = vec!["push"];
    if set_upstream {
        args.push("-u");
    }
    for o in options {
        args.extend(["-o", o.as_str()]);
    }
    args.extend([remote, "HEAD"]);
    git(root, &args, cancel).map(|o| o.stderr)
}

fn config(root: &Path, key: &str) -> Option<String> {
    let v = run(root, &["config", "--get", key], None).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The configured remotes, in git's order.
pub fn remotes(root: &Path) -> Vec<String> {
    run(root, &["remote"], None).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

/// Where this branch is pushed: `branch.<b>.pushRemote`, `remote.pushDefault`, the upstream's remote, `origin`,
/// the only remote. None: no remote, or several and nothing chooses one (the caller lists them).
pub fn push_target(root: &Path, branch: &str, upstream: Option<&str>) -> Option<String> {
    let all = remotes(root);
    let pick = config(root, &format!("branch.{branch}.pushRemote"))
        .or_else(|| config(root, "remote.pushDefault"))
        .or_else(|| upstream.and_then(|u| all.iter().find(|r| u.starts_with(&format!("{r}/"))).cloned()))
        .or_else(|| all.iter().find(|r| *r == "origin").cloned())
        .or_else(|| (all.len() == 1).then(|| all[0].clone()));
    pick.filter(|r| all.contains(r))
}

/// The remote's push URL (`git remote get-url --push`).
pub fn push_url(root: &Path, remote: &str) -> Option<String> {
    let v = run(root, &["remote", "get-url", "--push", remote], None).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The remote's default branch: `refs/remotes/<r>/HEAD`, else `main`/`master` if the remote has it, else `main`.
pub fn default_branch(root: &Path, remote: &str) -> String {
    let head = format!("refs/remotes/{remote}/HEAD");
    if let Ok(s) = run(root, &["symbolic-ref", "--short", &head], None)
        && let Some(b) = s.trim().strip_prefix(&format!("{remote}/"))
    {
        return b.to_string();
    }
    for b in ["main", "master"] {
        if run(root, &["rev-parse", "--verify", "-q", &format!("refs/remotes/{remote}/{b}")], None).is_ok() {
            return b.to_string();
        }
    }
    "main".into()
}

/// Whether `<remote>/<branch>` exists and equals HEAD (a push would send nothing).
pub fn remote_has_head(root: &Path, remote: &str, branch: &str) -> bool {
    let theirs = run(root, &["rev-parse", "--verify", "-q", &format!("refs/remotes/{remote}/{branch}")], None);
    let ours = run(root, &["rev-parse", "HEAD"], None);
    matches!((theirs, ours), (Ok(a), Ok(b)) if a.trim() == b.trim())
}

/// The last commit's subject and body.
pub fn last_commit(root: &Path) -> (String, String) {
    let s = run(root, &["log", "-1", "--format=%s%x00%b"], None).unwrap_or_default();
    let (subject, body) = s.split_once('\0').unwrap_or((&s, ""));
    (subject.trim().to_string(), body.trim().to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
    Other,
}

/// A remote's web location, from its URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub forge: Forge,
    /// `github.com`, `gitlab.jakuubkoo.me` (no port, no user).
    pub host: String,
    /// `owner/repo`, `group/sub/project` (no `.git`).
    pub path: String,
}

impl Host {
    /// `git@host:owner/repo.git`, `ssh://git@host:2222/owner/repo.git`, `https://user@host/group/sub/repo(.git)`.
    /// None for local paths and anything without a host and a path.
    pub fn parse(url: &str) -> Option<Host> {
        let url = url.trim();
        let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
            if !matches!(scheme, "ssh" | "git+ssh" | "https" | "http" | "git") {
                return None;
            }
            let (auth, path) = rest.split_once('/')?;
            let host = auth.rsplit('@').next()?;
            (host.split(':').next()?, path)
        } else {
            // scp-like `[user@]host:path`, but not a local path (`./x`, `/x`, `C:\x`).
            let (auth, path) = url.split_once(':')?;
            if auth.contains('/') || auth.len() < 2 || path.starts_with('\\') {
                return None;
            }
            (auth.rsplit('@').next()?, path)
        };
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
        if host.is_empty() || path.is_empty() || !path.contains('/') {
            return None;
        }
        let host = host.to_ascii_lowercase();
        let forge = if host == "github.com" {
            Forge::GitHub
        } else if host.contains("gitlab") {
            Forge::GitLab
        } else {
            Forge::Other
        };
        Some(Host { forge, host, path: path.to_string() })
    }

    fn base(&self) -> String {
        format!("https://{}/{}", self.host, self.path)
    }

    /// GitHub: the compare page with the "Create pull request" form open.
    pub fn compare_url(&self, base: &str, branch: &str) -> String {
        format!("{}/compare/{}...{}?expand=1", self.base(), enc(base, true), enc(branch, true))
    }

    /// The branch's pull/merge requests (any state).
    pub fn requests_url(&self, branch: &str) -> String {
        match self.forge {
            Forge::GitLab => {
                format!("{}/-/merge_requests?scope=all&state=all&source_branch={}", self.base(), enc(branch, false))
            }
            _ => format!("{}/pulls?q={}", self.base(), enc(&format!("is:pr head:{branch}"), false)),
        }
    }

    /// GitLab: the new merge request form with both branches filled in.
    pub fn new_mr_url(&self, branch: &str, target: &str) -> String {
        format!(
            "{}/-/merge_requests/new?{}={}&{}={}",
            self.base(),
            enc("merge_request[source_branch]", false),
            enc(branch, false),
            enc("merge_request[target_branch]", false),
            enc(target, false)
        )
    }
}

/// Percent-encodes everything but unreserved characters (and `/` in paths).
fn enc(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What GitLab said about a merge request in a push's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MrPush {
    /// `View merge request for <b>:` (created by the push options, or one already open).
    Created(String),
    /// `To create a merge request for <b>, visit:` (none was created).
    Suggested(String),
    None,
}

/// Reads the URL GitLab prints after a push (on the line after the marker, prefixed with `remote:`).
pub fn parse_mr_push(stderr: &str) -> MrPush {
    let lines: Vec<&str> = stderr.lines().map(|l| l.trim_start_matches("remote:").trim()).collect();
    let url_after = |i: usize| lines[i + 1..].iter().find(|l| !l.is_empty()).filter(|l| l.starts_with("http"));
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("View merge request for")
            && let Some(u) = url_after(i)
        {
            return MrPush::Created(u.to_string());
        }
        if l.starts_with("To create a merge request for")
            && let Some(u) = url_after(i)
        {
            return MrPush::Suggested(u.to_string());
        }
    }
    MrPush::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        let gh = |s| Host::parse(s).map(|h| (h.forge, h.host, h.path));
        let want = |f, h: &str, p: &str| Some((f, h.to_string(), p.to_string()));
        assert_eq!(
            gh("git@github.com:jakuubkoo/Dirigent.git"),
            want(Forge::GitHub, "github.com", "jakuubkoo/Dirigent")
        );
        assert_eq!(
            gh("https://github.com/jakuubkoo/Dirigent"),
            want(Forge::GitHub, "github.com", "jakuubkoo/Dirigent")
        );
        assert_eq!(gh("https://user:tok@github.com/a/b.git/"), want(Forge::GitHub, "github.com", "a/b"));
        assert_eq!(
            gh("ssh://git@gitlab.jakuubkoo.me:2222/jakuubkoo/testingprojectlol.git"),
            want(Forge::GitLab, "gitlab.jakuubkoo.me", "jakuubkoo/testingprojectlol")
        );
        assert_eq!(
            gh("https://gitlab.jakuubkoo.me/group/sub/project.git"),
            want(Forge::GitLab, "gitlab.jakuubkoo.me", "group/sub/project")
        );
        assert_eq!(gh("git@GitLab.com:g/p"), want(Forge::GitLab, "gitlab.com", "g/p"));
        assert_eq!(gh("git@codeberg.org:me/x.git"), want(Forge::Other, "codeberg.org", "me/x"));
        for local in ["/srv/repo.git", "../repo", "./x:y", "file:///srv/r.git", "C:\\repo", "host:repo"] {
            assert_eq!(gh(local), None, "{local}");
        }
    }

    #[test]
    fn urls() {
        let h = Host::parse("git@github.com:o/r.git").unwrap();
        assert_eq!(h.compare_url("main", "feat/x#1"), "https://github.com/o/r/compare/main...feat/x%231?expand=1");
        assert_eq!(h.requests_url("feat/x"), "https://github.com/o/r/pulls?q=is%3Apr%20head%3Afeat%2Fx");
        let g = Host::parse("https://gitlab.example.com/g/sub/p.git").unwrap();
        assert_eq!(
            g.requests_url("feat/x"),
            "https://gitlab.example.com/g/sub/p/-/merge_requests?scope=all&state=all&source_branch=feat%2Fx"
        );
        assert_eq!(
            g.new_mr_url("feat/x", "main"),
            "https://gitlab.example.com/g/sub/p/-/merge_requests/new?merge_request%5Bsource_branch%5D=feat%2Fx&merge_request%5Btarget_branch%5D=main"
        );
    }

    #[test]
    fn errors() {
        let o = |e: &str| classify(Output { stdout: String::new(), stderr: e.into() }).to_string();
        let https = "fatal: could not read Username for 'https://github.com': terminal prompts disabled";
        assert!(o(https).starts_with("Authentication failed.") && o(https).ends_with(https));
        assert!(
            o("git@x: Permission denied (publickey).\nfatal: Could not read from remote").starts_with("Authentication")
        );
        assert!(matches!(
            classify(Output { stdout: String::new(), stderr: "fatal: Not possible to fast-forward, aborting.".into() }),
            GitError::NotFastForward
        ));
        assert_eq!(o("fatal: bad thing\n"), "fatal: bad thing");
    }

    #[test]
    fn gitlab_push_output() {
        let created = "remote: \nremote: View merge request for feat/x:\nremote:   https://gl.example/g/p/-/merge_requests/12\nremote: \nTo gl.example:g/p.git\n * [new branch]      HEAD -> feat/x\n";
        assert_eq!(parse_mr_push(created), MrPush::Created("https://gl.example/g/p/-/merge_requests/12".into()));
        let hint = "remote: \nremote: To create a merge request for feat/x, visit:\nremote:   https://gl.example/g/p/-/merge_requests/new?merge_request%5Bsource_branch%5D=feat%2Fx\nremote: \n";
        assert_eq!(
            parse_mr_push(hint),
            MrPush::Suggested(
                "https://gl.example/g/p/-/merge_requests/new?merge_request%5Bsource_branch%5D=feat%2Fx".into()
            )
        );
        assert_eq!(parse_mr_push("Everything up-to-date\n"), MrPush::None);
    }
}
