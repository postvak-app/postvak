-- The account's SMTP server as the daemon guessed it, and what the server said about itself; one row at most.
-- `host` is NULL when the guess found none. `guessed_for_*` are the IMAP host and login the guess was made for, so it
-- is made again when either changes; both are '' while only the capabilities of a configured server are known.
CREATE TABLE smtp_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    host TEXT,
    port INTEGER,
    tls TEXT,
    guessed_for_host TEXT NOT NULL,
    guessed_for_user TEXT NOT NULL,
    tried_at INTEGER NOT NULL,
    -- The server's SIZE limit in bytes, NULL when it gave none; and whether it offered 8BITMIME.
    size INTEGER,
    eight_bit_mime INTEGER NOT NULL DEFAULT 0,
    -- Why the last sent message got no copy in Sent, NULL when it did.
    sent_copy_problem TEXT
);
