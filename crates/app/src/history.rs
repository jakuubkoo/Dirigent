//! A project's past Claude Code sessions, read from Claude's own transcripts (`~/.claude/projects/<dir>/<id>.jsonl`).
//! No gpui, blocking IO: call off the UI thread. Transcripts are only ever opened for reading.
//!
//! Format (claude 2.1.319, verified on real files): `custom-title` (`/rename`, `-n`) beats `ai-title` (Claude's own),
//! each re-appended many times, last value wins; the last title line is often an ai-title even under a custom
//! name. `cwd`/`gitBranch` sit on conversation lines only; the first cwd is where the session started. Assistant
//! messages are split into one line per content block (same `message.id`, consecutive).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use memchr::memmem;
use serde_json::Value;

/// One past session, as listed.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    /// Where it was started (the first `cwd` in the transcript).
    pub cwd: PathBuf,
    pub cwd_exists: bool,
    /// The last branch seen (None: not a repo, or detached).
    pub branch: Option<String>,
    /// The custom name, else Claude's title.
    pub title: Option<String>,
    /// The first prompt, one line, ≤ `PROMPT_CHARS`.
    pub prompt: Option<String>,
    /// Your prompts + Claude's replies.
    pub messages: u32,
    /// The transcript's mtime (unix seconds): the last activity.
    pub modified: i64,
}

/// What one transcript says (cached per file version).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scan {
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub custom: Option<String>,
    pub ai: Option<String>,
    pub prompt: Option<String>,
    /// The first bare slash command (`/doctor`): the prompt shown when there is no other.
    pub command: Option<String>,
    pub prompts: u32,
    pub replies: u32,
}

const PROMPT_CHARS: usize = 200;

