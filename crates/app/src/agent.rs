//! Claude Code integration: per-run runtime dir (socket, `claude` shim, zsh wrappers, per-tab settings),
//! a listener thread receiving `dirigent-hook` datagrams, and the per-tab agent state they drive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_channel::Receiver;
use hook::{Kind, Limit, Msg};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    NoAgent,
    Ready,
    Working,
    NeedsYou,
    Done,
}

/// Where an automatic session name came from; a higher one replaces a lower one of the same Claude session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The session's first prompt, shortened (`hook::notice::prompt_title`).
    Prompt,
    /// Claude Code's own title (statusLine `session_name`: `-n`, `/rename`, or its generated title).
    Claude,
}

/// An automatic session name and the Claude session it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Auto {
    pub text: String,
    pub source: Source,
    #[serde(default)]
    pub session: Option<String>,
}

/// Whether `new` replaces the current automatic name: a new Claude session (a new claude, `/clear`) always may;
/// within one session a better source wins, Claude's title follows its latest value, and the first prompt stays.
pub fn offer(cur: Option<&Auto>, new: &Auto) -> bool {
    let Some(cur) = cur else { return true };
    if cur == new {
        return false;
    }
    cur.session != new.session || new.source > cur.source || (new.source == Source::Claude && cur.source == new.source)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Agent {
    pub state: State,
    /// Finished while not being looked at; cleared when the tab is viewed.
    pub unseen: bool,
    pub model: Option<String>,
    pub context_pct: Option<u8>,
    /// Last Claude Code session id seen in this tab (SessionStart; changes on /clear, /resume, /branch).
    pub session_id: Option<String>,
    /// Arguments of the last `claude` started in this tab (from the shim).
    pub args: Option<Vec<String>>,
    /// A SessionStart arrived since that `claude` was started.
    started: bool,
    /// `claude --resume <id>` failed: SessionEnd without any SessionStart (verified on 2.1.318).
    pub resume_failed: Option<String>,
    /// Claude Code's working directory, from its last hook (None when no claude runs).
    pub cwd: Option<PathBuf>,
    /// Unix secs of the last hook from this tab's claude (kept after it exits).
    pub last_activity: Option<i64>,
    /// Unix secs when `state` last changed.
    pub state_since: Option<i64>,
    /// Bumped each time `state` changes to NeedsYou or Done: one desktop notification per change at most.
    pub notice_seq: u64,
    /// What that notification says (built by the hook helper); None = a generic text.
    pub note: Option<String>,
    /// The latest automatic name candidate (the workspace applies it through `offer`).
    pub title: Option<Auto>,
    /// UserPromptSubmit count: an interrupt check started before a newer prompt must not end that one.
    pub prompts: u64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Default, Debug)]
pub struct Board {
    pub tabs: HashMap<u32, Agent>,
    /// Account-wide; last known value is kept when a statusline omits it.
    pub five_hour: Option<Limit>,
    pub seven_day: Option<Limit>,
    /// Hooks arriving while our statusLine never ran mean its command didn't work in Claude's shell: said once.
    status_check: StatusCheck,
}

#[derive(Default, Debug)]
struct StatusCheck {
    seen: bool,
    first_hook: Option<std::time::Instant>,
    warned: bool,
}

impl StatusCheck {
    /// Claude runs the statusLine at start and after every message, well within this.
    const WITHIN: std::time::Duration = std::time::Duration::from_secs(20);

    fn hook(&mut self) {
        if self.seen || self.warned {
            return;
        }
        let first = *self.first_hook.get_or_insert_with(std::time::Instant::now);
        if first.elapsed() > Self::WITHIN {
            self.warned = true;
            eprintln!(
                "dirigent: Claude Code's hooks arrive but its statusLine never ran dirigent-hook (in {} s): no model or \
                 limits. The statusLine's shell (Git Bash or PowerShell) didn't find it on PATH or failed to run it.",
                Self::WITHIN.as_secs()
            );
        }
    }
}

impl Board {
    /// Applies one message; returns whether anything the UI shows changed.
    pub fn apply(&mut self, msg: Msg) -> bool {
        let agent = self.tabs.entry(msg.tab).or_default();
        let before = agent.clone();
        let (five, seven) = (self.five_hour, self.seven_day);
        match msg.kind {
            Kind::Argv { args } => {
                agent.args = Some(args);
                agent.started = false;
            }
            Kind::Hook { event, notification_type, session_id, cwd, message, title, .. } => {
                self.status_check.hook();
                agent.last_activity = Some(now_secs());
                if let Some(text) = title {
                    let new = Auto { text, source: Source::Prompt, session: session_id.clone() };
                    if offer(agent.title.as_ref(), &new) {
                        agent.title = Some(new);
                    }
                }
                if let Some(cwd) = cwd {
                    agent.cwd = Some(cwd.into());
                }
                if event == "UserPromptSubmit" {
                    agent.prompts += 1;
                }
                if event == "SessionStart" {
                    agent.started = true;
                    if session_id.is_some() {
                        agent.session_id = session_id.clone();
                    }
                }
                let next = match (event.as_str(), notification_type.as_deref()) {
                    ("SessionStart", _) => Some(State::Ready),
                    ("UserPromptSubmit" | "PreToolUse" | "PostToolUse", _) => Some(State::Working),
                    ("PermissionRequest", _) | ("Notification", Some("permission_prompt" | "elicitation_dialog")) => {
                        Some(State::NeedsYou)
                    }
                    ("Stop" | "StopFailure", _) => Some(State::Done),
                    // Esc/Ctrl-C interrupts send no hook at all: typed ones are caught by `Board::interrupt`;
                    // idle_prompt (~60 s later) remains the fallback for the rest.
                    ("Notification", Some("idle_prompt")) if agent.state == State::Working => Some(State::Done),
                    ("SessionEnd", _) => {
                        // The session stays resumable: keep its id and the claude args.
                        let failed = (!agent.started).then_some(session_id).flatten();
                        *agent = Agent {
                            session_id: agent.session_id.take(),
                            args: agent.args.take(),
                            title: agent.title.take(),
                            resume_failed: failed,
                            last_activity: agent.last_activity,
                            state_since: Some(now_secs()),
                            ..Agent::default()
                        };
                        None
                    }
                    _ => None,
                };
                if let Some(next) = next {
                    agent.unseen = next == State::Done && (agent.state != State::Done || agent.unseen);
                    if next != agent.state {
                        agent.state_since = Some(now_secs());
                        if matches!(next, State::NeedsYou | State::Done) {
                            agent.notice_seq += 1;
                            agent.note = None;
                        }
                    }
                    agent.state = next;
                    // The first detail wins: the generic Notification after a PermissionRequest keeps its text.
                    if matches!(next, State::NeedsYou | State::Done) && agent.note.is_none() {
                        agent.note = match event.as_str() {
                            "StopFailure" => Some("Stopped with an error".into()),
                            "Stop" | "Notification" if next == State::Done => None,
                            _ => message,
                        };
                    }
                }
            }
            Kind::Status { model, context_pct, five_hour, seven_day, session_name, session_id } => {
                self.status_check.seen = true;
                if let Some(text) = session_name {
                    let session = session_id.or_else(|| agent.session_id.clone());
                    let new = Auto { text, source: Source::Claude, session };
                    if offer(agent.title.as_ref(), &new) {
                        agent.title = Some(new);
                    }
                }
                if agent.state == State::NoAgent {
                    agent.state = State::Ready;
                    agent.state_since = Some(now_secs());
                }
                if model.is_some() {
                    agent.model = model;
                }
                if let Some(p) = context_pct {
                    agent.context_pct = Some(p.round().clamp(0., 100.) as u8);
                }
                self.five_hour = five_hour.or(self.five_hour);
                self.seven_day = seven_day.or(self.seven_day);
            }
        }
        // `last_activity` is stamped on every hook but shown only as "N m ago" (the minute timer redraws it): the
        // clock moving on is no visible change (a second ticking between two hooks woke the UI for nothing).
        let mut now = self.tabs.get(&msg.tab).unwrap().clone();
        if before.last_activity.is_some() {
            now.last_activity = before.last_activity;
        }
        now.prompts = before.prompts; // a count only interrupt checks read

        now != before || self.five_hour != five || self.seven_day != seven
    }

