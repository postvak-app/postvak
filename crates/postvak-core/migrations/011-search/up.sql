-- Search: one rebuild carries every change to the FTS table, so this is the only time the index is rebuilt.
ALTER TABLE messages ADD COLUMN attachment_names TEXT;   -- one name per line, from BODYSTRUCTURE; NULL until indexed
ALTER TABLE messages ADD COLUMN indexed_at INTEGER;      -- when the indexer last tried this message; NULL never
CREATE INDEX messages_unindexed ON messages (internaldate DESC) WHERE indexed_at IS NULL;

DROP TRIGGER messages_ai;
DROP TRIGGER messages_ad;
DROP TRIGGER messages_au;
DROP TABLE messages_fts;

-- remove_diacritics 2 folds accents in both the index and the query, so `cafe` finds `café` and `Müller` `Muller`.
CREATE VIRTUAL TABLE messages_fts USING fts5 (
  subject, from_addr, to_addr, cc_addr, delivered_to, attachment_names, body_text,
  content='messages', content_rowid='id',
  tokenize="unicode61 remove_diacritics 2"
);
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, subject, from_addr, to_addr, cc_addr, delivered_to, attachment_names, body_text)
  VALUES (new.id, new.subject, new.from_addr, new.to_addr, new.cc_addr, new.delivered_to, new.attachment_names, new.body_text);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_addr, to_addr, cc_addr, delivered_to, attachment_names, body_text)
  VALUES ('delete', old.id, old.subject, old.from_addr, old.to_addr, old.cc_addr, old.delivered_to, old.attachment_names, old.body_text);
END;
CREATE TRIGGER messages_au AFTER UPDATE OF subject, from_addr, to_addr, cc_addr, delivered_to, attachment_names, body_text ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_addr, to_addr, cc_addr, delivered_to, attachment_names, body_text)
  VALUES ('delete', old.id, old.subject, old.from_addr, old.to_addr, old.cc_addr, old.delivered_to, old.attachment_names, old.body_text);
  INSERT INTO messages_fts(rowid, subject, from_addr, to_addr, cc_addr, delivered_to, attachment_names, body_text)
  VALUES (new.id, new.subject, new.from_addr, new.to_addr, new.cc_addr, new.delivered_to, new.attachment_names, new.body_text);
END;
INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');

-- The index's terms with per-column document counts; what close matches are drawn from.
CREATE VIRTUAL TABLE messages_vocab USING fts5vocab('messages_fts', 'col');
