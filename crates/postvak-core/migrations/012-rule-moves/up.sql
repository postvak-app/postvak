-- Mail a rule moved out of a folder. When it comes back, by hand or by another client's rule, rules no longer move it.
CREATE TABLE rule_moves (
  folder        TEXT NOT NULL,
  message_id    TEXT NOT NULL,     -- '' when the message has none
  internaldate  INTEGER NOT NULL,  -- kept by MOVE and COPY; tells copies sharing a Message-ID apart
  PRIMARY KEY (folder, message_id, internaldate)
) WITHOUT ROWID;
