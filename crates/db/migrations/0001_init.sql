-- Setu control-plane schema (spec §3, revision 3).
-- Pre-release: this file was rewritten in place (ledger R45). From the
-- first release on, migrations are forward-only: later changes go in
-- 0002_*.sql, never by editing this file.

CREATE TABLE operators (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE CHECK (email = lower(email)),
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'operator', 'viewer')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Web sessions: the cookie is 32 random bytes; only its SHA-256 (hex)
-- is stored. Sliding 7-day expiry; every row of an operator is deleted
-- on password change.
CREATE TABLE sessions (
    id_hash TEXT PRIMARY KEY,
    operator_id TEXT NOT NULL REFERENCES operators (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX sessions_operator_id_idx ON sessions (operator_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-- Status is computed on read (pending | online | offline), never stored.
CREATE TABLE servers (
    id TEXT PRIMARY KEY,
    short_id TEXT NOT NULL UNIQUE CHECK (short_id ~ '^[a-z0-9]{8}$'),
    name TEXT NOT NULL,
    hostname TEXT,
    public_ip TEXT,
    token_hash TEXT UNIQUE,
    join_token_hash TEXT UNIQUE,
    join_expires_at TIMESTAMPTZ,
    join_nonce TEXT,
    -- The issued server token, sealed, kept 60 s for an idempotent retry.
    join_token_sealed TEXT,
    joined_at TIMESTAMPTZ,
    agent_version TEXT,
    boot_id TEXT,
    arch TEXT,
    os TEXT,
    cpu_cores INTEGER,
    memory_mb BIGINT,
    disk_mb BIGINT,
    last_seen_at TIMESTAMPTZ,
    cordoned BOOLEAN NOT NULL DEFAULT FALSE,
    is_control_plane BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Load figures of the latest heartbeat (shown on the servers page).
    load1 DOUBLE PRECISION,
    mem_free_mb BIGINT,
    disk_free_mb BIGINT
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

-- allowed_app_ids NULL = every app.
CREATE TABLE plans (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    memory_mb INTEGER NOT NULL CHECK (memory_mb > 0),
    cpu_percent INTEGER NOT NULL CHECK (cpu_percent > 0),
    disk_mb INTEGER NOT NULL CHECK (disk_mb > 0),
    allowed_app_ids TEXT[],
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sites (
    id TEXT PRIMARY KEY,
    short_id TEXT NOT NULL CHECK (short_id ~ '^[a-z0-9]{8}$'),
    name TEXT NOT NULL,
    app_id TEXT NOT NULL REFERENCES apps (id),
    app_version TEXT NOT NULL,
    server_id TEXT NOT NULL REFERENCES servers (id),
    plan_id TEXT NOT NULL REFERENCES plans (id),
    domain TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'provisioning' CHECK (status IN
        ('provisioning', 'running', 'stopped', 'failed', 'updating', 'deleting')),
    failed_step TEXT,
    port INTEGER NOT NULL CHECK (port BETWEEN 20000 AND 29999),
    db_name TEXT NOT NULL,
    db_role TEXT NOT NULL,
    -- Values sealed; generated exactly once at creation.
    env JSONB NOT NULL DEFAULT '[]',
    owner_email TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, port),
    UNIQUE (server_id, short_id)
);
CREATE INDEX sites_plan_id_idx ON sites (plan_id);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN
        ('provision_site', 'update_site', 'backup_site', 'restore_site',
         'delete_site', 'delete_backup', 'env_update', 'restart_site',
         'stop_site', 'start_site')),
    site_id TEXT REFERENCES sites (id) ON DELETE CASCADE,
    server_id TEXT REFERENCES servers (id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN
        ('queued', 'running', 'done', 'failed')),
    -- Includes the `step` cursor.
    payload JSONB NOT NULL DEFAULT '{}',
    attempts INTEGER NOT NULL DEFAULT 0,
    locked_by TEXT,
    locked_until TIMESTAMPTZ,
    log TEXT NOT NULL DEFAULT '',
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);
CREATE INDEX jobs_status_created_at_idx ON jobs (status, created_at);
-- One active job per site.
CREATE UNIQUE INDEX jobs_one_active_per_site_idx
    ON jobs (site_id) WHERE status IN ('queued', 'running');

-- Null the secret fields of a stored agent command: `password` of
-- create_database, every env `value` of write_env. Applied whenever a
-- row leaves queued/delivered (completion, cancel, expiry, prune).
CREATE FUNCTION setu_redact_command(cmd JSONB) RETURNS JSONB
LANGUAGE SQL IMMUTABLE AS $$
    SELECT CASE
        WHEN cmd->>'kind' = 'create_database' AND cmd ? 'password'
            THEN jsonb_set(cmd, '{password}', 'null'::jsonb)
        WHEN cmd->>'kind' = 'write_env' AND jsonb_typeof(cmd->'env') = 'array'
            THEN jsonb_set(cmd, '{env}', COALESCE(
                (SELECT jsonb_agg(CASE WHEN jsonb_typeof(e) = 'object'
                                       THEN jsonb_set(e, '{value}', 'null'::jsonb)
                                       ELSE e END
                                  ORDER BY ord)
                 FROM jsonb_array_elements(cmd->'env') WITH ORDINALITY AS t(e, ord)),
                '[]'::jsonb))
        ELSE cmd
    END
$$;

-- Secret fields of `command` are nulled whenever the row leaves
-- queued/delivered (completion, cancel, expiry, prune).
CREATE TABLE agent_commands (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    site_short_id TEXT,
    command JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    delivered_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    result JSONB,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN
        ('queued', 'delivered', 'done', 'failed', 'cancelled', 'expired')),
    attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX agent_commands_server_status_created_idx
    ON agent_commands (server_id, status, created_at);

-- Files live on the server at
-- /var/lib/setu/backups/<site_short_id>/<id>/{db.dump,dirs.tar.zst};
-- the row outlives its site (site_id SET NULL).
CREATE TABLE backups (
    id TEXT PRIMARY KEY,
    site_id TEXT REFERENCES sites (id) ON DELETE SET NULL,
    server_id TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    site_short_id TEXT NOT NULL,
    site_name TEXT NOT NULL,
    domain TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('scheduled', 'manual', 'pre_update', 'pre_restore', 'pre_delete')),
    taken_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    size_bytes BIGINT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN
        ('pending', 'completed', 'failed', 'deleted'))
);
CREATE INDEX backups_site_id_idx ON backups (site_id, taken_at);

CREATE TABLE audit_log (
    id TEXT PRIMARY KEY,
    operator_id TEXT REFERENCES operators (id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    target_kind TEXT,
    target_id TEXT,
    ip TEXT,
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_at_idx ON audit_log (at);

-- 1-minute resolution, pruned after 30 days by the scheduler.
CREATE TABLE site_metrics (
    site_id TEXT NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    at TIMESTAMPTZ NOT NULL,
    cpu_seconds DOUBLE PRECISION NOT NULL,
    memory_bytes BIGINT NOT NULL,
    disk_bytes BIGINT NOT NULL,
    PRIMARY KEY (site_id, at)
);

-- Setup token hash, trusted proxies, backup defaults.
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL
);
