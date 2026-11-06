//! Pull / merge requests for the current branch, per forge:
//! - GitHub: `gh` when it's installed and logged in for the host (state + checks, create); without it the compare
//!   page opens in the browser and the strip is a link to the branch's pull requests.
//! - GitLab: the merge request is created by the push itself (push options); its state comes from `glab` when it's
//!   installed and logged in, else a link to the branch's merge requests.
//! - Other hosts: nothing (their URL schemes can't be derived reliably).
//!
//! No token is ever read, asked for or stored: gh/glab use their own login. Blocking: run off the UI thread.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use git::remote::{self, Cancel, Forge, Host, MrPush};
use git::write::GitError;
use serde_json::Value;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Open,
    Draft,
    Merged,
    Closed,
}

/// One check's (GitHub check run / status, GitLab job) state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    Pending,
    Passing,
    Failing,
    /// Finished without running (skipped, manual, neutral): counts as done, neither passed nor failed.
    Skipped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub state: Checks,
    /// Unix seconds; None when the forge doesn't say (or hasn't started / finished).
    pub started: Option<i64>,
    pub finished: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pr {
    pub number: u64,
    pub state: State,
    pub checks: Vec<Check>,
    pub url: String,
    /// The request's checks / pipeline page.
    pub checks_url: String,
}

/// The strip's checks text and its red "· N failing" part: "checks 3/5" + "· 1 failing", "all 5 passed".
/// None without checks.
pub fn summary(checks: &[Check]) -> Option<(String, Option<String>)> {
    let n = checks.len();
    let count = |s| checks.iter().filter(|c| c.state == s).count();
    let (pending, failed, skipped) = (count(Checks::Pending), count(Checks::Failing), count(Checks::Skipped));
    let failing = (failed > 0).then(|| format!("· {failed} failing"));
    Some(match () {
        _ if n == 0 => return None,
        _ if pending > 0 || failed > 0 => (format!("checks {}/{n}", n - pending), failing),
        _ if skipped > 0 => (format!("{} passed, {skipped} skipped", n - skipped), None),
        _ => (format!("all {n} passed"), None),
    })
}

/// Checks are still running (the panel refreshes meanwhile).
pub fn running(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.state == Checks::Pending)
}

/// The tooltip: one line per check, "fmt — passed in 14s", "build — running for 2m 05s".
pub fn check_lines(checks: &[Check], now: i64) -> String {
    let lines = checks.iter().map(|c| {
        let took = |end: Option<i64>| c.started.zip(end).map(|(s, e)| fmt_dur(e - s));
        let what = match c.state {
            Checks::Pending if c.started.is_some() => format!("running for {}", took(Some(now)).unwrap_or_default()),
            Checks::Pending => "queued".into(),
            Checks::Passing => took(c.finished).map_or("passed".into(), |d| format!("passed in {d}")),
            Checks::Failing => took(c.finished).map_or("failed".into(), |d| format!("failed after {d}")),
            Checks::Skipped => "skipped".into(),
        };
        format!("{} — {what}", c.name)
    });
    lines.collect::<Vec<_>>().join("\n")
}

