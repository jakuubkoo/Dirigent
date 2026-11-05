#!/usr/bin/env bash
# Builds agent-ide (release) and installs it for this user. Re-run after `git pull` to update.
#
#   ~/.local/lib/agent-ide/agent-ide        the app
#   ~/.local/lib/agent-ide/agent-ide-hook   its Claude Code hook helper (must sit next to the app)
#   ~/.local/bin/agent-ide                  symlink to the app
#   ~/.local/share/applications/agent-ide.desktop + the icon (via `agent-ide --install-desktop`)
#
# Safe while the app runs: binaries are replaced by rename, the running app keeps its old copy until restarted.
# Never runs sudo: missing build packages are printed as an apt command.
#
# Env: PREFIX (default ~/.local; when set, the desktop entry goes to $PREFIX/share too: for test installs).
#      AGENT_IDE_DEPS_EXTRA: extra package names to check (testing the missing-package path).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -n "${PREFIX:-}" ]]; then
    data="$PREFIX/share"
else
    PREFIX="$HOME/.local"
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
lib="$PREFIX/lib/agent-ide"
bin="$PREFIX/bin"

# --- build dependencies ---------------------------------------------------------------------------------------
if ! command -v cargo >/dev/null; then
    echo "cargo not found. Install Rust from https://rustup.rs and open a new shell." >&2
    exit 1
fi
pkgs=(build-essential clang lld cmake pkg-config libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev
      libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan1 mesa-vulkan-drivers libzstd-dev libssl-dev
      libasound2-dev)
read -ra extra <<< "${AGENT_IDE_DEPS_EXTRA:-}"
pkgs+=("${extra[@]}")
if command -v dpkg-query >/dev/null; then
    missing=()
    for p in "${pkgs[@]}"; do
        dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed" || missing+=("$p")
    done
    if ((${#missing[@]})); then
        echo "Missing build packages. Install them, then run this script again:" >&2
        echo "  sudo apt install ${missing[*]}" >&2
        exit 1
    fi
else
    echo "Not a dpkg system: make sure these (or your distro's equivalents) are installed: ${pkgs[*]}"
fi

# --- build ----------------------------------------------------------------------------------------------------
cd "$repo"
if git rev-parse --git-dir >/dev/null 2>&1; then
    hash="$(git rev-parse --short HEAD)"
    git diff --quiet HEAD -- 2>/dev/null || hash="$hash-dirty"
    AGENT_IDE_BUILD="$hash $(git log -1 --format=%cs)"
else
    AGENT_IDE_BUILD="unknown $(date +%F)"
fi
export AGENT_IDE_BUILD
cargo build --release --locked

# --- install (atomic: copy next to the target, then rename over it) -------------------------------------------
mkdir -p "$lib" "$bin"
put() {
    local tmp="$lib/.$1.new.$$"
    cp "target/release/$1" "$tmp"
    if command -v strip >/dev/null; then strip "$tmp"; fi
    chmod 755 "$tmp"
    mv -f "$tmp" "$lib/$1"
}
put agent-ide-hook # the helper first: a new app never runs with an old helper
put agent-ide
ln -sfn "../lib/agent-ide/agent-ide" "$bin/agent-ide"
XDG_DATA_HOME="$data" "$lib/agent-ide" --install-desktop

echo
echo "Installed $("$lib/agent-ide" --version)"
echo "  app:     $lib/agent-ide (+ agent-ide-hook)"
echo "  command: $bin/agent-ide"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "  $bin is not on your PATH: add  export PATH=\"$bin:\$PATH\"  to ~/.zshrc" ;;
esac
if pgrep -x agent-ide >/dev/null; then
    echo "agent-ide is running: restart it to use the new version."
fi
