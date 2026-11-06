# Changelog

All notable changes to Dirigent are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- New session dialog: a branch prefix dropdown (`feat/`, `fix/`, `chore/`, `docs/`, none) next to the branch field,
  `feat/` by default and remembered per project. The slug made from the session name never takes a `/` from it
  (`esc-ctrl-c`, not `esc/ctrl-c`).
- New session dialog: while Name is empty, the first prompt names the session and its branch as you type (its first
  words at once, then `claude -p` on Haiku off the UI thread: no tools, no saved session). A name you type stays;
  a `Branch: x` line in the prompt gives the branch as written.

### Changed

- New session dialog: the first prompt is multi-line and wraps (3 to 10 lines, then it scrolls). Enter adds a line
  there, Ctrl+Enter creates the session. The prompt reaches claude as written (new lines and quotes intact, whatever
  your shell): it goes through a file in the runtime folder that the `claude` shim reads and deletes, never through
  the typed command.
- Every single-line field (dialogs, search, find, settings): a click places the cursor, drag or Shift+click selects,
  a double click selects a word, the field scrolls to keep the cursor visible, and Ctrl+C / X / V / A, Ctrl+Z / Y
  (undo, redo) and Ctrl+←/→ (word moves) work.

## [0.3.0] - 2026-11-06

Find in the terminal, check progress in the PR/MR strip, and a round of fixes.

### Added

- **Find in the terminal** (Ctrl+Alt+F): searches the screen and the whole scrollback (also in full-screen apps),
  highlights every match, Enter / Shift+Enter step to the next / previous one, match case and regex toggles, Esc
  closes. Not Ctrl+F: the shell, Claude Code and less/vim use it.

### Changed

- The PR/MR strip shows check progress ("checks 3/5 · 1 failing", "all 5 passed") with each check's state and
  duration in a tooltip; clicking it opens the checks page. While checks run and the Changes panel is visible, the
  strip refreshes every 30 s. GitLab (with `glab`) lists the pipeline's jobs.
- The `+` on a project group header opens the New session setup dialog (name, git worktree, first prompt) for that
  project; Shift+click starts a session right away. The old instant behavior stays available as "Plain + opens" in
  Settings (Project default, Shell, Claude).

### Fixed

- Dirigent started from a terminal inside Dirigent used that session's Claude hook file as its settings.json
  ("Settings: 2 problems") and wrote into it. Settings are never read from or written to Dirigent's runtime folder.
- Interrupting Claude with Esc or Ctrl+C now ends the session's "working" state within about a second, instead of
  a minute later. Esc that only closes a Claude menu or switches its vim mode doesn't count.
- Settings: typing into a number field (e.g. Font size) no longer jumps back to the search box. After Enter or a
  click elsewhere the field keeps focus and shows the value just applied (it showed the previous one), and 13.52
  no longer shows as 13.520000457763672.
- Scrollbars in the diff (unified and split), file preview, Changes list, file tree and Search results can be
  dragged again, and clicking their track jumps there. They stopped responding as soon as any other part of the
  window redrew (e.g. a session spinner).
- In a window that isn't maximized (GNOME), the right panel's scrollbar and row buttons no longer sit under the
  window's resize edge, which took the clicks meant for them.

## [0.2.0] - 2026-11-06

A redesigned git workflow (branches, sync, push and pull, pull and merge requests) and sessions that name
themselves.

### Added

- **Automatic session names**
  - Sessions name themselves instead of "Session N": Claude Code's own title (`claude -n`, `/rename`, or the title
    it generates after the first response), else the first prompt shortened to a readable title, else the
    worktree's branch or the directory name.
  - A name you set yourself always wins and is never replaced; "Reset to automatic name" in the session menu
    brings the automatic one back.
  - A new Claude session in the same tab (a new `claude`, `/clear`) may update the automatic name.
  - Long names are cut with an ellipsis; the full name shows in a tooltip.
- **Branches** (Ctrl+Shift+O): switch, search, create, or check out a remote branch. Uncommitted changes that
  would move along ask first. The status bar shows the branch with ahead/behind counts.
- **Sync**: one button in the Changes panel that does the safe next step: Publish, Push, Pull, or tells you the
  branch has diverged or is up to date. Fetch, pull and push are also in the panel's ⋯ menu.
  - Pull is fast-forward only; Sync never merges or rebases.
  - Network commands run without a terminal, never prompt or store credentials, and can be cancelled (120 s
    timeout).
  - Optional background fetch (`git.fetch_minutes` in settings.json, off by default).