    pub fn get(&self, tab: u32) -> Agent {
        self.tabs.get(&tab).cloned().unwrap_or_default()
    }

    /// The tab's shell restarted: live agent state is gone, the resumable session id and args stay.
    pub fn reset_live(&mut self, tab: u32) {
        if let Some(a) = self.tabs.get_mut(&tab) {
            *a = Agent {
                session_id: a.session_id.take(),
                args: a.args.take(),
                title: a.title.take(),
                last_activity: a.last_activity,
                ..Agent::default()
            };
        }
    }

    /// Forgets a session id that can no longer be resumed.
    pub fn forget_session(&mut self, tab: u32) {
        if let Some(a) = self.tabs.get_mut(&tab) {
            a.session_id = None;
            a.resume_failed = None;
        }
    }

    pub fn mark_seen(&mut self, tab: u32) {
        if let Some(a) = self.tabs.get_mut(&tab) {
            a.unseen = false;
        }
    }

    /// An Esc/Ctrl-C typed into the tab interrupted its turn: Ready (you're in front of it: no "done", no
    /// notification), unless the turn already ended or a newer prompt started since the key (`prompts`).
    pub fn interrupt(&mut self, tab: u32, prompts: u64) -> bool {
        let Some(a) = self.tabs.get_mut(&tab) else { return false };
        if a.state != State::Working || a.prompts != prompts {
            return false;
        }
        a.state = State::Ready;
        a.state_since = Some(now_secs());
        true
    }
}

/// Whether the transcript's last conversational entry is Claude Code's interrupt marker (`[Request interrupted
/// by user]`, `… for tool use`; written 120–315 ms after the key on 2.1.319). `tail` is the end of the file,
/// its first line possibly cut. Subagent (sidechain) entries and bookkeeping lines are skipped.
pub fn interrupted(tail: &str) -> bool {
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let kind = v["type"].as_str().unwrap_or_default();
        if !matches!(kind, "user" | "assistant") || v["isSidechain"] == true {
            continue;
        }
        let content = &v["message"]["content"];
        let text = content.as_str().or_else(|| content[0]["text"].as_str()).unwrap_or_default();
        return kind == "user" && text.starts_with("[Request interrupted by user");
    }
    false
}

/// The last 64 KB of a transcript (the marker and what follows it are a few small lines). Blocking IO.
pub fn transcript_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

pub struct Hub {
    pub board: Arc<Mutex<Board>>,
    /// One message per batch of visible changes (bounded(1)).
    pub changes: Receiver<()>,
    pub dir: PathBuf,
    sock: PathBuf,
    bin: PathBuf,
    zdotdir: PathBuf,
    helper: PathBuf,
}

impl Hub {
    /// `tracker` gets every hook too (per-agent changed files), on the listener thread.
    pub fn start(tracker: Arc<crate::changes::Tracker>) -> std::io::Result<Self> {
        let dir = runtime_dir();
        if let Some(runtime) = dir.parent() {
            remove_stale_dirs(runtime);
        }
        let helper = std::env::current_exe()?.with_file_name(format!("dirigent-hook{}", std::env::consts::EXE_SUFFIX));
        Self::start_in(tracker, dir, helper)
    }

    /// `start` with a given runtime dir and helper (tests).
    fn start_in(tracker: Arc<crate::changes::Tracker>, dir: PathBuf, helper: PathBuf) -> std::io::Result<Self> {
        let (bin, zdotdir) = (dir.join("bin"), dir.join("zsh"));
        std::fs::create_dir_all(&bin)?;
        std::fs::create_dir_all(&zdotdir)?;
        // The `claude` shim is the helper under another name (`hook::shim`).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            std::os::unix::fs::symlink(&helper, bin.join("claude"))?;
        }
        // Windows: hardlinks (a copy across volumes): `claude.exe` the shim, `dirigent-hook.exe` for the statusLine,
        // which runs through a shell and finds it on the session's PATH by name (no path to quote).
        #[cfg(windows)]
        for name in ["claude.exe", "dirigent-hook.exe"] {
            let to = bin.join(name);
            std::fs::hard_link(&helper, &to).or_else(|_| std::fs::copy(&helper, &to).map(drop))?;
        }
        for (name, body) in ZSH_WRAPPERS {
            std::fs::write(zdotdir.join(name), body)?;
        }

