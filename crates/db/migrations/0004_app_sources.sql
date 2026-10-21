-- Followed manifest URLs (0.1.5 §1): an app published from a URL is
-- checked again on a schedule. One row per app; dropping the app drops it.
CREATE TABLE app_sources (
    app_id TEXT PRIMARY KEY REFERENCES apps (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    checked_at TIMESTAMPTZ,
    last_error TEXT
);
