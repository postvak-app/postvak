-- Covers the thread list and unread counts, so they never read the wide rows with headers and bodies.
CREATE INDEX messages_folder_thread ON messages (folder, thread_id, internaldate, uid, flags, from_addr, to_addr, subject);
-- Every thread lookup also names the folder, which the index above leads with.
DROP INDEX messages_thread;