/// "14s", "2m 05s", "1h 12m".
fn fmt_dur(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// `2026-11-06T20:05:00Z` / `…:00.085Z` (UTC, fractions ignored) → unix seconds; gh's "not yet" (`0001-…`) → None.
fn unix(ts: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    if y < 1970 {
        return None;
    }
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = y % 400 * 365 + y % 400 / 4 - y % 400 / 100 + doy;
    let days = y / 400 * 146097 + doe - 719468;
    Some(days * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?)
}

/// What the PR strip shows for a branch.
#[derive(Clone, Debug, PartialEq)]
pub enum Info {
    /// No remote / an unknown forge: no strip.
    Hidden,
    /// gh/glab works: the branch's request, or None.
    Known(Option<Pr>),
    /// No gh/glab (or not logged in): a link to the branch's requests.
    Link(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub forge: Forge,
    pub info: Info,
    /// The branch has an upstream (pushed): "Create PR…" makes sense without pushing first.
    pub pushed: bool,
    /// The remote's default branch (`main`), for "Switch to main and pull"; None without a remote.
    pub default: Option<String>,
}

impl View {
    /// "PR" or "MR".
    pub fn noun(&self) -> &'static str {
        if self.forge == Forge::GitLab { "MR" } else { "PR" }
    }
}

/// gh/glab readiness per host, checked once (again on a manual refresh: a login mid-session counts).
static READY: LazyLock<Mutex<HashMap<(&'static str, String), bool>>> = LazyLock::new(Default::default);

fn tool_ready(tool: &'static str, host: &str, recheck: bool, dir: &Path) -> bool {
    let key = (tool, host.to_string());
    if !recheck && let Some(r) = READY.lock().unwrap().get(&key) {
        return *r;
    }
    let ok =
        remote::run_tool(tool, dir, &["auth", "status", "--hostname", host], &Cancel::default(), TOOL_TIMEOUT).is_ok();
    READY.lock().unwrap().insert(key, ok);
    ok
}

/// Whether the last lookup found gh/glab missing for a forge that could use it (for the one-time hint).
pub fn tool_for(forge: Forge) -> Option<&'static str> {
    match forge {
        Forge::GitHub => Some("gh"),
        Forge::GitLab => Some("glab"),
        Forge::Other => None,
    }
}

/// The remote the branch goes to and its forge.
fn target(root: &Path, branch: &str) -> Option<(String, Host)> {
    let up = remote::upstream(root);
    let r = remote::push_target(root, branch, up.as_deref())?;
    let host = Host::parse(&remote::push_url(root, &r)?)?;
    Some((r, host))
}

/// The strip for `branch`. `recheck`: ask gh/glab whether they're logged in again.
pub fn lookup(root: &Path, branch: &str, recheck: bool) -> View {
    let pushed = remote::upstream(root).is_some();
    let Some((r, host)) = target(root, branch) else {
        return View { forge: Forge::Other, info: Info::Hidden, pushed, default: None };
    };
    let default = Some(remote::default_branch(root, &r));
    let info = match host.forge {
        Forge::Other => Info::Hidden,
        forge => {
            let tool = tool_for(forge).unwrap_or("gh");
            if tool_ready(tool, &host.host, recheck, root) {
                let args: &[&str] = if forge == Forge::GitHub {
                    &["pr", "view", branch, "--json", "number,state,isDraft,url,statusCheckRollup"]
                } else {
                    &["mr", "view", branch, "-F", "json"]
                };
                match remote::run_tool(tool, root, args, &Cancel::default(), TOOL_TIMEOUT) {
                    Ok(o) if forge == Forge::GitHub => Info::Known(parse_gh(&o.stdout)),
                    Ok(o) => {
                        let mut pr = parse_glab(&o.stdout);
                        if let (Some(pr), Some((project, pipeline))) = (&mut pr, glab_pipeline(&o.stdout)) {
                            let path = format!("projects/{project}/pipelines/{pipeline}/jobs?per_page=100");
                            let jobs =
                                remote::run_tool("glab", root, &["api", &path], &Cancel::default(), TOOL_TIMEOUT);
                            // Without the jobs the pipeline stays one check.
                            if let Some(jobs) = jobs.ok().and_then(|o| parse_glab_jobs(&o.stdout)) {
                                pr.checks = jobs;
                            }
                        }
                        Info::Known(pr)
                    }
                    // "no pull requests found for branch …" / "no open merge request available for …"
                    Err(e) if is_none_found(&e.to_string()) => Info::Known(None),
                    Err(_) => Info::Link(host.requests_url(branch)),
                }
            } else {
                Info::Link(host.requests_url(branch))
            }
        }
    };
    View { forge: host.forge, info, pushed, default }
}

fn is_none_found(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("no pull requests found") || e.contains("no merge request") || e.contains("no open merge request")
}

/// `gh pr view --json number,state,isDraft,url,statusCheckRollup`.
pub fn parse_gh(json: &str) -> Option<Pr> {
    let v: Value = serde_json::from_str(json).ok()?;
    let state = match (v["state"].as_str()?, v["isDraft"].as_bool().unwrap_or(false)) {
        ("OPEN", true) => State::Draft,
        ("OPEN", false) => State::Open,
        ("MERGED", _) => State::Merged,
        _ => State::Closed,
    };
    let runs = v["statusCheckRollup"].as_array().map(Vec::as_slice).unwrap_or_default();
    let checks = runs
        .iter()
        .map(|r| {
            // CheckRun: name, status + conclusion; StatusContext: context, state.
            let s = r["status"].as_str().or(r["state"].as_str()).unwrap_or("");
            let c = r["conclusion"].as_str().filter(|c| !c.is_empty()).unwrap_or(s);
            let state = match c {
                "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "ERROR" => {
                    Checks::Failing
                }
                "QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED" | "EXPECTED" => Checks::Pending,
                "SKIPPED" | "NEUTRAL" | "STALE" => Checks::Skipped,
                _ => Checks::Passing,
            };
            let name = r["name"].as_str().or(r["context"].as_str()).unwrap_or("check").to_string();
            let at = |k: &str| r[k].as_str().and_then(unix);
            Check { name, state, started: at("startedAt"), finished: at("completedAt") }
        })
        .collect();
    let url = v["url"].as_str()?.to_string();
    Some(Pr { number: v["number"].as_u64()?, state, checks, checks_url: format!("{url}/checks"), url })
}

