# Dirigent

A native desktop app for conducting many [Claude Code](https://claude.com/claude-code) sessions at once. Each
session is a real terminal; Dirigent watches what the agents in them are doing, tells you which one needs you, shows
what each agent changed in git, and lets you stage, commit or put an agent in its own git worktree, without leaving
the window. Written in Rust on [gpui](https://github.com/zed-industries/zed) (GPU-rendered, no web view), built to
stay at zero lag and 0 % CPU when idle. Linux (Wayland and X11) for now.

<!-- TODO: add docs/screenshot.png, then uncomment:
![Dirigent](docs/screenshot.png)
-->

## Features

- **Sessions:** a list grouped by project, agent states (working / needs you / done) with desktop notifications,
  Claude usage limits, sessions restored and resumed after a restart.
- **Projects:** a fuzzy project picker, pinned projects, and per-project defaults (start claude automatically, its flags).
- **Worktrees:** give a session its own branch and directory in one dialog; when you close it, keep it or remove it
  safely.
- **Git:** changes and diffs (word-level, split, syntax-highlighted, structural with moved code), per-agent change
  tracking, staging per file or hunk, commits with an optional generated message.
- **Files:** a file tree and a file preview.
- **Layout:** two docks around the terminal; Sessions, Files, Changes and Search go in either one (drag a tab to the
  other dock, or right click it). Settings → Layout sets docks and widths, and resets them.
- **Settings:** a searchable page (Ctrl+,) over `settings.json`; hand edits of the file apply live.
- **From the terminal:** `dirigent .` opens a session in the running app (single instance).

## Install

Needs Rust (https://rustup.rs) and the build packages; the script checks them first and prints the exact
`sudo apt install …` line if something is missing (it never runs sudo itself).

```sh
git clone https://github.com/jakuubkoo/Dirigent.git
cd Dirigent
scripts/install.sh
```

This builds a release and installs `~/.local/bin/dirigent` (the binaries live in `~/.local/lib/dirigent/`) plus a
launcher entry with its icon. `dirigent --version` shows the installed build.

## Update

```sh
git pull && scripts/install.sh
```

Safe while Dirigent runs: the binaries are replaced atomically, the running window keeps its copy until you
restart it.

## Uninstall

```sh
scripts/uninstall.sh          # keeps your state
scripts/uninstall.sh --purge  # also removes it
```

## Usage

```sh
dirigent                  # start, or bring the running window to the front
dirigent .                # a session in this directory (focuses an existing one there)
dirigent src/main.rs      # a session in the file's directory, the file in the preview
dirigent --new ~/proj     # always a new session
dirigent --claude .       # ... and start claude in it
```

In the app, Ctrl+Shift+T opens the project picker: type to search your projects, or a path (`/`, `~`, `.`) with Tab
completion. Enter opens the setup dialog (name, shared directory or a new worktree, start claude with a first
prompt); Ctrl+Enter opens a session at once. Hover a sidebar group for `+` (new session there) and `⋯` (pin, rename,
defaults, worktrees). Right click a session (or Shift+F10 / the Menu key) for rename, close, restart, duplicate,
resume, mark as read, copy path, open folder and forget the agent's files; drag sessions to reorder them within their
group.

## Shortcuts

| Keys | Action |
|---|---|
| Ctrl+, | Settings |
| Ctrl+= or Ctrl++ / Ctrl+- / Ctrl+0 | Zoom the whole UI in / out / back to 100 % (saved as UI zoom) |
| Ctrl+P | Go to file in the active session's project (`name:42` jumps to a line, Ctrl+Enter opens its diff) |
| Ctrl+Shift+P | Command palette (`>` in the title-bar search box) |
| Ctrl+Shift+F | Search in files (`#`; results in the Search panel; `.*` regex, `Aa` case, `ab` whole word) |
| Ctrl+Shift+T | New session (project picker) |
| Ctrl+Shift+N | New session in the current project |
| Ctrl+Shift+W / middle click / hover x | Close session (asks first while claude runs in it) |
| Shift+F10 / Menu key / right click | Session menu (arrows, Enter, Esc) |
| Drag / double click / triple click | Select text / a word / a line in the terminal (Shift+click extends; hold Shift while an app uses the mouse) |
| Ctrl+Shift+C / Ctrl+Shift+V | Copy the selection / paste text (Ctrl+V stays the app's, e.g. image paste in Claude Code) |
| Middle click | Paste the last selection (primary selection) |
| Ctrl+click | Open a link in the browser, or a path like `src/main.rs:42` in the preview |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous session |
| Ctrl+1..9 | Jump to session |
| Ctrl+Shift+J | Next session that needs you |
| Ctrl+Shift+R | Resume all saved Claude sessions |
| F2 / double click | Rename session |
| Ctrl+Shift+B | Show / hide Sessions (in whichever dock it lives) |
| Ctrl+Shift+G / Ctrl+Shift+E | Show Changes / Files |
| Ctrl+Shift+D | Unified / split diff |
| Ctrl+Enter | Commit (in the commit box) |
| Esc | Close a diff, preview, Settings, menu or dialog |

## State

Preferences live in `~/.config/dirigent/settings.json` (`$XDG_CONFIG_HOME`): change them on the Settings page
(Ctrl+, or the gear) or edit the file, both apply at once. Tabs, layout and projects live in
`~/.local/state/dirigent/state.json` (`$XDG_STATE_HOME`). Your
`~/.claude/settings.json` is never modified: Dirigent passes its hooks to each claude it starts with `--settings`.

## License

MIT, see [LICENSE](LICENSE). Bundled fonts, icons, grammars and linked crates are listed with their licenses in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
