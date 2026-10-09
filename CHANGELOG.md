# Changelog

All notable changes to Setu. Release notes are cut from the section
matching the tag.

## 0.1.0 — Unreleased

First release (MVP): one binary (`setu serve` control plane +
`setu agent` per server), single-server native runtime on Ubuntu
24.04, REST API + embedded React UI, first-owner setup wizard
(`GET/POST /api/v1/setup`, `POST /api/v1/setup/local-server`),
`install.sh` single-node installer, app manifests with validation
corpus, sites with Postgres/Caddy/TLS/limits, local backups,
updates with rollback, job runner, audit log, end-to-end test.
