-- A first sync fetches a folder newest first; uids below this one are history not fetched yet, 0 when there is none.
ALTER TABLE folders ADD COLUMN backfill_uid INTEGER NOT NULL DEFAULT 0;
