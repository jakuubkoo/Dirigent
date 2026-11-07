//! Git writes: stage/unstage/discard files and hunks, commit. Blocking: run off the UI thread, one at a time per
//! repo. Every call is a single git command (atomic under git's own `index.lock`), so a failure — e.g. another
//! git process holding the lock — leaves the index as it was. We never touch `index.lock` ourselves.
//!
//! Hunks are git's: the diff view shows HEAD→worktree hunks; its buttons act on the overlapping hunks of git's
//! HEAD→index (`--cached`) and index→worktree diffs, recomputed when the action runs.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::{FileChange, Repo};

#[derive(Debug)]
pub enum GitError {
    /// Another git process holds `.git/index.lock`: retry later.
    IndexLocked,
    /// git failed: its combined output (hook output for a failed commit).
    Failed(String),
    Io(std::io::Error),
    /// The user stopped a network command.
    Cancelled,
    /// The branch and its upstream diverged: a fast-forward isn't possible.
    NotFastForward,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::IndexLocked => f.write_str("Another git process holds the index (index.lock)"),
            GitError::Failed(out) => f.write_str(out.trim()),
            GitError::Io(e) => write!(f, "{e}"),
            GitError::Cancelled => f.write_str("Cancelled"),
            GitError::NotFastForward => f.write_str(
                "Can't fast-forward: this branch and its upstream both have new commits. Merge or rebase in a terminal.",
            ),
        }
    }
}

type Result<T> = std::result::Result<T, GitError>;

/// git for writes: the user's config applies (author, signing, hooks); no `--no-optional-locks` (writes take
/// locks anyway); output captured for errors.
pub(crate) fn run(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
    let mut c = crate::native_or_wsl(root, "git");
    c.args(args)
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Never retried: a commit or apply may have happened before its server died.
    let out = crate::exec::output(&mut c, stdin, crate::exec::WRITE_TIMEOUT, false).map_err(GitError::Io)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("index.lock") && stderr.contains("File exists") {
        return Err(GitError::IndexLocked);
    }
    Err(GitError::Failed(format!("{stdout}{stderr}")))
}

/// A path as an argument to git run in `root`: its Linux form when git runs inside a WSL distro.
fn arg_path(root: &Path, path: &Path) -> String {
    match crate::wsl::of(root) {
        Some(_) => crate::wsl::to_linux(&path.to_string_lossy()).unwrap_or_else(|| path.to_string_lossy().into()),
        None => path.to_string_lossy().into(),
    }
}

fn has_head(root: &Path) -> bool {
    run(root, &["rev-parse", "--verify", "-q", "HEAD"], None).is_ok()
}

/// The paths a file change covers (both sides of a rename).
fn paths(f: &FileChange) -> Vec<&str> {
    std::iter::once(f.path.as_str()).chain(f.orig.as_deref()).collect()
}

fn with_paths<'a>(args: &[&'a str], paths: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().chain(std::iter::once("--")).chain(paths.iter().copied()).collect()
}

/// `git add -A` of these files (new, modified, deleted).
pub fn stage(repo: &Repo, files: &[FileChange]) -> Result<()> {
    let p: Vec<&str> = files.iter().flat_map(paths).collect();
    run(&repo.root, &with_paths(&["add", "-A"], &p), None).map(drop)
}

pub fn stage_all(repo: &Repo) -> Result<()> {
    run(&repo.root, &["add", "-A"], None).map(drop)
}

/// Back to "not staged" (the worktree keeps its content).
pub fn unstage(repo: &Repo, files: &[FileChange]) -> Result<()> {
    let p: Vec<&str> = files.iter().flat_map(paths).collect();
    if has_head(&repo.root) {
        run(&repo.root, &with_paths(&["restore", "--staged"], &p), None).map(drop)
    } else {
        run(&repo.root, &with_paths(&["rm", "--cached", "-r", "-q", "--ignore-unmatch"], &p), None).map(drop)
    }
}

