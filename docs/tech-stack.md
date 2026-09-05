# term-mem — Tech stack

Status: design sketch. Load-bearing choices (language, storage engine, capture
strategy) are argued here and should be hard to change later. Specific crates
and models are provisional.

Everything below is constrained by one line in [mission.md](mission.md):
*nothing leaves the machine.* That rules out hosted embedding APIs, hosted
vector stores, crash reporters, and update pings, and it forces every component
here to have a local answer.

## The short version

| Layer | Choice | Why |
| --- | --- | --- |
| Language | Rust | Single static binary, sub-10ms startup, no runtime on the user's machine |
| CLI | `clap` (derive) | Subcommands, shell completions, `--help` that isn't hand-maintained |
| Storage | SQLite (WAL) | One file, the user can open it with tools they already have |
| Keyword search | SQLite FTS5, BM25 ranking | In-process, no separate service, good enough to carry the common path |
| Vector search | `sqlite-vec` extension | Same file, same transaction, no second datastore |
| Embeddings | `fastembed-rs` / ONNX Runtime, `bge-small-en-v1.5` | Runs locally on CPU, ~130MB, 384 dims |
| Encryption (opt-in) | SQLCipher (`sqlite3mc`) | Page-level AES on the same file format |
| Capture | Transcript parsing, triggered by file watch or hook; PTY wrapper as fallback | The response is already on disk in structured form |
| File watching | `notify` (FSEvents/inotify) | Transcript tailing without polling |
| Redaction | `gitleaks`-style rules + Shannon entropy, pre-write | The archive must never contain the secret in the first place |
| Agent interface | MCP server over stdio, plus `--json` on a pipe | One protocol for MCP clients, one escape hatch for everything else |
| Config | TOML at `~/.config/term-mem/config.toml` | Editable by hand, greppable, diffable |
| Packaging | `cargo-dist`, Homebrew tap, static musl builds | `curl \| sh` and `brew install` with no toolchain |

### Why Rust

Two requirements pick the language. Capture sits in the hot path of every
assistant invocation, so startup cost is user-visible — a runtime that takes
80ms to boot is a tax paid thousands of times. And the tool must ship as one
file with no interpreter, because a memory layer that breaks when the user
upgrades Python is a memory layer they uninstall.

Go is the credible alternative and would be fine; the tiebreakers are
`rusqlite`'s extension loading (needed for `sqlite-vec` and SQLCipher) and
mature local-inference bindings for the embedding path.

Rejected: Python (startup cost, dependency fragility), Node (same, plus
`node_modules` next to a privacy tool is a bad look), a shell script (no index).

---

## Storage

One SQLite file at `~/.local/share/term-mem/memory.db`, WAL mode so a capture
write never blocks a search read.

```sql
CREATE TABLE exchanges (
  id           TEXT PRIMARY KEY,     -- ULID: sorts by time, no coordination
  session_id   TEXT NOT NULL,        -- transcript file identity, NOT a thread
  thread_id    TEXT NOT NULL,        -- root uuid of the conversation tree
  ts           INTEGER NOT NULL,     -- unix ms, UTC
  cwd          TEXT NOT NULL,
  repo         TEXT,                 -- resolved at capture, not derived later
  git_branch   TEXT,
  assistant    TEXT NOT NULL,        -- 'claude-code', 'aider', 'ollama', …
  model        TEXT,
  prompt       TEXT NOT NULL,
  response     TEXT NOT NULL,
  redacted     INTEGER NOT NULL DEFAULT 0
);

CREATE VIRTUAL TABLE exchanges_fts USING fts5(
  prompt, response, commands,
  content='exchanges', tokenize='porter unicode61'
);

CREATE TABLE commands (                -- extracted from fenced blocks
  exchange_id TEXT, cmd TEXT, lang TEXT
);

CREATE VIRTUAL TABLE exchanges_vec USING vec0(
  exchange_id TEXT PRIMARY KEY, embedding FLOAT[384]
);
```

Four notes on this shape:

**`thread_id` is what `--session` groups on, not `session_id`.** Phase 0 found
that `/clear` starts a fresh conversation tree inside the same transcript file
under the same session id, so grouping by `session_id` merges unrelated
conversations. `thread_id` is derived at ingest by walking to the tree root.
See [phases/phase-0.md](phases/phase-0.md).

