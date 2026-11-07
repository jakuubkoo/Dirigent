//! Remembered projects (directories you opened sessions in: a repo root, or the directory itself), the picker's
//! fuzzy matching and its path-mode completion. Pure (no gpui, no IO except `list_dirs`), unit-tested.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Unpinned projects beyond this are forgotten, oldest first.
const MAX_PROJECTS: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub root: PathBuf,
    /// Display name; None: the directory's name.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    /// Unix seconds.
    #[serde(default)]
    pub last_used: i64,
    /// Last branch seen while a session was in it (the picker shows it without touching the disk).
    #[serde(default)]
    pub branch: Option<String>,
    /// New sessions here start claude.
    #[serde(default)]
    pub auto_claude: bool,
    /// Flags for that claude (already filtered through `hook::resume_args`).
    #[serde(default)]
    pub claude_args: Vec<String>,
    /// Where new worktrees go (None: `<repo parent>/<repo>-worktrees`).
    #[serde(default)]
    pub worktrees_dir: Option<PathBuf>,
    /// The branch prefix last picked in the New session dialog (`""`: none; None: never picked → `feat/`).
    #[serde(default)]
    pub branch_prefix: Option<String>,
    /// Run in a new worktree before claude starts (e.g. `pnpm install`).
    #[serde(default)]
    pub setup_cmd: Option<String>,
    /// Files last opened through the Command Center (relative paths, newest first; quick open lists them first).
    #[serde(default)]
    pub recent: Vec<String>,
}

/// Project defaults as typed in the dialog or on the Settings page.
pub struct Typed<'a> {
    pub auto: bool,
    pub flags: &'a str,
    pub setup: &'a str,
    pub wt_dir: &'a str,
}

impl Project {
    /// Flags only as `hook::resume_args` keeps them (never a permission bypass); empty setup/dir = none; a worktree
    /// dir with `~` is under `home`, a relative one under the project.
    pub fn apply_defaults(&mut self, t: &Typed, home: &Path) {
        self.auto_claude = t.auto;
        self.claude_args = hook::resume_args(&hook::split_words(t.flags));
        self.setup_cmd = Some(t.setup.trim().to_string()).filter(|c| !c.is_empty());
        let dir = t.wt_dir.trim();
        self.worktrees_dir = (!dir.is_empty()).then(|| match dir.strip_prefix('~') {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => self.root.join(dir), // absolute stays absolute
        });
    }

    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            self.root.file_name().map_or_else(|| self.root.display().to_string(), |n| n.to_string_lossy().into_owned())
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projects(pub Vec<Project>);

impl Projects {
    pub fn get(&self, root: &Path) -> Option<&Project> {
        self.0.iter().find(|p| p.root == root)
    }

    pub fn get_mut(&mut self, root: &Path) -> Option<&mut Project> {
        self.0.iter_mut().find(|p| p.root == root)
    }

    /// Remembers `root` (or bumps its last use).
    pub fn touch(&mut self, root: &Path, now: i64) {
        match self.get_mut(root) {
            Some(p) => p.last_used = p.last_used.max(now),
            None => {
                self.0.push(Project { root: root.to_path_buf(), last_used: now, ..Default::default() });
                if self.0.len() > MAX_PROJECTS
                    && let Some(oldest) = self
                        .0
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| !p.pinned)
                        .min_by_key(|(_, p)| p.last_used)
                        .map(|(i, _)| i)
                {
                    self.0.remove(oldest);
                }
            }
        }
    }

    /// Forgets it (never touches files).
    pub fn remove(&mut self, root: &Path) {
        self.0.retain(|p| p.root != root);
    }

    /// The deepest known project containing `dir`.
    pub fn of_dir(&self, dir: &Path) -> Option<&Project> {
        self.0.iter().filter(|p| dir.starts_with(&p.root)).max_by_key(|p| p.root.components().count())
    }

    /// Indices: pinned first, then most recently used.
    pub fn sorted(&self) -> Vec<usize> {
        let mut ix: Vec<usize> = (0..self.0.len()).collect();
        ix.sort_by_key(|&i| (!self.0[i].pinned, std::cmp::Reverse(self.0[i].last_used)));
        ix
    }
}

/// A branch name from a session name: `Fix login bug!` → `fix-login-bug` (lowercase ASCII words joined by `-`,
/// `/` kept for `feat/x`, at most 60 chars).
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut s: String = out.trim_matches(|c| c == '-' || c == '.').chars().take(60).collect();
    while s.ends_with(['-', '.']) {
        s.pop();
    }
    s
}

