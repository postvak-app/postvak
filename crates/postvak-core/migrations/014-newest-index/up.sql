-- The index backlog in the order the indexer takes it, newest first, so a slice reads its candidates and no more.
-- `backlog` in store.rs reads the two halves apart, one per index; an OR over both read the whole table.
DROP INDEX messages_unindexed;
CREATE INDEX messages_unindexed ON messages (internaldate DESC, uid DESC) WHERE indexed_at IS NULL;
DROP INDEX messages_retry;
CREATE INDEX messages_retry ON messages (internaldate DESC, uid DESC) WHERE body_text IS NULL AND indexed_at IS NOT NULL;
-- Every message newest first, across folders: a search of predicates alone, such as `after:` or `is:unread` without
-- a word, walks it until the limit instead of sorting every row.
CREATE INDEX messages_newest ON messages (internaldate DESC, uid DESC);
