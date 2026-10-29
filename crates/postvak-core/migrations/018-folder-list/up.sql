-- When a pass last went through every folder the server listed, so a folder the account does not sync can be told
-- from one a first sync has not reached yet. One row, absent until the first such pass.
CREATE TABLE folder_list (
  id         INTEGER PRIMARY KEY CHECK (id = 1),
  synced_at  INTEGER NOT NULL
);
