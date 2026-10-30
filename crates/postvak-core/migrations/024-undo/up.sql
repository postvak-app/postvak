-- Undo: the action each row belongs to, what finds a moved copy again (internaldate, the folder it went to), and the
-- action an undo row reverses.
ALTER TABLE rule_log ADD COLUMN action_id INTEGER;
ALTER TABLE rule_log ADD COLUMN internaldate INTEGER;
ALTER TABLE rule_log ADD COLUMN target TEXT;
ALTER TABLE rule_log ADD COLUMN undoes INTEGER;
CREATE INDEX rule_log_action ON rule_log(action_id);
CREATE INDEX rule_log_undoes ON rule_log(undoes);
