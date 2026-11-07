//! Read-only git for the app: repo discovery, working-tree status with line counts, one file's diff, and a
//! watcher that refreshes the status only after real changes. No UI dependencies. Shells out to `git`.

mod fs;
pub mod index;
pub mod layout;
pub mod remote;
pub mod search;
pub mod text;
mod watch;
pub mod write;
pub mod wsl;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub use fs::{DirWatcher, Entry, FileWatcher, Listing, list_dir};
pub use watch::{Toucher, Update, Watcher};

/// Files listed per repo; the rest are counted in `Status::more`.
pub const MAX_FILES: usize = 2000;
/// Untracked files bigger than this aren't read to count their lines.
const MAX_COUNT_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Repo {
    /// Working tree root (`--show-toplevel`).
    pub root: PathBuf,
    /// `--absolute-git-dir` (outside `root` for linked worktrees: `<main>/.git/worktrees/<name>`).
    pub git_dir: PathBuf,
    /// `--git-common-dir` (absolute): the shared `.git` with refs and objects; == `git_dir` except in linked worktrees.
    pub common_dir: PathBuf,
}

impl Repo {
    /// A linked worktree's main checkout (where its shared `.git` lives); None for the main checkout itself.
    pub fn main_root(&self) -> Option<&Path> {
        (self.common_dir != self.git_dir && self.common_dir.file_name().is_some_and(|n| n == ".git"))
            .then(|| self.common_dir.parent())
            .flatten()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Discovery {
    Repo(Repo),
    NotRepo,
    GitMissing,
}

/// `DIRIGENT_PERF=2`: logs watcher setup, git status timings and ignored event batches.
fn perf(what: impl FnOnce() -> String) {
    static VERBOSE: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var("DIRIGENT_PERF").is_ok_and(|v| v == "2"));
    if *VERBOSE {
        eprintln!("[perf] {}", what());
    }
}

/// `Command::new` for every program we start (git, gh, glab, claude, …). On Windows it adds `CREATE_NO_WINDOW`:
/// a GUI app's child would otherwise flash a console window.
pub fn cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// A path git printed (`C:/x` on Windows) in the native form: `C:\x`, drive letter uppercase. Unix: as is.
pub fn native_path(s: &str) -> PathBuf {
    if cfg!(windows) { upper_drive(s.replace('/', "\\")).into() } else { s.into() }
}

/// A `canonicalize` result without Windows' `\\?\` prefix (git can't use it) when the rest is a drive path; UNC
/// paths are kept, except a WSL distro's (`\\?\UNC\wsl$\D\x`), which becomes the app's `\\wsl.localhost\D\x`.
/// Drive letter uppercase. Unix: as is.
pub fn plain_path(p: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return p;
    }
    match wsl::of(&p) {
        Some((distro, linux)) => wsl::to_windows(&distro, &linux),
        None => strip_verbatim(p),
    }
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    if s.as_bytes().get(1) != Some(&b':') {
        return p; // UNC (`\\?\UNC\…`, `\\server\…`) or relative
    }
    upper_drive(s.to_string()).into()
}

fn upper_drive(mut s: String) -> String {
    if s.as_bytes().get(1) == Some(&b':') {
        s[..1].make_ascii_uppercase();
    }
    s
}

/// A repo path git printed as the identity key for projects, history and the watcher. Windows: the real on-disk
/// path (case as stored, long names instead of 8.3, `\`, no `\\?\`), so `c:\USERS\x` and `C:\Users\X` are one
/// project. A `subst` or mapped network drive keeps git's form: its canonical path is another drive or UNC, and
/// the panels must keep matching the shell's cwd on that drive. Unix: as printed.
pub fn root_key(s: &str) -> PathBuf {
    let p = native_path(s);
    if cfg!(windows) { p.canonicalize().map(|c| same_drive_or(p.clone(), c)).unwrap_or(p) } else { p }
}