- **Pull requests and merge requests**
  - GitHub: with `gh` logged in, the panel shows the PR's number, state and checks, and creates a PR; without it,
    it opens the compare page.
  - GitLab: the merge request is created by the push itself; the state comes from `glab` when installed.
  - Commit & push and Commit, push & create PR from the commit button's menu.
- **Host mapping**: `git.hosts` in settings.json maps an SSH host (for example a self-hosted GitLab behind a
  separate SSH name) to its forge and web address.

### Changed

- A cleaner Changes panel: a repo bar with the branch, Sync and a ⋯ menu; checkboxes that stage and unstage; one
  commit button that follows the "This agent | All" choice; one view menu in the diff header.
- When a branch's remote was deleted (usually after a merge), Sync, Push and Create PR refuse to publish it again
  and offer "Switch to the default branch and pull" instead.
- settings.json keeps sections and keys it doesn't know, so an older or newer version of Dirigent never deletes
  your settings.

### Fixed

- The dock width fields in Settings → Layout can be typed into again.
- The APT repository build no longer fails when the release workflow passes relative paths.

## [0.1.1] - 2026-11-06

Install and update Dirigent with apt.

### Added

- A `.deb` package (`/usr/lib/dirigent`, `/usr/bin/dirigent`, a launcher entry and icon, runtime dependencies
  computed from the binaries), attached to each release.
- An APT repository on GitHub Pages, signed and rebuilt by every release: `sudo apt update && sudo apt upgrade`
  updates Dirigent.
- A hint at startup, and from `install.sh`, when a user-local install and the package are both installed.

## [0.1.0] - 2026-11-06

The first release: a native Linux app (Wayland and X11) for running many Claude Code sessions side by side.

### Added

- **Sessions**
  - One window, many terminal sessions: real PTYs rendered on the GPU with gpui.
  - Session list grouped by project; reorder sessions by dragging.
  - Rename sessions.
  - Context menu: close, restart, duplicate, resume, mark as read, copy path, open the folder.
- **Claude Code agents**
  - Per-session agent states (working, needs you, done) through hooks, without touching `~/.claude/settings.json`.
  - Claude usage limits in the status bar.
  - Desktop notifications that bring the window to the front when clicked.
  - Ctrl+Shift+J jumps to the next session that needs you.
- **Persistence**
  - Sessions, layout and window are restored after a restart.
  - Claude sessions are resumed with a safe set of their flags; permission bypasses are never replayed.
- **Projects and worktrees**
  - Fuzzy project picker with path completion.
  - Pinned projects.
  - Per-project defaults: start claude, its flags, a setup command.
  - A setup dialog that can give a session its own git worktree; when the last session closes, the worktree is kept or safely removed.
- **Git**
  - Changes panel that follows each shell's directory.
  - Diffs: unified or split, word-level, syntax-highlighted, and structural (moved code, reformatting ignored).
  - Per-agent change tracking: which files each agent changed.
  - Stage, unstage and discard per file or hunk.
  - Commit, amend, or commit only an agent's files.
  - Optional commit messages generated by `claude -p`.
- **Files**
  - Lazy, gitignore-aware file tree.
  - Read-only file preview with syntax highlighting.
- **Command Center** (title bar)
  - Quick open (Ctrl+P).
  - Command palette (`>`, Ctrl+Shift+P).
  - Search in files (`#`, Ctrl+Shift+F) with a Search panel.
  - Go to line (`:`).
- **Terminal**
  - Mouse selection: characters, words, lines.
  - Copy and paste with Ctrl+Shift+C and Ctrl+Shift+V; plain Ctrl+V still reaches the terminal.
  - Mouse reporting for terminal apps.
  - Ctrl+click opens links and `path:line` locations.
- **Layout**
  - Two docks around the terminal; Sessions, Files, Changes and Search can each live in either dock.
  - Move panels by dragging a tab or from its menu.
  - Collapse and resize each dock.
- **Settings**
  - A searchable page (Ctrl+,) over `settings.json`, with live hand edits.
  - UI zoom (Ctrl+= and Ctrl+-).
  - Terminal font size and scrollback.
  - Notifications, diff options and layout.
- **Command line**
  - `dirigent [--new] [--claude] [PATH]` opens a session in the running window (single instance).
  - `--install-desktop` adds a launcher entry and icon.
- **Releases**
  - Prebuilt Linux x86_64 tarballs with `install.sh --prebuilt` and SHA256 checksums.
  - CI checks formatting, clippy, tests, licenses and RustSec advisories.

[Unreleased]: https://github.com/jakuubkoo/Dirigent/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/jakuubkoo/Dirigent/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jakuubkoo/Dirigent/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/jakuubkoo/Dirigent/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jakuubkoo/Dirigent/releases/tag/v0.1.0
