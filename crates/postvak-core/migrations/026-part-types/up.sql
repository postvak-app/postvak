-- The MIME type of every part but a multipart, one per line, from BODYSTRUCTURE; NULL until known.
ALTER TABLE messages ADD COLUMN part_types TEXT;
-- Messages indexed before part types were stored, whose structure alone the indexer fetches again; spelled as `RESTRUCTURE`.
CREATE INDEX messages_restructure ON messages (internaldate DESC, uid DESC)
    WHERE part_types IS NULL AND body_text IS NOT NULL AND indexed_at IS NOT NULL;