/// Moves a file to the desktop trash (`gio trash`); never deletes. Fails if it can't be trashed.
#[cfg(unix)]
pub fn trash(path: &Path) -> Result<()> {
    let out = std::process::Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(GitError::Io)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(GitError::Failed(format!(
            "Couldn't move {} to the trash: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// The app's main window (owner of the Recycle Bin's "delete permanently?" question) and its UI thread.
#[cfg(windows)]
static UI_OWNER: std::sync::OnceLock<(isize, std::thread::ThreadId)> = std::sync::OnceLock::new();

/// Windows: call once on the UI thread with the main window's HWND. A Recycle Bin question is then owned by that
/// window (on top of it, never hidden behind it), and `trash` refuses to run on that thread.
#[cfg(windows)]
pub fn set_ui_owner(hwnd: isize) {
    let _ = UI_OWNER.set((hwnd, std::thread::current().id()));
}

/// Moves a file or directory to the Recycle Bin; never deletes. A drive without a Recycle Bin (network share) is
/// an error and the file stays. If Windows would still delete it outright (e.g. bigger than the bin), it asks
/// the user first. Blocking: never on the UI thread (an error there).
#[cfg(windows)]
pub fn trash(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW,
        SHFileOperationW, SHQUERYRBINFO, SHQueryRecycleBinW,
    };
    let owner = UI_OWNER.get();
    if owner.is_some_and(|(_, ui)| *ui == std::thread::current().id()) {
        return Err(GitError::Failed("trash must not run on the UI thread".into()));
    }
    let fail = |why: &str| GitError::Failed(format!("Couldn't move {} to the Recycle Bin: {why}", path.display()));
    if crate::wsl::of(path).is_some() {
        return Err(fail("files inside WSL have no Recycle Bin. Nothing was deleted"));
    }
    let abs = crate::plain_path(std::path::absolute(path).map_err(GitError::Io)?);
    let wide = |p: &Path| p.as_os_str().encode_wide().chain([0, 0]).collect::<Vec<u16>>();
    let root = abs.ancestors().last().unwrap_or(&abs).to_path_buf();
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, i64Size: 0, i64NumItems: 0 };
    // SAFETY: a NUL-terminated path and a sized out struct.
    if unsafe { SHQueryRecycleBinW(wide(&root).as_ptr(), &mut info) } < 0 {
        return Err(fail("this drive has no Recycle Bin. Nothing was deleted"));
    }
    let from = wide(&abs); // double-NUL terminated list of one
    // SAFETY: all-zero is a valid SHFILEOPSTRUCTW (null handles and pointers); the fields we need are set below.
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = owner.map_or(0, |(h, _)| *h) as _;
    op.wFunc = FO_DELETE;
    op.pFrom = from.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING) as u16;
    // SAFETY: `op` points at `from`, alive for the call.
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        return Err(fail(&format!("error {rc:#x}")));
    }
    if op.fAnyOperationsAborted != 0 {
        return Err(fail("cancelled"));
    }
    Ok(())
}

/// Back to HEAD, index and worktree. Files HEAD doesn't have (untracked, or added in the index) go to the trash.
pub fn discard(repo: &Repo, f: &FileChange) -> Result<()> {
    let root = &repo.root;
    let in_head = has_head(root)
        && run(root, &["cat-file", "-e", &format!("HEAD:{}", f.orig.as_deref().unwrap_or(&f.path))], None).is_ok();
    if f.kind == '?' {
        return trash(&root.join(&f.path));
    }
    if !in_head {
        // Added in the index only: drop it from the index, then trash the file.
        run(root, &with_paths(&["rm", "--cached", "-q", "--ignore-unmatch"], &[&f.path]), None)?;
        return trash(&root.join(&f.path));
    }
    if let Some(orig) = &f.orig {
        // A rename: restore the old path, trash the new one.
        run(root, &with_paths(&["restore", "--source=HEAD", "--staged", "--worktree"], &[orig]), None)?;
        run(root, &with_paths(&["rm", "--cached", "-q", "--ignore-unmatch"], &[&f.path]), None)?;
        let new = root.join(&f.path);
        return if new.exists() { trash(&new) } else { Ok(()) };
    }
    run(root, &with_paths(&["restore", "--source=HEAD", "--staged", "--worktree"], &[&f.path]), None).map(drop)
}

/// Changed-line positions of a hunk on each side (inclusive; a pure insertion/deletion is its insertion point):
/// what the view's HEAD→worktree hunks are matched against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub old: (u32, u32),
    pub new: (u32, u32),
}

/// Builds a hunk's `Span` line by line (shared by the patch parser and the diff layout, so both agree).
pub(crate) struct SpanBuilder {
    old: u32,
    new: u32,
    span: Option<Span>,
}

