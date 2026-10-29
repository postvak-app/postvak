-- A reply without References joins the thread of the stored message its In-Reply-To names, which `Store::thread_of`
-- looks up by Message-ID.
CREATE INDEX messages_message_id ON messages (message_id);

-- Re-threads such replies already stored, one level: a parent that is itself such a reply gives its stored thread.
UPDATE messages SET thread_id = (
  SELECT parent.thread_id FROM messages AS parent WHERE parent.message_id = messages.in_reply_to LIMIT 1
)
WHERE refs IS NULL AND in_reply_to IS NOT NULL
  AND EXISTS (SELECT 1 FROM messages AS parent WHERE parent.message_id = messages.in_reply_to);