/// `glab mr view <branch> -F json` (GitLab's merge request object). Built from GitLab's API docs: glab isn't
/// installed on the machine this was written on, so it reads leniently.
pub fn parse_glab(json: &str) -> Option<Pr> {
    let v: Value = serde_json::from_str(json).ok()?;
    let draft = v["draft"].as_bool().or(v["work_in_progress"].as_bool()).unwrap_or(false);
    let state = match v["state"].as_str()? {
        "opened" if draft => State::Draft,
        "opened" => State::Open,
        "merged" => State::Merged,
        _ => State::Closed,
    };
    let pipe = if v["head_pipeline"].is_object() { &v["head_pipeline"] } else { &v["pipeline"] };
    let url = v["web_url"].as_str()?.to_string();
    // The pipeline as one check until its jobs are read (glab_jobs).
    let checks = pipe["status"].as_str().map(|s| vec![gl_check("pipeline", s, pipe)]).unwrap_or_default();
    let checks_url = pipe["web_url"].as_str().map_or_else(|| format!("{url}/pipelines"), str::to_string);
    Some(Pr { number: v["iid"].as_u64()?, state, checks, url, checks_url })
}

/// The MR's head pipeline `(project id, pipeline id)`, for its jobs.
fn glab_pipeline(json: &str) -> Option<(u64, u64)> {
    let v: Value = serde_json::from_str(json).ok()?;
    let p = if v["head_pipeline"].is_object() { &v["head_pipeline"] } else { &v["pipeline"] };
    Some((p["project_id"].as_u64().or(v["project_id"].as_u64())?, p["id"].as_u64()?))
}

/// `glab api projects/<id>/pipelines/<id>/jobs` (GitLab's job objects; retried jobs are left out by the API).
pub fn parse_glab_jobs(json: &str) -> Option<Vec<Check>> {
    let v: Value = serde_json::from_str(json).ok()?;
    let mut jobs: Vec<Check> = v
        .as_array()?
        .iter()
        .map(|j| {
            let mut c = gl_check(j["name"].as_str().unwrap_or("job"), j["status"].as_str().unwrap_or(""), j);
            // allow_failure: the pipeline still passes.
            if c.state == Checks::Failing && j["allow_failure"].as_bool() == Some(true) {
                c.state = Checks::Skipped;
            }
            c
        })
        .collect();
    // The API lists the newest job first.
    jobs.reverse();
    (!jobs.is_empty()).then_some(jobs)
}

/// A GitLab pipeline or job by its status.
fn gl_check(name: &str, status: &str, v: &Value) -> Check {
    let state = match status {
        "success" => Checks::Passing,
        "failed" | "canceled" => Checks::Failing,
        "skipped" | "manual" => Checks::Skipped,
        _ => Checks::Pending,
    };
    let at = |k: &str| v[k].as_str().and_then(unix);
    Check { name: name.to_string(), state, started: at("started_at"), finished: at("finished_at") }
}