/// The New session dialog's branch prefixes; `""` is "none".
pub const PREFIXES: [&str; 5] = ["feat/", "fix/", "chore/", "docs/", ""];

/// The remembered prefix's index (never picked: `feat/`; an unknown one: none).
pub fn prefix_index(saved: Option<&str>) -> usize {
    let p = saved.unwrap_or(PREFIXES[0]);
    PREFIXES.iter().position(|x| *x == p).unwrap_or(PREFIXES.len() - 1)
}

/// An explicit `Branch: x` line in a prompt: x as written (first word, without quotes/backticks or a trailing
/// `.`/`,`), split into a known prefix's index and the rest; an unknown prefix stays in the rest (prefix none).
pub fn branch_line(prompt: &str) -> Option<(usize, String)> {
    let line = prompt.lines().find_map(|l| {
        let l = l.trim_start().trim_start_matches(['-', '*', ' ']);
        l.get(..7).filter(|h| h.eq_ignore_ascii_case("branch:")).map(|_| &l[7..])
    })?;
    let word = line.split_whitespace().next()?.trim_matches(['`', '"', '\'', '.', ',', ';']);
    if word.is_empty() {
        return None;
    }
    let none = PREFIXES.len() - 1;
    Some(match PREFIXES[..none].iter().position(|p| word.starts_with(p) && word.len() > p.len()) {
        Some(i) => (i, word[PREFIXES[i].len()..].to_string()),
        None => (none, word.to_string()),
    })
}

/// A session name without claude: the first words of the prompt's first line that isn't `Branch: x`.
pub fn fallback_name(prompt: &str) -> String {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty() && branch_line(l).is_none()).unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().take(5).collect();
    words.join(" ").trim_end_matches(|c: char| !c.is_alphanumeric()).to_string()
}

/// Default directory for a new worktree: `<worktrees_dir or <parent>/<repo>-worktrees>/<branch with / → ->`.
pub fn worktree_path(repo_root: &Path, worktrees_dir: Option<&Path>, branch: &str) -> PathBuf {
    worktrees_base(repo_root, worktrees_dir).join(branch.replace('/', "-"))
}

/// Where a project's new worktrees go: its `worktrees_dir`, else `<repo>-worktrees` next to the repo.
pub fn worktrees_base(repo_root: &Path, worktrees_dir: Option<&Path>) -> PathBuf {
    worktrees_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        let name = repo_root.file_name().map_or_else(|| "repo".into(), |n| n.to_string_lossy().into_owned());
        repo_root.parent().unwrap_or(repo_root).join(format!("{name}-worktrees"))
    })
}

/// A worktree location inside the repo itself would be watched and walked as part of the main checkout.
pub fn inside_repo(repo_root: &Path, path: &Path) -> bool {
    path.starts_with(repo_root)
}

/// What a new session types: `<setup> && [DIRIGENT_PROMPT_FILE=<file>] claude <flags>` (each part optional; None:
/// nothing). The first prompt never goes through your shell: it waits in `prompt_file` and our `claude` shim passes
/// it on as one argument, newlines and quotes intact (`agent::SHIM`). An env var, not an argument: an alias that
/// adds flags (`alias claude='claude --model opus'`) still expands after the assignment and can't move it. Flags
/// are shell-quoted; the setup command is typed as you wrote it.
pub fn start_line(setup: Option<&str>, claude: bool, args: &[String], prompt_file: Option<&Path>) -> Option<String> {
    let claude = claude.then(|| {
        let mut words = Vec::new();
        if let Some(f) = prompt_file {
            words.push(format!("{}={}", crate::agent::PROMPT_ENV, crate::workspace::sh_word(&f.to_string_lossy())));
        }
        words.push("claude".to_string());
        words.extend(args.iter().map(|a| crate::workspace::sh_word(a)));
        words.join(" ")
    });
    match (setup.map(str::trim).filter(|s| !s.is_empty()), claude) {
        (Some(s), Some(c)) => Some(format!("{s} && {c}")),
        (s, c) => s.map(str::to_string).or(c),
    }
}

/// A fuzzy match: score (higher is better) and the matched char indices in `text`.
#[derive(Debug, PartialEq)]
pub struct Match {
    pub score: i32,
    pub chars: Vec<usize>,
}

