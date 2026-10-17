-- A guess from the stored headers, as Thunderbird makes it: mail with attachments is multipart/mixed at the top.
ALTER TABLE messages ADD COLUMN has_attachment INTEGER
    GENERATED ALWAYS AS (instr(lower(CAST(headers AS TEXT)), 'multipart/mixed') > 0) VIRTUAL;
