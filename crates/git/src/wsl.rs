//! Paths and commands inside a WSL distro (Windows). The app's form of a path in a distro is
//! `\\wsl.localhost\<distro>\<linux path>`: a real Windows path (std::fs reads work over WSL's 9P share), turned
//! into `(distro, /linux/path)` wherever Linux tools need it. Git and gh for such a repo run inside the distro
//! (`wsl.exe -d <distro> --cd <dir> -e git …`): Windows git over 9P is ~6x slower and doesn't see Linux config.
//! The string helpers are pure and tested on every OS; the path ones only match on Windows.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// `(distro, /linux/path)` of a path inside a WSL distro, in any form Windows or the user hands out:
/// `\\wsl.localhost\D\x`, `\\wsl$\D\x`, `\\?\UNC\wsl.localhost\D\x` (a `canonicalize`), `//wsl.localhost/D/x`,
/// `wsl://D/x`. The Linux path has no trailing `/` (except `/` itself).
pub fn parse(s: &str) -> Option<(String, String)> {
    let rest = match s.strip_prefix("wsl://") {
        Some(r) => r.replace('/', "\\"),
        None => {
            let t = s.replace('/', "\\");
            let t = t.strip_prefix(r"\\?\UNC\").map_or(t.clone(), |r| format!(r"\\{r}"));
            let (host, r) = t.strip_prefix(r"\\")?.split_once('\\')?;
            if !(host.eq_ignore_ascii_case("wsl.localhost") || host.eq_ignore_ascii_case("wsl$")) {
                return None;
            }
            r.to_string()
        }
    };
    let (distro, path) = rest.split_once('\\').unwrap_or((&rest, ""));
    if distro.is_empty() {
        return None;
    }
    let linux: Vec<&str> = path.split('\\').filter(|c| !c.is_empty()).collect();
    Some((distro.to_string(), format!("/{}", linux.join("/"))))
}

/// A Linux path of `distro` as the app's Windows path. `/mnt/c/x` is `C:\x` (DrvFs): the same file, and native
/// tools (git, the Recycle Bin, watchers) work on it.
pub fn to_windows(distro: &str, linux: &str) -> PathBuf {
    if let Some(r) = linux.strip_prefix("/mnt/") {
        let (d, rest) = r.split_once('/').unwrap_or((r, ""));
        if d.len() == 1 && d.as_bytes()[0].is_ascii_alphabetic() {
            return format!("{}:\\{}", d.to_ascii_uppercase(), rest.replace('/', "\\")).into();
        }
    }
    let rest = linux.trim_end_matches('/').replace('/', "\\");
    format!(r"\\wsl.localhost\{distro}{}", if rest.is_empty() { r"\".into() } else { rest }).into()
}

/// A Windows path as WSL sees it: inside a distro its Linux path, a drive path under `/mnt/<drive>`.
pub fn to_linux(s: &str) -> Option<String> {
    if let Some((_, linux)) = parse(s) {
        return Some(linux);
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let rest = s[2..].replace('\\', "/");
        return Some(
            format!("/mnt/{}/{}", (b[0] as char).to_ascii_lowercase(), rest.trim_matches('/'))
                .trim_end_matches('/')
                .to_string(),
        );
    }
    None
}

/// `path` (Linux) is `root` or inside it: component-wise and case-sensitive, like Linux.
pub fn under(path: &str, root: &str) -> bool {
    let mut p = path.split('/').filter(|c| !c.is_empty());
    root.split('/').filter(|c| !c.is_empty()).all(|r| p.next() == Some(r))
}

/// `(distro, /linux/path)` of a path inside a distro. Always None off Windows (there `\\wsl.localhost\…` is just a
/// file name).
pub fn of(p: &Path) -> Option<(String, String)> {
    if cfg!(windows) { parse(p.to_str()?) } else { None }
}

/// The env names our command builders set that must reach the Linux side (WSL passes only what `WSLENV` names).
const ENV: &str = "LC_ALL:GIT_OPTIONAL_LOCKS:GIT_TERMINAL_PROMPT:GH_PROMPT_DISABLED:NO_COLOR:DIRIGENT_JOB";

static SPAWNS: AtomicU64 = AtomicU64::new(0);

/// `program` run inside the distro of `dir`, in `dir`: `wsl.exe -d <distro> --cd <linux dir> -e <program>`; add its
/// args with `.arg(s)`. None when `dir` isn't inside a distro (run it natively).
pub fn command(dir: &Path, program: &str) -> Option<Command> {
    let (distro, linux) = of(dir)?;
    SPAWNS.fetch_add(1, Ordering::Relaxed);
    crate::perf(|| format!("wsl {distro}: {program} in {linux}"));
    let mut c = crate::cmd("wsl.exe");
    c.args(["-d", &distro, "--cd", &linux, "-e", program]).env("WSLENV", ENV).env_remove("WSL_UTF8");
    Some(c)
}

/// How many commands `command` built (perf/tests: what one refresh costs).
pub fn spawns() -> u64 {
    SPAWNS.load(Ordering::Relaxed)
}

/// The distro user's home (Linux path), asked once per distro per run (it starts the distro: seconds when it was
/// stopped). Blocking: call off the UI thread.
pub fn home(distro: &str) -> Option<String> {
    static HOMES: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    if let Some((_, h)) = HOMES.lock().unwrap().iter().find(|(d, _)| d == distro) {
        return Some(h.clone());
    }
    let out = crate::cmd("wsl.exe")
        .args(["-d", distro, "-e", "sh", "-c", r#"printf %s "$HOME""#])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let h = String::from_utf8(out.stdout).ok().filter(|h| out.status.success() && h.starts_with('/'))?;
    HOMES.lock().unwrap().push((distro.to_string(), h.clone()));
    Some(h)
}

/// `distro` as WSL lists it (`ubuntu` → `Ubuntu`), so a repo has one key however its path was typed; an unknown
/// name stays as given. The list is read once per run (`distros`), on first use: off the UI thread.
pub fn listed(distro: &str) -> String {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    listed_in(distro, LIST.get_or_init(distros))
}

fn listed_in(distro: &str, list: &[String]) -> String {
    list.iter().find(|d| d.eq_ignore_ascii_case(distro)).unwrap_or(&distro.to_string()).clone()
}

/// The installed distros (`wsl.exe -l -q`), Docker Desktop's internal ones left out. Empty without WSL. Blocking
/// (doesn't start a distro): call off the UI thread.
pub fn distros() -> Vec<String> {
    let mut c = crate::cmd("wsl.exe");
    // `WSL_UTF8=1` switches the listing to UTF-8; without it, it's UTF-16LE. Either is decoded, but take the default.
    c.args(["-l", "-q"]).env_remove("WSL_UTF8").stdin(std::process::Stdio::null());
    c.output().ok().filter(|o| o.status.success()).map_or_else(Vec::new, |o| parse_list(&o.stdout))
}

/// `wsl.exe -l -q` output: UTF-16LE (with or without a BOM) or UTF-8, one name per line.
pub fn parse_list(b: &[u8]) -> Vec<String> {
    let b = b.strip_prefix(&[0xff, 0xfe]).unwrap_or(b);
    let utf16 = b.len() >= 2 && b.len().is_multiple_of(2) && b.iter().skip(1).step_by(2).all(|&x| x == 0);
    let text = if utf16 {
        String::from_utf16_lossy(&b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>())
    } else {
        String::from_utf8_lossy(b).into_owned()
    };
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0' || c == '\u{feff}').to_string())
        .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_of_one_path() {
        let want = Some(("Ubuntu".to_string(), "/home/x/proj".to_string()));
        for s in [
            r"\\wsl.localhost\Ubuntu\home\x\proj",
            r"\\wsl$\Ubuntu\home\x\proj\",
            r"\\WSL.LOCALHOST\Ubuntu\home\x\proj",
            r"\\?\UNC\wsl.localhost\Ubuntu\home\x\proj",
            r"\\?\UNC\wsl$\Ubuntu\home\x\proj",
            "//wsl.localhost/Ubuntu/home/x/proj",
            "wsl://Ubuntu/home/x/proj/",
        ] {
            assert_eq!(parse(s), want, "{s}");
        }
        assert_eq!(parse(r"\\wsl.localhost\Ubuntu"), Some(("Ubuntu".into(), "/".into())));
        for s in [r"\\server\share\x", r"C:\x", "/home/x", r"\\wsl.localhost\", "wsl://"] {
            assert_eq!(parse(s), None, "{s}");
        }
    }

    #[test]
    fn windows_and_linux_forms() {
        assert_eq!(to_windows("Ubuntu", "/home/x"), PathBuf::from(r"\\wsl.localhost\Ubuntu\home\x"));
        assert_eq!(to_windows("Ubuntu", "/"), PathBuf::from(r"\\wsl.localhost\Ubuntu\"));
        assert_eq!(to_windows("Ubuntu", "/mnt/c/Users/x"), PathBuf::from(r"C:\Users\x"));
        assert_eq!(to_windows("Ubuntu", "/mnt/d"), PathBuf::from(r"D:\"));
        assert_eq!(to_windows("Ubuntu", "/mnt/wsl/x"), PathBuf::from(r"\\wsl.localhost\Ubuntu\mnt\wsl\x"));
        assert_eq!(to_linux(r"C:\Users\x\AppData"), Some("/mnt/c/Users/x/AppData".into()));
        assert_eq!(to_linux(r"D:\"), Some("/mnt/d".into()));
        assert_eq!(to_linux(r"\\wsl$\Ubuntu\home\x"), Some("/home/x".into()));
        assert_eq!(to_linux(r"\\server\share"), None);
        // Round trip through both forms.
        for linux in ["/home/x/a b", "/mnt/c/x", "/"] {
            let w = to_windows("Debian", linux);
            assert_eq!(to_linux(w.to_str().unwrap()).as_deref(), Some(linux), "{linux}");
        }
    }

    #[test]
    fn one_key_however_the_distro_is_typed() {
        let list = ["Ubuntu".to_string(), "Debian".to_string()];
        let key = |s: &str| parse(s).map(|(d, l)| to_windows(&listed_in(&d, &list), &l));
        assert_eq!(key(r"\\wsl.localhost\ubuntu\home\x"), key(r"\\wsl.localhost\Ubuntu\home\x"));
        assert_eq!(
            key(r"\\wsl$\UBUNTU\home\x"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\home\x")),
            "listed spelling"
        );
        assert_eq!(listed_in("Arch", &list), "Arch", "unknown: as given");
    }

    #[test]
    fn distro_lists() {
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        let want = ["Ubuntu-24.04", "Debian"];
        assert_eq!(parse_list(&utf16("Ubuntu-24.04\r\nDebian\r\ndocker-desktop\r\n")), want);
        let mut bom = vec![0xff, 0xfe];
        bom.extend(utf16("Ubuntu-24.04\r\nDebian\r\n"));
        assert_eq!(parse_list(&bom), want);
        assert_eq!(parse_list(b"Ubuntu-24.04\nDebian\n\n"), want, "WSL_UTF8=1");
        assert!(parse_list(b"").is_empty());
    }

    #[test]
    fn linux_paths_are_case_sensitive() {
        assert!(under("/home/a/b", "/home/a"));
        assert!(under("/home/a", "/home/a/"));
        assert!(!under("/home/A/b", "/home/a"));
        assert!(!under("/home/ab", "/home/a"));
        assert!(under("/x", "/"));
    }

    #[test]
    fn commands_only_for_distro_paths() {
        assert!(command(Path::new("/home/x"), "git").is_none());
        let c = command(Path::new(r"\\wsl.localhost\Ubuntu\home\x"), "git");
        if cfg!(windows) {
            let c = c.unwrap();
            let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(c.get_program(), "wsl.exe");
            assert_eq!(args, ["-d", "Ubuntu", "--cd", "/home/x", "-e", "git"]);
        } else {
            assert!(c.is_none(), "a file name off Windows");
        }
    }
}
