# Releasing

Versions follow [semver](https://semver.org). There is one version for everything: `version` under
`[workspace.package]` in the root `Cargo.toml` (`dirigent --version` shows it with the commit hash and date).

1. On a branch from `main`:
   - bump `version` in `Cargo.toml`, then run `cargo build` so `Cargo.lock` follows;
   - in `CHANGELOG.md`, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, start a new empty
     `## [Unreleased]` above it, and update the links at the bottom.
2. Open a PR and merge it once CI is green.
3. Tag the merge commit on `main` and push the tag:

   ```sh
   git checkout main && git pull --ff-only
   git tag -a vX.Y.Z -m "Dirigent X.Y.Z"
   git push origin vX.Y.Z
   ```

4. The `Release` workflow builds Linux x86_64 binaries and checks that the tag matches the version and that the
   changelog has that section. It then publishes a GitHub Release with
   `dirigent-X.Y.Z-x86_64-linux.tar.gz`, `SHA256SUMS`, and the changelog section as its notes.

The same run attaches `dirigent_X.Y.Z_amd64.deb` to the release. It then rebuilds the APT repository on GitHub
Pages from the `.deb`s of the last 5 releases and signs it with the repository key. Re-running the workflow by hand
(Actions → Release → Run workflow) only rebuilds and redeploys the repository.

A tag from which a release was published is never moved or deleted. If that release turns out broken, fix it on
`main` and release the next patch version. A tag whose Release workflow failed before publishing anything (no GitHub
Release, no APT repository deploy) may be deleted and pushed again once the cause is fixed, for example a tag pushed
before the version bump was merged:

```sh
gh release view vX.Y.Z                  # must say "release not found"
git push origin :refs/tags/vX.Y.Z && git tag -d vX.Y.Z
```

## APT repository key

The repository is signed with a dedicated ed25519 key without expiry. Fingerprint:
`A604D170945C76BBA9E4EE3025E2F2B4838194E1`.

- The private half lives in two places only:
  - the `APT_SIGNING_KEY` secret of the `github-pages` environment, which only the deploying job can read (there is
    no repository-level secret);
  - an encrypted backup.
- The public half is committed as `packaging/dirigent-archive-keyring.asc`.
- The fingerprint is pinned in `.github/workflows/release.yml` (`APT_KEY_FPR`). The workflow refuses to sign unless
  both the committed public key and the secret are that key.

One-time setup, run locally (the key never leaves your machine except into the secret):

```sh
export GNUPGHOME=$(mktemp -d)
gpg --batch --passphrase '' --quick-gen-key "Dirigent APT repository <33603479+jakuubkoo@users.noreply.github.com>" ed25519 sign never
FPR=$(gpg --list-keys --with-colons | awk -F: '/^fpr/ {print $10; exit}')
# The encrypted backup (gpg asks for a passphrase; keep the file and the passphrase apart, off this machine):
gpg --armor --export-secret-keys "$FPR" | gpg --symmetric --armor --output dirigent-apt-key.asc.gpg
gpg --armor --export-secret-keys "$FPR" | gh secret set APT_SIGNING_KEY --env github-pages --repo jakuubkoo/Dirigent
gpg --armor --export "$FPR" > packaging/dirigent-archive-keyring.asc             # run in the checkout; committed
echo "$FPR"     # goes into README.md, release.yml (APT_KEY_FPR) and this file
rm -rf "$GNUPGHOME"
```

GitHub Pages must deploy from Actions, and tags must be allowed to deploy:

```sh
gh api -X POST repos/jakuubkoo/Dirigent/pages -f build_type=workflow
gh api -X POST repos/jakuubkoo/Dirigent/environments/github-pages/deployment-branch-policies -f name='v*' -f type=tag
```

The key never expires, so there is no renewal date to track.

- **Secret lost or overwritten:** restore it from the backup with
  `gpg --decrypt dirigent-apt-key.asc.gpg | gh secret set APT_SIGNING_KEY --env github-pages --repo jakuubkoo/Dirigent`.
- **Key may have leaked:** rotate it. Create a new key the same way, then replace the secret, the committed public
  key and the fingerprint in README.md, release.yml (`APT_KEY_FPR`) and here. Then release, or re-run the workflow. Users fetch the new key once
  with the `curl` line from the README.

A PR from a `release/*` branch, or one that changes the release workflow, `scripts/install.sh`,
`scripts/build-deb.sh`, `scripts/apt-repo.sh` or `packaging/`, runs the same build and packaging as a dry run
(the `gate` job decides; other PRs show the release jobs as skipped): the tarball and the `.deb` are workflow artifacts, and nothing is released. The dry run also installs and
upgrades the `.deb` from a throwaway-key repository in clean Ubuntu 22.04 and 24.04 containers.