/// Reads a whole transcript. Only a few lines per file are JSON-parsed: a substring prefilter picks title lines
/// and user lines that aren't tool results; replies are counted by their `"id":"msg_…"` alone.
pub fn scan(path: &Path) -> std::io::Result<Scan> {
    let f = std::fs::File::open(path)?;
    let mut r = BufReader::with_capacity(256 * 1024, f);
    let user = memmem::Finder::new(br#""type":"user""#);
    let assistant = memmem::Finder::new(br#""type":"assistant""#);
    let tool = memmem::Finder::new(br#""tool_result""#);
    let tool_use = memmem::Finder::new(br#""toolUseResult":"#);
    let sidechain = memmem::Finder::new(br#""isSidechain":true"#);
    let msg_id = memmem::Finder::new(br#""id":"msg_"#);
    let mut s = Scan::default();
    let mut last_id: Vec<u8> = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        // Bookkeeping lines are short and start with their type.
        if line.starts_with(br#"{"type":"custom-title""#) || line.starts_with(br#"{"type":"ai-title""#) {
            if let Ok(v) = serde_json::from_slice::<Value>(&line) {
                let one = |k| v.get(k).and_then(Value::as_str).map(one_line).filter(|t| !t.is_empty());
                match v.get("type").and_then(Value::as_str) {
                    Some("custom-title") => s.custom = one("customTitle").or(s.custom.take()),
                    _ => s.ai = one("aiTitle").or(s.ai.take()),
                }
            }
            continue;
        }
        let is_user = user.find(&line).is_some();
        let is_assistant = !is_user && assistant.find(&line).is_some();
        if s.cwd.is_none() {
            s.cwd = str_field(&line, "cwd");
        }
        if !(is_user || is_assistant) || sidechain.find(&line).is_some() {
            continue;
        }
        if let Some(b) = str_field(&line, "gitBranch") {
            s.branch = Some(b).filter(|b| !b.is_empty() && b != "HEAD");
        }
        if is_assistant {
            match msg_id.find(&line) {
                Some(at) => {
                    let id = &line[at + 6..];
                    let id = &id[..id.iter().position(|&c| c == b'"').unwrap_or(id.len())];
                    if id != last_id.as_slice() {
                        s.replies += 1;
                        last_id = id.to_vec();
                    }
                }
                // Claude's own lines (API errors, `<synthetic>`): one reply each.
                None => {
                    s.replies += 1;
                    last_id.clear();
                }
            }
            continue;
        }
        if tool.find(&line).is_some() || tool_use.find(&line).is_some() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        let Some(p) = prompt_of(&v) else { continue };
        s.prompts += 1;
        match p {
            Prompt::Text(p) if s.prompt.is_none() && !p.is_empty() => {
                s.prompt = Some(p.chars().take(PROMPT_CHARS).collect())
            }
            Prompt::Command(c) if s.command.is_none() => s.command = Some(c),
            _ => {}
        }
    }
    Ok(s)
}

enum Prompt {
    Text(String),
    /// A slash command without arguments (`/clear`, `/doctor`): typed, but says little about the session.
    Command(String),
}

/// A user line's prompt as you typed it (whitespace collapsed), None for everything else Claude stores as a user
/// line: tool results, meta/caveat lines, compact summaries, notifications, interrupt markers.
fn prompt_of(v: &Value) -> Option<Prompt> {
    let flag = |k| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    if v.get("type").and_then(Value::as_str) != Some("user")
        || flag("isSidechain")
        || flag("isMeta")
        || flag("isCompactSummary")
        || v.get("toolUseResult").is_some()
    {
        return None;
    }
    if let Some(kind) = v.pointer("/origin/kind").and_then(Value::as_str)
        && kind != "human"
    {
        return None;
    }
    let text = match v.pointer("/message/content")? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
                return None;
            }
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            texts.join("\n")
        }
        _ => return None,
    };
    const SKIP: [&str; 4] = ["<local-command-", "<task-notification", "<system-reminder", "[Request interrupted"];
    if SKIP.iter().any(|p| text.starts_with(p)) {
        return None;
    }
    if text.starts_with("<command-") {
        let name = one_line(tag(&text, "command-name")?);
        let args = tag(&text, "command-args").map(one_line).unwrap_or_default();
        return Some(if args.is_empty() { Prompt::Command(name) } else { Prompt::Text(format!("{name} {args}")) });
    }
    // An image-only prompt counts, with no text to show.
    Some(Prompt::Text(one_line(&text)))
}

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))?;
    Some(&text[start..start + end])
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `"key":"<value>"` in a compact JSON line, unescaped.
fn str_field(line: &[u8], key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = memmem::find(line, needle.as_bytes())? + needle.len() - 1;
    let mut i = start + 1;
    while i < line.len() {
        match line[i] {
            b'\\' => i += 2,
            b'"' => return serde_json::from_slice(&line[start..=i]).ok(),
            _ => i += 1,
        }
    }
    None
}

