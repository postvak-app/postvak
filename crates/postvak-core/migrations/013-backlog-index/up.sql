-- Messages the indexer tried without finding text, which it tries again a day later. With `messages_unindexed` for the
-- ones never tried, counting what is left reads only those, not every row with its stored body. The condition is
-- `RETRY` in store.rs, spelled the same so the count's query uses this index.
CREATE INDEX messages_retry ON messages (indexed_at) WHERE body_text IS NULL AND indexed_at IS NOT NULL;