impl SpanBuilder {
    /// From a `@@ -a,b +c,d @@` header.
    pub(crate) fn new(header: &str) -> Self {
        let (old, new) = hunk_starts(header);
        Self { old, new, span: None }
    }

    fn grow(&mut self) {
        let (o, n) = (self.old, self.new);
        let s = self.span.get_or_insert(Span { old: (o, o), new: (n, n) });
        s.old = (s.old.0.min(o), s.old.1.max(o));
        s.new = (s.new.0.min(n), s.new.1.max(n));
    }

    pub(crate) fn del(&mut self) {
        self.grow();
        self.old += 1;
    }

    pub(crate) fn add(&mut self) {
        self.grow();
        self.new += 1;
    }

    pub(crate) fn context(&mut self) {
        self.old += 1;
        self.new += 1;
    }

    pub(crate) fn span(&self) -> Option<Span> {
        self.span
    }
}

impl Span {
    fn overlaps_old(&self, o: &Span) -> bool {
        self.old.0 <= o.old.1 && o.old.0 <= self.old.1
    }

    fn overlaps_new(&self, o: &Span) -> bool {
        self.new.0 <= o.new.1 && o.new.0 <= self.new.1
    }
}

/// One file's diff split into its header and hunks (each with its exact text and span).
#[derive(Debug, Default)]
struct Patch {
    header: String,
    hunks: Vec<(String, Span)>,
}

impl Patch {
    fn parse(text: &str) -> Patch {
        let mut p = Patch::default();
        let mut cur: Option<(String, SpanBuilder)> = None;
        let close = |p: &mut Patch, cur: Option<(String, SpanBuilder)>| {
            if let Some((t, b)) = cur
                && let Some(s) = b.span()
            {
                p.hunks.push((t, s));
            }
        };
        for line in text.split_inclusive('\n') {
            if line.starts_with("@@") {
                close(&mut p, cur.take());
                cur = Some((line.to_string(), SpanBuilder::new(line)));
                continue;
            }
            let Some((t, b)) = cur.as_mut() else {
                p.header.push_str(line);
                continue;
            };
            t.push_str(line);
            match line.as_bytes().first() {
                Some(b'-') => b.del(),
                Some(b'+') => b.add(),
                Some(b' ') => b.context(),
                _ => {} // "\ No newline at end of file"
            }
        }
        close(&mut p, cur);
        p
    }

    /// The header plus the hunks whose span `keep` accepts; None if no hunk was kept.
    fn select(&self, keep: impl Fn(&Span) -> bool) -> Option<String> {
        let mut out = self.header.clone();
        let mut any = false;
        for (t, s) in &self.hunks {
            if keep(s) {
                out.push_str(t);
                any = true;
            }
        }
        any.then_some(out)
    }
}

/// `@@ -a[,b] +c[,d] @@` → (a, c), with 0-line sides starting one later (git counts from the line before).
fn hunk_starts(line: &str) -> (u32, u32) {
    let mut parts = line.split_whitespace().skip(1);
    let side = |p: Option<&str>| -> u32 {
        let p = p.unwrap_or("").trim_start_matches(['-', '+']);
        let (start, len) = p.split_once(',').map_or((p, "1"), |(s, l)| (s, l));
        let (start, len) = (start.parse().unwrap_or(0), len.parse::<u32>().unwrap_or(1));
        if len == 0 { start + 1 } else { start }
    };
    (side(parts.next()), side(parts.next()))
}

fn diff(repo: &Repo, f: &FileChange, cached: bool) -> Result<Patch> {
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-U3"];
    if cached {
        args.push("--cached");
    }
    let p = paths(f);
    Ok(Patch::parse(&run(&repo.root, &with_paths(&args, &p), None)?))
}

/// What a view hunk (by its HEAD→worktree span) has: (staged parts, unstaged parts).
pub fn hunk_state(repo: &Repo, f: &FileChange, spans: &[Span]) -> Vec<(bool, bool)> {
    if f.kind == '?' {
        return spans.iter().map(|_| (false, true)).collect();
    }
    let staged = diff(repo, f, true).unwrap_or_default();
    let unstaged = diff(repo, f, false).unwrap_or_default();
    spans
        .iter()
        .map(|s| {
            (staged.hunks.iter().any(|(_, h)| h.overlaps_old(s)), unstaged.hunks.iter().any(|(_, h)| h.overlaps_new(s)))
        })
        .collect()
}