/// `~/.claude/projects` naming: every non-alphanumeric char → `-` (lossy: `/a/b-c` and `/a/b/c` collide, so the
/// cwd inside the file decides).
pub fn encode(path: &Path) -> String {
    path.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Transcripts read so far, by path, valid while (mtime, len) stays. Lives as long as the app.
// ponytail: any change rescans the whole file; a running session's transcript (100+ MB) could be scanned from the
// last length instead (transcripts are append-only), if reopening the list with one turns out slow.
#[derive(Default)]
pub struct Cache(Mutex<HashMap<PathBuf, Version>>);

/// (mtime, length, what it said).
type Version = (SystemTime, u64, Arc<Scan>);

impl Cache {
    fn get(&self, path: &Path, meta: &std::fs::Metadata) -> Option<Arc<Scan>> {
        let mtime = meta.modified().ok()?;
        if let Some((t, len, s)) = self.0.lock().unwrap().get(path)
            && (*t, *len) == (mtime, meta.len())
        {
            return Some(s.clone());
        }
        let s = Arc::new(scan(path).ok()?);
        self.0.lock().unwrap().insert(path.to_path_buf(), (mtime, meta.len(), s.clone()));
        Some(s)
    }
}

/// The sessions started in `root` (or below it) or in one of its worktrees (under `worktrees`), newest first.
/// `projects` = `~/.claude/projects`. Sessions with neither a title nor a prompt are left out.
pub fn list(projects: &Path, root: &Path, worktrees: &Path, cache: &Cache) -> Vec<Entry> {
    let prefixes = [encode(root), encode(worktrees)];
    let Ok(dirs) = std::fs::read_dir(projects) else { return Vec::new() };
    let mut files = Vec::new();
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if !prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(d.path()) else { continue };
        for f in inner.flatten() {
            let p = f.path();
            let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            if p.extension().is_some_and(|e| e == "jsonl") && is_session_id(id) {
                files.push(p);
            }
        }
    }
    let next = AtomicUsize::new(0);
    let out = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1)).clamp(1, 8);
    std::thread::scope(|s| {
        for _ in 0..threads.min(files.len()) {
            s.spawn(|| {
                while let Some(p) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(e) = entry(p, root, worktrees, cache) {
                        out.lock().unwrap().push(e);
                    }
                }
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    v
}

fn entry(path: &Path, root: &Path, worktrees: &Path, cache: &Cache) -> Option<Entry> {
    let meta = std::fs::metadata(path).ok()?;
    let s = cache.get(path, &meta)?;
    let cwd = PathBuf::from(s.cwd.as_deref()?);
    let prompt = s.prompt.clone().or_else(|| s.command.clone());
    if !(git::path_under(&cwd, root) || git::path_under(&cwd, worktrees))
        || (prompt.is_none() && s.custom.is_none() && s.ai.is_none())
    {
        return None;
    }
    let modified = meta.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    Some(Entry {
        id: path.file_stem()?.to_str()?.to_string(),
        cwd_exists: cwd.is_dir(),
        cwd,
        branch: s.branch.clone(),
        title: s.custom.clone().or_else(|| s.ai.clone()),
        prompt,
        messages: s.prompts + s.replies,
        modified,
    })
}

/// A transcript file name: a UUID (36 chars, hex + dashes). Same check `persist::transcript_path` does.
fn is_session_id(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Claude processes this app didn't start (Linux: read from `/proc`; elsewhere nothing is known).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Running {
    /// Session ids named on a command line (`--resume <id>`, `-r <id>`, `--session-id <id>`): exact.
    pub sessions: Vec<String>,
    /// Every such process's working directory (a fresh `claude` names no session).
    pub cwds: Vec<PathBuf>,
}

impl Running {
    /// Any of them works in `root` or under `worktrees`.
    pub fn in_project(&self, root: &Path, worktrees: &Path) -> bool {
        self.cwds.iter().any(|c| git::path_under(c, root) || git::path_under(c, worktrees))
    }
}

/// The claude processes running now, minus this app's own (their `--settings` file lies in `own_runtime`).
/// Verified on 2.1.319: an idle claude keeps no fd on its transcript (appends open and close it), so only the
/// command line and the cwd tell. Blocking IO: call off the UI thread.
#[cfg(target_os = "linux")]
pub fn running(own_runtime: &Path) -> Running {
    running_in(Path::new("/proc"), own_runtime)
}

#[cfg(not(target_os = "linux"))]
pub fn running(_own_runtime: &Path) -> Running {
    Running::default()
}

/// `running` over a `/proc`-shaped directory (tests use a fake one).
#[cfg(target_os = "linux")]
fn running_in(proc: &Path, own_runtime: &Path) -> Running {
    let mut r = Running::default();
    let Ok(dir) = std::fs::read_dir(proc) else { return r };
    for d in dir.flatten() {
        if !d.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(raw) = std::fs::read(d.path().join("cmdline")) else { continue };
        let args = split_cmdline(&raw);
        let exe = std::fs::read_link(d.path().join("exe")).unwrap_or_default();
        if !is_claude(&args, &exe) || is_own(&args, own_runtime) {
            continue;
        }
        r.sessions.extend(named_session(&args));
        if let Ok(cwd) = std::fs::read_link(d.path().join("cwd")) {
            r.cwds.push(cwd);
        }
    }
    r
}

/// `/proc/<pid>/cmdline`: NUL-separated, NUL-terminated.
#[cfg(target_os = "linux")]
fn split_cmdline(raw: &[u8]) -> Vec<String> {
    let raw = raw.strip_suffix(b"\0").unwrap_or(raw);
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
}

/// The CLI itself: `claude` as typed or through a path (native installs exec `~/.local/share/claude/versions/<v>`),
/// or node running the npm package. Not Claude Desktop (`claude-desktop`).
#[cfg(target_os = "linux")]
fn is_claude(args: &[String], exe: &Path) -> bool {
    let name = |s: &str| Path::new(s).file_name().is_some_and(|n| n == "claude");
    let node = args.first().is_some_and(|a| Path::new(a).file_name().is_some_and(|n| n == "node"))
        && args.get(1).is_some_and(|a| a.contains("claude-code"));
    args.first().is_some_and(|a| name(a)) || node || exe.to_string_lossy().contains("/claude/versions/")
}

/// Started by this app's shim: `--settings <own runtime dir>/tab-<id>.json`.
#[cfg(target_os = "linux")]
fn is_own(args: &[String], own_runtime: &Path) -> bool {
    args.windows(2).any(|w| w[0] == "--settings" && Path::new(&w[1]).starts_with(own_runtime))
}

/// The session a command line names (before a `--`: what follows is the prompt).
#[cfg(target_os = "linux")]
fn named_session(args: &[String]) -> Option<String> {
    let args = &args[..args.iter().position(|a| a == "--").unwrap_or(args.len())];
    args.iter().enumerate().skip(1).find_map(|(i, a)| {
        let value = match a.split_once('=') {
            Some((k, v)) if k == "--resume" || k == "--session-id" => Some(v),
            _ if a == "--resume" || a == "-r" || a == "--session-id" => args.get(i + 1).map(String::as_str),
            _ => None,
        };
        value.filter(|v| is_session_id(v)).map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(format!("{}/../hook/tests/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR")))
    }

    #[test]
    fn edge_cases() {
        // custom name over the later ai-title; the first real prompt after commands, meta, sidechain,
        // notification, compact summary and interrupt lines (`/clear` counts but isn't shown); one reply per
        // message id (3), sidechain replies skipped; first cwd, last branch; a broken line skipped.
        let s = scan(&fixture("history_edge")).unwrap();
        assert_eq!(s.custom.as_deref(), Some("my name"));
        assert_eq!(s.ai.as_deref(), Some("Fix login bug in auth"));
        assert_eq!(s.prompt.as_deref(), Some("Fix the login bug please"));
        assert_eq!(s.command.as_deref(), Some("/clear"));
        assert_eq!((s.prompts, s.replies), (3, 3));
        assert_eq!(s.cwd.as_deref(), Some("/r/app"));
        assert_eq!(s.branch.as_deref(), Some("fix/login"));
    }

    #[test]
    fn real_transcripts() {
        // Redacted from claude 2.1.319 sessions; expected values from an independent reading (scratch ref.py).
        let s = scan(&fixture("history_real")).unwrap();
        assert_eq!((s.custom.as_deref(), s.ai.as_deref()), (None, Some("ai title 2")));
        assert_eq!((s.prompt.as_deref(), s.prompts, s.replies), (Some("prompt 1"), 1, 16));
        assert_eq!(s.branch.as_deref(), Some("feat/x"));
        // `claude -p -n "probe named"`, then resumed from another directory: the name stays, the branch is
        // "HEAD" outside a repo (shown as none).
        let s = scan(&fixture("history_named")).unwrap();
        assert_eq!(s.custom.as_deref(), Some("custom title"));
        assert_eq!((s.prompt.as_deref(), s.prompts, s.replies), (Some("prompt 1"), 2, 2));
        assert_eq!((s.cwd.as_deref(), s.branch), (Some("/r/app"), None));
    }

    #[test]
    fn json_strings_are_unescaped() {
        assert_eq!(str_field(br#"{"a":1,"cwd":"/x/\"q\"/\u00e9","b":2}"#, "cwd").as_deref(), Some("/x/\"q\"/é"));
        assert_eq!(str_field(br#"{"cwd":"unterminated"#, "cwd"), None);
    }

    #[test]
    fn lists_only_this_projects_sessions() {
        let tmp = std::env::temp_dir().join(format!("dirigent-history-{}", std::process::id()));
        let projects = tmp.join("projects");
        let line = |cwd: &str, text: &str| {
            format!(
                r#"{{"type":"user","isSidechain":false,"message":{{"role":"user","content":"{text}"}},"cwd":"{cwd}","gitBranch":"main"}}"#
            ) + "\n"
        };
        let put = |dir: &str, id: &str, body: String| {
            let d = projects.join(encode(Path::new(dir)));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("{id}.jsonl")), body).unwrap();
        };
        let id = |n: u8| format!("{n:08}-0000-4000-8000-000000000000");
        put("/r/app", &id(1), line("/r/app", "in the root"));
        put("/r/app/sub", &id(2), line("/r/app/sub", "in a subdir"));
        put("/r/app-worktrees/feat", &id(3), line("/r/app-worktrees/feat", "in a worktree"));
        // Same encoded dir name, different project: `/r/app-x` vs `/r/app/x`.
        put("/r/app-x", &id(4), line("/r/app-x", "look-alike"));
        put("/r/other", &id(5), line("/r/other", "elsewhere"));
        put("/r/app", &id(6), "{\"type\":\"mode\",\"mode\":\"normal\"}\n".into()); // no prompt, no title
        put("/r/app", "not-a-session", line("/r/app", "skipped"));
        let sub = projects.join(encode(Path::new("/r/app"))).join(id(1)).join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("{}.jsonl", id(7))), line("/r/app", "subagent")).unwrap();
        let before =
            std::fs::read(projects.join(encode(Path::new("/r/app"))).join(format!("{}.jsonl", id(1)))).unwrap();

        let cache = Cache::default();
        let got = list(&projects, Path::new("/r/app"), Path::new("/r/app-worktrees"), &cache);
        let mut prompts: Vec<_> = got.iter().filter_map(|e| e.prompt.clone()).collect();
        prompts.sort();
        assert_eq!(prompts, ["in a subdir", "in a worktree", "in the root"]);
        assert!(got.iter().all(|e| e.messages == 1 && e.branch.as_deref() == Some("main") && !e.cwd_exists));

        // Cached: same answer without rereading; a changed file is read again.
        assert_eq!(list(&projects, Path::new("/r/app"), Path::new("/r/app-worktrees"), &cache), got);
        let p = projects.join(encode(Path::new("/r/app"))).join(format!("{}.jsonl", id(1)));
        assert_eq!(std::fs::read(&p).unwrap(), before, "never written");
        std::fs::write(&p, line("/r/app", "in the root") + &line("/r/app", "second")).unwrap();
        let again = list(&projects, Path::new("/r/app"), Path::new("/r/app-worktrees"), &cache);
        assert_eq!(again.iter().find(|e| e.id == id(1)).map(|e| e.messages), Some(2));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Command lines as seen on this machine (2.1.319; the prompt redacted).
    #[cfg(target_os = "linux")]
    #[test]
    fn claude_processes_are_recognized() {
        let args = |s: &str| split_cmdline(s.replace('|', "\0").as_bytes());
        let id = "37649fe7-f190-49cd-bfb8-233041d7377a";
        let resumed = args(&format!("claude|--resume|{id}|"));
        assert_eq!(resumed, ["claude", "--resume", id]);
        assert_eq!(named_session(&resumed).as_deref(), Some(id));
        assert_eq!(named_session(&args(&format!("claude|--model|opus|-r|{id}|"))).as_deref(), Some(id));
        assert_eq!(named_session(&args(&format!("claude|--resume={id}|"))).as_deref(), Some(id));
        assert_eq!(named_session(&args(&format!("claude|--session-id|{id}|"))).as_deref(), Some(id));
        // A prompt after `--` that looks like a flag, an interactive `--resume` picker, a bad id: nothing named.
        assert_eq!(named_session(&args(&format!("claude|--|--resume {id}|"))), None);
        assert_eq!(named_session(&args("claude|--resume|")), None);
        assert_eq!(named_session(&args("claude|--resume|../../etc|")), None);

        let shim = args("/home/u/.local/bin/claude|--settings|/run/user/1000/dirigent-866966/tab-2.json|--|<prompt>|");
        let versions = Path::new("/home/u/.local/share/claude/versions/2.1.319");
        assert!(is_claude(&args("claude||"), versions));
        assert!(is_claude(&shim, Path::new("")));
        assert!(is_claude(&args("node|/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js|"), Path::new("")));
        assert!(!is_claude(&args("/usr/lib/claude-desktop/claude-desktop|--type=gpu-process|"), Path::new("")));
        assert!(!is_claude(&args("vim|claude|"), Path::new("/usr/bin/vim")));
        assert!(is_own(&shim, Path::new("/run/user/1000/dirigent-866966")));
        assert!(!is_own(&shim, Path::new("/run/user/1000/dirigent-1")), "another instance's claude is elsewhere");
        assert!(split_cmdline(b"").is_empty(), "kernel threads have an empty cmdline");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scans_a_proc_tree() {
        let tmp = std::env::temp_dir().join(format!("dirigent-procs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let proc = tmp.join("proc");
        let own = tmp.join("run/dirigent-7");
        let id = "37649fe7-f190-49cd-bfb8-233041d7377a";
        let put = |pid: &str, cmdline: &str, cwd: &str| {
            let d = proc.join(pid);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("cmdline"), cmdline.replace('|', "\0")).unwrap();
            std::os::unix::fs::symlink(cwd, d.join("cwd")).unwrap();
        };
        put("100", &format!("claude|--resume|{id}|"), "/r/app");
        put("101", "claude||", "/r/app-worktrees/feat");
        put("102", &format!("claude|--settings|{}/tab-3.json|", own.display()), "/r/app"); // ours
        put("103", "zsh||", "/r/app");
        put("104", "claude||", "/r/other");
        std::fs::create_dir_all(proc.join("self")).unwrap(); // not a pid
        let r = running_in(&proc, &own);
        assert_eq!(r.sessions, [id]);
        let mut cwds = r.cwds.clone();
        cwds.sort();
        assert_eq!(cwds, [PathBuf::from("/r/app"), "/r/app-worktrees/feat".into(), "/r/other".into()]);
        assert!(r.in_project(Path::new("/r/app"), Path::new("/r/app-worktrees")));
        let only_other = Running { sessions: Vec::new(), cwds: vec!["/r/other".into(), "/r/app-x".into()] };
        assert!(!only_other.in_project(Path::new("/r/app"), Path::new("/r/app-worktrees")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Every transcript on this machine (read-only): outliers and timings, to inspect after a claude upgrade.
    /// `cargo test --release -p dirigent history_real_data -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn history_real_data() {
        let projects = PathBuf::from(std::env::var("HOME").unwrap()).join(".claude/projects");
        let t = std::time::Instant::now();
        let r = running(Path::new("/nonexistent")); // every claude counts as outside here
        println!("running: {:?} named, cwds {:?} ({} ms)", r.sessions, r.cwds, t.elapsed().as_millis());
        let cache = Cache::default();
        for pass in ["cold", "warm"] {
            let t = std::time::Instant::now();
            let all = list(&projects, Path::new("/"), Path::new("/"), &cache);
            let ms = t.elapsed().as_millis();
            println!("{pass}: {} sessions in {ms} ms", all.len());
            if pass == "warm" {
                if let Ok(out) = std::env::var("HISTORY_DUMP") {
                    let rows: Vec<String> =
                        all.iter().map(|e| format!("{} {} {}\n", e.id, e.messages, e.cwd.display())).collect();
                    std::fs::write(out, rows.concat()).unwrap();
                }
                for e in &all {
                    let odd = e.title.is_none() || e.prompt.is_none() || e.messages == 0;
                    let t = e.title.as_deref().unwrap_or("-").chars().take(30).collect::<String>();
                    if odd {
                        println!("  odd {} msgs={} title={t:?} prompt={}", e.id, e.messages, e.prompt.is_some());
                    }
                }
            }
        }
    }
}