/// The request's title: the last commit's subject, else the branch name made readable.
fn title(root: &Path, branch: &str) -> (String, String) {
    let (subject, body) = remote::last_commit(root);
    if subject.is_empty() { (humanize(branch), String::new()) } else { (subject, body) }
}

/// `feat/git-redesign` → "Git redesign".
fn humanize(branch: &str) -> String {
    let last = branch.rsplit('/').next().unwrap_or(branch).replace(['-', '_'], " ");
    let mut c = last.trim().chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_else(|| branch.to_string())
}

/// Creates the branch's PR/MR and returns the URL to open (the new request, or the browser form to make one).
/// Pushes (publishes) the branch first when the remote doesn't have HEAD yet.
pub fn create(root: &Path, cancel: &Cancel) -> Result<(String, String), GitError> {
    // A merged branch whose remote branch is gone: creating would publish it again.
    if remote::upstream_gone(root) {
        return Err(GitError::Failed(crate::git_ops::GONE.into()));
    }
    let branch =
        remote::current_branch(root).ok_or_else(|| GitError::Failed("Not on a branch (detached HEAD)".into()))?;
    let (r, host) = target(root, &branch)
        .ok_or_else(|| GitError::Failed("No remote on GitHub or GitLab to open a request on.".into()))?;
    let base = remote::default_branch(root, &r);
    if branch == base {
        return Err(GitError::Failed(format!(
            "You're on {base}, the default branch: switch to a feature branch first."
        )));
    }
    let (title, body) = title(root, &branch);
    let unpushed = !remote::remote_has_head(root, &r, &branch);
    let set_upstream = remote::upstream(root).is_none();
    match host.forge {
        Forge::GitLab => {
            if unpushed {
                // The push carries the request: push options only apply when it sends something.
                let opts = [
                    "merge_request.create".to_string(),
                    format!("merge_request.target={base}"),
                    format!("merge_request.title={title}"),
                ];
                let out = remote::push_to(root, &r, set_upstream, &opts, cancel)?;
                if let MrPush::Created(url) = remote::parse_mr_push(&out) {
                    return Ok((url, "Merge request created".into()));
                }
            }
            Ok((host.new_mr_url(&branch, &base), "Opened the new merge request form".into()))
        }
        Forge::GitHub => {
            if unpushed {
                remote::push_to(root, &r, set_upstream, &[], cancel)?;
            }
            if tool_ready("gh", &host.host, false, root) {
                let args = ["pr", "create", "--head", &branch, "--base", &base, "--title", &title, "--body", &body];
                let url_in =
                    |s: &str| s.split_whitespace().rev().find(|w| w.starts_with("https://")).map(str::to_string);
                match remote::run_tool("gh", root, &args, cancel, remote::TIMEOUT) {
                    Ok(o) => {
                        if let Some(url) = url_in(&o.stdout) {
                            return Ok((url, "Pull request created".into()));
                        }
                    }
                    // "a pull request for branch "x" into branch "main" already exists:\nhttps://…"
                    Err(GitError::Failed(e)) if e.contains("already exists") && url_in(&e).is_some() => {
                        return Ok((url_in(&e).unwrap_or_default(), "This branch already has a pull request".into()));
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok((host.compare_url(&base, &branch), "Opened the new pull request form".into()))
        }
        Forge::Other => {
            Err(GitError::Failed(format!("{} isn't GitHub or GitLab: open the request on its website.", host.host)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Pr {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        parse_gh(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn text(pr: &Pr) -> String {
        summary(&pr.checks).map(|(t, f)| f.map_or(t.clone(), |f| format!("{t} {f}"))).unwrap_or_default()
    }

    #[test]
    fn gh_real_output() {
        // Captured with `gh pr view <n> --json number,state,isDraft,url,statusCheckRollup` (gh 2.46).
        // This repo's #8: 6 passed, 2 skipped.
        let pr = fixture("gh_pr_view_merged.json");
        assert_eq!((pr.number, pr.state), (8, State::Merged));
        assert!(pr.url.ends_with("/pull/8") && pr.checks_url.ends_with("/pull/8/checks"));
        assert_eq!(text(&pr), "6 passed, 2 skipped");
        assert!(!running(&pr.checks));
        // This repo's #19 mid-run: 3 of 5 done, gh's "not finished" completedAt is no time.
        let pr = fixture("gh_pr_view_running.json");
        assert_eq!((text(&pr), pr.state), ("checks 3/5".into(), State::Open));
        assert!(running(&pr.checks));
        let build = &pr.checks[0];
        assert_eq!((build.name.as_str(), build.state, build.finished), ("build", Checks::Pending, None));
        // A public PR mid-run with a failed status context (StatusContext: `context`, `state`).
        let pr = fixture("gh_pr_view_failing_running.json");
        assert_eq!(text(&pr), "checks 3/7 · 1 failing");
        assert!(pr.checks.iter().any(|c| c.name == "renovate/artifacts" && c.state == Checks::Failing));
        // A public PR, finished: 2 failed, 2 skipped, 1 neutral.
        let pr = fixture("gh_pr_view_failed.json");
        assert_eq!(text(&pr), "checks 5/5 · 2 failing");
        assert!(!running(&pr.checks));

        let all = parse_gh(
            r#"{"number":1,"state":"OPEN","isDraft":false,"url":"u","statusCheckRollup":[{"state":"SUCCESS"},{"state":"SUCCESS"}]}"#,
        );
        assert_eq!(text(&all.unwrap()), "all 2 passed");
        let draft = parse_gh(r#"{"number":2,"state":"OPEN","isDraft":true,"url":"u","statusCheckRollup":[]}"#).unwrap();
        assert_eq!((draft.state, summary(&draft.checks)), (State::Draft, None));
        assert!(is_none_found("no pull requests found for branch \"x\""));
    }

    #[test]
    fn check_tooltip() {
        let pr = fixture("gh_pr_view_running.json");
        let now = unix("2026-11-06T20:07:05Z").unwrap();
        let lines = check_lines(&pr.checks, now);
        assert_eq!(lines.lines().next(), Some("build — running for 2m 05s"));
        assert!(lines.contains("fmt — passed in 14s"), "{lines}");
        assert_eq!(unix("1970-01-02T00:00:01.085Z"), Some(86401));
        assert_eq!(unix("0001-01-01T00:00:00Z"), None);
        assert_eq!(fmt_dur(4380), "1h 13m");
    }

    #[test]
    fn glab_output() {
        // From GitLab's API docs (merge request + pipeline jobs objects); not captured live.
        let v = r#"{"iid":12,"state":"opened","draft":false,"project_id":7,"web_url":"https://gl/x/-/merge_requests/12","head_pipeline":{"id":99,"project_id":7,"status":"running","web_url":"https://gl/x/-/pipelines/99"}}"#;
        let pr = parse_glab(v).unwrap();
        assert_eq!((pr.number, pr.state, text(&pr)), (12, State::Open, "checks 0/1".into()));
        assert_eq!((pr.checks_url.as_str(), glab_pipeline(v)), ("https://gl/x/-/pipelines/99", Some((7, 99))));
        let jobs = r#"[
            {"id":3,"name":"deploy","status":"manual","started_at":null,"finished_at":null,"allow_failure":false},
            {"id":2,"name":"lint","status":"failed","started_at":"2026-11-06T10:00:00.000Z","finished_at":"2026-11-06T10:00:30.000Z","allow_failure":true},
            {"id":1,"name":"test","status":"running","started_at":"2026-11-06T10:00:00.000Z","finished_at":null,"allow_failure":false}
        ]"#;
        let jobs = parse_glab_jobs(jobs).unwrap();
        assert_eq!(
            jobs.iter().map(|c| (c.name.as_str(), c.state)).collect::<Vec<_>>(),
            [("test", Checks::Pending), ("lint", Checks::Skipped), ("deploy", Checks::Skipped)]
        );
        assert_eq!(summary(&jobs).unwrap().0, "checks 2/3");
        let merged = parse_glab(r#"{"iid":3,"state":"merged","web_url":"u"}"#).unwrap();
        assert_eq!((merged.state, merged.checks.len(), merged.checks_url.as_str()), (State::Merged, 0, "u/pipelines"));
    }

    /// Live: `DIRIGENT_LIVE_REPO=<clone of a GitLab test project>` (credentials from your git setup or
    /// GIT_CONFIG_* env), optionally `DIRIGENT_LIVE_SETTINGS=<settings.json>` for its `git.hosts`,
    /// `cargo test -p dirigent gitlab_live -- --ignored`. Pushes a branch and opens one MR.
    #[test]
    #[ignore]
    fn gitlab_live() {
        let Ok(repo) = std::env::var("DIRIGENT_LIVE_REPO") else { return };
        let root = std::path::Path::new(&repo);
        let mut web = None;
        if let Ok(path) = std::env::var("DIRIGENT_LIVE_SETTINGS") {
            let parsed = crate::settings::parse(&std::fs::read_to_string(path).unwrap(), &Default::default());
            assert!(parsed.notes.is_empty(), "{:?}", parsed.notes);
            web = parsed.settings.hosts.first().and_then(|r| r.web.clone());
            remote::set_host_rules(parsed.settings.hosts);
        }
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").args(args).current_dir(root).status().unwrap().success();
            assert!(ok, "git {args:?}");
        };
        let branch = format!("dirigent/live-{}", std::process::id());
        git(&["switch", "-q", "-c", &branch, "origin/main"]);
        std::fs::write(root.join(format!("{branch}.txt").replace('/', "-")), "x\n").unwrap();
        git(&["add", "."]);
        git(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "Live test from Dirigent"]);
        // New commit, no upstream: the push publishes the branch and GitLab creates the MR.
        let (url, note) = create(root, &Cancel::default()).unwrap();
        assert!(url.contains("/-/merge_requests/") && !url.contains("/new?"), "{url}");
        eprintln!("live: created {url}");
        assert_eq!(note, "Merge request created");
        // Nothing new to push: push options would be dropped, so the new-MR form opens instead.
        let (url, _) = create(root, &Cancel::default()).unwrap();
        assert!(url.contains("/-/merge_requests/new?"), "{url}");
        if let Some(w) = &web {
            assert!(url.starts_with(w.as_str()), "{url} isn't on {w}");
        }
        let v = lookup(root, &branch, true);
        assert_eq!(v.forge, Forge::GitLab);
        assert!(v.pushed);
        eprintln!("live: {branch} → {:?}", v.info);
    }

    /// Live: `DIRIGENT_LIVE_GH_REPO=<clone of a GitHub repo>` with gh logged in, `cargo test -p dirigent github_live
    /// -- --ignored`. Pushes a throwaway branch and opens a PR (close it and delete the branch afterwards).
    #[test]
    #[ignore]
    fn github_live() {
        let Ok(repo) = std::env::var("DIRIGENT_LIVE_GH_REPO") else { return };
        let root = std::path::Path::new(&repo);
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").args(args).current_dir(root).status().unwrap().success();
            assert!(ok, "git {args:?}");
        };
        let branch = format!("test/dirigent-create-pr-{}", std::process::id());
        git(&["switch", "-q", "-c", &branch, "origin/main"]);
        std::fs::write(root.join("dirigent-create-pr-test.txt"), "temporary\n").unwrap();
        git(&["add", "."]);
        git(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "Temporary: test Create PR from Dirigent"]);
        // Not pushed yet: create publishes the branch, then gh creates the PR.
        let (url, note) = create(root, &Cancel::default()).unwrap();
        assert!(url.contains("/pull/"), "{url}");
        assert_eq!(note, "Pull request created");
        // Again: gh says it exists; we open that one.
        let (again, note) = create(root, &Cancel::default()).unwrap();
        assert_eq!((again.as_str(), note.as_str()), (url.as_str(), "This branch already has a pull request"));
        let v = lookup(root, &branch, true);
        let Info::Known(Some(pr)) = &v.info else { panic!("{v:?}") };
        assert_eq!((pr.state, pr.url.as_str()), (State::Open, url.as_str()));
        eprintln!("live: {branch} → #{} {url}", pr.number);
    }

    #[test]
    fn titles() {
        assert_eq!(humanize("feat/git-redesign"), "Git redesign");
        assert_eq!(humanize("fix_thing"), "Fix thing");
        assert_eq!(humanize("x"), "X");
    }
}