/// `root_key` for a path git printed while running in `dir`: inside a WSL distro git prints Linux paths, which
/// become the app's `\\wsl.localhost\D\x` with the distro named as WSL lists it (never canonicalized: 9P is slow
/// and Linux is case-sensitive), or a drive path for `/mnt/c/x`, which then gets the drive rules.
pub fn root_key_in(dir: &Path, printed: &str) -> PathBuf {
    match wsl::of(dir) {
        Some((distro, _)) => {
            let p = wsl::to_windows(&wsl::listed(&distro), printed);
            if wsl::of(&p).is_some() { p } else { root_key(&p.to_string_lossy()) }
        }
        None => root_key(printed),
    }
}

/// `canon` without `\\?\` when it is on `printed`'s drive, else `printed`.
fn same_drive_or(printed: PathBuf, canon: PathBuf) -> PathBuf {
    let drive = |p: &Path| {
        p.to_str().filter(|s| s.as_bytes().get(1) == Some(&b':')).map(|s| s.as_bytes()[0].to_ascii_uppercase())
    };
    let canon = strip_verbatim(canon);
    if drive(&printed).is_some() && drive(&printed) == drive(&canon) { canon } else { printed }
}

/// `path` is `root` or inside it. Windows compares components ignoring ASCII case (a shell's cwd keeps the case
/// it was typed in, `cd c:\users`), except inside a WSL distro: the distro name ignores case, the Linux path
/// doesn't. Unix is `Path::starts_with`.
pub fn path_under(path: &Path, root: &Path) -> bool {
    if !cfg!(windows) {
        return path.starts_with(root);
    }
    match (wsl::of(path), wsl::of(root)) {
        (Some((pd, p)), Some((rd, r))) => return pd.eq_ignore_ascii_case(&rd) && wsl::under(&p, &r),
        (None, None) => {}
        _ => return false,
    }
    let mut p = path.components();
    root.components().all(|r| p.next().is_some_and(|c| c.as_os_str().eq_ignore_ascii_case(r.as_os_str())))
}

/// Same directory (`path_under` both ways).
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_under(a, b) && path_under(b, a)
}

/// `git` with the options every call needs. `--no-optional-locks`: a plain `git status` refreshes and rewrites
/// `.git/index`, which our own watcher would see as a change (a refresh loop).
fn git(dir: &Path) -> Command {
    let mut c = native_or_wsl(dir, "git");
    c.arg("--no-optional-locks")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// `program` in `dir`: natively, or inside the distro for a WSL path (`wsl::command`).
pub(crate) fn native_or_wsl(dir: &Path, program: &str) -> Command {
    wsl::command(dir, program).unwrap_or_else(|| {
        let mut c = cmd(program);
        c.current_dir(dir);
        c
    })
}

fn output(mut c: Command) -> std::io::Result<Vec<u8>> {
    let out = c.output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(std::io::Error::other(format!("git exited with {}", out.status)))
    }
}

/// Which repo `dir` is in. Blocking (one ~2 ms `git rev-parse`): call off the UI thread.
pub fn discover(dir: &Path) -> Discovery {
    let mut c = git(dir);
    c.args(["rev-parse", "--show-toplevel", "--absolute-git-dir", "--path-format=absolute", "--git-common-dir"]);
    match c.output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Discovery::GitMissing,
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines();
            match (lines.next(), lines.next(), lines.next()) {
                (Some(root), Some(git_dir), common) => Discovery::Repo(Repo {
                    root: root_key_in(dir, root),
                    git_dir: root_key_in(dir, git_dir),
                    common_dir: root_key_in(dir, common.unwrap_or(git_dir)),
                }),
                _ => Discovery::NotRepo, // bare repo or inside .git: no working tree
            }
        }
        _ => Discovery::NotRepo,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// Branch name, or `(detached abc1234)`.
    pub branch: String,
    /// The branch's upstream (`origin/main`); None when it has none.
    pub upstream: Option<String>,
    /// (ahead, behind) vs the upstream; None without one or when the upstream ref is gone.
    pub ab: Option<(u32, u32)>,
    pub files: Vec<FileChange>,
    /// Changed files beyond `MAX_FILES` (not listed, not counted in the line totals).
    pub more: usize,
    /// Set when the repo couldn't be fully watched (e.g. the OS watch limit): updates may be missed.
    pub watch_error: Option<String>,
}