        let board = Arc::new(Mutex::new(Board::default()));
        let (tx, changes) = async_channel::bounded(1);
        let handle = {
            let board = board.clone();
            // False once the UI is gone.
            move |bytes: &[u8]| {
                let Ok(msg) = serde_json::from_slice::<Msg>(bytes) else { return true };
                crate::perf::event_with(|| format!("agent msg {msg:?}"));
                tracker.hook(msg.tab, &msg.kind);
                !(board.lock().unwrap().apply(msg) && tx.try_send(()).is_err() && tx.is_closed())
            }
        };
        #[cfg(unix)]
        let sock = {
            let sock = dir.join("sock");
            let socket = std::os::unix::net::UnixDatagram::bind(&sock)?;
            std::thread::Builder::new().name("agent-listener".into()).spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n) = socket.recv(&mut buf) {
                    if !handle(&buf[..n]) {
                        return;
                    }
                }
            })?;
            sock
        };
        // One message per connection, read to EOF. Four instances (one thread each, blocked while idle) so hooks
        // of many sessions firing together rarely find the pipe busy; the name stays registered while we run.
        #[cfg(windows)]
        let sock = {
            let name = format!("dirigent-hook-{}", std::process::id());
            let handle = Arc::new(Mutex::new(handle));
            for i in 0..4 {
                let (server, handle) = (hook::pipe::Server::bind(&name, i == 0)?, handle.clone());
                std::thread::Builder::new().name("agent-listener".into()).spawn(move || {
                    use std::io::Read as _;
                    while let Ok(conn) = server.accept() {
                        let mut buf = Vec::new();
                        if conn.file().take(64 * 1024).read_to_end(&mut buf).is_ok() && !handle.lock().unwrap()(&buf) {
                            return;
                        }
                    }
                })?;
            }
            PathBuf::from(name)
        };
        Ok(Self { board, changes, dir, sock, bin, zdotdir, helper })
    }

    /// Writes the tab's settings file and returns the env for its shell.
    pub fn session_env(&self, tab: u32) -> std::io::Result<Vec<(String, String)>> {
        let settings = self.dir.join(format!("tab-{tab}.json"));
        std::fs::write(&settings, self.settings_json(tab))?;
        let path = std::env::var("PATH").unwrap_or_default();
        let mut env = vec![
            ("DIRIGENT_TAB_ID".into(), tab.to_string()),
            ("DIRIGENT_SOCKET".into(), self.sock.display().to_string()),
            ("DIRIGENT_CLAUDE_SETTINGS".into(), settings.display().to_string()),
            ("DIRIGENT_BIN".into(), self.bin.display().to_string()),
            ("DIRIGENT_HOOK".into(), self.helper.display().to_string()),
            ("PATH".into(), format!("{}{PATH_SEP}{path}", self.bin.display())),
        ];
        // zsh rc files usually prepend ~/.local/bin, which would shadow the shim: wrap them via ZDOTDIR so the shim
        // dir is put first again after they ran. ponytail: bash/fish only get the PATH prepend (add --rcfile etc.
        // when someone uses them).
        let shell = std::env::var("SHELL").unwrap_or_default();
        if shell.ends_with("/zsh") {
            let (orig, unset) = match std::env::var("ZDOTDIR") {
                Ok(z) => (z, ""),
                Err(_) => (std::env::var("HOME").unwrap_or_default(), "1"),
            };
            env.push(("DIRIGENT_ZDOTDIR".into(), self.zdotdir.display().to_string()));
            env.push(("DIRIGENT_ZDOTDIR_ORIG".into(), orig));
            env.push(("DIRIGENT_ZDOTDIR_WAS_UNSET".into(), unset.into()));
            env.push(("ZDOTDIR".into(), self.zdotdir.display().to_string()));
        }
        Ok(env)
    }

    #[cfg(unix)]
    fn settings_json(&self, tab: u32) -> String {
        let cmd = |mode: &str| format!("{} {} {tab} {mode}", sh_quote(&self.helper), sh_quote(&self.sock));
        let handler = serde_json::json!([{ "hooks": [{ "type": "command", "command": cmd("hook"), "async": true }] }]);
        let status = serde_json::json!({ "type": "command", "command": cmd("status") });
        hooks_json(handler, status)
    }

    /// Windows: hooks in exec form (the helper's path and arguments, no shell, nothing to quote); the statusLine has
    /// no exec form, so it is a bare command line for the shell Claude Code will pick (`hook::shell`).
    #[cfg(windows)]
    fn settings_json(&self, tab: u32) -> String {
        let (helper, pipe) = (self.helper.to_string_lossy().replace('\\', "/"), self.sock.to_string_lossy());
        let handler = serde_json::json!([{ "hooks": [{
            "type": "command", "command": helper, "args": [pipe, tab.to_string(), "hook"], "async": true,
        }] }]);
        let line = hook::shell::status_command(&hook::shell::claude_shell(), &pipe, tab);
        hooks_json(handler, serde_json::json!({ "type": "command", "command": line }))
    }

    pub fn remove_tab(&self, tab: u32) {
        self.board.lock().unwrap().tabs.remove(&tab);
        let _ = std::fs::remove_file(self.dir.join(format!("tab-{tab}.json")));
    }
}

/// The settings file: `handler` for every hook event we follow, `status` as the statusLine.
fn hooks_json(handler: serde_json::Value, status: serde_json::Value) -> String {
    let events = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PermissionRequest",
        "Notification",
        "Stop",
        "StopFailure",
        "SessionEnd",
    ];
    let hooks: serde_json::Map<String, serde_json::Value> =
        events.iter().map(|e| (e.to_string(), handler.clone())).collect();
    serde_json::json!({ "hooks": hooks, "statusLine": status }).to_string()
}

const PATH_SEP: char = if cfg!(windows) { ';' } else { ':' };

/// This instance's runtime dir (`$XDG_RUNTIME_DIR/dirigent-<pid>`), removed on quit.
pub fn runtime_dir() -> PathBuf {
    runtime_parent().join(format!("dirigent-{}", std::process::id()))
}

