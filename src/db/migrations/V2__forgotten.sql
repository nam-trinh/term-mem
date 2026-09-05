-- `tmem forget` deletes a row, but the transcript it came from is still on
-- disk, and the next hook-triggered ingest of that session reparses the whole
-- file (the watermark is a change detector, not a seek offset — see
-- docs/phases/phase-1.md finding 5). Without a tombstone the deleted exchange
-- is re-inserted on the user's very next turn, which defeats the entire point
-- of the command.
--
-- This stores the adapter's dedup key and nothing else: no prompt, no response,
-- no commands. The forgotten *content* is still genuinely gone.
CREATE TABLE forgotten (
  assistant    TEXT NOT NULL,
  session_id   TEXT NOT NULL,
  source_key   TEXT NOT NULL,
  forgotten_at INTEGER NOT NULL,
  PRIMARY KEY (assistant, session_id, source_key)
);
