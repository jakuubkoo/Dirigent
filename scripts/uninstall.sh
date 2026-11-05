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
lib="$PREFIX/lib/dirigent"
bin="$PREFIX/bin"
state="${XDG_STATE_HOME:-$HOME/.local/state}/dirigent"

if [[ -x "$lib/dirigent" ]]; then
    XDG_DATA_HOME="$data" "$lib/dirigent" --uninstall-desktop
else
    rm -fv "$data/applications/dirigent.desktop" "$data/icons/hicolor/scalable/apps/dirigent.svg"
fi
# Only our symlink, never someone else's dirigent.
if [[ -L "$bin/dirigent" && "$(readlink "$bin/dirigent")" == "../lib/dirigent/dirigent" ]]; then
    rm -v "$bin/dirigent"
fi
rm -fv "$lib/dirigent" "$lib/dirigent-hook"
rmdir "$lib" 2>/dev/null || true

if ((purge)); then
    rm -rfv "$state"
else
    echo "Kept your state in $state (run with --purge to remove it)."
fi
if pgrep -x dirigent >/dev/null; then
    echo "dirigent is still running: that window keeps working until you close it."
fi
