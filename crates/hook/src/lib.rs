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
    Hook { event: String, notification_type: Option<String> },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn hook(event: &str, nt: Option<&str>) -> Kind {
        Kind::Hook { event: event.into(), notification_type: nt.map(Into::into) }
    }

    #[test]
    fn hooks_from_real_claude() {
        for e in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
            assert_eq!(parse("hook", &fixture(&format!("hook_{e}"))), Some(hook(e, None)));
        }
    }

    #[test]
    fn notification_from_docs() {
        // No captured fixture (auto mode never prompted); shape from code.claude.com/docs/en/hooks.
        let json = r#"{"session_id":"x","hook_event_name":"Notification","message":"Claude needs your permission",
            "notification_type":"permission_prompt"}"#;
        assert_eq!(parse("hook", json), Some(hook("Notification", Some("permission_prompt"))));
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
        let m = Msg { tab: 3, kind: hook("Stop", None) };
        let back: Msg = serde_json::from_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }
}