**`repo` and `git_branch` are resolved at write time.** Deriving them from
`cwd` at query time fails the moment a checkout is renamed or deleted, which is
exactly when old memories matter most.

**Commands get their own table and their own FTS column.** As the scenarios
show, the extracted command line is the highest-signal region of a response and
the thing users are most often actually looking for. It's weighted above prose
at rank time.

**Deletes are real deletes.** `tmem forget` runs one transaction across
`exchanges`, `exchanges_fts`, `commands`, and `exchanges_vec`, then a `VACUUM`
so the text isn't recoverable from a free page. No `deleted=1` flag on a row
that stays greppable on disk — a tool that keeps a "deleted" secret around is
worse than one that never stored it.

**Encryption is opt-in and has a cost the user is told about.** SQLCipher makes
the file useless to `grep`, `sqlite3`, and every other tool the mission promises
the user can reach for. That's why `tmem export` is not a nice-to-have: with
encryption on, the guaranteed open-format export *is* the ownership promise.

---

## How we know the user is talking to an AI

This is the part that determines whether the tool works at all, and it's where
the privacy premise is either honored or quietly broken.

**The rule: we never watch the terminal.** No keylogger, no shell-wide PTY
shim, no scraping of arbitrary scrollback. term-mem records exchanges from
processes it has an explicit *adapter* for, and nothing else. If you run `psql`
in the same pane, `psql` isn't captured. This is a narrower design than "capture
everything and filter later," and it's narrower on purpose — the broad version
is one bug away from archiving a password typed at a prompt.

Adapters come in three tiers, best first.

### Tier 1 — The transcript on disk (the actual source of responses)

**The assistant's response is read from the transcript file. This is verified,
not assumed.** Claude Code writes one JSONL file per session under
`~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`, appending as the
session runs. An `assistant` record looks like:

```json
{
  "type": "assistant",
  "uuid": "…", "parentUuid": "…",
  "sessionId": "0a36b825-…",
  "timestamp": "2026-09-04T23:36:19.486Z",
  "cwd": "/Users/namtrinh/Desktop/codes/term-mem",
  "gitBranch": "main",
  "isSidechain": false,
  "version": "…",
  "message": {
    "role": "assistant",
    "model": "claude-opus-5",
    "content": [ {"type": "text", "text": "…"},
                 {"type": "thinking", "thinking": "…"},
                 {"type": "tool_use", "name": "Edit", "input": {…}} ],
    "usage": {"output_tokens": 370},
    "stop_reason": "…"
  }
}
```

Every column in the `exchanges` schema is already present and authoritative —
`cwd`, `gitBranch`, `model`, `sessionId`, `timestamp`. Nothing is derived, and
nothing is guessed.

Three decisions this record shape forces:

- **`text` blocks are the response. `thinking` blocks are not stored.** Phase 0
  found they arrive with `thinking: ""` and a signature — empty on disk, so
  there is currently nothing to store. Codex CLI withholds reasoning the same
  way (all 237 records empty), so this is a property of the category rather than
  of one tool. The rule stays as defense-in-depth because that is
  version-dependent behavior, but it is now guarding a less likely event.
- **`tool_use` blocks are mined, not stored whole.** A `Bash` input is a command
  line and belongs in the `commands` table; an `Edit` input is a file diff and
  is recorded as a path reference, not a body. Measured: `tool_use` is 69% of
  assistant content against 31% for prose, so this is a 3.2× reduction, not a
  tidiness preference.
- **`isSidechain: true` marks subagent turns.** Captured, but attributed to the
  parent exchange, so a search doesn't return five near-identical rows for one
  question. **Unverified** — no sidechain records appeared in the Phase 0
  sample.

### What the parser actually has to do

The naive reading of the above — take `user` records as prompts, `assistant`
records as responses — produces a badly wrong archive.
[phases/phase-0.md](phases/phase-0.md) has the evidence; the load-bearing
results:

1. **Eleven top-level record types exist**, most without a `uuid` or `message`.
   Filter with an allowlist so an unknown type is skipped, not fatal.
