-- Per-site HTTP basic auth (0.1.7): one user name and a bcrypt hash of the
-- password per site, enforced by Caddy in front of the app. The password
-- itself is never stored. Dropping the site drops the row.
CREATE TABLE site_basic_auth (
    site_id TEXT PRIMARY KEY REFERENCES sites (id) ON DELETE CASCADE,
    username TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The route_update job (rewrites a site's Caddy route from the table above).
ALTER TABLE jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN
    ('provision_site', 'update_site', 'backup_site', 'restore_site',
     'delete_site', 'delete_backup', 'env_update', 'restart_site',
     'stop_site', 'start_site', 'route_update'));

-- A `set_route` command carries the site's bcrypt hash: null
-- `basic_auth.password_hash` whenever the row leaves queued/delivered, like
-- the other secrets (the value stays in site_basic_auth, where it is
-- needed). The user name stays; a redacted command does not deserialize.
CREATE OR REPLACE FUNCTION setu_redact_command(cmd JSONB) RETURNS JSONB
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
        WHEN cmd->>'kind' = 'set_route' AND jsonb_typeof(cmd->'basic_auth') = 'object'
            THEN jsonb_set(cmd, '{basic_auth,password_hash}', 'null'::jsonb)
        ELSE cmd
    END
$$;