/// Case-insensitive subsequence match of `query` in `text`. Rewards consecutive chars, word starts (after
/// `/ - _ . space` or a lower→upper case change) and a match at the very start. Greedy with one look-ahead per char:
/// it prefers the next word-start occurrence over an earlier mid-word one. Cheap enough for hundreds of rows per
/// keystroke.
pub fn fuzzy(query: &str, text: &str) -> Option<Match> {
    if query.is_empty() {
        return Some(Match { score: 0, chars: Vec::new() });
    }
    let t: Vec<char> = text.chars().collect();
    let lower: Vec<char> = t.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let starts = |i: usize| {
        i == 0 || matches!(t[i - 1], '/' | '-' | '_' | '.' | ' ') || (t[i - 1].is_lowercase() && t[i].is_uppercase())
    };
    fuzzy_core(&q, &lower, starts)
}

/// `fuzzy` over already lowercased units (chars, or ASCII bytes for file paths); `starts(i)` = a word starts at `i`.
/// Indices in the result are unit indices.
pub fn fuzzy_core<T: Copy + PartialEq>(query: &[T], lower: &[T], starts: impl Fn(usize) -> bool) -> Option<Match> {
    let mut chars = Vec::with_capacity(query.len());
    let mut score = 0;
    let mut from = 0;
    for &q in query {
        let first = (from..lower.len()).find(|&i| lower[i] == q)?;
        // Continue a run if possible; else prefer a word start a bit further on.
        let at = if chars.last().is_some_and(|&l| l + 1 == first) {
            first
        } else {
            (first..lower.len()).find(|&i| lower[i] == q && starts(i)).unwrap_or(first)
        };
        score += 1;
        if chars.last().is_some_and(|&l| l + 1 == at) {
            score += 5;
        }
        if starts(at) {
            score += 8;
        }
        if at == 0 {
            score += 6;
        }
        chars.push(at);
        from = at + 1;
    }
    score -= (lower.len() / 8) as i32; // shorter texts win ties
    Some(Match { score, chars })
}

/// A picker row's match: the display name counts more than the path.
pub fn match_project(query: &str, name: &str, path: &str) -> Option<(i32, Vec<usize>)> {
    let on_name = fuzzy(query, name).map(|m| (m.score + 10, m.chars));
    let on_path = fuzzy(query, path).map(|m| (m.score, Vec::new()));
    match (on_name, on_path) {
        (Some(a), Some(b)) => Some(if a.0 >= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Path mode: the query starts like a path.
pub fn is_path_query(q: &str) -> bool {
    q.starts_with('/') || q.starts_with('~') || q.starts_with('.')
}

/// Splits a typed path into (directory to list, last segment to match): `~/Code/ag` → (`$HOME/Code`, `ag`).
pub fn split_path_query(q: &str, home: &Path, cwd: &Path) -> (PathBuf, String) {
    let (dir, seg) = match q.rfind('/') {
        Some(i) => (&q[..=i], &q[i + 1..]),
        None if q == "~" => ("~/", ""),
        None => ("", q),
    };
    let dir = match dir.strip_prefix('~') {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if dir.is_empty() => cwd.to_path_buf(),
        None if dir.starts_with('/') => PathBuf::from(dir),
        None => cwd.join(dir),
    };
    (dir, seg.to_string())
}

/// Subdirectory names of `dir` (sorted, capped). Blocking: run off the UI thread.
pub fn list_dirs(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) || e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .take(2000)
        .collect();
    out.sort_by_key(|n| n.to_lowercase());
    out
}

/// The listed names that complete `seg` (case-insensitive prefix; hidden ones only when `seg` starts with `.`).
pub fn complete<'a>(names: &'a [String], seg: &str) -> Vec<&'a str> {
    let seg_l = seg.to_lowercase();
    names
        .iter()
        .filter(|n| (seg.starts_with('.') || !n.starts_with('.')) && n.to_lowercase().starts_with(&seg_l))
        .map(String::as_str)
        .collect()
}

