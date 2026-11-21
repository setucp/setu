-- Per-site `www.` → apex redirect (0.1.7). Sites that exist now keep the
-- behaviour they have (no redirect); a site created from now on has it on,
-- and `PUT /sites/{id}/www-redirect` turns it on or off.
ALTER TABLE sites ADD COLUMN www_redirect BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE sites ALTER COLUMN www_redirect SET DEFAULT true;