2. **Most `user` records are not prompts.** In the sample, 24 of 41 were tool
   results and several more were IDE telemetry, slash-command echoes, and one
   14,872-character injected compaction summary. Prompts require
   `toolUseResult == null && isMeta != true && isCompactSummary != true`, plus a
   content-shape filter rejecting `<ide_opened_file>`, `<command-name>`, and
   `<local-command-*>` wrappers — because those carry `origin.kind: "human"` and
   the metadata alone will wave them through.
3. **`message.content` is polymorphic** — a bare string or an array of blocks.
4. **The parent chain breaks at compaction.** A `compact_boundary` record has
   `parentUuid: null` and moves the real link to `logicalParentUuid`. Thread
   walking must follow `parentUuid ?? logicalParentUuid` or `--session` silently
   truncates at the boundary.
5. **`isApiErrorMessage: true` marks a failed turn** stored in assistant shape.
   Skip it, or the archive fills with "OAuth session expired" as a response.
6. **Records are not written in parent order.** Children precede parents, so
   assembly is a second pass over a buffered set rather than inline work.
7. **Assembly is many-to-one.** Roughly six assistant records per prompt, one
   per tool round trip, folding into a single `exchanges` row.

Five of those seven fail *silently*. That is the argument for the snapshot
fixtures below being non-negotiable rather than good practice.

A background watcher (`notify` — FSEvents on macOS, inotify on Linux) tails
registered transcript directories and parses newly appended records. This needs
no cooperation from the assistant, survives its updates better than a hook
config does, and captures sessions that crashed, since the transcript was
already on disk.

**It's also the same code path as import.** `tmem init` offering to backfill
months of existing transcripts is just this parser run over history — the
difference `cli.md` names between proving the tool on day one and asking for
faith for six weeks.

The parsers are the maintenance burden: these formats are internal and will
change without notice. Each adapter declares a format version, fails loudly
rather than silently writing garbage, and is covered by snapshot tests against
recorded fixtures.

### Tier 2 — Hooks as a trigger (not as a source)

Claude Code's hooks (`SessionStart`, `UserPromptSubmit`, `Stop`, `PreToolUse`,
`PostToolUse`) fire at lifecycle points with a JSON payload on stdin. The `Stop`
payload carries `session_id`, `cwd`, and `transcript_path` — **it does not carry
the response text.** So the hook cannot replace tier 1; what it does is tell us
precisely *when* a turn completed, and where to read it.

```json
{
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "tmem capture --hook claude-code" }] }]
  }
}
```

`tmem capture` reads `transcript_path` from the payload and ingests the records
appended since its last watermark for that session. That makes the two tiers one
mechanism with two triggers — a filesystem event, or a hook — rather than two
parallel ingest paths that can disagree.

This is a simplification worth having: it removes the double-write problem
entirely, since both triggers converge on the same parser, keyed by
`(session_id, uuid)` — Claude Code's key, not every adapter's; see the open
question below. The hook is still worth registering, because it fires on a
known turn boundary instead of on a partial flush, and because
`UserPromptSubmit` is the injection point for automatic recall further down.

### Tier 3 — PTY wrapper (universal fallback)

For an assistant with neither hooks nor a transcript file:

```
tmem run <assistant> [args…]
```

`portable-pty` allocates a pty, proxies stdin/stdout so the TUI behaves
normally, and tees the stream through an ANSI parser (`vte`) that strips escape
sequences and reconstructs turn boundaries from the prompt pattern the adapter
declares.

This is lossy and it's last for a reason: TUIs redraw, spinners emit thousands
of frames, and turn detection on a repainting screen is heuristic. It's here so
that "my assistant isn't supported" has an answer, not because it's good.

### Rejected

- **Shell history scraping.** Commands you ran are a different thing from
  questions you asked. `atuin` does that well; `mission.md` names it a non-goal.
- **A global `$PROMPT_COMMAND` / `preexec` shim.** Captures everything, which is
  precisely what we don't want.
- **`LD_PRELOAD` / `DYLD_INSERT_LIBRARIES` interception.** Works, and would get
  the tool flagged by every EDR on a corporate laptop. Deserved.

### On the way in: redaction, then indexing

Between capture and commit, every exchange passes a filter:

