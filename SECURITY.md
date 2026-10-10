# Security policy

## Reporting a vulnerability

Please report security issues privately through GitHub's
[private vulnerability reporting](https://github.com/setucp/setu/security/advisories/new)
for this repository. Do not open a public issue.

You should get an acknowledgement within three days. Fixes for the
current minor release are published as a patch release with a security
note in the changelog; reporters are credited unless they ask not to be.

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x | yes |

## Scope

In scope: anything that lets a hosted site, an agent process or a
non-owner operator do more than the trust model allows
([docs/TRUST.md](docs/TRUST.md)), secrets exposed where
[docs/SECURITY.md](docs/SECURITY.md) says they are not, and the
installer or helper writing outside the paths it documents.

Out of scope: issues that require already having root on a managed
server or owner access to the panel, and the documented sharing of the
kernel, Postgres instance and loopback between hosted sites.
