-- A forward rule runs only with its exact definition approved here; any edit of the rule needs a new approval.
CREATE TABLE forward_approvals (
    rule TEXT PRIMARY KEY,
    definition TEXT NOT NULL,
    approved_at INTEGER NOT NULL
);

-- What each forward rule queued, so a message is forwarded once: `key` is `id:<Message-ID>`, or
-- `uid:<folder>/<uidvalidity>/<uid>` for a message without one. `at` counts the rule's rate.
CREATE TABLE forwarded (
    rule TEXT NOT NULL,
    key TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (rule, key)
);
CREATE INDEX forwarded_rule_at ON forwarded (rule, at);

-- A forward that could not be queued, so none of the rule's actions ran on the message; cleared once it is queued.
CREATE TABLE forward_failures (
    rule TEXT NOT NULL,
    key TEXT NOT NULL,
    reason TEXT NOT NULL,
    first_at INTEGER NOT NULL,
    PRIMARY KEY (rule, key)
);