fn apply(repo: &Repo, patch: &str, cached: bool, reverse: bool) -> Result<()> {
    let mut args = vec!["apply", "--recount", "--whitespace=nowarn"];
    if cached {
        args.push("--cached");
    }
    if reverse {
        args.push("-R");
    }
    args.push("-");
    run(&repo.root, &args, Some(patch.as_bytes())).map(drop)
}

/// Stages the unstaged changes inside the view hunk `span` (untracked file: the whole file).
pub fn stage_hunk(repo: &Repo, f: &FileChange, span: Span) -> Result<()> {
    if f.kind == '?' {
        return stage(repo, std::slice::from_ref(f));
    }
    let p = diff(repo, f, false)?;
    match p.select(|h| h.overlaps_new(&span)) {
        Some(patch) => apply(repo, &patch, true, false),
        None => Ok(()),
    }
}

/// Unstages the staged changes inside the view hunk `span`.
pub fn unstage_hunk(repo: &Repo, f: &FileChange, span: Span) -> Result<()> {
    let p = diff(repo, f, true)?;
    match p.select(|h| h.overlaps_old(&span)) {
        Some(patch) => apply(repo, &patch, true, true),
        None => Ok(()),
    }
}

/// Puts the view hunk `span` back to HEAD, index and worktree (index first: if that fails, nothing changed).
/// An untracked file goes to the trash as a whole.
pub fn discard_hunk(repo: &Repo, f: &FileChange, span: Span) -> Result<()> {
    if f.kind == '?' {
        return discard(repo, f);
    }
    let staged = diff(repo, f, true)?;
    if let Some(patch) = staged.select(|h| h.overlaps_old(&span)) {
        apply(repo, &patch, true, true)?;
    }
    // The worktree against HEAD, for exactly this region (the index now matches HEAD there).
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-U3"];
    if has_head(&repo.root) {
        args.push("HEAD");
    }
    let whole = Patch::parse(&run(&repo.root, &with_paths(&args, &paths(f)), None)?);
    match whole.select(|h| h.overlaps_new(&span)) {
        Some(patch) => apply(repo, &patch, false, true),
        None => Ok(()),
    }
}

/// `git commit` with the user's config (author, signing, hooks). Err(Failed) carries hook output.
pub fn commit(repo: &Repo, message: &str, amend: bool) -> Result<()> {
    let mut args = vec!["commit", "-F", "-"];
    if amend {
        args.push("--amend");
    }
    run(&repo.root, &args, Some(message.as_bytes())).map(drop)
}

/// Commits exactly these files' worktree content (`--only`); everything else staged stays staged and out of the
/// commit. Untracked ones are added first (`--only` needs tracked paths).
pub fn commit_only(repo: &Repo, message: &str, files: &[FileChange], amend: bool) -> Result<()> {
    let untracked: Vec<&str> = files.iter().filter(|f| f.kind == '?').map(|f| f.path.as_str()).collect();
    if !untracked.is_empty() {
        run(&repo.root, &with_paths(&["add"], &untracked), None)?;
    }
    let p: Vec<&str> = files.iter().flat_map(paths).collect();
    let mut args = vec!["commit", "-F", "-", "--only"];
    if amend {
        args.push("--amend");
    }
    run(&repo.root, &with_paths(&args, &p), Some(message.as_bytes())).map(drop)
}

/// The last commit's message (for amend); None without commits.
pub fn last_message(repo: &Repo) -> Option<String> {
    run(&repo.root, &["log", "-1", "--format=%B"], None).ok().map(|m| m.trim_end().to_string())
}

/// The staged diff (for generating a message), cut at `max` bytes; (text, cut).
pub fn staged_diff(repo: &Repo, max: usize) -> Result<(String, bool)> {
    let d = run(&repo.root, &["diff", "--cached", "--no-color", "--no-ext-diff", "--stat", "-p"], None)?;
    Ok(cut(d, max))
}