1. **Pattern rules** for known-shaped credentials — `sk-…`, `ghp_…`, AWS keys,
   JWTs, `Authorization:` headers, PEM blocks. The `gitleaks` ruleset is the
   obvious starting corpus.
2. **Entropy scan** over assignment-shaped tokens, to catch the shapes no rule
   knows.
3. **Path-based ignore** (`tmem ignore <path>`) evaluated before any of it.

Matches are replaced with `[redacted:aws-key]` and the row is flagged. This is
prevention; `tmem forget` is the valve for what prevention misses. It is not a
substitute for having it.

Capture then hands off to a background writer so the assistant's exit isn't
blocked on embedding generation. Latency budget at the hook: **under 5ms** —
serialize, append to a queue, return.

---

## How retrieval works

Three stages, and only the first is always paid for.

### 1. Metadata pre-filter

`--in`, `--since`, `--repo` become SQL predicates that run *before* any ranking.
This is the cheapest and most effective lever in the system: as scenario 2
shows, constraining by where and when you were collapses the candidate set by
an order of magnitude and recovers much of what would otherwise demand semantic
search.

### 2. Hybrid candidate generation

Two retrievers run over the filtered set:

- **BM25 over FTS5**, with column weights — `commands` ≫ `prompt` > `response`.
  Query terms are stemmed and OR-ed, which is what lets `tmem <query>` accept
  bare multi-word input with no quoting.
- **Vector kNN over `sqlite-vec`**, when embeddings are enabled, for the case
  where the user's words don't overlap the archive's at all ("that thing about
  making the migration not lock the table").

Merged with **reciprocal rank fusion** — `score = Σ 1/(k + rank_i)`, k=60 —
rather than tuned score blending. RRF needs no calibration between two scoring
systems with incomparable scales, and it degrades gracefully: with embeddings
off it's plain BM25 and the tool still works.

### 3. Snippet extraction

FTS5's `snippet()` for the matched region, expanded to a whole line or fenced
block so a command is never shown truncated. A response can be hundreds of
lines; a result list of full responses is unusable.

**Budget: p95 under 100ms** for a cold `tmem <query>` on a 100k-exchange
archive. Above that, people stop reaching for it and the archive dies.

### Embeddings, and how not to require them

`bge-small-en-v1.5` through ONNX Runtime on CPU — 384 dims, quantized, roughly
5ms per exchange on an M-series laptop. Generated in the background writer, never
in the hook.

But **embeddings are optional and off until the model is downloaded.** The tool
must be fully useful with zero model on disk, because a first run that pulls
130MB before answering anything is a first run people abandon. Scenario 1 is the
proof obligation: the common case has to land on keyword search alone.

---

## How memory reaches the agent

Retrieval that ends in the user's eyes is half the mission. The third pillar is
feeding it back.

### MCP server — the primary path

```
tmem mcp        # speaks Model Context Protocol over stdio
```

MCP is the right shape here: it's an open protocol, it's spoken by Claude Code
and a growing set of clients, and it means term-mem exposes *one* interface
rather than a per-assistant integration. Registration is a line of config:

```bash
claude mcp add term-mem -- tmem mcp
```

Tools exposed:

| Tool | Arguments | Returns |
| --- | --- | --- |
| `search_memory` | `query`, `in?`, `since?`, `repo?`, `limit?` | Ranked exchanges with snippets |
| `get_exchange` | `id`, `session?` | One exchange, or its whole thread |
| `recent` | `in?`, `limit?` | Latest exchanges |

Two rules on this surface. **It is read-only** — an agent can search the
archive, never write to or delete from it. Capture is the user's, not the
model's. And **results carry provenance** (id, timestamp, cwd) so the agent can
cite where a claim came from and the user can go read it.

### Automatic recall via hooks

The same `UserPromptSubmit` hook that captures can also inject. Given a prompt,
run the retrieval pipeline, and if anything clears a relevance floor, prepend a
short block of prior exchanges to the context.

This is powerful and it is also how the tool becomes annoying, so: **off by
default**, capped hard (3 exchanges / ~1500 tokens), and always visibly
attributed in the transcript. Silently steering a model with retrieved text the
user can't see is the opposite of the tool's premise.

### Open-source and self-hosted models

