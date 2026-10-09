# Releasing Setu

## Cadence

Semver; `0.1.0` is the MVP (currently `Unreleased` in
[CHANGELOG.md](../CHANGELOG.md)). Keep the changelog current on every
user-visible change; the release notes are cut from it.

## Cutting a release

1. Update `CHANGELOG.md`: rename `Unreleased` to the version + date.
2. Bump `version` in the workspace `Cargo.toml` and `ui/package.json`
   (the tag gate refuses a mismatch: the binary must report its own
   version — `setu --version`).
3. Commit, tag `vX.Y.Z` (`v0.1.0-rc.1` style suffixes become
   prereleases and never move `latest`), push the tag.
4. `.github/workflows/release.yml` builds everything:
   - musl static binaries for `x86_64` and `aarch64`
     (`x86_64-unknown-linux-musl` natively, `aarch64` via `cross`),
     packaged by `scripts/package-release.sh` as
     `setu-<ver>-<target>.tar.gz` + `.sha256`;
   - the multi-arch control-plane image `ghcr.io/<org>/setu:<ver>`
     (plus `latest` for stable releases);
   - a GitHub release with the changelog section as notes and the
     archives attached.
5. Smoke: `scripts/test-install.sh` against the release archives
   (override the version: `SETU_TEST_INSTALL_VERSION=X.Y.Z`).

Tags with a `-` suffix are prereleases and never move `latest`;
the aarch64 runner entry is skipped on private repositories (no free
arm64 Linux runners there), like Vyasa's release workflow.

## The image

`Dockerfile` runs the musl `setu` binary as a non-root user,
`CMD ["serve"]`. It needs `SETU_DATABASE_URL`, `SETU_SECRET_KEY`,
`SETU_PUBLIC_URL` at runtime (and `SETU_BIND_ADDR=0.0.0.0:7000` when
the port must leave the container):

```sh
docker run -e SETU_DATABASE_URL=… -e SETU_SECRET_KEY=… \
  -e SETU_PUBLIC_URL=https://setu.example.com -p 7000:7000 \
  ghcr.io/<org>/setu:0.1.0
```

## Org and licence

Still the owner's call (spec §12): currently `vyasa-cms/setu`
(private) and `AGPL-3.0-only`. Changing the org means updating
`install.sh` (`REPO=`), this file's image names, and the release
workflow's `IMAGE`.
