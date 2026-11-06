#!/usr/bin/env bash
# End-to-end check of the .deb and the APT repository, with a throwaway signing key (never a real one):
# a clean Ubuntu container adds the repo the way the README says, installs dirigent and checks it, then a newer
# version is published under the same URL and `apt upgrade` must pick it up. A repo signed by another key must be
# refused.
#
#   scripts/test-apt-repo.sh <dirigent .deb> <work dir> [image ...]   (default images: ubuntu:22.04 ubuntu:24.04)
#
# Needs docker, gpg, dpkg-deb, python3, and apt-ftparchive (or APT_FTPARCHIVE pointing at a wrapper).
set -euo pipefail
umask 022

deb="$(realpath "$1")"
work="$(mkdir -p "$2" && cd "$2" && pwd)"
shift 2
images=("$@")
((${#images[@]})) || images=(ubuntu:22.04 ubuntu:24.04)
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
port=8765

rm -rf "${work:?}"/*
export GNUPGHOME="$work/gnupg"
mkdir -m 700 "$GNUPGHOME"
key() { # a fresh signing key; prints its fingerprint
    gpg --batch --passphrase '' --quick-gen-key "$1" ed25519 sign 1d 2>/dev/null
    gpg --list-keys --with-colons "$1" | awk -F: '/^fpr/ {print $10; exit}'
}
fpr="$(key "Dirigent test repo <test@invalid>")"
gpg --armor --export "$fpr" > "$work/test-key.asc"
bad="$(key "Someone else <bad@invalid>")"
gpg --armor --export "$bad" > "$work/bad-key.asc"

# The same package one version up (same binaries): what a new release looks like to apt.
version="$(dpkg-deb -f "$deb" Version)"
newer="$version+1~test"
mkdir -p "$work/v1" "$work/v2" "$work/x"
cp "$deb" "$work/v1/"
dpkg-deb -R "$deb" "$work/x/pkg"
sed -i "s/^Version: .*/Version: $newer/" "$work/x/pkg/DEBIAN/control"
dpkg-deb --root-owner-group -Zxz --build "$work/x/pkg" "$work/v2/dirigent_${newer}_amd64.deb" >/dev/null
cp "$deb" "$work/v2/"

url="http://127.0.0.1:$port/repo"
SITE_URL="$url" "$here/apt-repo.sh" "$work/v1" "$work/site1" "$fpr" "$work/test-key.asc"
SITE_URL="http://127.0.0.1:$port/bad" "$here/apt-repo.sh" "$work/v1" "$work/www/bad" "$bad" "$work/bad-key.asc"
ln -sfn ../site1 "$work/www/repo"

python3 -m http.server "$port" --bind 127.0.0.1 --directory "$work/www" >"$work/http.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT

# Runs inside the container; waits for the host to publish the newer version between the two phases.
cat > "$work/client.sh" <<'EOF'
#!/bin/bash
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
url="$1" newer="$2"
apt-get update -qq && apt-get install -yqq --no-install-recommends curl ca-certificates >/dev/null
# As in the README.
install -d -m 0755 /etc/apt/keyrings
curl -fsSLo /etc/apt/keyrings/dirigent.gpg "$url/dirigent.gpg"
curl -fsSLo /etc/apt/sources.list.d/dirigent.sources "$url/dirigent.sources"

# A repo signed by another key is refused.
sed "s#$url#${url%/repo}/bad#" /etc/apt/sources.list.d/dirigent.sources > /etc/apt/sources.list.d/bad.sources
if apt-get update -qq 2>/tmp/bad.err; then echo "FAIL: a repo signed by another key was accepted"; exit 1; fi
grep -q "NO_PUBKEY\|not signed\|BADSIG\|EXPKEYSIG\|signatures were invalid" /tmp/bad.err
rm /etc/apt/sources.list.d/bad.sources
echo "ok: wrong key refused"

apt-get update -qq
apt-get install -yqq dirigent >/dev/null
dirigent --version
test "$(readlink -f "$(command -v dirigent)")" = /usr/lib/dirigent/dirigent
helper="$(dirname "$(readlink -f "$(command -v dirigent)")")/dirigent-hook"
test -x "$helper"
"$helper" /nonexistent.sock 1 hook </dev/null # always exits 0, even with no app listening
grep -q '^Exec="/usr/lib/dirigent/dirigent"$' /usr/share/applications/dirigent.desktop
test -f /usr/share/icons/hicolor/scalable/apps/dirigent.svg
echo "ok: installed $(dpkg-query -W -f='${Version}' dirigent), helper next to the app"

touch /t/phase1-done
for _ in $(seq 120); do [ -e /t/phase2-ready ] && break; sleep 1; done
[ -e /t/phase2-ready ]
apt-get update -qq
apt-get upgrade -yqq >/dev/null
got="$(dpkg-query -W -f='${Version}' dirigent)"
if [ "$got" != "$newer" ]; then
    echo "FAIL: after apt upgrade dirigent is $got, expected $newer"
    apt-cache policy dirigent
    apt-get -s upgrade
    exit 1
fi
echo "ok: apt upgrade → $newer"
EOF
chmod +x "$work/client.sh"

for image in "${images[@]}"; do
    rm -f "$work/phase1-done" "$work/phase2-ready"
    ln -sfn ../site1 "$work/www/repo"
    echo "== $image"
    docker run --rm --network host -v "$work:/t" "$image" bash /t/client.sh "$url" "$newer" &
    client=$!
    for _ in $(seq 600); do [[ -e $work/phase1-done ]] && break; kill -0 $client 2>/dev/null || break; sleep 0.5; done
    if [[ -e $work/phase1-done ]]; then
        # Publish the newer version under the same URL, built only now like a real release: built together with
        # site1, its files would share a second with site1's and http.server would answer apt's If-Modified-Since
        # with 304 (it compares whole seconds), leaving the old index in place.
        sleep 1
        SITE_URL="$url" "$here/apt-repo.sh" "$work/v2" "$work/site2" "$fpr" "$work/test-key.asc"
        ln -sfn ../site2 "$work/www/repo"
        touch "$work/phase2-ready"
    fi
    wait $client
done
echo "all good"
