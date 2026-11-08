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
