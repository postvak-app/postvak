-- The reader looks up what rules did to a message, and the Rules view counts each rule's hits.
CREATE INDEX rule_log_message ON rule_log(message_id);
CREATE INDEX rule_log_rule_at ON rule_log(rule_name, at);
