//! The `claude` shim: `dirigent-hook` started under the name `claude` (a symlink on Linux, a hardlink `claude.exe` on
//! Windows, first on the session's PATH). Runs the next `claude` on PATH with our settings (hooks + statusLine)
//! layered on top; your settings stay untouched. A first prompt from the New session dialog comes in a file
//! (`DIRIGENT_PROMPT_FILE`), so it reaches claude intact whatever the shell and its aliases are.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Kind, Msg};

pub const PROMPT_ENV: &str = "DIRIGENT_PROMPT_FILE";
/// Windows: names a file with claude's flags as a JSON array (typed lines carry no arguments there: Windows
/// PowerShell 5.1 mangles native arguments that end in `\` or contain `"`). They go before the typed ones.
pub const ARGS_ENV: &str = "DIRIGENT_ARGS_FILE";

/// Runs claude; returns the exit code (Unix: only when exec failed).
pub fn run(argv0: &OsStr, typed: Vec<OsString>) -> i32 {
    let mut args: Vec<OsString> = take_file(ARGS_ENV)
        .and_then(|text| serde_json::from_slice::<Vec<String>>(&text).ok())
        .unwrap_or_default()
        .into_iter()
        .map(OsString::from)
        .collect();
    args.extend(typed);
    let prompt = take_file(PROMPT_ENV).map(|t| String::from_utf8_lossy(&t).trim_end_matches('\n').to_string());
    if let Some(prompt) = prompt {
        args.push("--".into());
        args.push(prompt.into());
    }
    // Our own bin dir is skipped (we are `claude` there): `DIRIGENT_BIN`, else the dir we were started from (argv[0]
    // is just "claude" when the shell found us on PATH, so not `current_exe()`'s dir, which resolves our symlink).
    let own = std::env::var_os("DIRIGENT_BIN").filter(|d| !d.is_empty()).map(PathBuf::from);
    let own = own.or_else(|| Path::new(argv0).parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let Some(claude) = find_claude(&path, own.as_deref()) else {
        eprintln!("dirigent: no claude found on PATH");
        return 127;
    };
    let settings = std::env::var_os("DIRIGENT_CLAUDE_SETTINGS").map(PathBuf::from).filter(|s| s.is_file());
    if let Some(settings) = &settings {
        report(&args);
        args.splice(0..0, [OsString::from("--settings"), settings.clone().into_os_string()]);
    }
    launch(&claude, args)
}

/// The contents of the file `var` names (the prompt's trailing new lines are dropped by the caller, as `$(cat …)`
/// did); the file and the variable are gone after. A file that no longer exists (a line run again from history,
/// or a variable PowerShell / cmd kept): nothing.
fn take_file(var: &str) -> Option<Vec<u8>> {
    let file = std::env::var_os(var).filter(|f| !f.is_empty())?;
    // SAFETY: single-threaded so far; claude must not inherit the variable.
    unsafe { std::env::remove_var(var) };
    let text = std::fs::read(&file).ok()?;
    let _ = std::fs::remove_file(&file);
    Some(text)
}

/// The flags for resuming later, sent in-process (the old sh shim ran the helper's `argv` mode for this).
fn report(args: &[OsString]) {
    let (Some(socket), Some(tab)) =
        (std::env::var("DIRIGENT_SOCKET").ok(), std::env::var("DIRIGENT_TAB_ID").ok().and_then(|t| t.parse().ok()))
    else {
        return;
    };
    let mut total = 0;
    let args = args.iter().map(|a| a.to_string_lossy().into_owned()).take_while(|a| {
        total += a.len();
        total <= 8 * 1024
    });
    if let Ok(bytes) = serde_json::to_vec(&Msg { tab, kind: Kind::Argv { args: args.collect() } }) {
        crate::send(&socket, &bytes);
    }
}

/// The first claude in `path`, skipping empty entries and `own` (compared canonically). Windows: per directory
/// `claude.exe`, then the npm launchers `claude.cmd` / `claude.bat`.
/// Anything that is this very binary (another symlink to it) is skipped too, so we never run ourselves.
pub fn find_claude(path: &OsStr, own: Option<&Path>) -> Option<PathBuf> {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let own = own.map(canon);
    let me = std::env::current_exe().ok().map(|e| canon(&e));
    let names: &[&str] = if cfg!(windows) { &["claude.exe", "claude.cmd", "claude.bat"] } else { &["claude"] };
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty() && own.as_ref() != Some(&canon(d)))
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| runnable(p) && me.as_ref() != Some(&canon(p)))
}

#[cfg(unix)]
fn runnable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn runnable(p: &Path) -> bool {
    p.is_file()
}

/// Unix: replaces this process (Ctrl-C, exit codes and the terminal behave exactly as with claude itself).
#[cfg(unix)]
fn launch(claude: &Path, args: Vec<OsString>) -> i32 {
    use std::os::unix::process::CommandExt;
    let err = Command::new(claude).args(args).exec();
    eprintln!("dirigent: can't run {}: {err}", claude.display());
    126
}

