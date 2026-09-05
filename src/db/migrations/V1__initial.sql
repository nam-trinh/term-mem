-- Phase 1 schema. Follows docs/tech-stack.md, minus the FTS5 and vec0 virtual
-- tables: those are owned by Phase 2 and Phase 5 respectively, and an index
-- that nothing maintains is an index that drifts from its table.

CREATE TABLE exchanges (
  id           TEXT PRIMARY KEY,     -- ULID: sorts by time, no coordination
  assistant    TEXT NOT NULL,        -- 'claude-code', 'aider', 'ollama', ...
  session_id   TEXT NOT NULL,        -- transcript file identity, NOT a thread
  thread_id    TEXT NOT NULL,        -- root uuid of the conversation tree
  source_key   TEXT NOT NULL,        -- adapter-declared dedup key
  ts           INTEGER NOT NULL,     -- unix ms, UTC
  cwd          TEXT NOT NULL,
  repo         TEXT,                 -- resolved at capture, not derived later
  git_branch   TEXT,
  model        TEXT,
  prompt       TEXT NOT NULL,
  response     TEXT NOT NULL,
  redacted     INTEGER NOT NULL DEFAULT 0
);

-- Idempotency. Re-running ingest over the same transcript upserts onto this.
CREATE UNIQUE INDEX exchanges_source ON exchanges (assistant, session_id, source_key);
CREATE INDEX exchanges_ts     ON exchanges (ts);
CREATE INDEX exchanges_cwd    ON exchanges (cwd);
CREATE INDEX exchanges_thread ON exchanges (assistant, session_id, thread_id);

-- Mined from tool_use blocks and fenced blocks at capture time. Extraction is
-- capture-time and irreversible (the raw tool_use body is never stored), so it
-- lands in Phase 1; only the FTS column and its weighting are Phase 2's.
CREATE TABLE commands (
  exchange_id TEXT NOT NULL REFERENCES exchanges (id) ON DELETE CASCADE,
  seq         INTEGER NOT NULL,
  cmd         TEXT NOT NULL,
  lang        TEXT
);
CREATE INDEX commands_exchange ON commands (exchange_id);

-- tech-stack.md: an Edit input "is recorded as a path reference, not a body".
CREATE TABLE file_refs (
  exchange_id TEXT NOT NULL REFERENCES exchanges (id) ON DELETE CASCADE,
  seq         INTEGER NOT NULL,
  path        TEXT NOT NULL,
  tool        TEXT NOT NULL
);
CREATE INDEX file_refs_exchange ON file_refs (exchange_id);

-- One row per transcript file. Lets ingest skip files that have not grown.
CREATE TABLE watermarks (
  assistant   TEXT NOT NULL,
  source_path TEXT NOT NULL,
  session_id  TEXT,
  bytes       INTEGER NOT NULL,
  mtime_ms    INTEGER NOT NULL,
  exchanges   INTEGER NOT NULL DEFAULT 0,
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (assistant, source_path)
);
