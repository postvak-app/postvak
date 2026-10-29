-- The start of the body text for the list's second line, in the thread list's covering index so the list still never
-- reads the wide rows. NULL until the body is indexed. Keep the length equal to `store::PREVIEW_CHARS`.
ALTER TABLE messages ADD COLUMN preview TEXT GENERATED ALWAYS AS (substr(body_text, 1, 160)) VIRTUAL;
DROP INDEX messages_folder_thread;
CREATE INDEX messages_folder_thread ON messages (folder, thread_id, internaldate, uid, flags, from_addr, to_addr, subject, has_attachment, preview);