/// Windows has no exec: claude runs as our child and we pass its exit code on. Ctrl-C reaches both of us through
/// the console; we survive it (a handler that says "handled") and wait for claude. Claude runs in a kill-on-close
/// Job Object, so it ends with us (a closed tab).
#[cfg(windows)]
fn launch(claude: &Path, args: Vec<OsString>) -> i32 {
    win::survive_ctrl_c();
    let target = target(claude);
    let mut cmd = match &target {
        Target::Exe(exe) => Command::new(exe),
        Target::Node { node, script } => {
            let mut c = Command::new(node);
            c.arg(script);
            c
        }
        Target::Batch => Command::new(claude),
    };
    let spawned = cmd.args(&args).spawn().or_else(|e| match target {
        // std refuses arguments cmd.exe can't quote safely (new lines, `"`, `%`): a first prompt usually.
        Target::Batch if e.kind() == std::io::ErrorKind::InvalidInput && args.iter().any(|a| a == "--") => {
            eprintln!(
                "dirigent: {} can't take this first prompt (cmd.exe quoting): paste it into claude, or install claude \
                 with the native installer",
                claude.display()
            );
            let flags = args.iter().take_while(|a| *a != "--");
            Command::new(claude).args(flags).spawn()
        }
        _ => Err(e),
    });
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            eprintln!("dirigent: can't run {}: {e}", claude.display());
            return 126;
        }
    };
    let _job = win::kill_with_us(&child);
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

/// What a `claude` found on PATH really starts.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Exe(PathBuf),
    /// An npm launcher whose script runs on node.
    Node {
        node: PathBuf,
        script: PathBuf,
    },
    /// A launcher we couldn't see through: run it as a batch file.
    Batch,
}

/// `claude.exe` as is; an npm `claude.cmd` resolved to what it launches (`"%dp0%\…\cli.js"` or an `.exe`), so
/// arguments never pass through cmd.exe's quoting.
pub fn target(claude: &Path) -> Target {
    let is_batch = claude.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !is_batch {
        return Target::Exe(claude.to_path_buf());
    }
    let dir = claude.parent().unwrap_or(Path::new("."));
    let text = std::fs::read_to_string(claude).unwrap_or_default();
    let launched = launcher_target(&text).map(|rel| dir.join(rel)).filter(|p| p.is_file());
    let default_js = dir.join(r"node_modules\@anthropic-ai\claude-code\cli.js");
    let launched = launched.or_else(|| default_js.is_file().then_some(default_js));
    match launched {
        Some(p) if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) => Target::Exe(p),
        Some(script) => {
            let local = dir.join("node.exe");
            let node = if local.is_file() { Some(local) } else { crate::shell::find_on_path("node.exe") };
            node.map_or(Target::Batch, |node| Target::Node { node, script })
        }
        None => Target::Batch,
    }
}

/// The last `"%dp0%\…"` (or `"%~dp0\…"`) path in an npm cmd-shim that isn't node itself, relative to the launcher.
pub fn launcher_target(cmd: &str) -> Option<String> {
    let launched = |rel: &&str| {
        let l = rel.to_ascii_lowercase();
        l != "node.exe" && [".js", ".mjs", ".cjs", ".exe"].iter().any(|e| l.ends_with(e))
    };
    cmd.split('"')
        .filter_map(|s| s.strip_prefix("%dp0%\\").or_else(|| s.strip_prefix("%~dp0\\")))
        .rfind(launched)
        .map(str::to_owned)
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    unsafe extern "system" fn handled(_: u32) -> i32 {
        1
    }

    /// A real handler, not `SetConsoleCtrlHandler(NULL, TRUE)`: that one is inherited and claude would ignore
    /// Ctrl-C too.
    pub fn survive_ctrl_c() {
        // SAFETY: a static function that touches nothing.
        unsafe { SetConsoleCtrlHandler(Some(handled), 1) };
    }

    pub struct Job(HANDLE);

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: our job handle; closing it kills what is still in it (claude, if we are going away first).
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Puts `child` into a job that kills it when our handle closes (we exit or get killed).
    pub fn kill_with_us(child: &std::process::Child) -> Option<Job> {
        // SAFETY: plain Win32 calls on a fresh job handle and the child's live process handle.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let job = Job(job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            ) != 0
                && AssignProcessToJobObject(job.0, child.as_raw_handle()) != 0;
            ok.then_some(job)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_launcher_targets() {
        // npm's cmd-shim for a node script (the shape npm writes).
        let node_shim = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\n\
            CALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  \
            SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || \
            title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n";
        assert_eq!(
            launcher_target(node_shim).as_deref(),
            Some(r"node_modules\@anthropic-ai\claude-code\cli.js"),
            "the script, never node.exe"
        );
        let exe_shim = "@ECHO off\r\n\"%~dp0\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe\" %*\r\n";
        assert_eq!(
            launcher_target(exe_shim).as_deref(),
            Some(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe")
        );
        assert_eq!(launcher_target("@echo off\r\nclaude-real %*\r\n"), None);
    }
}
