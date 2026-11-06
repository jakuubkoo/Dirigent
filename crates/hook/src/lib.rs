//! Message format between `dirigent-hook` (run by Claude Code) and the app: one small JSON datagram per call.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod notice;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Msg {
    pub tab: u32,
    pub kind: Kind,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Kind {
    /// `cwd`: Claude Code's working directory (every payload has it). `message`: what a desktop notification
    /// says (PermissionRequest: the tool, and the path for file tools; Notification: its message), short.
    Hook {
        event: String,
        notification_type: Option<String>,
        session_id: Option<String>,
        cwd: Option<String>,
        #[serde(default)]
        message: Option<String>,
        /// Pre-/PostToolUse: the tool and its call id (pairs a Pre with its Post, also for parallel calls).
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        tool_use_id: Option<String>,
        /// PostToolUse of a tool that writes files: the absolute path it touched.
        #[serde(default)]
        path: Option<String>,
    },
    /// The arguments `claude` was started with in this tab (sent by the shim before it execs claude).
    Argv {
        args: Vec<String>,
    },
    Status {
        model: Option<String>,
        context_pct: Option<f32>,
        five_hour: Option<Limit>,
        seven_day: Option<Limit>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    pub used_pct: f32,
    /// Unix epoch seconds.
    pub resets_at: i64,
}

/// Tools whose path field is only read: they never mark a file as changed by the agent.
const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep", "LS", "NotebookRead", "WebFetch", "WebSearch", "TodoWrite"];

/// Extracts the few fields the app needs from a hook (`mode == "hook"`) or statusLine (`"status"`) payload.
pub fn parse(mode: &str, json: &str) -> Option<Kind> {
    let v: Value = serde_json::from_str(json).ok()?;
    let s = |v: &Value| v.as_str().map(str::to_owned);
    match mode {
        "hook" => {
            let event = s(&v["hook_event_name"])?;
            let message = match event.as_str() {
                "PermissionRequest" => s(&v["tool_name"]).map(|t| notice::permission_text(&t, &v["tool_input"])),
                "Notification" => v["message"].as_str().map(notice::message_text),
                _ => None,
            };
            let tool = s(&v["tool_name"]).filter(|_| event.ends_with("ToolUse"));
            let path =
                tool.as_deref().filter(|t| event == "PostToolUse" && !READ_ONLY_TOOLS.contains(t)).and_then(|_| {
                    let input = &v["tool_input"];
                    let p = ["file_path", "notebook_path", "path"]
                        .iter()
                        .find_map(|k| input[*k].as_str())
                        .filter(|p| !p.is_empty())?;
                    let p = std::path::Path::new(p);
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        std::path::Path::new(v["cwd"].as_str()?).join(p)
                    };
                    abs.to_str().map(str::to_owned)
                });
            Some(Kind::Hook {
                event,
                notification_type: s(&v["notification_type"]),
                session_id: s(&v["session_id"]),
                cwd: s(&v["cwd"]),
                message,
                tool,
                tool_use_id: s(&v["tool_use_id"]),
                path,
            })
        }
        "status" => {
            let limit = |w: &Value| {
                Some(Limit { used_pct: w["used_percentage"].as_f64()? as f32, resets_at: w["resets_at"].as_i64()? })
            };
            Some(Kind::Status {
                model: s(&v["model"]["display_name"]).or_else(|| s(&v["model"]["id"])),
                context_pct: v["context_window"]["used_percentage"].as_f64().map(|p| p as f32),
                five_hour: limit(&v["rate_limits"]["five_hour"]),
                seven_day: limit(&v["rate_limits"]["seven_day"]),
            })
        }
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Arity {
    Flag,
    /// `<value>`
    One,
    /// `<values...>`: every following arg up to the next one starting with `-` (as claude parses it).
    Many,
}

/// The only flags replayed on resume: verified in `claude --help` (2.1.318) to be valid in an interactive session
/// and safe to repeat. Everything else is dropped — print-mode flags, session-starting flags, permission grants
/// and bypasses, flags that disable our hooks (--bare, --safe-mode), and any flag Claude adds in the future.
const RESUME_ALLOWLIST: &[(&str, Arity)] = &[
    ("--model", Arity::One),
    ("-n", Arity::One),
    ("--name", Arity::One),
    ("--permission-mode", Arity::One), // except bypassPermissions, see below
    ("--effort", Arity::One),
    ("--agent", Arity::One),
    ("--agents", Arity::One),
    ("--autocompact", Arity::One),
    ("--plugin-dir", Arity::One),
    ("--add-dir", Arity::Many),
    ("--mcp-config", Arity::Many),
    ("--tools", Arity::Many),
    ("--disallowed-tools", Arity::Many),
    ("--disallowedTools", Arity::Many),
    ("--verbose", Arity::Flag),
    ("--strict-mcp-config", Arity::Flag),
    ("--ide", Arity::Flag),
    ("--chrome", Arity::Flag),
    ("--no-chrome", Arity::Flag),
    ("--disable-slash-commands", Arity::Flag),
    ("--brief", Arity::Flag),
];

/// The flags to repeat after `claude --resume <id>`: only allowlisted ones (with their values), never a
/// permission bypass. Positional args (the prompt) and every other flag are dropped; a dropped flag's value is
/// a non-flag token, so it is dropped with it.
pub fn resume_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" {
            break; // everything after is positional
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (arg.as_str(), None),
        };
        let Some(&(_, arity)) = RESUME_ALLOWLIST.iter().find(|(f, _)| *f == name) else { continue };
        let mut group = vec![arg.clone()];
        if inline.is_none() {
            match arity {
                Arity::Flag => {}
                Arity::One => {
                    if let Some(v) = args.get(i) {
                        group.push(v.clone());
                        i += 1;
                    }
                }
                Arity::Many => {
                    while let Some(v) = args.get(i).filter(|v| !v.starts_with('-')) {
                        group.push(v.clone());
                        i += 1;
                    }
                }
            }
        }
        let value = inline.or(group.get(1).map(String::as_str));
        let bypass = name == "--permission-mode" && value == Some("bypassPermissions");
        let complete = arity == Arity::Flag || group.len() > 1 || inline.is_some();
        if !bypass && complete {
            out.extend(group);
        }
    }
    out
}

