#!/usr/bin/env bash
# Builds the APT repository site from a directory of .debs: pool, Packages, a signed Release (InRelease +
# Release.gpg), the public key and the install instructions. Stateless: run it on all the .debs to publish.
#
#   scripts/apt-repo.sh <debs dir> <site dir> <signing key fingerprint> <public key .asc>
#
# The secret key must be in the gpg keyring ($GNUPGHOME). SITE_URL (default the GitHub Pages URL) goes into the
# instructions. Needs apt-utils (apt-ftparchive; APT_FTPARCHIVE overrides the command) and gpg.
set -euo pipefail
umask 022

if (($# != 4)); then
    echo "usage: apt-repo.sh <debs dir> <site dir> <key fingerprint> <public key .asc>" >&2
    exit 2
fi
# Absolute paths: the script works inside the site directory later on.
debs="$(realpath "$1")" site="$(realpath -m "$2")" fpr="$3" pub="$(realpath "$4")"
url="${SITE_URL:-https://jakuubkoo.github.io/Dirigent}"

rm -rf "$site"
mkdir -p "$site/pool/main/d/dirigent" "$site/dists/stable/main/binary-amd64"
shopt -s nullglob
files=("$debs"/*.deb)
((${#files[@]})) || { echo "no .deb in $debs" >&2; exit 1; }
cp "${files[@]}" "$site/pool/main/d/dirigent/"

cd "$site"
"${APT_FTPARCHIVE:-apt-ftparchive}" packages pool > dists/stable/main/binary-amd64/Packages
gzip -9nk dists/stable/main/binary-amd64/Packages
"${APT_FTPARCHIVE:-apt-ftparchive}" \
    -o APT::FTPArchive::Release::Origin=Dirigent \
    -o APT::FTPArchive::Release::Label=Dirigent \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures=amd64 \
    -o APT::FTPArchive::Release::Components=main \
    -o "APT::FTPArchive::Release::Description=Dirigent (https://github.com/jakuubkoo/Dirigent)" \
    release dists/stable > Release.tmp # outside dists/stable, or it would list itself
mv Release.tmp dists/stable/Release
gpg --batch --yes --local-user "$fpr" --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --batch --yes --local-user "$fpr" --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release

# What users download: the key (binary, for Signed-By) and the source entry.
gpg --batch --dearmor < "$pub" > dirigent.gpg
cat > dirigent.sources <<EOF
Types: deb
URIs: $url
Suites: stable
Components: main
Architectures: amd64
Signed-By: /etc/apt/keyrings/dirigent.gpg
EOF
cat > index.html <<EOF
<!doctype html>
<meta charset="utf-8">
<title>Dirigent APT repository</title>
<h1>Dirigent APT repository</h1>
<p>Ubuntu 22.04+ / Debian 12+, amd64. Signing key fingerprint: <code>$fpr</code></p>
<pre>
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSLo /etc/apt/keyrings/dirigent.gpg $url/dirigent.gpg
sudo curl -fsSLo /etc/apt/sources.list.d/dirigent.sources $url/dirigent.sources
sudo apt update &amp;&amp; sudo apt install dirigent
</pre>
<p>More: <a href="https://github.com/jakuubkoo/Dirigent#install">github.com/jakuubkoo/Dirigent</a></p>
EOF
echo "built $site: ${files[*]##*/}"