/// What `commit_only` would commit for these files (HEAD→worktree; untracked files and files HEAD doesn't have
/// as their whole content), cut at `max` bytes; (text, cut).
pub fn files_diff(repo: &Repo, files: &[FileChange], max: usize) -> Result<(String, bool)> {
    let head = has_head(&repo.root);
    let (tracked, new): (Vec<&FileChange>, Vec<&FileChange>) = files.iter().partition(|f| head && f.kind != '?');
    let mut d = String::new();
    if !tracked.is_empty() {
        let p: Vec<&str> = tracked.iter().flat_map(|f| paths(f)).collect();
        let args = with_paths(&["diff", "HEAD", "--no-color", "--no-ext-diff", "--stat", "-p"], &p);
        d = run(&repo.root, &args, None)?;
    }
    for f in new {
        let bytes = std::fs::read(repo.root.join(&f.path)).unwrap_or_default();
        if bytes[..bytes.len().min(8192)].contains(&0) {
            d += &format!("new binary file {}\n", f.path);
        } else {
            d += &format!("new file {}\n", f.path);
            for l in String::from_utf8_lossy(&bytes).lines() {
                d += &format!("+{l}\n");
            }
        }
        if d.len() > max {
            break;
        }
    }
    Ok(cut(d, max))
}

fn cut(mut d: String, max: usize) -> (String, bool) {
    let cut = d.len() > max;
    if cut {
        let mut end = max;
        while !d.is_char_boundary(end) {
            end -= 1;
        }
        d.truncate(end);
    }
    (d, cut)
}

/// Local branches, most recently committed first, and the current one ("" when detached). For the setup dialog.
pub fn branches(root: &Path) -> Result<(String, Vec<String>)> {
    let current = run(root, &["branch", "--show-current"], None).unwrap_or_default().trim().to_string();
    let list = run(root, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)", "refs/heads"], None)?;
    Ok((current, list.lines().map(str::to_string).collect()))
}

/// A branch for the switcher.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Branch {
    /// Short name: `feat/x` (local) or `origin/feat/x` (remote).
    pub name: String,
    pub remote: bool,
    pub current: bool,
    /// Local branches: the upstream (`origin/feat/x`).
    pub upstream: Option<String>,
    /// (ahead, behind) vs the upstream; None without one or when it's gone.
    pub ab: Option<(u32, u32)>,
}

/// Local branches (most recently committed first) with upstream and ahead/behind, then remote branches that have
/// no local branch of the same name. One `for-each-ref`.
pub fn branches_full(root: &Path) -> Result<Vec<Branch>> {
    let fmt = "--format=%(HEAD)%00%(refname)%00%(refname:short)%00%(upstream:short)%00%(upstream:track,nobracket)%00%(symref)";
    let out = run(root, &["for-each-ref", "--sort=-committerdate", fmt, "refs/heads", "refs/remotes"], None)?;
    let (mut local, mut remote) = (Vec::new(), Vec::new());
    for line in out.lines() {
        let f: Vec<&str> = line.split('\0').collect();
        let [head, full, short, up, track, symref] = f[..] else { continue };
        if !symref.is_empty() {
            continue; // origin/HEAD
        }
        let ab = (!up.is_empty() && track != "gone").then(|| {
            let n = |k: &str| {
                track.split(", ").find_map(|p| p.strip_prefix(k)).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
            };
            (n("ahead "), n("behind "))
        });
        let b = Branch {
            name: short.to_string(),
            remote: full.starts_with("refs/remotes/"),
            current: head == "*",
            upstream: (!up.is_empty()).then(|| up.to_string()),
            ab,
        };
        if b.remote { remote.push(b) } else { local.push(b) }
    }
    remote.retain(|r: &Branch| {
        let local_name = r.name.split_once('/').map_or(r.name.as_str(), |(_, n)| n);
        !local.iter().any(|l: &Branch| l.name == local_name || l.upstream.as_deref() == Some(&r.name))
    });
    local.extend(remote);
    Ok(local)
}

/// `git switch <branch>`. Local changes that would be overwritten → git refuses, nothing changes.
pub fn switch(root: &Path, branch: &str) -> Result<()> {
    run(root, &["switch", branch], None).map(drop)
}

/// `git switch --track <remote>/<b>`: a local branch tracking the remote one, checked out.
pub fn switch_remote(root: &Path, remote_branch: &str) -> Result<()> {
    run(root, &["switch", "--track", remote_branch], None).map(drop)
}

/// `git switch -c <name>`: a new branch from HEAD (uncommitted changes come along).
pub fn create_branch(root: &Path, name: &str) -> Result<()> {
    run(root, &["switch", "-c", name], None).map(drop)
}

