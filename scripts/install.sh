#!/usr/bin/env bash
# Builds dirigent (release) and installs it for this user. Re-run after `git pull` to update.
#
#   install.sh               build from this checkout, then install
#   install.sh --prebuilt    install the binaries next to this script (a release tarball), no Rust needed
#   install.sh --print-deps  print the apt build packages (CI installs them from here)
#
#   ~/.local/lib/dirigent/dirigent        the app
#   ~/.local/lib/dirigent/dirigent-hook   its Claude Code hook helper (must sit next to the app)
#   ~/.local/bin/dirigent                  symlink to the app
#   ~/.local/share/applications/dirigent.desktop + the icon (via `dirigent --install-desktop`)
#
# Safe while the app runs: binaries are replaced by rename, the running app keeps its old copy until restarted.
# Never runs sudo: missing build packages are printed as an apt command.
#
# Env: PREFIX (default ~/.local; when set, the desktop entry goes to $PREFIX/share too: for test installs).
#      DIRIGENT_DEPS_EXTRA: extra package names to check (testing the missing-package path).
set -euo pipefail

mode=build
case "${1:-}" in
    "") ;;
    --prebuilt) mode=prebuilt ;;
    --print-deps) mode=deps ;;
    *)
        echo "usage: install.sh [--prebuilt | --print-deps]" >&2
        exit 2
        ;;
esac
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pkgs=(build-essential clang lld cmake pkg-config libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev
      libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan1 mesa-vulkan-drivers libzstd-dev libssl-dev
      libasound2-dev)
if [[ $mode == deps ]]; then
    echo "${pkgs[*]}"
    exit 0
fi
if [[ -n "${PREFIX:-}" ]]; then
    data="$PREFIX/share"
else
    PREFIX="$HOME/.local"
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
lib="$PREFIX/lib/dirigent"
bin="$PREFIX/bin"

if [[ $mode == prebuilt ]]; then
    src="$here"
    for b in dirigent dirigent-hook; do
        [[ -x "$src/$b" ]] || { echo "$src/$b not found: --prebuilt runs from an unpacked release" >&2; exit 1; }
    done
    # A release binary needs the desktop's graphics and font libraries: say which are missing before installing.
    missing="$(ldd "$src/dirigent" | awk '/not found/ {print $1}' | tr '\n' ' ')"
    if [[ -n $missing ]]; then
        echo "Missing libraries: $missing" >&2
        echo "On Ubuntu/Debian: sudo apt install libxkbcommon0 libxkbcommon-x11-0 libvulkan1 mesa-vulkan-drivers" \
            "libfontconfig1 libfreetype6 libwayland-client0 libx11-xcb1 libxcb1 libzstd1 libasound2" >&2
        exit 1
    fi
else
    src="target/release"
    # --- build dependencies ---------------------------------------------------------------------------------------
    if ! command -v cargo >/dev/null; then
        echo "cargo not found. Install Rust from https://rustup.rs and open a new shell." >&2
        exit 1
    fi
    read -ra extra <<< "${DIRIGENT_DEPS_EXTRA:-}"
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
    cd "$here/.."
    if git rev-parse --git-dir >/dev/null 2>&1; then
        hash="$(git rev-parse --short HEAD)"
        git diff --quiet HEAD -- 2>/dev/null || hash="$hash-dirty"
        DIRIGENT_BUILD="$hash $(git log -1 --format=%cs)"
    else
        DIRIGENT_BUILD="unknown $(date +%F)"
    fi
    export DIRIGENT_BUILD
    cargo build --release --locked
fi

# --- install (atomic: copy next to the target, then rename over it) -------------------------------------------
mkdir -p "$lib" "$bin"
put() {
    local tmp="$lib/.$1.new.$$"
    cp "$src/$1" "$tmp"
    if command -v strip >/dev/null; then strip "$tmp"; fi
    chmod 755 "$tmp"
    mv -f "$tmp" "$lib/$1"
}
put dirigent-hook # the helper first: a new app never runs with an old helper
put dirigent
ln -sfn "../lib/dirigent/dirigent" "$bin/dirigent"
XDG_DATA_HOME="$data" "$lib/dirigent" --install-desktop

# --- the app was called agent-ide: remove that install (exact paths only) -------------------------------------
old_lib="$PREFIX/lib/agent-ide"
if [[ -L "$bin/agent-ide" && "$(readlink "$bin/agent-ide")" == "../lib/agent-ide/agent-ide" ]]; then
    rm -v "$bin/agent-ide"
fi
rm -fv "$old_lib/agent-ide" "$old_lib/agent-ide-hook" "$data/applications/agent-ide.desktop" \
    "$data/icons/hicolor/scalable/apps/agent-ide.svg"
rmdir "$old_lib" 2>/dev/null || true
if pgrep -x agent-ide >/dev/null; then
    echo "agent-ide (the old name) is still running: close it and start Dirigent. Its sessions keep working until"
    echo "then, but their Claude hooks stopped with the old helper gone. Your state moves over on Dirigent's first start."
fi

echo
echo "Installed $("$lib/dirigent" --version)"
echo "  app:     $lib/dirigent (+ dirigent-hook)"
echo "  command: $bin/dirigent"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "  $bin is not on your PATH: add  export PATH=\"$bin:\$PATH\"  to ~/.zshrc" ;;
esac
if pgrep -x dirigent >/dev/null; then
    echo "dirigent is running: restart it to use the new version."
fi