impl Status {
    /// (changed files, lines added, lines removed) — the sidebar summary.
    pub fn totals(&self) -> (usize, u64, u64) {
        let (mut add, mut del) = (0, 0);
        for f in &self.files {
            if let Some((a, d)) = f.lines {
                add += a as u64;
                del += d as u64;
            }
        }
        (self.files.len() + self.more, add, del)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    /// Repo-relative path (the new one for renames).
    pub path: String,
    /// The old path of a rename/copy.
    pub orig: Option<String>,
    /// M modified, A added, D deleted, R renamed, C copied, T type change, U unmerged, ? untracked.
    pub kind: char,
    /// Has changes in the index (vs HEAD).
    pub staged: bool,
    /// Has changes in the working tree (vs the index); untracked files count as unstaged.
    pub unstaged: bool,
    /// (+, −) vs HEAD, staged and unstaged together; None for binary or too-big files.
    pub lines: Option<(u32, u32)>,
}

/// Working-tree status with +/- per file. Blocking (two or three `git` calls): run off the UI thread.
pub fn status(repo: &Repo) -> std::io::Result<Status> {
    let mut c = git(&repo.root);
    c.args(["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"]);
    let (mut st, unborn) = parse_status(&output(c)?);
    if st.files.iter().any(|f| f.kind != '?') {
        let mut c = git(&repo.root);
        c.args(["diff", "--numstat", "-z", "-M", "--no-ext-diff", "--no-textconv"]);
        c.arg(if unborn { empty_tree(repo)? } else { "HEAD".into() });
        apply_numstat(&mut st, &output(c)?);
    }
    for f in st.files.iter_mut().filter(|f| f.kind == '?') {
        f.lines = count_lines(&repo.root.join(&f.path)).map(|n| (n, 0));
    }
    Ok(st)
}

/// A file's text at HEAD, for syntax highlighting the old side of a diff. None: unborn HEAD, not in HEAD, binary
/// (NUL in the first 8 KB) or over `max` bytes. Blocking: run off the UI thread.
pub fn head_text(repo: &Repo, path: &str, max: usize) -> Option<String> {
    let mut c = git(&repo.root);
    c.args(["cat-file", "blob"]).arg(format!("HEAD:{path}"));
    let out = output(c).ok()?;
    if out.len() > max || out[..out.len().min(8192)].contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// The empty tree's id (differs between SHA-1 and SHA-256 repos): the base to diff an unborn HEAD against.
fn empty_tree(repo: &Repo) -> std::io::Result<String> {
    let mut c = git(&repo.root);
    c.args(["hash-object", "-t", "tree", "/dev/null"]);
    Ok(String::from_utf8_lossy(&output(c)?).trim().to_string())
}

/// Parses `git status --porcelain=v2 -z --branch`; returns the status and whether HEAD is unborn.
fn parse_status(out: &[u8]) -> (Status, bool) {
    let mut st = Status::default();
    let (mut oid, mut head) = (String::new(), String::new());
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    let mut all = Vec::new();
    while let Some(entry) = fields.next() {
        let text = String::from_utf8_lossy(entry);
        if let Some(h) = text.strip_prefix("# branch.") {
            match h.split_once(' ') {
                Some(("oid", v)) => oid = v.to_string(),
                Some(("head", v)) => head = v.to_string(),
                Some(("upstream", v)) => st.upstream = Some(v.to_string()),
                Some(("ab", v)) => {
                    let mut it = v.split(' ').map(|n| n.trim_start_matches(['+', '-']).parse::<u32>().unwrap_or(0));
                    st.ab = Some((it.next().unwrap_or(0), it.next().unwrap_or(0)));
                }
                _ => {}
            }
            continue;
        }
        // `1 XY sub mH mI mW hH hI path`, `2 XY ... Xscore path` + NUL + origPath, `u XY ... path`, `? path`.
        let (kind, rest) = text.split_at(1.min(text.len()));
        let rest = rest.trim_start();
        let change = match kind {
            "?" => Some(FileChange {
                path: rest.to_string(),
                orig: None,
                kind: '?',
                staged: false,
                unstaged: true,
                lines: None,
            }),
            "1" | "2" | "u" => {
                let n = match kind {
                    "1" => 7,
                    "2" => 8,
                    _ => 9,
                };
                let mut parts = rest.splitn(n + 1, ' ');
                let xy: Vec<char> = parts.next().unwrap_or("..").chars().collect();
                let path = parts.nth(n - 1).unwrap_or("").to_string();
                let orig =
                    (kind == "2").then(|| fields.next().map(|o| String::from_utf8_lossy(o).into_owned())).flatten();
                let (x, y) = (xy.first().copied().unwrap_or('.'), xy.get(1).copied().unwrap_or('.'));
                let letter = if kind == "u" {
                    'U'
                } else if x != '.' {
                    x
                } else {
                    y
                };
                Some(FileChange {
                    path,
                    orig,
                    kind: letter,
                    staged: kind != "u" && x != '.',
                    unstaged: kind == "u" || y != '.',
                    lines: None,
                })
            }
            _ => None,
        };
        all.extend(change);
    }
    st.branch = match head.as_str() {
        "(detached)" => format!("(detached {})", &oid[..oid.len().min(7)]),
        _ => head,
    };
    st.more = all.len().saturating_sub(MAX_FILES);
    all.truncate(MAX_FILES);
    st.files = all;
    (st, oid == "(initial)")
}

/// `git diff --numstat -z`: `A\tD\tpath\0`, or `A\tD\t\0old\0new\0` for a rename; `-\t-\t` for binary.
fn apply_numstat(st: &mut Status, out: &[u8]) {
    let mut fields = out.split(|&b| b == 0);
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(entry);
        let mut parts = text.splitn(3, '\t');
        let (a, d, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let path = if path.is_empty() {
            let _old = fields.next();
            fields.next().map(|p| String::from_utf8_lossy(p).into_owned()).unwrap_or_default()
        } else {
            path.to_string()
        };
        let lines = a.parse().ok().zip(d.parse().ok());
        if let Some(f) = st.files.iter_mut().find(|f| f.path == path) {
            f.lines = lines;
        }
    }
}

/// Lines in an untracked file; None for binary (a NUL byte), unreadable or > 1 MB files.
fn count_lines(path: &Path) -> Option<u32> {
    let f = std::fs::File::open(path).ok()?;
    if !f.metadata().ok()?.is_file() || f.metadata().ok()?.len() > MAX_COUNT_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    f.take(MAX_COUNT_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.contains(&0) {
        return None;
    }
    let newlines = buf.iter().filter(|&&b| b == b'\n').count();
    Some((newlines + (!buf.is_empty() && !buf.ends_with(b"\n")) as usize) as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// The file header: `diff --git`, `index`, `---`/`+++`, mode and rename lines.
    Header,
    /// "Binary files … differ", "\ No newline at end of file".
    Meta,
    Hunk,
    Add,
    Del,
    Context,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diff {
    pub lines: Vec<Line>,
    /// Output was cut at `MAX_DIFF_LINES` / `MAX_DIFF_BYTES`.
    pub truncated: bool,
}

pub const MAX_DIFF_LINES: usize = 100_000;
const MAX_DIFF_BYTES: usize = 20 << 20;

/// The file's diff against HEAD (staged + unstaged), or against nothing for an untracked file. Blocking:
/// run off the UI thread. `cancelled` is polled while reading; when it returns true the `git` child is killed and
/// None returned.
pub fn diff(repo: &Repo, file: &FileChange, cancelled: impl Fn() -> bool) -> Option<Diff> {
    let mut c = git(&repo.root);
    c.args(["diff", "--no-color", "--no-ext-diff", "-M"]);
    if file.kind == '?' {
        c.args(["--no-index", "--", "/dev/null", &file.path]);
    } else {
        let mut head = git(&repo.root);
        head.args(["rev-parse", "--verify", "-q", "HEAD"]);
        let base = if output(head).is_ok() { "HEAD".to_string() } else { empty_tree(repo).ok()? };
        c.args([base.as_str(), "--"]);
        c.args(file.orig.iter());
        c.arg(&file.path);
    }
    c.stdout(Stdio::piped());
    let mut child = c.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let mut raw = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut truncated = false;
    loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
        if raw.len() > MAX_DIFF_BYTES {
            truncated = true;
            let _ = child.kill();
            break;
        }
    }
    let _ = child.wait();
    let mut d = parse_diff(&raw);
    d.truncated |= truncated;
    Some(d)
}

/// Parses `git diff` output (also used by tests that diff two commits).
pub fn parse_diff(raw: &[u8]) -> Diff {
    let text = String::from_utf8_lossy(raw);
    let mut d = Diff::default();
    let mut in_hunk = false;
    for line in text.lines() {
        if d.lines.len() >= MAX_DIFF_LINES {
            d.truncated = true;
            break;
        }
        let kind = if line.starts_with("@@") {
            in_hunk = true;
            LineKind::Hunk
        } else if line.starts_with("diff --git") {
            in_hunk = false;
            LineKind::Header
        } else if !in_hunk {
            if line.starts_with("Binary files ") { LineKind::Meta } else { LineKind::Header }
        } else {
            match line.as_bytes().first() {
                Some(b'+') => LineKind::Add,
                Some(b'-') => LineKind::Del,
                Some(b' ') | None => LineKind::Context,
                _ => LineKind::Meta, // "\ No newline at end of file"
            }
        };
        d.lines.push(Line { kind, text: text::display_line(line, 1) });
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_under_roots() {
        let p = Path::new;
        // (path, root, under on Windows, under on Unix)
        let cases = [
            (r"c:\Users\x\src", "C:/users/X", true, false),
            ("C:/x/y", r"C:\x", true, false),
            (r"C:\xy", r"C:\x", false, false),
            ("/home/a/b", "/home/a", true, true),
            ("/home/A/b", "/home/a", true, false),
            ("/home/ab", "/home/a", false, false),
            (r"\\wsl.localhost\Ubuntu\home\a\b", r"\\wsl$\ubuntu\home\a", true, false),
            (r"\\wsl.localhost\Ubuntu\home\A", r"\\wsl.localhost\Ubuntu\home\a", false, false),
            (r"\\wsl.localhost\Ubuntu\home\a", r"\\wsl.localhost\Debian\home\a", false, false),
            (r"\\wsl.localhost\Ubuntu\home", r"C:\", false, false),
        ];
        for (path, root, win, unix) in cases {
            let want = if cfg!(windows) { win } else { unix };
            assert_eq!(path_under(p(path), p(root)), want, "{path} under {root}");
        }
        assert!(same_path(p("/home/a/"), p("/home/a")));
        assert!(!same_path(p("/home/a/b"), p("/home/a")));
    }

    #[test]
    fn native_and_plain_paths() {
        if cfg!(windows) {
            assert_eq!(native_path("c:/Users/x"), Path::new(r"C:\Users\x"));
            assert_eq!(plain_path(r"\\?\d:\a\b".into()), Path::new(r"D:\a\b"));
            assert_eq!(plain_path(r"\\?\UNC\srv\share".into()), Path::new(r"\\?\UNC\srv\share"));
            assert_eq!(plain_path(r"c:\a".into()), Path::new(r"C:\a"));
        } else {
            assert_eq!(native_path("/a/b\\c"), Path::new("/a/b\\c"));
            assert_eq!(plain_path("/a/b".into()), Path::new("/a/b"));
            assert_eq!(root_key("/no/such/dir"), Path::new("/no/such/dir"), "Unix: as git printed it");
        }
    }

    #[test]
    fn wsl_roots() {
        let dir = Path::new(r"\\wsl$\Ubuntu\home\x\src");
        if cfg!(windows) {
            assert_eq!(root_key_in(dir, "/home/x"), Path::new(r"\\wsl.localhost\Ubuntu\home\x"));
            assert_eq!(plain_path(r"\\?\UNC\wsl$\Ubuntu\home\x".into()), Path::new(r"\\wsl.localhost\Ubuntu\home\x"));
            assert_eq!(plain_path(r"\\?\UNC\srv\share".into()), Path::new(r"\\?\UNC\srv\share"), "other UNC kept");
        } else {
            assert_eq!(root_key_in(dir, "/home/x"), Path::new("/home/x"), "Unix: as printed");
        }
    }

    #[test]
    fn root_key_keeps_the_drive() {
        let key = |printed: &str, canon: &str| same_drive_or(printed.into(), canon.into());
        assert_eq!(key(r"C:\users\x", r"\\?\C:\Users\X"), Path::new(r"C:\Users\X"), "same drive: on-disk form");
        assert_eq!(key(r"S:\repo", r"\\?\C:\work\repo"), Path::new(r"S:\repo"), "subst drive");
        assert_eq!(key(r"Z:\repo", r"\\?\UNC\srv\share\repo"), Path::new(r"Z:\repo"), "mapped network drive");
        assert_eq!(key(r"\\srv\share\repo", r"\\?\UNC\srv\share\repo"), Path::new(r"\\srv\share\repo"), "UNC");
    }

    /// Typed case, other separators and the 8.3 short name of one directory are one project key, in its on-disk case.
    #[test]
    #[cfg(windows)]
    fn root_key_is_one_per_directory() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let base = plain_path(std::env::temp_dir().canonicalize().unwrap());
        let dir = base.join(format!("Dirigent-Key-Test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = dir.to_str().unwrap();
        let key = root_key(s);
        assert_eq!(key, dir, "on-disk case");
        let mut forms = vec![s.to_uppercase(), s.to_lowercase().replace('\\', "/")];
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: NUL-terminated input, a buffer of the length we pass.
        let n = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                wide.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
            )
        } as usize;
        let short = std::ffi::OsString::from_wide(&buf[..n]).into_string().unwrap();
        if n == 0 || short.eq_ignore_ascii_case(s) {
            eprintln!("8.3 names are off on this volume: short form not tested");
        } else {
            forms.push(short);
        }
        for f in &forms {
            assert_eq!(root_key(f), key, "{f}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn porcelain_v2() {
        let out = b"# branch.oid 1234567890abcdef\0# branch.head main\0\
            1 .M N... 100644 100644 100644 aaa aaa src/a b.rs\0\
            1 MM N... 100644 100644 100644 aaa bbb both.rs\0\
            1 A. N... 000000 100644 100644 000 bbb new.rs\0\
            1 .D N... 100644 100644 000000 aaa aaa gone.rs\0\
            2 R. N... 100644 100644 100644 aaa aaa R100 to.rs\0from.rs\0\
            u UU N... 100644 100644 100644 100644 a b c conflict.rs\0\
            ? \xc5\xbelu\xc5\xa5ou\xc4\x8dk\xc3\xbd k\xc5\xaf\xc5\x88.txt\0";
        let (st, unborn) = parse_status(out);
        assert!(!unborn);
        assert_eq!(st.branch, "main");
        let got: Vec<_> =
            st.files.iter().map(|f| (f.path.as_str(), f.orig.as_deref(), f.kind, f.staged, f.unstaged)).collect();
        assert_eq!(
            got,
            vec![
                ("src/a b.rs", None, 'M', false, true),
                ("both.rs", None, 'M', true, true),
                ("new.rs", None, 'A', true, false),
                ("gone.rs", None, 'D', false, true),
                ("to.rs", Some("from.rs"), 'R', true, false),
                ("conflict.rs", None, 'U', false, true),
                ("žluťoučký kůň.txt", None, '?', false, true),
            ]
        );
        let (st, unborn) = parse_status(b"# branch.oid (initial)\0# branch.head main\0? x\0");
        assert!(unborn);
        assert_eq!(st.files.len(), 1);
        let (st, _) = parse_status(b"# branch.oid deadbeefcafe\0# branch.head (detached)\0");
        assert_eq!(st.branch, "(detached deadbee)");
        assert_eq!((st.upstream, st.ab), (None, None));
    }

    #[test]
    fn upstream_ahead_behind() {
        let (st, _) =
            parse_status(b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -13\0");
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!(st.ab, Some((2, 13)));
        // Upstream configured but its ref is gone (deleted on the remote and pruned): no `branch.ab` line.
        let (st, _) = parse_status(b"# branch.oid abc\0# branch.head f\0# branch.upstream origin/f\0");
        assert_eq!((st.upstream.as_deref(), st.ab), (Some("origin/f"), None));
    }

    #[test]
    fn numstat_with_rename_and_binary() {
        let (mut st, _) = parse_status(
            b"1 .M N... 1 1 1 a a a.rs\x002 R. N... 1 1 1 a a R90 to.rs\0from.rs\0\
              1 .M N... 1 1 1 a a img.png\0",
        );
        apply_numstat(&mut st, b"3\t1\ta.rs\x002\t0\t\0from.rs\0to.rs\0-\t-\timg.png\0");
        let lines: Vec<_> = st.files.iter().map(|f| f.lines).collect();
        assert_eq!(lines, vec![Some((3, 1)), Some((2, 0)), None]);
        assert_eq!(st.totals(), (3, 5, 1));
    }

    #[test]
    fn list_is_capped() {
        let mut out = Vec::new();
        for i in 0..MAX_FILES + 5 {
            out.extend_from_slice(format!("? f{i}\0").as_bytes());
        }
        let (st, _) = parse_status(&out);
        assert_eq!((st.files.len(), st.more, st.totals().0), (MAX_FILES, 5, MAX_FILES + 5));
    }

    #[test]
    fn untracked_line_counts() {
        let d = std::env::temp_dir().join(format!("dirigent-git-count-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let w = |name: &str, body: &[u8]| {
            std::fs::write(d.join(name), body).unwrap();
            count_lines(&d.join(name))
        };
        assert_eq!(w("a", b"one\ntwo\n"), Some(2));
        assert_eq!(w("b", b"one\ntwo"), Some(2));
        assert_eq!(w("c", b""), Some(0));
        assert_eq!(w("d", b"bin\0ary"), None);
        assert_eq!(w("e", &vec![b'x'; MAX_COUNT_BYTES as usize + 1]), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn diff_lines() {
        let raw = b"diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@ fn x\n ctx\n-old\n+new\n\
            \\ No newline at end of file\n+\tt\n";
        let d = parse_diff(raw);
        let kinds: Vec<_> = d.lines.iter().map(|l| l.kind).collect();
        use LineKind::*;
        assert_eq!(kinds, vec![Header, Header, Header, Header, Hunk, Context, Del, Add, Meta, Add]);
        let bin = parse_diff(b"diff --git a/i b/i\nindex 1..2\nBinary files a/i and b/i differ\n");
        assert_eq!(bin.lines.iter().map(|l| l.kind).collect::<Vec<_>>(), vec![Header, Header, Meta]);
        assert_eq!(d.lines[9].text, "+    t", "tab after the marker column goes to the next stop");
        assert!(!d.truncated);
        let long = format!("@@ -1 +1 @@\n+{}\n", "y".repeat(5000));
        assert_eq!(parse_diff(long.as_bytes()).lines[1].text.chars().count(), text::MAX_LINE_CHARS + 1);
    }
}
