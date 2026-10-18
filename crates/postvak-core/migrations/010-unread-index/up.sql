-- Only the unread messages, so a folder's unread count reads those and not the whole folder. The condition is `UNSEEN`
-- in store.rs, spelled the same so the count's query uses this index.
CREATE INDEX messages_unread ON messages (folder) WHERE instr(' ' || flags || ' ', ' \Seen ') = 0;