/// The longest common prefix (case-sensitive) of the candidates, for Tab with several matches.
pub fn common_prefix(c: &[&str]) -> String {
    let Some(first) = c.first() else { return String::new() };
    let mut end = first.len();
    for s in &c[1..] {
        end = end.min(first.bytes().zip(s.bytes()).take_while(|(a, b)| a == b).count());
    }
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    first[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_defaults_keep_only_safe_flags_and_resolve_dirs() {
        let mut p = Project { root: "/r/app".into(), ..Default::default() };
        let t = Typed {
            auto: true,
            flags: "--model opus --dangerously-skip-permissions --permission-mode bypassPermissions hi",
            setup: "  ",
            wt_dir: "~/wt",
        };
        p.apply_defaults(&t, Path::new("/home/u"));
        assert!(p.auto_claude);
        assert_eq!(p.claude_args, vec!["--model", "opus"], "bypasses and the prompt never stored");
        assert_eq!(p.setup_cmd, None);
        assert_eq!(p.worktrees_dir, Some(PathBuf::from("/home/u/wt")));
        p.apply_defaults(&Typed { auto: false, flags: "", setup: "pnpm i", wt_dir: "../trees" }, Path::new("/home/u"));
        assert_eq!((p.claude_args.len(), p.setup_cmd.as_deref()), (0, Some("pnpm i")));
        assert_eq!(p.worktrees_dir, Some(PathBuf::from("/r/app/../trees")));
    }

    fn p(root: &str, last_used: i64, pinned: bool) -> Project {
        Project { root: root.into(), last_used, pinned, ..Default::default() }
    }

    #[test]
    fn touch_order_remove_and_of_dir() {
        let mut ps = Projects::default();
        ps.touch(Path::new("/c/a"), 10);
        ps.touch(Path::new("/c/b"), 20);
        ps.touch(Path::new("/c/a"), 5); // never goes back in time
        assert_eq!(ps.get(Path::new("/c/a")).unwrap().last_used, 10);
        ps.get_mut(Path::new("/c/a")).unwrap().pinned = true;
        ps.touch(Path::new("/c/d"), 30);
        let order: Vec<_> = ps.sorted().into_iter().map(|i| ps.0[i].root.clone()).collect();
        assert_eq!(order, [PathBuf::from("/c/a"), "/c/d".into(), "/c/b".into()], "pinned, then recent");
        ps.touch(Path::new("/c/a/sub"), 40);
        assert_eq!(ps.of_dir(Path::new("/c/a/sub/x")).unwrap().root, Path::new("/c/a/sub"), "deepest");
        assert_eq!(ps.of_dir(Path::new("/c/a/other")).unwrap().root, Path::new("/c/a"));
        assert!(ps.of_dir(Path::new("/c/ab")).is_none(), "path components, not string prefix");
        ps.remove(Path::new("/c/b"));
        assert!(ps.get(Path::new("/c/b")).is_none());
        assert_eq!(p("/x/name", 0, false).display(), "name");
    }

    #[test]
    fn capped_dropping_the_oldest_unpinned() {
        let mut ps = Projects(vec![p("/old-pinned", 0, true)]);
        for i in 1..=MAX_PROJECTS as i64 {
            ps.touch(Path::new(&format!("/p{i}")), i);
        }
        assert_eq!(ps.0.len(), MAX_PROJECTS);
        assert!(ps.get(Path::new("/old-pinned")).is_some(), "pinned stay");
        assert!(ps.get(Path::new("/p1")).is_none(), "the oldest unpinned went");
    }

    #[test]
    fn start_lines() {
        let args = vec!["--model".to_string(), "opus".to_string()];
        let file = Path::new("/run/user/1/dirigent-9/prompt-3-1.txt");
        assert_eq!(start_line(None, false, &args, Some(file)), None);
        assert_eq!(start_line(None, true, &args, None).unwrap(), "claude --model opus");
        assert_eq!(
            start_line(Some(" pnpm install "), true, &args, Some(file)).unwrap(),
            "pnpm install && DIRIGENT_PROMPT_FILE=/run/user/1/dirigent-9/prompt-3-1.txt claude --model opus"
        );
        assert_eq!(
            start_line(None, true, &[], Some(Path::new("/tmp/it's here/p.txt"))).unwrap(),
            "DIRIGENT_PROMPT_FILE='/tmp/it'\\''s here/p.txt' claude"
        );
        assert_eq!(start_line(Some("cargo fetch"), false, &[], None).unwrap(), "cargo fetch");
        assert_eq!(start_line(Some("  "), true, &[], None).unwrap(), "claude");
    }

    #[test]
    fn slugs_and_worktree_paths() {
        assert_eq!(slug("Fix login bug!"), "fix-login-bug");
        assert_eq!(slug("  feat/Login: OAuth 2.0  "), "feat-login-oauth-2.0", "a / from the name never nests");
        assert_eq!(slug("Esc/Ctrl-C"), "esc-ctrl-c");
        assert_eq!(slug("Příliš žluťoučký"), "p-li-lu-ou-k");
        assert_eq!(slug("---"), "");
        assert_eq!(slug(&"a".repeat(80)).len(), 60);
        let root = Path::new("/c/app");
        assert_eq!(worktree_path(root, None, "feat/x"), Path::new("/c/app-worktrees/feat-x"));
        assert_eq!(worktree_path(root, Some(Path::new("/w")), "b"), Path::new("/w/b"));
        assert!(inside_repo(root, Path::new("/c/app/wt/b")) && !inside_repo(root, Path::new("/c/app-worktrees/b")));
    }

    #[test]
    fn prefixes_branch_lines_and_fallback_names() {
        assert_eq!(prefix_index(None), 0, "never picked: feat/");
        assert_eq!(PREFIXES[prefix_index(Some(""))], "", "none is remembered");
        assert_eq!(PREFIXES[prefix_index(Some("docs/"))], "docs/");
        assert_eq!(PREFIXES[prefix_index(Some("wip/"))], "", "unknown: none");
        let p = "Branch: feat/branch-prefix from main. Small change, no plan mode needed.\nAdd a prefix.";
        assert_eq!(branch_line(p), Some((0, "branch-prefix".into())));
        assert_eq!(branch_line("x\n- branch: `fix/esc-key`."), Some((1, "esc-key".into())));
        assert_eq!(branch_line("Branch: jakub/try-this"), Some((4, "jakub/try-this".into())), "unknown prefix kept");
        assert_eq!(branch_line("Branch: feat/"), Some((4, "feat/".into())));
        assert_eq!(branch_line("Branch:"), None);
        assert_eq!(branch_line("The branch: is wrong"), None, "only at a line start");
        assert_eq!(fallback_name(p), "Add a prefix");
        assert_eq!(
            fallback_name("\n  Fix the Esc/Ctrl+C interrupt in the terminal now"),
            "Fix the Esc/Ctrl+C interrupt in"
        );
        assert_eq!(fallback_name("Why?"), "Why");
        assert_eq!(fallback_name(""), "");
    }

    #[test]
    fn fuzzy_ranks_word_starts_and_runs() {
        let s = |q, t| fuzzy(q, t).map(|m| m.score);
        assert!(s("ai", "agent-ide") > s("ai", "tracemytask"), "word starts beat mid-word");
        assert!(s("agent", "agent-ide") > s("agent", "my-big-agent-ide"), "at the start beats later");
        assert_eq!(s("xyz", "agent-ide"), None);
        assert_eq!(fuzzy("AI", "agent-ide").unwrap().chars, [0, 6], "case-insensitive, word start for `i`");
        assert_eq!(fuzzy("tmt", "TraceMyTask").unwrap().chars, [0, 5, 7], "camelCase starts");
        assert_eq!(fuzzy("", "x").unwrap().score, 0);
        // Name matches outrank path-only matches.
        let by_name = match_project("ide", "agent-ide", "~/Code/agent-ide").unwrap();
        let by_path = match_project("code", "agent-ide", "~/Code/agent-ide").unwrap();
        assert!(by_name.0 > by_path.0);
        assert!(by_path.1.is_empty(), "no name highlight for a path match");
    }

    #[test]
    fn path_queries_split_complete_and_prefix() {
        let (home, cwd) = (Path::new("/home/u"), Path::new("/w"));
        assert!(is_path_query("~/x") && is_path_query("./a") && is_path_query("/") && !is_path_query("agent"));
        assert_eq!(split_path_query("~/Code/ag", home, cwd), ("/home/u/Code".into(), "ag".into()));
        assert_eq!(split_path_query("~", home, cwd), ("/home/u".into(), "".into()));
        assert_eq!(split_path_query("/usr/", home, cwd), ("/usr/".into(), "".into()));
        assert_eq!(split_path_query("./src/m", home, cwd), ("/w/./src/".into(), "m".into()));
        let names: Vec<String> = ["Code", "code-old", ".config", "Docs"].map(String::from).into();
        assert_eq!(complete(&names, "co"), ["Code", "code-old"]);
        assert_eq!(complete(&names, ""), ["Code", "code-old", "Docs"], "hidden only when asked");
        assert_eq!(complete(&names, ".c"), [".config"]);
        assert_eq!(common_prefix(&["code-old", "code-new"]), "code-");
        assert_eq!(common_prefix(&["Code", "code-old"]), "");
        assert_eq!(common_prefix(&[]), "");
    }
}