/// `$XDG_RUNTIME_DIR` (else the temp dir).
pub fn runtime_parent() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Removes runtime dirs left behind by app instances that are no longer running (crash, kill -9).
fn remove_stale_dirs(runtime: &Path) {
    let Ok(entries) = std::fs::read_dir(runtime) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        // `agent-ide-` too: dirs left by the app before its rename.
        let Some(pid) =
            name.to_str().and_then(|n| n.strip_prefix("dirigent-").or_else(|| n.strip_prefix("agent-ide-")))
        else {
            continue;
        };
        if pid.parse().is_ok_and(|pid| !alive(pid)) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: plain handle queries; the handle is closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false; // no such process (or not ours to see: then not one of our instances either)
        }
        let mut code = 0;
        let running = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(h);
        running
    }
}

#[cfg(unix)]
fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// Names a file whose text the shim passes to claude as the first prompt (then the file and the variable go).
pub const PROMPT_ENV: &str = hook::shim::PROMPT_ENV;
/// Windows: names a file with claude's flags (JSON), which the shim reads; typed lines carry no flags there.
pub const ARGS_ENV: &str = hook::shim::ARGS_ENV;

/// A new file for tab `tab`'s first prompt in this run's runtime dir (`start_line`, then the shim reads it).
pub fn prompt_file(tab: usize) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    runtime_dir().join(format!("prompt-{tab}-{nanos}.txt"))
}

/// A new file for tab `tab`'s claude flags (Windows; `start_line`, then the shim reads it).
pub fn args_file(tab: usize) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    runtime_dir().join(format!("args-{tab}-{nanos}.json"))
}

/// Writes a first prompt or the flags file for the shim (only you can read it, never over an existing file).
/// Blocking: call off the UI thread.
pub fn write_prompt(path: &Path, prompt: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    // Windows: the runtime dir is in your profile's temp dir, which only you can read.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)?.write_all(prompt.as_bytes())
}

/// zsh startup wrappers: each sources the user's own file, then (rc/login) puts the shim dir first in `path` and
/// restores ZDOTDIR, so the user's config and child shells behave exactly as without dirigent.
const ZSH_WRAPPERS: [(&str, &str); 4] =
    [(".zshenv", ZSHENV), (".zprofile", ZPROFILE), (".zshrc", ZSHRC), (".zlogin", ZLOGIN)];

const ZSHENV: &str = r#"# dirigent wrapper
ZDOTDIR=$DIRIGENT_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zshenv ]] && builtin source $ZDOTDIR/.zshenv
DIRIGENT_ZDOTDIR_ORIG=$ZDOTDIR
ZDOTDIR=$DIRIGENT_ZDOTDIR
"#;

const ZPROFILE: &str = r#"# dirigent wrapper
ZDOTDIR=$DIRIGENT_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zprofile ]] && builtin source $ZDOTDIR/.zprofile
ZDOTDIR=$DIRIGENT_ZDOTDIR
"#;

