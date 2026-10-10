-- 0.1.1 (#6, ledger R142): a job outlives its site. `delete_site` removes
-- the site row in its `finish` step; with SET NULL the job keeps its row
-- and log, and the runner can mark it done. NULL site ids never collide
-- in jobs_one_active_per_site_idx (NULLs are distinct in a unique index).
-- Forward-only: 0001_init.sql shipped in v0.1.0 and is never edited.
ALTER TABLE jobs DROP CONSTRAINT jobs_site_id_fkey;
ALTER TABLE jobs ADD CONSTRAINT jobs_site_id_fkey
    FOREIGN KEY (site_id) REFERENCES sites (id) ON DELETE SET NULL;
