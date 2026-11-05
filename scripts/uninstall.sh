#!/usr/bin/env bash
# Removes what scripts/install.sh installed: the binaries, the bin symlink, the desktop entry and icon.
# Keeps your state file (open tabs, settings) unless you pass --purge.
#
# Env: PREFIX (default ~/.local; when set, the desktop entry is looked for in $PREFIX/share too).
set -euo pipefail

purge=0
case "${1:-}" in
    "") ;;
    --purge) purge=1 ;;
    *) echo "usage: $0 [--purge]" >&2; exit 2 ;;
esac
if [[ -n "${PREFIX:-}" ]]; then
    data="$PREFIX/share"
else
    PREFIX="$HOME/.local"
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
lib="$PREFIX/lib/agent-ide"
bin="$PREFIX/bin"
state="${XDG_STATE_HOME:-$HOME/.local/state}/agent-ide"

if [[ -x "$lib/agent-ide" ]]; then
    XDG_DATA_HOME="$data" "$lib/agent-ide" --uninstall-desktop
else
    rm -fv "$data/applications/agent-ide.desktop" "$data/icons/hicolor/scalable/apps/agent-ide.svg"
fi
# Only our symlink, never someone else's agent-ide.
if [[ -L "$bin/agent-ide" && "$(readlink "$bin/agent-ide")" == "../lib/agent-ide/agent-ide" ]]; then
    rm -v "$bin/agent-ide"
fi
rm -fv "$lib/agent-ide" "$lib/agent-ide-hook"
rmdir "$lib" 2>/dev/null || true

if ((purge)); then
    rm -rfv "$state"
else
    echo "Kept your state in $state (run with --purge to remove it)."
fi
if pgrep -x agent-ide >/dev/null; then
    echo "agent-ide is still running: that window keeps working until you close it."
fi
