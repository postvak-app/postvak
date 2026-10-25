-- The UIDNEXT the last complete sync of the folder saw at its SELECT, 0 when unknown. A pass compares it with what
-- STATUS reports and skips the folder when nothing changed.
ALTER TABLE folders ADD COLUMN uid_next INTEGER NOT NULL DEFAULT 0;
