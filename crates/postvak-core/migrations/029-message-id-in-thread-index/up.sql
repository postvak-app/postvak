-- The Message-ID in the thread list's covering index, so the list marks a message whose reply waits in the outbox
-- without reading the wide rows.
DROP INDEX messages_folder_thread;
CREATE INDEX messages_folder_thread ON messages (folder, thread_id, internaldate, uid, flags, from_addr, to_addr, subject, has_attachment, preview, message_id);