const ZSHRC: &str = r#"# dirigent wrapper
ZDOTDIR=$DIRIGENT_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zshrc ]] && builtin source $ZDOTDIR/.zshrc
path=($DIRIGENT_BIN ${path:#$DIRIGENT_BIN})
# Report the cwd at every prompt (OSC 7) so the app follows `cd`; `%` is escaped for the file:// URL.
__dirigent_osc7() { builtin print -rn -- $'\e]7;file://'"${HOST}${PWD//\%/%25}"$'\e\\' }
typeset -ga precmd_functions
precmd_functions=(${precmd_functions:#__dirigent_osc7} __dirigent_osc7)
if [[ -o login ]]; then
  ZDOTDIR=$DIRIGENT_ZDOTDIR
elif [[ -n $DIRIGENT_ZDOTDIR_WAS_UNSET && $DIRIGENT_ZDOTDIR_ORIG == $HOME ]]; then
  unset ZDOTDIR
fi
"#;

const ZLOGIN: &str = r#"# dirigent wrapper
ZDOTDIR=$DIRIGENT_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zlogin ]] && builtin source $ZDOTDIR/.zlogin
path=($DIRIGENT_BIN ${path:#$DIRIGENT_BIN})
if [[ -n $DIRIGENT_ZDOTDIR_WAS_UNSET && $DIRIGENT_ZDOTDIR_ORIG == $HOME ]]; then
  unset ZDOTDIR
fi
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(tab: u32, event: &str, nt: Option<&str>) -> Msg {
        hook_id(tab, event, nt, "s1")
    }

    fn hook_id(tab: u32, event: &str, nt: Option<&str>, id: &str) -> Msg {
        Msg {
            tab,
            kind: Kind::Hook {
                event: event.into(),
                notification_type: nt.map(Into::into),
                session_id: Some(id.into()),
                cwd: None,
                message: None,
                tool: None,
                tool_use_id: None,
                path: None,
                title: None,
            },
        }
    }

    /// A real hook payload (crates/hook/tests/fixtures), parsed like the helper does.
    fn real(tab: u32, name: &str) -> Msg {
        let path = format!("{}/../hook/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
        Msg { tab, kind: hook::parse("hook", &std::fs::read_to_string(path).unwrap()).unwrap() }
    }

    /// One notification per change into NeedsYou/Done; the PermissionRequest's detail survives the generic
    /// Notification that follows it.
    #[test]
    fn notices() {
        let mut b = Board::default();
        let seq = |b: &Board| (b.get(1).notice_seq, b.get(1).note.clone());
        b.apply(hook(1, "UserPromptSubmit", None));
        assert_eq!(seq(&b), (0, None));
        b.apply(real(1, "hook_PermissionRequest"));
        let (n, note) = seq(&b);
        assert_eq!(n, 1);
        assert_eq!(note.as_deref(), Some("Wants to run a command"));
        b.apply(real(1, "hook_Notification"));
        assert_eq!(seq(&b), (1, note.clone()), "same prompt: no new notice, detail kept");
        b.apply(hook(1, "PostToolUse", None));
        b.apply(real(1, "hook_Stop"));
        assert_eq!(seq(&b), (2, None), "done: generic text");
        b.apply(hook(1, "Stop", None));
        assert_eq!(seq(&b).0, 2, "done again: no new notice");
        b.apply(hook(1, "UserPromptSubmit", None));
        b.apply(hook(1, "StopFailure", None));
        assert_eq!(seq(&b), (3, Some("Stopped with an error".into())));
        // A Notification-only prompt (elicitation) uses its message.
        b.apply(hook(1, "UserPromptSubmit", None));
        let mut ask = real(1, "hook_Notification");
        if let Kind::Hook { notification_type, .. } = &mut ask.kind {
            *notification_type = Some("elicitation_dialog".into());
        }
        b.apply(ask);
        assert_eq!(seq(&b), (4, Some("Claude needs your permission".into())));
    }

    fn status(tab: u32, ctx: Option<f32>, five: Option<Limit>) -> Msg {
        Msg {
            tab,
            kind: Kind::Status {
                model: Some("Opus 5.5".into()),
                context_pct: ctx,
                five_hour: five,
                seven_day: None,
                session_name: None,
                session_id: None,
            },
        }
    }

    fn named(tab: u32, name: &str) -> Msg {
        named_in(tab, name, None)
    }

    fn named_in(tab: u32, name: &str, id: Option<&str>) -> Msg {
        let mut m = status(tab, None, None);
        if let Kind::Status { session_name, session_id, .. } = &mut m.kind {
            *session_name = Some(name.into());
            *session_id = id.map(Into::into);
        }
        m
    }

    /// The status carries its own session id: its order against SessionStart doesn't matter.
    #[test]
    fn names_follow_the_status_session() {
        let mut b = Board::default();
        let t = |b: &Board| b.get(1).title.map(|a| a.text);
        // `claude -n foo`: the first status can come before SessionStart.
        b.apply(named_in(1, "foo", Some("s1")));
        b.apply(hook_id(1, "SessionStart", None, "s1"));
        b.apply(prompt(1, "s1", "first prompt"));
        assert_eq!(t(&b).as_deref(), Some("foo"), "the -n name stays");
        // /clear, then a late status of the old session: the new session's prompt still wins.
        b.apply(hook_id(1, "SessionEnd", None, "s1"));
        b.apply(hook_id(1, "SessionStart", None, "s2"));
        b.apply(named_in(1, "foo", Some("s1")));
        b.apply(prompt(1, "s2", "new topic"));
        assert_eq!(t(&b).as_deref(), Some("new topic"));
    }

    fn prompt(tab: u32, id: &str, text: &str) -> Msg {
        let mut m = hook_id(tab, "UserPromptSubmit", None, id);
        if let Kind::Hook { title, .. } = &mut m.kind {
            *title = Some(text.into());
        }
        m
    }

    #[test]
    fn automatic_names() {
        let mut b = Board::default();
        let t = |b: &Board| b.get(1).title.map(|a| (a.text, a.source));
        b.apply(hook_id(1, "SessionStart", None, "s1"));
        assert!(b.apply(prompt(1, "s1", "first prompt")));
        assert_eq!(t(&b), Some(("first prompt".into(), Source::Prompt)));
        b.apply(prompt(1, "s1", "second prompt"));
        assert_eq!(t(&b).unwrap().0, "first prompt", "only the first prompt of a session counts");
        assert!(b.apply(named(1, "Claude title")));
        assert_eq!(t(&b), Some(("Claude title".into(), Source::Claude)));
        assert!(!b.apply(named(1, "Claude title")), "a repeated statusline is no change");
        b.apply(prompt(1, "s1", "third prompt"));
        assert_eq!(t(&b).unwrap().0, "Claude title", "a prompt never beats Claude's title");
        b.apply(named(1, "renamed in claude"));
        assert_eq!(t(&b).unwrap().0, "renamed in claude", "/rename: latest wins");
        // /clear: SessionEnd(old), SessionStart(new); the old name stays until the new session offers one.
        b.apply(hook_id(1, "SessionEnd", None, "s1"));
        b.apply(hook_id(1, "SessionStart", None, "s2"));
        assert_eq!(t(&b).unwrap().0, "renamed in claude");
        b.apply(prompt(1, "s2", "new topic"));
        assert_eq!(t(&b), Some(("new topic".into(), Source::Prompt)));
    }

    #[test]
    fn offer_rules() {
        let a = |text: &str, source, s: &str| Auto { text: text.into(), source, session: Some(s.into()) };
        let claude = a("c", Source::Claude, "s1");
        assert!(offer(None, &claude));
        assert!(!offer(Some(&claude), &claude));
        assert!(!offer(Some(&claude), &a("p", Source::Prompt, "s1")), "resumed session: its prompt stays below");
        assert!(offer(Some(&claude), &a("p", Source::Prompt, "s2")), "new session");
        assert!(!offer(Some(&a("p", Source::Prompt, "s1")), &a("q", Source::Prompt, "s1")), "first prompt stays");
        assert!(offer(Some(&claude), &a("d", Source::Claude, "s1")));
    }

    #[test]
    fn the_clock_moving_on_is_no_visible_change() {
        let mut b = Board::default();
        assert!(b.apply(hook(1, "UserPromptSubmit", None)));
        // As if the last hook came long ago (a second boundary between two hooks is the same thing).
        b.tabs.get_mut(&1).unwrap().last_activity = Some(0);
        assert!(!b.apply(hook(1, "PreToolUse", None)), "same state: no UI wakeup");
        assert!(b.get(1).last_activity.unwrap() > 0, "still stamped");
        assert!(b.apply(hook(1, "Stop", None)), "a real change still counts");
    }

    #[test]
    fn state_machine() {
        let mut b = Board::default();
        let st = |b: &Board| b.get(1).state;
        assert!(b.apply(hook(1, "SessionStart", None)));
        assert_eq!(st(&b), State::Ready);
        assert!(b.apply(hook(1, "UserPromptSubmit", None)));
        assert_eq!(st(&b), State::Working);
        assert!(!b.apply(hook(1, "PreToolUse", None)), "no visible change, no UI wakeup");
        assert!(b.apply(hook(1, "PermissionRequest", None)));
        assert_eq!(st(&b), State::NeedsYou);
        assert!(b.apply(hook(1, "PostToolUse", None)));
        assert!(!b.apply(hook(1, "Notification", Some("auth_success"))));
        assert!(b.apply(hook(1, "Notification", Some("permission_prompt"))));
        assert_eq!(st(&b), State::NeedsYou);
        assert!(b.apply(hook(1, "Stop", None)));
        assert_eq!((b.get(1).state, b.get(1).unseen), (State::Done, true));
        b.mark_seen(1);
        assert!(!b.apply(hook(1, "Notification", Some("idle_prompt"))), "idle after Done changes nothing");
        assert!(!b.get(1).unseen);
        // Interrupted turn: no Stop, idle_prompt ends it.
        b.apply(hook(1, "UserPromptSubmit", None));
        assert!(b.apply(hook(1, "Notification", Some("idle_prompt"))));
        assert_eq!(st(&b), State::Done);
        assert!(b.apply(hook(1, "SessionEnd", None)));
        assert_eq!(b.get(1).state, State::NoAgent);
        assert_eq!(b.get(1).session_id.as_deref(), Some("s1"), "still resumable after exit");
        assert_eq!(b.get(1).resume_failed, None);
    }

    #[test]
    fn typed_interrupts() {
        let mut b = Board::default();
        let st = |b: &Board| b.get(1).state;
        b.apply(hook(1, "SessionStart", None));
        assert!(!b.interrupt(1, 0), "only a working turn can be interrupted");
        b.apply(hook(1, "UserPromptSubmit", None));
        let at_key = b.get(1).prompts;
        assert!(b.interrupt(1, at_key));
        assert_eq!(
            (st(&b), b.get(1).unseen, b.get(1).notice_seq),
            (State::Ready, false, 0),
            "no done, no notification"
        );
        assert!(!b.interrupt(1, at_key), "once");
        // The prompt went back to the input and was sent again before the check finished: that turn stays.
        b.apply(hook(1, "UserPromptSubmit", None));
        assert!(!b.apply(hook(1, "UserPromptSubmit", None)), "a queued prompt while working wakes no UI");
        assert!(!b.interrupt(1, at_key));
        assert_eq!(st(&b), State::Working);
        b.apply(hook(1, "PermissionRequest", None));
        assert!(!b.interrupt(1, b.get(1).prompts), "a permission prompt isn't working");
    }

    #[test]
    fn interrupt_marker_from_real_claude() {
        let fixture = |name| {
            std::fs::read_to_string(format!("{}/../hook/tests/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR")))
                .unwrap()
        };
        // claude 2.1.319: Esc while text streamed / during a Bash tool → a marker entry.
        assert!(interrupted(&fixture("transcript_interrupted")));
        assert!(interrupted(&fixture("transcript_interrupted_tool")));
        // Esc before any output (the prompt goes back to the input) writes nothing; a finished turn ends on Claude.
        assert!(!interrupted(&fixture("transcript_early_esc")));
        assert!(!interrupted(&fixture("transcript_done")));
        // An earlier interrupt followed by a new prompt is old news; a cut first line and sidechains are skipped.
        let marker = r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        let prompt = r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":"next"}}"#;
        let side = r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[]}}"#;
        assert!(!interrupted(&format!("{marker}\n{prompt}\n")));
        assert!(interrupted(&format!("ted\":true}}\n{marker}\n{side}\n{{\"type\":\"ai-title\"}}\n")));
        assert!(!interrupted(""));
    }

    #[test]
    fn session_ids_and_failed_resume() {
        let mut b = Board::default();
        let argv =
            |args: &[&str]| Msg { tab: 1, kind: Kind::Argv { args: args.iter().map(|s| s.to_string()).collect() } };
        // /clear: SessionEnd(old) then SessionStart(new) — as observed on claude 2.1.318.
        b.apply(argv(&["--model", "haiku"]));
        b.apply(hook_id(1, "SessionStart", None, "old"));
        b.apply(hook_id(1, "SessionEnd", Some("clear"), "old"));
        b.apply(hook_id(1, "SessionStart", None, "new"));
        assert_eq!(b.get(1).session_id.as_deref(), Some("new"));
        b.apply(hook_id(1, "SessionEnd", None, "new"));
        assert_eq!(b.get(1).resume_failed, None);
        // `claude --resume gone`: shim argv, then SessionEnd without SessionStart.
        b.apply(argv(&["--resume", "gone"]));
        assert!(b.apply(hook_id(1, "SessionEnd", None, "gone")));
        assert_eq!(b.get(1).resume_failed.as_deref(), Some("gone"));
        assert_eq!(b.get(1).args.as_deref(), Some(&["--resume".to_string(), "gone".to_string()][..]));
        b.forget_session(1);
        assert_eq!((b.get(1).session_id, b.get(1).resume_failed), (None, None));
    }

    #[test]
    fn status_and_limits() {
        let mut b = Board::default();
        let five = Limit { used_pct: 14., resets_at: 1793836800 };
        assert!(b.apply(status(1, None, None)));
        assert_eq!(b.get(1).state, State::Ready);
        assert!(b.apply(status(1, Some(6.2), Some(five))));
        assert_eq!(b.get(1).context_pct, Some(6));
        assert!(!b.apply(status(1, Some(6.4), Some(five))), "same rounded %, no wakeup");
        assert!(!b.apply(status(1, Some(6.), None)), "missing window keeps the last known value");
        assert_eq!(b.five_hour, Some(five));
    }

    #[cfg(unix)]
    fn write_exec(path: &Path, body: &str) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
    }

    /// The built `dirigent-hook` (`cargo test` at the workspace root builds it; the app finds it next to itself).
    fn helper_bin() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let p = exe.parent().unwrap().parent().unwrap().join(format!("dirigent-hook{}", std::env::consts::EXE_SUFFIX));
        assert!(p.is_file(), "{} missing: run cargo test at the workspace root", p.display());
        p
    }

    /// The shim exactly as `Hub::start` makes it.
    #[cfg(unix)]
    fn install_shim(dir: &Path) {
        std::os::unix::fs::symlink(helper_bin(), dir.join("claude")).unwrap();
    }

    #[test]
    #[cfg(unix)] // sh quoting: the Windows file has its own test
    fn settings_file_shape() {
        let hub = Hub {
            board: Default::default(),
            changes: async_channel::bounded(1).1,
            dir: "/run/x".into(),
            sock: "/run/x/sock".into(),
            bin: "/run/x/bin".into(),
            zdotdir: "/run/x/zsh".into(),
            helper: "/opt/it's/dirigent-hook".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&hub.settings_json(4)).unwrap();
        assert_eq!(v["statusLine"]["command"], r#"'/opt/it'\''s/dirigent-hook' '/run/x/sock' 4 status"#);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["async"], true);
        assert_eq!(v["hooks"].as_object().unwrap().len(), 9);
    }

    #[test]
    #[cfg(unix)] // sh scripts as claude; Windows: crates/hook/tests/shim.rs
    fn shim_runs_real_claude_with_settings_and_skips_itself() {
        let d = std::env::temp_dir().join(format!("dirigent-shim-test-{}", std::process::id()));
        let (shim_dir, real_dir) = (d.join("shim"), d.join("real"));
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        install_shim(&shim_dir);
        write_exec(&real_dir.join("claude"), "#!/bin/sh\necho real \"$@\"\n").unwrap();
        // Stand-in for the app: receives what the shim reports.
        let app_sock = d.join("app.sock");
        let app = std::os::unix::net::UnixDatagram::bind(&app_sock).unwrap();
        app.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let settings = d.join("s.json");
        std::fs::write(&settings, "{}").unwrap();
        let run = |settings: &Path| {
            let out = std::process::Command::new(shim_dir.join("claude"))
                .arg("--model")
                .arg("a b")
                .env("PATH", format!("{}:{}:/usr/bin:/bin", shim_dir.display(), real_dir.display()))
                .env("DIRIGENT_CLAUDE_SETTINGS", settings)
                .env("DIRIGENT_SOCKET", &app_sock)
                .env("DIRIGENT_TAB_ID", "3")
                .env_remove("DIRIGENT_BIN")
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(run(&settings), format!("real --settings {} --model a b\n", settings.display()));
        let mut buf = [0u8; 4096];
        let n = app.recv(&mut buf).unwrap();
        let msg: Msg = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(msg.kind, Kind::Argv { args: vec!["--model".into(), "a b".into()] }, "argv reported before exec");
        // Settings gone (app quit): plain claude, never a broken --settings.
        assert_eq!(run(&d.join("missing.json")), "real --model a b\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[cfg(unix)] // POSIX shells; Windows: typed_lines_reach_claude_intact_in_powershell_and_cmd
    fn shim_passes_a_prompt_file_intact_through_aliases_and_deletes_it() {
        let d = std::env::temp_dir().join(format!("dirigent-shim-prompt-test-{}", std::process::id()));
        let (shim_dir, real_dir) = (d.join("shim"), d.join("real"));
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        install_shim(&shim_dir);
        // Each argument in brackets (one per prompt, exactly as written), then whether the variable reached claude.
        write_exec(
            &real_dir.join("claude"),
            "#!/bin/sh\nprintf '[%s]' \"$@\"\nprintf '{%s}' \"${DIRIGENT_PROMPT_FILE-unset}\"\n",
        )
        .unwrap();
        let prompt = "Branch: feat/x\n\nFix the 'login' \"bug\": $HOME `id` \\n\n  - keep it\n-starts with a dash";
        let file = d.join("prompt-1-1.txt");
        let line = crate::projects::start_line(
            Default::default(),
            None,
            true,
            &["--effort".into(), "high".into()],
            Some(&file),
            None,
        )
        .unwrap();
        // The line as a session's shell runs it, after an alias that adds flags of its own.
        let script = d.join("typed.sh");
        std::fs::write(&script, format!("alias claude='claude --model \"a b\"'\n{line}\n")).unwrap();
        let shells: [(&str, &[&str]); 3] = [("sh", &[]), ("bash", &["-O", "expand_aliases"]), ("zsh", &["-f"])];
        let mut ran = 0;
        for (shell, flags) in shells {
            if std::process::Command::new(shell).arg("-c").arg("true").status().is_err() {
                continue; // not installed
            }
            let run = || {
                let out = std::process::Command::new(shell)
                    .args(flags)
                    .arg(&script)
                    .env("PATH", format!("{}:{}:/usr/bin:/bin", shim_dir.display(), real_dir.display()))
                    .env_remove("DIRIGENT_CLAUDE_SETTINGS")
                    .env_remove("DIRIGENT_BIN")
                    .env_remove(PROMPT_ENV)
                    .output()
                    .unwrap();
                String::from_utf8(out.stdout).unwrap()
            };
            write_prompt(&file, prompt).unwrap();
            assert_eq!(run(), format!("[--model][a b][--effort][high][--][{prompt}]{{unset}}"), "{shell}");
            assert!(!file.exists(), "{shell}: deleted once read");
            assert_eq!(run(), "[--model][a b][--effort][high]{unset}", "{shell}: again (history): no prompt, no error");
            ran += 1;
        }
        assert!(ran > 0, "no shell to test with");
        assert!(write_prompt(&file, "x").is_ok() && write_prompt(&file, "y").is_err(), "never overwrites");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[cfg(unix)] // zsh; Windows: shell.rs tests
    fn zsh_wrapper_reports_cwd_after_users_rc() {
        if !Path::new("/usr/bin/zsh").exists() && !Path::new("/bin/zsh").exists() {
            return;
        }
        let d = std::env::temp_dir().join(format!("dirigent-zsh-test-{}", std::process::id()));
        let (wrappers, user) = (d.join("wrap"), d.join("user"));
        std::fs::create_dir_all(&wrappers).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        for (name, body) in ZSH_WRAPPERS {
            std::fs::write(wrappers.join(name), body).unwrap();
        }
        // A stand-in for the user's own .zshrc (never the real one) that has a precmd hook of its own.
        std::fs::write(user.join(".zshrc"), "precmd_functions+=(mine); mine() { :; }\nPS1='> '\n").unwrap();
        let mut cmd = session::CommandBuilder::new("zsh");
        cmd.arg("-i");
        for (k, v) in [
            ("ZDOTDIR", wrappers.to_str().unwrap()),
            ("DIRIGENT_ZDOTDIR", wrappers.to_str().unwrap()),
            ("DIRIGENT_ZDOTDIR_ORIG", user.to_str().unwrap()),
            ("DIRIGENT_BIN", "/nonexistent"),
        ] {
            cmd.env(k, v);
        }
        let size = session::Size { cols: 80, rows: 10, cell_width: 8, cell_height: 16 };
        let s = session::Session::spawn_command(cmd, &d, size).unwrap();
        let wait_for = |want: &Path| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut last = None;
            while last.as_deref() != Some(want) {
                assert!(std::time::Instant::now() < deadline, "cwd {last:?}, want {want:?}");
                std::thread::sleep(std::time::Duration::from_millis(20));
                last = s.take_cwd_change().or(last);
            }
        };
        wait_for(&d); // first prompt
        s.write(b"cd '/tmp'\r");
        wait_for(Path::new("/tmp"));
        drop(s);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn hook_fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/../hook/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// The real helper → the app's listener (datagram / pipe) → the board: states and limits as the UI shows them.
    #[test]
    fn states_and_limits_from_the_real_helper() {
        let d = std::env::temp_dir().join(format!("dirigent-hub-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (tracker, _rx) = crate::changes::Tracker::new();
        let hub = Hub::start_in(tracker, d.clone(), helper_bin()).unwrap();
        let send = |mode: &str, fixture: &str| {
            use std::io::Write as _;
            let mut child = std::process::Command::new(helper_bin())
                .args([hub.sock.to_str().unwrap(), "7", mode])
                .env("CLAUDE_CONFIG_DIR", d.join("no-user-settings"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(hook_fixture(fixture).as_bytes()).unwrap();
            assert!(child.wait().unwrap().success());
        };
        let wait_for = |what: &str, f: &dyn Fn(&Board) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !f(&hub.board.lock().unwrap()) {
                assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        send("hook", "hook_UserPromptSubmit");
        wait_for("working", &|b| b.get(7).state == State::Working);
        send("hook", "hook_PermissionRequest");
        wait_for("needs you", &|b| b.get(7).state == State::NeedsYou);
        send("hook", "hook_Stop");
        wait_for("done", &|b| b.get(7).state == State::Done);
        send("status", "status_after_response");
        wait_for("limits", &|b| b.five_hour.is_some_and(|l| l.used_pct.round() == 14.));
        drop(hub);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[cfg(windows)]
    fn settings_file_shape_windows() {
        let hub = Hub {
            board: Default::default(),
            changes: async_channel::bounded(1).1,
            dir: r"C:\T\dirigent-9".into(),
            sock: "dirigent-hook-9".into(),
            bin: r"C:\T\dirigent-9\bin".into(),
            zdotdir: r"C:\T\dirigent-9\zsh".into(),
            helper: r"C:\Program Files\Dirigent\dirigent-hook.exe".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&hub.settings_json(4)).unwrap();
        let stop = &v["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(stop["command"], "C:/Program Files/Dirigent/dirigent-hook.exe", "exec form: no shell, no quoting");
        assert_eq!(stop["args"], serde_json::json!(["dirigent-hook-9", "4", "hook"]));
        assert_eq!(stop["async"], true);
        assert_eq!(v["hooks"].as_object().unwrap().len(), 9);
        let line = hook::shell::status_command(&hook::shell::claude_shell(), "dirigent-hook-9", 4);
        assert_eq!(v["statusLine"]["command"], line.as_str());
    }

    /// Windows: the lines a new session types, typed into real PowerShell 5.1 / 7 and cmd sessions (our shell setup,
    /// the shim first on PATH), with the flags in the args file: claude gets flags that end in `\`, hold `"`, spaces
    /// and `%`, and the first prompt, intact.
    #[test]
    #[cfg(windows)]
    fn typed_lines_reach_claude_intact_in_powershell_and_cmd() {
        use crate::settings::Shell;
        let d = std::env::temp_dir().join(format!("dirigent-typed-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (shim, real) = (d.join("shim"), d.join("real"));
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::copy(helper_bin(), shim.join("claude.exe")).unwrap();
        let fake = helper_bin().parent().unwrap().join("examples").join("fake_claude.exe");
        std::fs::copy(&fake, real.join("claude.exe")).unwrap();
        let prompt = "Fix the 'login' \"bug\": $HOME %PATH% `id`\n  - C:\\x\\ & | ^ ;\n-dash";
        let flags: Vec<String> =
            ["--add-dir", r"C:\it's here\", "--name", r#"say "hi" 100%"#, "--model", "a b"].map(String::from).into();
        // A PATH where the real claude comes first: the PowerShell setup must put the shim back in front.
        let sys = std::env::var_os("PATH").unwrap();
        let path = std::env::join_paths([real.clone(), shim.clone()].into_iter().chain(std::env::split_paths(&sys)));
        let mut shells = vec![Shell::Powershell, Shell::Cmd];
        if hook::shell::find_on_path("pwsh.exe").is_some() {
            shells.push(Shell::Pwsh);
        }
        for (n, shell) in shells.into_iter().enumerate() {
            let out = d.join(format!("out-{shell:?}.txt"));
            let (prompt_file, args_file) = (d.join(format!("prompt-{n}.txt")), d.join(format!("args-{n}.json")));
            write_prompt(&prompt_file, prompt).unwrap();
            write_prompt(&args_file, &serde_json::to_string(&flags).unwrap()).unwrap();
            let mut cmd = crate::shell::command(shell);
            cmd.env("PATH", path.as_ref().unwrap());
            cmd.env("DIRIGENT_BIN", &shim);
            cmd.env("FAKE_CLAUDE_OUT", &out);
            if shell == Shell::Cmd {
                // cmd has no profile to fix PATH: the shim must come first, as the app's PATH prepend does.
                cmd.env("PATH", std::env::join_paths([shim.clone(), real.clone()]).unwrap());
            }
            let size = session::Size { cols: 200, rows: 30, cell_width: 8, cell_height: 16 };
            let s = session::Session::spawn_command(cmd, &d, size).unwrap();
            let syntax = crate::shell::syntax(shell);
            let line = crate::projects::start_line(
                syntax,
                Some("echo setup"),
                true,
                &flags,
                Some(&prompt_file),
                Some(&args_file),
            )
            .unwrap();
            s.write(format!("{line}\r").as_bytes());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !out.is_file() {
                assert!(std::time::Instant::now() < deadline, "{shell:?}: claude never ran for `{line}`");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            let text = std::fs::read_to_string(&out).unwrap();
            let mut lines = text.lines();
            let got: Vec<String> = serde_json::from_str(lines.next().unwrap()).unwrap();
            let mut want = flags.clone();
            want.extend(["--".to_string(), prompt.to_string()]);
            assert_eq!(got, want, "{shell:?}: `{line}`");
            assert_eq!(lines.next(), Some("noenv"), "{shell:?}: neither variable reaches claude");
            assert!(!prompt_file.exists() && !args_file.exists(), "{shell:?}: both files are deleted");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
