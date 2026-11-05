//! Claude Code integration: per-run runtime dir (socket, `claude` shim, zsh wrappers, per-tab settings),
//! a listener thread receiving `agent-ide-hook` datagrams, and the per-tab agent state they drive.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
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
            Kind::Hook { event, notification_type, session_id, cwd, message, .. } => {
                agent.last_activity = Some(now_secs());
                if let Some(cwd) = cwd {
                    agent.cwd = Some(cwd.into());
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
                    // Esc/Ctrl-C interrupts send no Stop; idle_prompt (~60 s later) is the only signal.
                    ("Notification", Some("idle_prompt")) if agent.state == State::Working => Some(State::Done),
                    ("SessionEnd", _) => {
                        // The session stays resumable: keep its id and the claude args.
                        let failed = (!agent.started).then_some(session_id).flatten();
                        *agent = Agent {
                            session_id: agent.session_id.take(),
                            args: agent.args.take(),
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
            Kind::Status { model, context_pct, five_hour, seven_day } => {
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
        *self.tabs.get(&msg.tab).unwrap() != before || self.five_hour != five || self.seven_day != seven
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
        let (bin, zdotdir) = (dir.join("bin"), dir.join("zsh"));
        std::fs::create_dir_all(&bin)?;
        std::fs::create_dir_all(&zdotdir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        write_exec(&bin.join("claude"), SHIM)?;
        for (name, body) in ZSH_WRAPPERS {
            std::fs::write(zdotdir.join(name), body)?;
        }
        let helper = std::env::current_exe()?.with_file_name("agent-ide-hook");

        let sock = dir.join("sock");
        let socket = UnixDatagram::bind(&sock)?;
        let board = Arc::new(Mutex::new(Board::default()));
        let (tx, changes) = async_channel::bounded(1);
        let board2 = board.clone();
        std::thread::Builder::new().name("agent-listener".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = socket.recv(&mut buf) {
                let Ok(msg) = serde_json::from_slice::<Msg>(&buf[..n]) else { continue };
                crate::perf::event_with(|| format!("agent msg {msg:?}"));
                tracker.hook(msg.tab, &msg.kind);
                if board2.lock().unwrap().apply(msg) && tx.try_send(()).is_err() && tx.is_closed() {
                    return;
                }
            }
        })?;
        Ok(Self { board, changes, dir, sock, bin, zdotdir, helper })
    }

    /// Writes the tab's settings file and returns the env for its shell.
    pub fn session_env(&self, tab: u32) -> std::io::Result<Vec<(String, String)>> {
        let settings = self.dir.join(format!("tab-{tab}.json"));
        std::fs::write(&settings, self.settings_json(tab))?;
        let path = std::env::var("PATH").unwrap_or_default();
        let mut env = vec![
            ("AGENT_IDE_TAB_ID".into(), tab.to_string()),
            ("AGENT_IDE_SOCKET".into(), self.sock.display().to_string()),
            ("AGENT_IDE_SETTINGS".into(), settings.display().to_string()),
            ("AGENT_IDE_BIN".into(), self.bin.display().to_string()),
            ("AGENT_IDE_HOOK".into(), self.helper.display().to_string()),
            ("PATH".into(), format!("{}:{path}", self.bin.display())),
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
            env.push(("AGENT_IDE_ZDOTDIR".into(), self.zdotdir.display().to_string()));
            env.push(("AGENT_IDE_ZDOTDIR_ORIG".into(), orig));
            env.push(("AGENT_IDE_ZDOTDIR_WAS_UNSET".into(), unset.into()));
            env.push(("ZDOTDIR".into(), self.zdotdir.display().to_string()));
        }
        Ok(env)
    }

    fn settings_json(&self, tab: u32) -> String {
        let cmd = |mode: &str| format!("{} {} {tab} {mode}", sh_quote(&self.helper), sh_quote(&self.sock));
        let handler = serde_json::json!([{ "hooks": [{ "type": "command", "command": cmd("hook"), "async": true }] }]);
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
        serde_json::json!({
            "hooks": hooks,
            "statusLine": { "type": "command", "command": cmd("status") },
        })
        .to_string()
    }

    pub fn remove_tab(&self, tab: u32) {
        self.board.lock().unwrap().tabs.remove(&tab);
        let _ = std::fs::remove_file(self.dir.join(format!("tab-{tab}.json")));
    }
}

/// This instance's runtime dir (`$XDG_RUNTIME_DIR/agent-ide-<pid>`), removed on quit.
pub fn runtime_dir() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    runtime.join(format!("agent-ide-{}", std::process::id()))
}

/// Removes runtime dirs left behind by app instances that are no longer running (crash, kill -9).
fn remove_stale_dirs(runtime: &Path) {
    let Ok(entries) = std::fs::read_dir(runtime) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("agent-ide-")) else { continue };
        if pid.parse::<u32>().is_ok() && !Path::new("/proc").join(pid).exists() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn write_exec(path: &Path, body: &str) -> std::io::Result<()> {
    std::fs::write(path, body)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// `claude` shim: runs the next `claude` on PATH with our settings (hooks + statusLine) layered on top.
const SHIM: &str = r#"#!/bin/sh
# agent-ide: run the real claude with agent-ide hooks + statusLine (--settings). Your settings stay untouched.
self_dir=$(dirname "$0")
IFS=:
for d in $PATH; do
  [ -z "$d" ] || [ "$d" = "$self_dir" ] && continue
  if [ -x "$d/claude" ] && [ ! -d "$d/claude" ]; then
    if [ -f "$AGENT_IDE_SETTINGS" ]; then
      # Remember the flags for resuming later (fire-and-forget, never reads the terminal).
      [ -x "$AGENT_IDE_HOOK" ] && "$AGENT_IDE_HOOK" "$AGENT_IDE_SOCKET" "$AGENT_IDE_TAB_ID" argv "$@" </dev/null
      exec "$d/claude" --settings "$AGENT_IDE_SETTINGS" "$@"
    fi
    exec "$d/claude" "$@"
  fi
done
echo "agent-ide: no claude found on PATH" >&2
exit 127
"#;

/// zsh startup wrappers: each sources the user's own file, then (rc/login) puts the shim dir first in `path` and
/// restores ZDOTDIR, so the user's config and child shells behave exactly as without agent-ide.
const ZSH_WRAPPERS: [(&str, &str); 4] = [
    (".zshenv", ZSHENV),
    (".zprofile", ZPROFILE),
    (".zshrc", ZSHRC),
    (".zlogin", ZLOGIN),
];

const ZSHENV: &str = r#"# agent-ide wrapper
ZDOTDIR=$AGENT_IDE_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zshenv ]] && builtin source $ZDOTDIR/.zshenv
AGENT_IDE_ZDOTDIR_ORIG=$ZDOTDIR
ZDOTDIR=$AGENT_IDE_ZDOTDIR
"#;

const ZPROFILE: &str = r#"# agent-ide wrapper
ZDOTDIR=$AGENT_IDE_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zprofile ]] && builtin source $ZDOTDIR/.zprofile
ZDOTDIR=$AGENT_IDE_ZDOTDIR
"#;

