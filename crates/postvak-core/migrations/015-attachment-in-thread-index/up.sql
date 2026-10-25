-- The thread list's covering index (migration 009) with the attachment guess, so the attachments filter reads the
-- index like the others; without it every row's headers were read to compute the guess. SQLite indexes a virtual
-- generated column by computing it once per row written.
DROP INDEX messages_folder_thread;
CREATE INDEX messages_folder_thread ON messages (folder, thread_id, internaldate, uid, flags, from_addr, to_addr, subject, has_attachment);
