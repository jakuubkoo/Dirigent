# agent-ide

A native desktop IDE (Rust + gpui) for running many Claude Code sessions at once: terminal sessions in a sidebar,
agent states and usage limits, a git panel with diffs, staging and commits, a file tree, and desktop notifications.
Linux (Ubuntu, Wayland and X11) for now.

## Install

```sh
scripts/install.sh
```

It checks the build dependencies first and, if something is missing, prints the exact `sudo apt install …` line
(it never runs sudo itself). Then it builds a release and installs:

| Path | What |
|---|---|
| `~/.local/lib/agent-ide/agent-ide` | the app |
| `~/.local/lib/agent-ide/agent-ide-hook` | Claude Code hook helper (must stay next to the app) |
| `~/.local/bin/agent-ide` | symlink to the app |
| `~/.local/share/applications/agent-ide.desktop` + icon | GNOME launcher entry |

`agent-ide --version` prints the installed build (git hash and commit date).

## Update

```sh
git pull && scripts/install.sh
```

Safe while the app is running: binaries are replaced atomically and the running window keeps its old copy.
Restart the app to use the new version.

## Uninstall

```sh
scripts/uninstall.sh          # keeps your state file
scripts/uninstall.sh --purge  # also removes it
```

## State

Open tabs, layout and settings live in `~/.local/state/agent-ide/state.json` (`$XDG_STATE_HOME`), or in the file
named by `AGENT_IDE_STATE`. Your `~/.claude/settings.json` is never modified.

## Shortcuts

| Keys | Action |
|---|---|
| Ctrl+Shift+T | New session |
| Ctrl+Shift+W | Close session |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous session |
| Ctrl+1..9 | Jump to session |
| Ctrl+Shift+J | Next session that needs you |
| Ctrl+Shift+R | Resume all saved Claude sessions |
| F2 / double click | Rename session |
| Ctrl+Shift+B | Toggle sidebar |
| Ctrl+Shift+G / Ctrl+Shift+E | Show Changes / Files |
| Ctrl+Shift+D | Unified / split diff |
| Ctrl+Enter | Commit (in the commit box) |
| Esc | Close a diff or preview |
