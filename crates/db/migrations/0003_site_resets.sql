-- Scheduled resets (0.1.4 §2): a site restored from one of its backups
-- every `every_minutes`. One row per site; dropping the site or the
-- backup drops the schedule.
CREATE TABLE site_resets (
    site_id TEXT PRIMARY KEY REFERENCES sites (id) ON DELETE CASCADE,
    backup_id TEXT NOT NULL REFERENCES backups (id) ON DELETE CASCADE,
    every_minutes INTEGER NOT NULL CHECK (every_minutes BETWEEN 15 AND 10080),
    last_reset_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX site_resets_backup_id_idx ON site_resets (backup_id);
