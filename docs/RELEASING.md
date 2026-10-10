# Releasing Setu

## Cadence

Semver; `0.1.0` is the MVP (currently `Unreleased` in
[CHANGELOG.md](../CHANGELOG.md)). Keep the changelog current on every
user-visible change; the release notes are cut from it.

## Cutting a release

1. Update `CHANGELOG.md`: rename `Unreleased` to the version + date.
2. Bump `version` in the workspace `Cargo.toml` and `ui/package.json`.
   `scripts/check-release-version.sh <ver>` refuses a tag that differs
   from `Cargo.toml` (the binary reports its own version, and the
   assets are named after it).
3. Run the gates: `bash scripts/test.sh`, the UI checks,
   `scripts/e2e.sh` (`E2E PASS`) and `scripts/test-install.sh`
   (`INSTALL PASS`), `scripts/check-links.sh`.
4. Commit, tag `vX.Y.Z` (`v0.1.0-rc.1` style suffixes become
   prereleases and never move `latest`), push the tag.
5. `.github/workflows/release.yml` (on `ubuntu-24.04`):
   - checks the tag against `Cargo.toml` and the changelog;
   - builds the UI, then `setu` for `x86_64-unknown-linux-musl` with
     `musl-tools`, **without** the `test-root` and `e2e-hooks`
     features (the job fails if the binary is not static, reports
     another version, or its helper offers `--root`);
   - `scripts/package-release.sh <ver> <bindir> dist` writes
     `setu-<ver>-x86_64-unknown-linux-musl.tar.gz` (one top-level
     `setu`), `install.sh` and one `SHA256SUMS` covering both;
   - creates the GitHub release with the changelog section as notes
     and those three files attached.

`install.sh` downloads `releases/download/v<ver>/<archive>` and
`SHA256SUMS` (the latest release when `--version` is absent) and
refuses an archive whose line in `SHA256SUMS` does not match.

## Testing a release locally

`scripts/test-install.sh` builds the same binary (musl, no features),
packages it with `scripts/package-release.sh`, serves the three files
on the host's address of a private Docker network and runs `install.sh
--tls internal --panel-host setu.test --archive-url … --sums-url …` in a
fresh systemd container; see [E2E.md](E2E.md) for the host-safety
rules it shares with `scripts/e2e.sh`.

## Not released (phase 2)

aarch64 builds and a container image. The repository's `Dockerfile`
builds a control-plane image for local use only; no workflow publishes
it.

## Org and licence

Still the owner's call (spec §12): currently `setucp/setu`
(private) and `AGPL-3.0-only`. Changing the org means updating
`install.sh` (`REPO=`) and these docs.
