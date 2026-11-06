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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    None,
    Pending,
    Passing,
    Failing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pr {
    pub number: u64,
    pub state: State,
    pub checks: Checks,
    pub url: String,
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
    let Some((_, host)) = target(root, branch) else { return View { forge: Forge::Other, info: Info::Hidden, pushed } };
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
                    Ok(o) => {
                        Info::Known(if forge == Forge::GitHub { parse_gh(&o.stdout) } else { parse_glab(&o.stdout) })
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
    View { forge: host.forge, info, pushed }
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
    let checks = v["statusCheckRollup"].as_array().map_or(Checks::None, |runs| {
        let each = runs.iter().map(|r| {
            // CheckRun: status + conclusion; StatusContext: state.
            let s = r["status"].as_str().or(r["state"].as_str()).unwrap_or("");
            let c = r["conclusion"].as_str().unwrap_or(s);
            match (s, c) {
                (_, "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "ERROR") => {
                    Checks::Failing
                }
                ("QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED" | "EXPECTED", _) => Checks::Pending,
                _ => Checks::Passing,
            }
        });
        summarize(each)
    });
    Some(Pr { number: v["number"].as_u64()?, state, checks, url: v["url"].as_str()?.to_string() })
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
    let pipeline = v["head_pipeline"]["status"].as_str().or(v["pipeline"]["status"].as_str());
    let checks = match pipeline {
        None => Checks::None,
        Some("success" | "skipped" | "manual") => Checks::Passing,
        Some("failed" | "canceled") => Checks::Failing,
        Some(_) => Checks::Pending,
    };
    Some(Pr { number: v["iid"].as_u64()?, state, checks, url: v["web_url"].as_str()?.to_string() })
}

fn summarize(each: impl Iterator<Item = Checks>) -> Checks {
    each.fold(Checks::None, |acc, c| match (acc, c) {
        (Checks::Failing, _) | (_, Checks::Failing) => Checks::Failing,
        (Checks::Pending, _) | (_, Checks::Pending) => Checks::Pending,
        _ => Checks::Passing,
    })
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

    #[test]
    fn gh_real_output() {
        // Captured from `gh pr view 8 --json number,state,isDraft,url,statusCheckRollup` (gh 2.46, this repo).
        let pr = parse_gh(include_str!("../tests/fixtures/gh_pr_view_merged.json")).unwrap();
        assert_eq!((pr.number, pr.state, pr.checks), (8, State::Merged, Checks::Passing));
        assert!(pr.url.ends_with("/pull/8"));
        let one = |s: &str| {
            parse_gh(&format!(r#"{{"number":1,"state":"OPEN","isDraft":false,"url":"u","statusCheckRollup":[{s}]}}"#))
                .unwrap()
                .checks
        };
        assert_eq!(
            one(r#"{"status":"IN_PROGRESS","conclusion":""},{"status":"COMPLETED","conclusion":"SUCCESS"}"#),
            Checks::Pending
        );
        assert_eq!(one(r#"{"status":"COMPLETED","conclusion":"FAILURE"},{"status":"QUEUED"}"#), Checks::Failing);
        assert_eq!(one(r#"{"state":"SUCCESS"}"#), Checks::Passing);
        assert_eq!(one(""), Checks::None);
        let draft = parse_gh(r#"{"number":2,"state":"OPEN","isDraft":true,"url":"u","statusCheckRollup":[]}"#).unwrap();
        assert_eq!(draft.state, State::Draft);
        assert!(is_none_found("no pull requests found for branch \"x\""));
    }

    #[test]
    fn glab_output() {
        let v = r#"{"iid":12,"state":"opened","draft":false,"web_url":"https://gl/x/-/merge_requests/12","head_pipeline":{"status":"running"}}"#;
        let pr = parse_glab(v).unwrap();
        assert_eq!((pr.number, pr.state, pr.checks), (12, State::Open, Checks::Pending));
        let merged = parse_glab(r#"{"iid":3,"state":"merged","web_url":"u"}"#).unwrap();
        assert_eq!((merged.state, merged.checks), (State::Merged, Checks::None));
    }

    /// Live: `DIRIGENT_LIVE_REPO=<clone of a GitLab test project>` (credentials from your git setup or
    /// GIT_CONFIG_* env), `cargo test -p dirigent gitlab_live -- --ignored`. Pushes two branches and opens one MR.
    #[test]
    #[ignore]
    fn gitlab_live() {
        let Ok(repo) = std::env::var("DIRIGENT_LIVE_REPO") else { return };
        let root = std::path::Path::new(&repo);
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
        assert_eq!(note, "Merge request created");
        // Nothing new to push: push options would be dropped, so the new-MR form opens instead.
        let (url, _) = create(root, &Cancel::default()).unwrap();
        assert!(url.contains("/-/merge_requests/new?"), "{url}");
        let v = lookup(root, &branch, true);
        assert_eq!(v.forge, Forge::GitLab);
        assert!(v.pushed);
        eprintln!("live: {branch} → {:?}", v.info);
    }

    #[test]
    fn titles() {
        assert_eq!(humanize("feat/git-redesign"), "Git redesign");
        assert_eq!(humanize("fix_thing"), "Fix thing");
        assert_eq!(humanize("x"), "X");
    }
}
