-- Covers the thread list, its filters and unread counts, so they never read the wide rows with headers and bodies.
-- Indexing has_attachment stores it, where the table computes it from the headers on every read.
CREATE INDEX messages_folder_thread
    ON messages (folder, thread_id, internaldate, uid, flags, has_attachment, from_addr, to_addr, subject);
-- Every thread lookup also names the folder, which the index above leads with.
DROP INDEX messages_thread;
