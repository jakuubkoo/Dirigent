#!/usr/bin/env bash
# Builds dirigent_<version>_amd64.deb from release binaries, with Debian's own tools (dpkg-deb, dpkg-shlibdeps).
#
#   scripts/build-deb.sh <version> <dir with dirigent + dirigent-hook> <out dir>
#
# Layout (like a user-local install): /usr/lib/dirigent/{dirigent,dirigent-hook}, /usr/bin/dirigent -> the app
# (it finds the helper next to its real path), the launcher entry, the icon, licenses + CHANGELOG in /usr/share/doc/dirigent.
# Depends is computed from the binaries. No maintainer scripts: icon cache / desktop database refresh through the
# hicolor-icon-theme and desktop-file-utils triggers. Needs dpkg-dev.
set -euo pipefail
umask 022

if (($# != 3)); then
    echo "usage: build-deb.sh <version> <bin dir> <out dir>" >&2
    exit 2
fi
version="$1"
bins="$(cd "$2" && pwd)"
mkdir -p "$3"
out="$(cd "$3" && pwd)"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
root="$work/root"
doc="$root/usr/share/doc/dirigent"
mkdir -p "$root/DEBIAN" "$root/usr/lib/dirigent" "$root/usr/bin" "$root/usr/share/applications" \
    "$root/usr/share/icons/hicolor/scalable/apps" "$root/usr/share/icons/hicolor/256x256/apps" \
    "$doc/licenses/fonts" "$doc/licenses/icons" "$doc/licenses/grammars"

for b in dirigent dirigent-hook; do
    install -m 0755 "$bins/$b" "$root/usr/lib/dirigent/$b"
    strip --strip-unneeded "$root/usr/lib/dirigent/$b"
done
ln -s ../lib/dirigent/dirigent "$root/usr/bin/dirigent"
# The launcher starts the packaged app even when ~/.local/bin/dirigent (a user-local install) shadows /usr/bin.
"$bins/dirigent" --desktop-entry /usr/lib/dirigent/dirigent > "$root/usr/share/applications/dirigent.desktop"
install -m 0644 "$repo/crates/app/assets/icon/dirigent.svg" "$root/usr/share/icons/hicolor/scalable/apps/dirigent.svg"
install -m 0644 "$repo/crates/app/assets/icon/dirigent-256.png" "$root/usr/share/icons/hicolor/256x256/apps/dirigent.png"

{
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: Dirigent"
    echo "Source: https://github.com/jakuubkoo/Dirigent"
    echo "Comment: Bundled third-party code and assets: THIRD-PARTY-NOTICES.md.gz and licenses/ in this directory."
    echo
    echo "Files: *"
    echo "Copyright: Jakub Holas"
    echo "License: MIT"
    sed -e 's/^$/./' -e 's/^/ /' "$repo/LICENSE"
} > "$doc/copyright"
gzip -9n < "$repo/CHANGELOG.md" > "$doc/CHANGELOG.md.gz"
# A native package needs a Debian-format changelog: one entry pointing at ours.
when="$(git -C "$repo" log -1 --format=%ct 2>/dev/null || date +%s)"
printf 'dirigent (%s) stable; urgency=medium\n\n  * Release %s: see CHANGELOG.md.gz.\n\n -- %s  %s\n' \
    "$version" "$version" "Jakub Holas <33603479+jakuubkoo@users.noreply.github.com>" "$(LC_ALL=C date -R -u -d "@$when")" \
    | gzip -9n > "$doc/changelog.gz"
gzip -9n < "$repo/THIRD-PARTY-NOTICES.md" > "$doc/THIRD-PARTY-NOTICES.md.gz"
install -m 0644 "$repo"/crates/app/assets/fonts/*.txt "$doc/licenses/fonts/"
install -m 0644 "$repo/crates/app/assets/icons/LICENSE.txt" "$doc/licenses/icons/"
install -m 0644 "$repo"/crates/syntax/licenses/* "$doc/licenses/grammars/"

# Runtime libraries from the binaries (dpkg-shlibdeps wants a debian/control next to it).
mkdir -p "$work/src/debian"
printf 'Source: dirigent\n\nPackage: dirigent\nArchitecture: amd64\n' > "$work/src/debian/control"
depends="$(cd "$work/src" && dpkg-shlibdeps -O "$root/usr/lib/dirigent/dirigent" "$root/usr/lib/dirigent/dirigent-hook" \
    | sed -n 's/^shlibs:Depends=//p')"
[[ -n $depends ]] || { echo "dpkg-shlibdeps found no dependencies" >&2; exit 1; }
# Loaded with dlopen at runtime, so shlibdeps can't see them: gpui renders through Vulkan (any driver providing
# vulkan-icd) and talks Wayland directly.
depends="$depends, libvulkan1, libwayland-client0, mesa-vulkan-drivers | vulkan-icd"

cat > "$root/DEBIAN/control" <<EOF
Package: dirigent
Version: $version
Architecture: amd64
Maintainer: Jakub Holas <33603479+jakuubkoo@users.noreply.github.com>
Installed-Size: $(du -sk --exclude=DEBIAN "$root" | cut -f1)
Depends: $depends
Recommends: git, libegl1, libwayland-egl1
Section: devel
Priority: optional
Homepage: https://github.com/jakuubkoo/Dirigent
Description: Many Claude Code sessions in one native window
 Dirigent runs many Claude Code sessions side by side, each in a real terminal.
 It shows which agent needs you, what each agent changed in git, and lets you
 stage, commit or give an agent its own git worktree. GPU-rendered with gpui,
 no web view; Wayland and X11.
EOF

deb="$out/dirigent_${version}_amd64.deb"
dpkg-deb --root-owner-group -Zxz --build "$root" "$deb" >/dev/null
echo "$deb"
