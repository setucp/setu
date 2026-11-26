-- Extra hostnames of a site (0.1.7, issue #18). A separate table, so no
-- query on `sites` changes. A host belongs to one site at most.
CREATE TABLE site_aliases (
    site_id TEXT NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    host    TEXT NOT NULL UNIQUE,
    mode    TEXT NOT NULL CHECK (mode IN ('redirect', 'serve')),
    PRIMARY KEY (site_id, host)
);
