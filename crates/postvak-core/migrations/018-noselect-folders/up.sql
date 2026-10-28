-- Folders the server lists as \Noselect, such as Gmail's [Gmail]: names in the hierarchy that hold no messages. Kept
-- apart from `folders`, which holds what sync selects, so a move to one is refused with a reason instead of a CREATE
-- and a failed MOVE. Replaced at every folder listing.
CREATE TABLE noselect_folders (
  name  TEXT PRIMARY KEY
) WITHOUT ROWID;