Nothing above assumes a specific vendor, and the local-LLM path has to be
first-class for a tool whose whole pitch is that nothing leaves the machine.
Three levels, depending on what the model can do:

**Tool-calling models** (via Ollama, llama.cpp's server, vLLM, or anything else
with an OpenAI-compatible `/v1/chat/completions`): `tmem tools --schema openai`
emits the same three tools as JSON Schema, and `tmem call search_memory --args
'{…}'` executes one and returns JSON. Same logic as the MCP server, different
envelope. Qwen, Llama, and Mistral instruction-tuned variants handle this well.

**Models without reliable tool use:** retrieve first, stuff after. `tmem <query>
--json --limit 3 | tmem render --prompt-block` produces a fenced context block
to prepend. Dumber, works everywhere, and it's what the pipe in scenario 3
already does.

**Local agent frameworks** get the MCP server, since most now speak it.

Capture from local models runs through the same tiers: Ollama's own chat history
where the frontend keeps one, otherwise `tmem run` around the CLI.

### Why not a subcommand for reuse

`cli.md` leaves open whether recall-and-reuse is a `tmem` subcommand or belongs
to the assistant integration. The answer this stack implies is **neither, mostly**:
`--json` plus a pipe covers the manual case, MCP covers the agentic one, and the
subcommand would be a third way to do what those two already do. `render` earns
its place only as a formatting helper.

---

## Supporting cast

- **Testing:** `insta` for snapshot tests over CLI output and transcript
  parsers, `criterion` for the retrieval latency budget, `proptest` on the
  redaction rules — a false negative there is a leaked credential.
- **Fixtures:** recorded (and scrubbed) transcript samples per adapter, checked
  in. Adapter parsers are the most fragile code in the project and need the most
  regression pressure.
- **Migrations:** `refinery`, forward-only. A user's archive is years of data
  that cannot be regenerated.
- **Optional TUI:** `ratatui` + `nucleo` for the fuzzy picker `cli.md` lists as
  an open question. Deliberately behind a flag until the non-interactive path is
  proven — the mission says terminal-native and pipe-friendly, and an
  interactive-first tool tends to stop being either.
- **No telemetry.** Not opt-in, not anonymized, not for crash reports. There is
  no code in the binary that opens a network socket, and that's a property worth
  being able to state and have a user verify with `strings` and `lsof`.

## Open questions

- Whether the background writer is a daemon or a spawned-per-capture process. A
  daemon amortizes model loading; a spawned process has no lifecycle to manage
  and nothing to leave running on a user's machine. Leaning spawned, with the
  embedding backlog processed in batches.
- ~~Whether transcript tailing and hook capture can coexist without
  double-writing.~~ **Resolved:** the hook is a trigger, not a source — both
  paths run the same parser against the same file, deduplicated on
  `(session_id, uuid)`.
- ~~Whether other assistants (aider, Codex CLI, Cursor's CLI) persist
  transcripts in a form this complete.~~ **Partly resolved:** Codex CLI does —
  [phases/codex-cli-format.md](phases/codex-cli-format.md) — so tier 1 holds for
  a second vendor. aider and Cursor's CLI still need the same check. What the
  survey *did* unsettle is the parser interface, below.
- **Whether the dedup key can be universal.** It can't. Codex CLI records carry
  no `uuid` and no parent pointers, and its timestamps collide (2,186 unique
  across 2,665 records), so its only safe key is positional —
  `(file_path, line_number)` over an append-only file. Each adapter must declare
  its own key rather than inheriting `(session_id, uuid)`. Blocking for
  Phase 6, and it changes the Phase 1 interface.
- **Whether the human-prompt discriminator holds outside the VSCode
  entrypoint.** Every Phase 0 session carried `promptSource: "sdk"`; a
  bare-terminal session may use another value or omit the field. Treat it as a
  positive signal and gate on the negative discriminators, which are
  entrypoint-independent. Blocking for Phase 1.
- Whether `sqlite-vec` is mature enough to bet the vector path on, or whether
  a flat brute-force scan over a `BLOB` column is honestly sufficient at the
  scale of one person's archive. It very likely is, up to ~100k rows.
- What happens to the embedding index when the model is upgraded. Re-embedding
  a large archive is expensive, and mixed-model vectors are silently wrong.