const ZSHRC: &str = r#"# agent-ide wrapper
ZDOTDIR=$AGENT_IDE_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zshrc ]] && builtin source $ZDOTDIR/.zshrc
path=($AGENT_IDE_BIN ${path:#$AGENT_IDE_BIN})
# Report the cwd at every prompt (OSC 7) so the app follows `cd`; `%` is escaped for the file:// URL.
__agent_ide_osc7() { builtin print -rn -- $'\e]7;file://'"${HOST}${PWD//\%/%25}"$'\e\\' }
typeset -ga precmd_functions
precmd_functions=(${precmd_functions:#__agent_ide_osc7} __agent_ide_osc7)
if [[ -o login ]]; then
  ZDOTDIR=$AGENT_IDE_ZDOTDIR
elif [[ -n $AGENT_IDE_ZDOTDIR_WAS_UNSET && $AGENT_IDE_ZDOTDIR_ORIG == $HOME ]]; then
  unset ZDOTDIR
fi
"#;

const ZLOGIN: &str = r#"# agent-ide wrapper
ZDOTDIR=$AGENT_IDE_ZDOTDIR_ORIG
[[ -f $ZDOTDIR/.zlogin ]] && builtin source $ZDOTDIR/.zlogin
path=($AGENT_IDE_BIN ${path:#$AGENT_IDE_BIN})
if [[ -n $AGENT_IDE_ZDOTDIR_WAS_UNSET && $AGENT_IDE_ZDOTDIR_ORIG == $HOME ]]; then
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
        Msg { tab, kind: Kind::Status { model: Some("Opus 5.5".into()), context_pct: ctx, five_hour: five, seven_day: None } }
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
    fn session_ids_and_failed_resume() {
        let mut b = Board::default();
        let argv = |args: &[&str]| Msg { tab: 1, kind: Kind::Argv { args: args.iter().map(|s| s.to_string()).collect() } };
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

    #[test]
    fn settings_file_shape() {
        let hub = Hub {
            board: Default::default(),
            changes: async_channel::bounded(1).1,
            dir: "/run/x".into(),
            sock: "/run/x/sock".into(),
            bin: "/run/x/bin".into(),
            zdotdir: "/run/x/zsh".into(),
            helper: "/opt/it's/agent-ide-hook".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&hub.settings_json(4)).unwrap();
        assert_eq!(v["statusLine"]["command"], r#"'/opt/it'\''s/agent-ide-hook' '/run/x/sock' 4 status"#);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["async"], true);
        assert_eq!(v["hooks"].as_object().unwrap().len(), 9);
    }

    #[test]
    fn shim_runs_real_claude_with_settings_and_skips_itself() {
        let d = std::env::temp_dir().join(format!("agent-ide-shim-test-{}", std::process::id()));
        let (shim_dir, real_dir) = (d.join("shim"), d.join("real"));
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        write_exec(&shim_dir.join("claude"), SHIM).unwrap();
        write_exec(&real_dir.join("claude"), "#!/bin/sh\necho real \"$@\"\n").unwrap();
        // Stand-in for agent-ide-hook: records what the shim reports.
        let reported = d.join("argv.txt");
        let fake_hook = d.join("hook.sh");
        write_exec(&fake_hook, &format!("#!/bin/sh\nshift 3; printf '%s|' \"$@\" > {}\n", reported.display())).unwrap();
        let settings = d.join("s.json");
        std::fs::write(&settings, "{}").unwrap();
        let run = |settings: &Path| {
            let out = std::process::Command::new(shim_dir.join("claude"))
                .arg("--model")
                .arg("a b")
                .env("PATH", format!("{}:{}:/usr/bin:/bin", shim_dir.display(), real_dir.display()))
                .env("AGENT_IDE_SETTINGS", settings)
                .env("AGENT_IDE_HOOK", &fake_hook)
                .env("AGENT_IDE_SOCKET", "/nowhere")
                .env("AGENT_IDE_TAB_ID", "3")
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(run(&settings), format!("real --settings {} --model a b\n", settings.display()));
        assert_eq!(std::fs::read_to_string(&reported).unwrap(), "--model|a b|", "argv reported before exec");
        // Settings gone (app quit): plain claude, never a broken --settings.
        assert_eq!(run(&d.join("missing.json")), "real --model a b\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn zsh_wrapper_reports_cwd_after_users_rc() {
        if !Path::new("/usr/bin/zsh").exists() && !Path::new("/bin/zsh").exists() {
            return;
        }
        let d = std::env::temp_dir().join(format!("agent-ide-zsh-test-{}", std::process::id()));
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
            ("AGENT_IDE_ZDOTDIR", wrappers.to_str().unwrap()),
            ("AGENT_IDE_ZDOTDIR_ORIG", user.to_str().unwrap()),
            ("AGENT_IDE_BIN", "/nonexistent"),
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
}
