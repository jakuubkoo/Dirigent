//! A reordered sidebar survives closing the window right away (before the 500 ms save throttle fires).
//! Opens a real window, so it is ignored by default: `cargo test -p dirigent --test reorder_close -- --ignored`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn names(state: &Path) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(state).unwrap()).unwrap();
    v["tabs"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[test]
#[ignore = "opens a window on the current display"]
fn reorder_is_saved_when_the_window_closes_at_once() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("reorder-close-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // One group (all three in the same directory), the first one active.
    let cwd = dir.display().to_string();
    let tab = |name: &str| serde_json::json!({ "name": name, "cwd": cwd });
    let state = dir.join("state.json");
    let saved = serde_json::json!({
        "version": 1,
        "tabs": [tab("a"), tab("b"), tab("c")],
        "active": 0,
        "sidebar": { "width": 220, "open": true },
        "window": { "mode": "windowed", "x": 0, "y": 0, "w": 800, "h": 600 },
    });
    std::fs::write(&state, saved.to_string()).unwrap();

    // The harness moves the group's last session to its top after 1 s, then closes the window 100 ms later.
    let mut child = Command::new(env!("CARGO_BIN_EXE_dirigent"))
        .env("DIRIGENT_STATE", &state)
        .env("DIRIGENT_PERF_REORDER", "100")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "dirigent did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(names(&state), ["c", "a", "b"]);
    let _ = std::fs::remove_dir_all(&dir);
}
