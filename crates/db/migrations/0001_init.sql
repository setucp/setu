-- Setu MVP initial schema (spec §3 plus agent command queue,
-- web sessions and CLI tokens). Forward-only: later changes go in
-- 0002_*.sql, never by editing this file.

CREATE TABLE operators (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    totp_secret TEXT,
    role TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    host TEXT,
    token_hash TEXT,
    join_token_hash TEXT,
    join_token_expires_at TIMESTAMPTZ,
    join_token_used BOOLEAN NOT NULL DEFAULT FALSE,
    agent_version TEXT,
    arch TEXT,
    os TEXT,
    caddy_admin BOOLEAN NOT NULL DEFAULT FALSE,
    postgres_version TEXT,
    last_seen_at TIMESTAMPTZ,
    cpu_cores INTEGER,
    memory_mb BIGINT,
    disk_mb BIGINT,
    load1 DOUBLE PRECISION,
    mem_free_mb BIGINT,
    disk_free_mb BIGINT,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE apps (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    homepage TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE app_versions (
    id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    version TEXT NOT NULL,
    manifest JSONB NOT NULL,
    channel TEXT NOT NULL DEFAULT 'stable',
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (app_id, version)
);

CREATE TABLE plans (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    limits JSONB NOT NULL,
    allowed_app_ids JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sites (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    app_id TEXT NOT NULL REFERENCES apps (id),
    app_version TEXT NOT NULL,
    server_id TEXT NOT NULL REFERENCES servers (id),
    plan_id TEXT NOT NULL REFERENCES plans (id),
    domain TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'provisioning',
    failed_step TEXT,
    port INTEGER NOT NULL CHECK (port BETWEEN 20000 AND 29999),
    data_dir TEXT NOT NULL,
    db_name TEXT NOT NULL,
    env JSONB NOT NULL DEFAULT '{}',
    owner_email TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, port)
);
CREATE INDEX sites_server_id_idx ON sites (server_id);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    site_id TEXT REFERENCES sites (id) ON DELETE CASCADE,
    server_id TEXT REFERENCES servers (id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0,
    payload JSONB,
    log TEXT NOT NULL DEFAULT '',
    error TEXT,
    worker TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);
CREATE INDEX jobs_status_idx ON jobs (status);
CREATE INDEX jobs_site_id_idx ON jobs (site_id);

CREATE TABLE backups (
    id TEXT PRIMARY KEY,
    site_id TEXT NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    taken_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    size_bytes BIGINT,
    location TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'manual',
    status TEXT NOT NULL DEFAULT 'pending'
);
CREATE INDEX backups_site_id_idx ON backups (site_id);

CREATE TABLE audit_log (
    id TEXT PRIMARY KEY,
    operator_id TEXT REFERENCES operators (id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    target TEXT,
    ip TEXT,
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_operator_id_idx ON audit_log (operator_id);

CREATE TABLE site_metrics (
    site_id TEXT NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    at TIMESTAMPTZ NOT NULL,
    cpu_seconds DOUBLE PRECISION NOT NULL,
    memory_bytes BIGINT NOT NULL,
    disk_bytes BIGINT NOT NULL,
    PRIMARY KEY (site_id, at)
);

CREATE TABLE agent_commands (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    command JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    result JSONB,
    attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX agent_commands_server_id_completed_at_idx
    ON agent_commands (server_id, completed_at);

-- tower-sessions-sqlx-store (Postgres) expects exactly this shape;
-- Task 7 wires the session layer onto it.
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    data BYTEA NOT NULL,
    expiry TIMESTAMPTZ NOT NULL
);

-- CLI bearer tokens (Task 7); stored hashed like server tokens.
CREATE TABLE operator_tokens (
    id TEXT PRIMARY KEY,
    operator_id TEXT NOT NULL REFERENCES operators (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ
);
