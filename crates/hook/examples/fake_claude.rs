//! A stand-in `claude` for the shim tests (built by `cargo test`, never shipped). Writes its arguments (JSON) and
//! whether `DIRIGENT_PROMPT_FILE` / `DIRIGENT_ARGS_FILE` reached it to `$FAKE_CLAUDE_OUT`, then exits with
//! `$FAKE_CLAUDE_EXIT`.
//! `FAKE_CLAUDE_CTRL_C=1` (Windows): waits up to 30 s for a Ctrl-C, writes `ctrl-c` and exits 42.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = std::env::var_os("FAKE_CLAUDE_OUT").expect("FAKE_CLAUDE_OUT");
    let prompt_env = ["DIRIGENT_PROMPT_FILE", "DIRIGENT_ARGS_FILE"].iter().any(|v| std::env::var_os(v).is_some());
    let report = format!("{}\n{}\n", serde_json::to_string(&args).unwrap(), if prompt_env { "env" } else { "noenv" });
    std::fs::write(&out, report).unwrap();
    #[cfg(windows)]
    if std::env::var_os("FAKE_CLAUDE_CTRL_C").is_some() {
        ctrl_c::wait(&out);
    }
    std::process::exit(std::env::var("FAKE_CLAUDE_EXIT").ok().and_then(|c| c.parse().ok()).unwrap_or(0));
}

#[cfg(windows)]
mod ctrl_c {
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    static GOT: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(_: u32) -> i32 {
        GOT.store(true, Ordering::SeqCst);
        1
    }

    pub fn wait(out: &std::ffi::OsStr) {
        // SAFETY: a static handler that only sets a flag.
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
        let mut report = std::fs::read_to_string(out).unwrap();
        report.push_str("ready\n");
        std::fs::write(out, &report).unwrap();
        for _ in 0..300 {
            if GOT.load(Ordering::SeqCst) {
                report.push_str("ctrl-c\n");
                std::fs::write(out, report).unwrap();
                std::process::exit(42);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::process::exit(3);
    }
}