/// Whether `name` is a valid new branch name (`git check-ref-format --branch`).
pub fn valid_branch(root: &Path, name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && run(root, &["check-ref-format", "--branch", name], None).is_ok()
}

/// `git worktree add -b <branch> <path> <base>`: a new branch from `base` checked out in a new directory. On failure
/// (branch or path exists, bad base) git leaves nothing behind.
pub fn worktree_add(root: &Path, branch: &str, path: &Path, base: &str) -> Result<()> {
    let path = arg_path(root, path);
    run(root, &["worktree", "add", "-b", branch, &path, base], None).map(drop)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Short branch name; None when detached (or bare).
    pub branch: Option<String>,
    pub head: String,
    /// The main checkout (first in the list): never removed from here.
    pub main: bool,
    pub locked: bool,
    /// Its directory is gone: `git worktree prune` drops the entry.
    pub prunable: bool,
}

/// Every worktree of the repo (also ones made outside the app), main checkout first.
pub fn worktree_list(root: &Path) -> Result<Vec<Worktree>> {
    let out = run(root, &["worktree", "list", "--porcelain"], None)?;
    Ok(parse_worktrees(root, &out))
}

fn parse_worktrees(root: &Path, out: &str) -> Vec<Worktree> {
    let mut list: Vec<Worktree> = Vec::new();
    for block in out.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let mut w = Worktree { main: list.is_empty(), ..Default::default() };
        for line in block.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "worktree" => w.path = crate::root_key_in(root, value),
                "HEAD" => w.head = value.to_string(),
                "branch" => w.branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value).to_string()),
                "locked" => w.locked = true,
                "prunable" => w.prunable = true,
                _ => {}
            }
        }
        list.push(w);
    }
    list
}

/// What removing a worktree would lose.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorktreeState {
    /// Uncommitted changes (incl. untracked files).
    pub dirty_files: usize,
    /// Commits on HEAD that no remote branch and no other local branch has.
    pub unpushed: usize,
}

pub fn worktree_state(path: &Path, branch: Option<&str>) -> Result<WorktreeState> {
    let dirty_files = run(path, &["status", "--porcelain"], None)?.lines().count();
    // With `--branches` the pattern is the short name (git rev-list docs).
    let exclude = branch.map(|b| format!("--exclude={b}"));
    let mut args = vec!["rev-list", "--count", "HEAD", "--not", "--remotes"];
    if let Some(e) = &exclude {
        args.push(e);
    }
    args.push("--branches");
    let unpushed = run(path, &args, None)?.trim().parse().unwrap_or(0);
    Ok(WorktreeState { dirty_files, unpushed })
}

/// `git worktree remove [--force] <path>`, run from the main checkout. Never deletes files any other way; without
/// `force` git refuses a worktree with changes.
pub fn worktree_remove(main_root: &Path, path: &Path, force: bool) -> Result<()> {
    let path = arg_path(main_root, path);
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path);
    run(main_root, &args, None).map(drop)
}

/// `git branch -d` (refuses unmerged work) or, after an explicit confirm, `-D`.
pub fn branch_delete(main_root: &Path, branch: &str, force: bool) -> Result<()> {
    run(main_root, &["branch", if force { "-D" } else { "-d" }, "--", branch], None).map(drop)
}

/// Drops entries of worktrees whose directory is gone.
pub fn worktree_prune(main_root: &Path) -> Result<()> {
    run(main_root, &["worktree", "prune"], None).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_spans() {
        let text = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -17,7 +17,7 @@\n 17\n 18\n 19\n-20\n+twenty\n 21\n 22\n 23\n\
                    @@ -30,0 +31,2 @@\n+a\n+b\n@@ -40,2 +41,0 @@\n-x\n-y\n";
        let p = Patch::parse(text);
        assert_eq!(p.header, "diff --git a/f b/f\n--- a/f\n+++ b/f\n");
        let spans: Vec<Span> = p.hunks.iter().map(|h| h.1).collect();
        assert_eq!(spans[0], Span { old: (20, 21), new: (20, 20) }, "an added line sits after the removed one");
        assert_eq!(spans[1], Span { old: (31, 31), new: (31, 32) }, "insertion after old line 30");
        assert_eq!(spans[2], Span { old: (40, 41), new: (42, 42) }, "deletion: before new line 42");
        assert_eq!(p.select(|s| s.old.0 == 31).unwrap().matches("@@").count(), 2, "header + one hunk");
    }
}
