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

A pushed tag is never moved or deleted. If a release turns out broken, fix it on `main` and release the next patch
version.

A PR that changes the release workflow, `scripts/install.sh`, `scripts/uninstall.sh` or `CHANGELOG.md` runs the same
build and packaging as a dry run: the tarball is a workflow artifact, and nothing is released.
