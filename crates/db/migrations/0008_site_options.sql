-- Per-site options (0.1.7). A separate table, so no query on `sites`
-- changes. `noindex` keeps search engines away from a staging site; a site
-- without a row is indexable.
CREATE TABLE site_options (
    site_id TEXT PRIMARY KEY REFERENCES sites(id) ON DELETE CASCADE,
    noindex BOOLEAN NOT NULL DEFAULT FALSE
);
