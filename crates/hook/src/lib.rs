//! Message format between `agent-ide-hook` (run by Claude Code) and the app: one small JSON datagram per call.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Msg {
    pub tab: u32,
    pub kind: Kind,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Kind {
    Hook { event: String, notification_type: Option<String>, session_id: Option<String> },
    /// The arguments `claude` was started with in this tab (sent by the shim before it execs claude).
    Argv { args: Vec<String> },
    Status { model: Option<String>, context_pct: Option<f32>, five_hour: Option<Limit>, seven_day: Option<Limit> },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    pub used_pct: f32,
    /// Unix epoch seconds.
    pub resets_at: i64,
}

/// Extracts the few fields the app needs from a hook (`mode == "hook"`) or statusLine (`"status"`) payload.
pub fn parse(mode: &str, json: &str) -> Option<Kind> {
    let v: Value = serde_json::from_str(json).ok()?;
    let s = |v: &Value| v.as_str().map(str::to_owned);
    match mode {
        "hook" => Some(Kind::Hook {
            event: s(&v["hook_event_name"])?,
            notification_type: s(&v["notification_type"]),
            session_id: s(&v["session_id"]),
        }),
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

/// Flags of claude 2.1.318 (`claude --help`) that take a value, so a value isn't mistaken for the prompt.
/// ponytail: refresh when Claude adds flags; an unknown value-taking flag's value would be dropped as a prompt.
const VALUE_FLAGS: &[&str] = &[
    "--agent", "--agents", "--append-system-prompt", "--append-system-prompt-file", "--autocompact", "--debug-file",
    "--effort", "--environment", "--fallback-model", "--input-format", "--json-schema", "--max-budget-usd",
    "--model", "-n", "--name", "--output-format", "--permission-mode", "--permission-prompts", "--plugin-dir",
    "--plugin-url", "--remote-control-session-name-prefix", "--session-id", "--setting-sources", "--settings",
    "--system-prompt", "--system-prompt-file", "--system-prompt-snapshot",
];
/// Variadic flags (`<x...>`): take every following arg up to the next one starting with `-`.
const VARIADIC_FLAGS: &[&str] = &[
    "--add-dir", "--allowedTools", "--allowed-tools", "--betas", "--disallowedTools", "--disallowed-tools", "--file",
    "--mcp-config", "--tools",
];
/// Optional-value flags (`[x]`): take the next arg only if it doesn't start with `-`.
const OPTIONAL_VALUE_FLAGS: &[&str] = &[
    "-r", "--resume", "-d", "--debug", "-w", "--worktree", "--from-pr", "--teleport", "--cloud", "--remote-control",
    "--prompt-suggestions",
];
/// Flags never replayed on resume: they start another session, print and exit, or bypass permission checks (a
/// bypass must always be the user's explicit choice for that run, never something we type for them).
const DROP_ON_RESUME: &[&str] = &[
    "-p", "--print", "-c", "--continue", "-r", "--resume", "--session-id", "--fork-session", "-w", "--worktree",
    "--from-pr", "--teleport", "--cloud", "-h", "--help", "-v", "--version",
    "--dangerously-skip-permissions", "--allow-dangerously-skip-permissions",
];

/// The flags to repeat after `claude --resume <id>`: everything the user started claude with, minus the prompt
/// (positional args) and the flags in [`DROP_ON_RESUME`]. Mirrors claude's own option parsing for values.
pub fn resume_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" {
            break; // everything after is positional
        }
        if !arg.starts_with('-') || arg == "-" {
            continue; // positional: the prompt
        }
        let (name, inline_value) = match arg.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (arg.as_str(), false),
        };
        let mut group = vec![arg.clone()];
        if !inline_value {
            if VALUE_FLAGS.contains(&name) {
                if let Some(v) = args.get(i) {
                    group.push(v.clone());
                    i += 1;
                }
            } else if VARIADIC_FLAGS.contains(&name) {
                while let Some(v) = args.get(i).filter(|v| !v.starts_with('-')) {
                    group.push(v.clone());
                    i += 1;
                }
            } else if OPTIONAL_VALUE_FLAGS.contains(&name)
                && let Some(v) = args.get(i).filter(|v| !v.starts_with('-'))
            {
                group.push(v.clone());
                i += 1;
            }
        }
        let value = arg.split_once('=').map(|(_, v)| v).or(group.get(1).map(String::as_str));
        let bypass = name == "--permission-mode" && value == Some("bypassPermissions");
        if !DROP_ON_RESUME.contains(&name) && !bypass {
            out.extend(group);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn hooks_from_real_claude() {
        for e in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
            let Some(Kind::Hook { event, session_id, .. }) = parse("hook", &fixture(&format!("hook_{e}"))) else {
                panic!("{e}")
            };
            assert_eq!(event, e);
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
        assert_eq!(resume_args(&v(&["-n", "my session", "--effort", "high"])), v(&["-n", "my session", "--effort", "high"]));
        // Session-starting / print flags are dropped, with their values.
        assert_eq!(
            resume_args(&v(&["-p", "-c", "-r", "abc", "--resume=def", "--session-id", "x", "--fork-session", "--verbose"])),
            v(&["--verbose"])
        );
        assert_eq!(resume_args(&v(&["-r", "--model", "opus"])), v(&["--model", "opus"]), "-r without value");
        assert_eq!(resume_args(&v(&["--worktree", "feat", "--permission-mode=plan"])), v(&["--permission-mode=plan"]));
        // Variadic flags keep all their values; positional after `--` is dropped.
        assert_eq!(
            resume_args(&v(&["--add-dir", "../a", "../b", "--verbose", "--", "prompt"])),
            v(&["--add-dir", "../a", "../b", "--verbose"])
        );
        assert_eq!(resume_args(&v(&["-d", "hooks", "hello"])), v(&["-d", "hooks"]), "optional value taken like claude does");
        assert!(resume_args(&[]).is_empty());
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

    #[test]
    fn notification_from_docs() {
        // No captured fixture (auto mode never prompted); shape from code.claude.com/docs/en/hooks.
        let json = r#"{"session_id":"x","hook_event_name":"Notification","message":"Claude needs your permission",
            "notification_type":"permission_prompt"}"#;
        let want = Kind::Hook {
            event: "Notification".into(),
            notification_type: Some("permission_prompt".into()),
            session_id: Some("x".into()),
        };
        assert_eq!(parse("hook", json), Some(want));
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