/// Splits a typed flag line into words like a shell would for plain words: whitespace separates, `'…'` and `"…"`
/// group (no expansion), `\` escapes the next char. An unterminated quote takes the rest of the line.
pub fn split_words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => out.extend(word.take()),
            '\'' | '"' => {
                let w = word.get_or_insert_with(String::new);
                for q in chars.by_ref() {
                    if q == c {
                        break;
                    }
                    w.push(q);
                }
            }
            '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    out.extend(word);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_words_like_a_shell() {
        assert_eq!(
            split_words("  --model opus --add-dir '/a b' \"c\"d\\ e ''"),
            ["--model", "opus", "--add-dir", "/a b", "cd e", ""]
        );
        assert!(split_words("   ").is_empty());
        // Typed defaults go through the resume allowlist: bypasses never survive.
        let words = split_words(
            "--model opus --dangerously-skip-permissions --permission-mode bypassPermissions --permission-mode=bypassPermissions --foo x --effort high",
        );
        assert_eq!(resume_args(&words), ["--model", "opus", "--effort", "high"]);
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn hooks_from_real_claude() {
        for e in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
            let Some(Kind::Hook { event, session_id, cwd, .. }) = parse("hook", &fixture(&format!("hook_{e}"))) else {
                panic!("{e}")
            };
            assert_eq!(event, e);
            assert_eq!(cwd.as_deref(), Some("/home/user/project"));
            assert_eq!(session_id.as_deref(), Some("00000000-0000-4000-8000-000000000003"));
        }
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resume_flag_filtering() {
        // The prompt goes, the model stays.
        assert_eq!(resume_args(&v(&["--model", "haiku", "fix it"])), v(&["--model", "haiku"]));
        // A value that looks like a prompt after a value flag is that flag's value.
        assert_eq!(
            resume_args(&v(&["-n", "my session", "--effort", "high"])),
            v(&["-n", "my session", "--effort", "high"])
        );
        // Session-starting flags are dropped, with their values.
        assert_eq!(
            resume_args(&v(&["-c", "-r", "abc", "--resume=def", "--session-id", "x", "--fork-session", "--verbose"])),
            v(&["--verbose"])
        );
        assert_eq!(resume_args(&v(&["-r", "--model", "opus"])), v(&["--model", "opus"]), "-r without value");
        assert_eq!(resume_args(&v(&["--worktree", "feat", "--permission-mode=plan"])), v(&["--permission-mode=plan"]));
        // Variadic flags keep all their values; positional after `--` is dropped.
        assert_eq!(
            resume_args(&v(&["--add-dir", "../a", "../b", "--verbose", "--", "prompt"])),
            v(&["--add-dir", "../a", "../b", "--verbose"])
        );
        // Not allowlisted: permission grants, settings, hook-disabling and debug flags.
        assert_eq!(
            resume_args(&v(&[
                "--allowed-tools",
                "Bash",
                "--settings",
                "x.json",
                "--bare",
                "--safe-mode",
                "-d",
                "hooks"
            ])),
            v(&[])
        );
        // A value flag at the very end without its value is not replayed half.
        assert_eq!(resume_args(&v(&["--verbose", "--model"])), v(&["--verbose"]));
        assert!(resume_args(&[]).is_empty());
    }

    #[test]
    fn print_mode_flags_are_dropped() {
        let args = v(&[
            "-p",
            "--print",
            "--output-format",
            "json",
            "--output-format=stream-json",
            "--input-format",
            "text",
            "--json-schema",
            "{}",
            "--include-partial-messages",
            "--include-hook-events",
            "--replay-user-messages",
            "--max-budget-usd",
            "1",
            "--no-session-persistence",
            "--fallback-model",
            "sonnet",
            "--model",
            "haiku",
            "reply ok",
        ]);
        assert_eq!(resume_args(&args), v(&["--model", "haiku"]));
    }

    #[test]
    fn unknown_flags_are_dropped() {
        // Flags Claude may add later: dropped, and so is their value (a non-flag token).
        let args = v(&["--future-flag", "value", "--future-bool", "--future-kv=1", "--model", "opus", "-x"]);
        assert_eq!(resume_args(&args), v(&["--model", "opus"]));
    }

    #[test]
    fn permission_bypass_is_never_replayed() {
        let args = v(&[
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "bypassPermissions",
            "--permission-mode=bypassPermissions",
            "--model",
            "opus",
        ]);
        assert_eq!(resume_args(&args), v(&["--model", "opus"]));
        // Other permission modes are kept, in both forms.
        assert_eq!(
            resume_args(&v(&["--permission-mode", "plan", "--permission-mode=acceptEdits"])),
            v(&["--permission-mode", "plan", "--permission-mode=acceptEdits"])
        );
    }

    /// Captured from claude 2.1.318 (interactive, default permission mode): the PermissionRequest names the tool
    /// and its input; the Notification that follows ~6 s later only has a generic message.
    #[test]
    fn permission_hooks_from_real_claude() {
        let Some(Kind::Hook { event, message, .. }) = parse("hook", &fixture("hook_PermissionRequest")) else {
            panic!()
        };
        assert_eq!(event, "PermissionRequest");
        assert_eq!(message.as_deref(), Some("Wants to run a command"), "the command itself is never shown");
        let want = Kind::Hook {
            event: "Notification".into(),
            notification_type: Some("permission_prompt".into()),
            session_id: Some("00000000-0000-4000-8000-000000000001".into()),
            cwd: Some("/home/user/project".into()),
            message: Some("Claude needs your permission".into()),
            tool: None,
            tool_use_id: None,
            path: None,
        };
        assert_eq!(parse("hook", &fixture("hook_Notification")), Some(want));
    }

    /// Captured from claude 2.1.318 in a scratch repo: file tools carry an absolute path, a subagent's edit has
    /// the parent's session id, Bash's Pre and Post share the call id, Read records nothing.
    #[test]
    fn tool_hooks_from_real_claude() {
        let get = |name: &str| match parse("hook", &fixture(name)) {
            Some(Kind::Hook { event, session_id, tool, tool_use_id, path, .. }) => {
                (event, session_id, tool, tool_use_id, path)
            }
            other => panic!("{name}: {other:?}"),
        };
        let repo = "/tmp/scratch/capture2/repo";
        for (name, file) in [
            ("hook_PostToolUse_Write", "new.txt"),
            ("hook_PostToolUse_Edit", "a.txt"),
            ("hook_PostToolUse_NotebookEdit", "nb.ipynb"),
            ("hook_PostToolUse_Edit_subagent", "a.txt"),
        ] {
            let (event, session, _, id, path) = get(name);
            assert_eq!(event, "PostToolUse");
            assert_eq!(
                session.as_deref(),
                Some("00000000-0000-4000-8000-000000000002"),
                "{name}: one session, subagents too"
            );
            assert!(id.is_some());
            assert_eq!(path, Some(format!("{repo}/{file}")), "{name}");
        }
        assert_eq!(get("hook_PostToolUse_Read").4, None, "a read changes nothing");
        let (pre, post) = (get("hook_PreToolUse_Bash"), get("hook_PostToolUse_Bash"));
        assert_eq!((pre.2.as_deref(), pre.4.as_deref()), (Some("Bash"), None));
        assert_eq!(pre.3, post.3, "Pre and Post pair by tool_use_id");
        assert_eq!(post.4, None, "Bash has no path");
    }

    #[test]
    fn relative_paths_join_the_cwd() {
        let json = r#"{"hook_event_name":"PostToolUse","cwd":"/r","tool_name":"Write","tool_use_id":"t","tool_input":{"file_path":"src/a.rs"}}"#;
        assert!(matches!(parse("hook", json), Some(Kind::Hook { path: Some(p), .. }) if p == "/r/src/a.rs"));
        let empty = r#"{"hook_event_name":"PostToolUse","cwd":"/r","tool_name":"Bash","tool_input":{"file_path":""}}"#;
        assert!(matches!(parse("hook", empty), Some(Kind::Hook { path: None, .. })), "an empty path is no path");
    }

    #[test]
    fn status_from_real_claude() {
        let first = parse("status", &fixture("status_first")).unwrap();
        assert_eq!(
            first,
            Kind::Status { model: Some("Haiku 5.5".into()), context_pct: None, five_hour: None, seven_day: None }
        );
        let after = parse("status", &fixture("status_after_response")).unwrap();
        assert_eq!(
            after,
            Kind::Status {
                model: Some("Haiku 5.5".into()),
                context_pct: Some(6.),
                five_hour: Some(Limit { used_pct: 14., resets_at: 1793836800 }),
                seven_day: Some(Limit { used_pct: 23., resets_at: 1793941200 }),
            }
        );
    }

    #[test]
    fn garbage_is_ignored() {
        assert_eq!(parse("hook", "not json"), None);
        assert_eq!(parse("hook", "{}"), None);
        assert_eq!(parse("other", "{}"), None);
    }

    #[test]
    fn wire_roundtrip() {
        let m = Msg { tab: 3, kind: Kind::Argv { args: v(&["--model", "a b"]) } };
        let back: Msg = serde_json::from_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }
}
