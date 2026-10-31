-- Rows an apply-existing run wrote, which a rule's hits leave out.
ALTER TABLE rule_log ADD COLUMN apply_existing INTEGER NOT NULL DEFAULT 0;
