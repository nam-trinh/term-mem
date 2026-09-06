-- Phase 2. The index docs/tech-stack.md describes, plus the one column it does
-- not: FTS5's `content=` option requires every indexed column to exist on the
-- content table, and `commands` is a table, not a column of `exchanges`.
--
-- The alternative was a standalone FTS5 table holding its own copy of every
-- prompt and response, which doubles the largest thing in the file. So the
-- mined command lines are denormalised onto the row — small, already derived at
-- capture time — and the index stays external-content over `exchanges`.
ALTER TABLE exchanges ADD COLUMN commands_text TEXT NOT NULL DEFAULT '';

UPDATE exchanges SET commands_text = COALESCE(
  (SELECT group_concat(cmd, char(10)) FROM commands WHERE exchange_id = exchanges.id), '');

-- `commands_text` is weighted above prose at query time. docs/scenarios.md:
-- the extracted command line is the highest-signal region of a response and the
-- thing users are most often actually looking for.
CREATE VIRTUAL TABLE exchanges_fts USING fts5(
  prompt, response, commands_text,
  content='exchanges',
  content_rowid='rowid',
  tokenize='porter unicode61'
);

-- Backfill whatever Phase 1 already captured, so search works on an existing
-- archive the moment the binary is upgraded rather than only on new exchanges.
INSERT INTO exchanges_fts (rowid, prompt, response, commands_text)
  SELECT rowid, prompt, response, commands_text FROM exchanges;

-- Triggers, not application code. docs/plan.md requires the index to be
-- "maintained transactionally with exchanges"; a trigger cannot be forgotten by
-- a new write path, and it makes `forget` remove the index entries for free —
-- which is the property Phase 3 has to grep the file to prove.
CREATE TRIGGER exchanges_fts_insert AFTER INSERT ON exchanges BEGIN
  INSERT INTO exchanges_fts (rowid, prompt, response, commands_text)
    VALUES (new.rowid, new.prompt, new.response, new.commands_text);
END;

CREATE TRIGGER exchanges_fts_delete AFTER DELETE ON exchanges BEGIN
  INSERT INTO exchanges_fts (exchanges_fts, rowid, prompt, response, commands_text)
    VALUES ('delete', old.rowid, old.prompt, old.response, old.commands_text);
END;

CREATE TRIGGER exchanges_fts_update AFTER UPDATE ON exchanges BEGIN
  INSERT INTO exchanges_fts (exchanges_fts, rowid, prompt, response, commands_text)
    VALUES ('delete', old.rowid, old.prompt, old.response, old.commands_text);
  INSERT INTO exchanges_fts (rowid, prompt, response, commands_text)
    VALUES (new.rowid, new.prompt, new.response, new.commands_text);
END;
