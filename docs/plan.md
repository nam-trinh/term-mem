# term-mem — Roadmap

Status: design sketch. A phased path from nothing to the mission in
[mission.md](mission.md), using the surface in [cli.md](cli.md), the machinery in
[tech-stack.md](tech-stack.md), and the walkthroughs in
[scenarios.md](scenarios.md) as acceptance tests.

The ordering principle: **capture is irreversible, retrieval is not.** An
exchange not captured in March cannot be recalled in May, but a ranking function
can be replaced any time. So capture correctness comes first and stays
conservative; retrieval quality is allowed to start crude and improve against a
growing archive.

The second principle: **each phase ends with something a real user can run.**
No phase exists only to enable the next one. If a phase would ship nothing
usable on its own, it's the wrong cut.

---

## Phase 0 — Prove the premise ✅

*Throwaway code. The point is to be wrong cheaply.*

**Done 2026-09-05 — [phases/phase-0.md](phases/phase-0.md).** Verdict: the
premise holds and the architecture is unchanged, but the parser is materially
harder than described — six traps, five of them silent. Two findings changed
[tech-stack.md](tech-stack.md) (a `thread_id` column; `parentUuid ??
logicalParentUuid` for thread walking). Volume and credential questions came
back too weakly sampled to move anything, and are re-asked before Phase 3.

The entire design rests on one empirical claim: that Claude Code persists
complete assistant responses, in structured form, on disk. That claim has been
spot-checked. What hasn't been checked is whether it holds across the shapes
real sessions take.

- Parse a month of local `~/.claude/projects/**/*.jsonl` into a throwaway
  SQLite file.
- Confirm the record shape holds under compaction, resumed sessions,
  subagent sidechains, interrupted turns, and very long tool outputs.
- Measure: how many exchanges does a heavy user actually produce per month, and
  how large is the resulting database?
- Grep the result for credentials. Find out empirically what a real archive
  contains before deciding how hard redaction has to work.

**Exit:** a defensible answer to "does the transcript contain what we need, and
what's the volume." If it doesn't, the capture tier ordering in
[tech-stack.md](tech-stack.md) is wrong and better to learn now.

**Deliberately not:** any code that survives into Phase 1.

---

## Phase 1 — Capture and browse

*The first honest version. It records, and it can show you what it recorded.*

Search is absent on purpose. A tool that captures reliably and lets you scroll
what it captured is already useful; a tool that searches an archive it fills
incorrectly is worse than useless, because the failures are silent.

**Scope**

- The schema from [tech-stack.md](tech-stack.md), under migrations from day one.
  Every subsequent phase changes this file; unversioned schemas become
  unupgradable ones.
- Transcript ingest for Claude Code, keyed by `(session_id, uuid)` and
  idempotent — re-running the parser over the same file must be a no-op.
  Everything downstream depends on this being safe to retry.
- The seven parser findings from Phase 0, each with a checked-in fixture. The
  first task is confirming the human-prompt discriminator against a
  bare-terminal session, which Phase 0 could not sample and which the whole
  ingest hangs on.
- A watermark per session, so ingest is incremental rather than a full reparse.
- `tmem init` — create the database, register the `Stop` hook, print what is
  about to be recorded.
- `tmem status`, `tmem doctor`, `tmem recent`, `tmem log`, `tmem show`.
- `tmem pause` / `resume` / `ignore`, and `TMEM=0`. Capture control ships *with*
  capture, never after it.
- `tmem forget --last | <id>`. The safety valve is not a later feature. From the
  first commit that writes to disk, there must be a way to unwrite.

**Exit:** the author runs it against their own daily work for two weeks without
losing an exchange, duplicating one, or noticing it running.

**Budget:** hook latency under 5ms. Measured, not assumed — a hook on the turn
boundary is in the user's way by construction.

---

## Phase 2 — Keyword recall

*Scenario 1 works end to end.*

FTS5 with `porter unicode61`, BM25 ranking, and the extracted-command index that
all three scenarios lean on. No embeddings, no fusion, nothing to configure.

**Scope**

- `exchanges_fts` maintained transactionally with `exchanges` — an index that
  can drift from its table produces results that point at rows that aren't
  there.
- Command extraction into its own table and its own FTS column, weighted above
  prose. This is the single highest-leverage ranking decision available and it
  costs nothing at query time.
- `tmem <query>` as the default verb, with the `PATH` collision check from
  [cli.md](cli.md).
- `--in`, `--since`, `--repo`, `--json`, `--limit`. Metadata filters run
  *before* the text query, so they collapse the space rather than filtering the
  results.
- Snippets with the matched region highlighted; pipe detection; exit codes
  `0`/`1`/`2`.
- `forget` extended to `--since` and `--in`, and now responsible for the index
  and the derived command rows too.

**Exit:** scenarios 1 and 2 run verbatim against a real archive. p95 query
latency under 100ms on 100k exchanges — generate the synthetic archive to prove
it rather than waiting to be surprised in year two.

**Deliberately not:** semantic search. If scenario 1 needs embeddings to work,
the tokenizer is wrong and adding vectors would hide that.

---

## Phase 3 — Redaction, and honest deletion

*The phase that earns the privacy claim in the mission.*

Scenario 3 argues redaction-on-capture is load-bearing, and Phase 0 will have
produced evidence about what a real archive contains. This phase acts on it.

**Scope**

- Gitleaks-style pattern rules plus a Shannon-entropy fallback, applied
  **pre-write**. A redactor that runs after the insert has already lost.
- `redacted` flagged on the row, with the count visible in `status` — silent
  redaction leaves the user unable to tell a mangled response from a bad one.
- A user rule file, because internal hostname and ticket-ID shapes are
  site-specific and no shipped ruleset will guess them.
- Deletion audited end to end: row, FTS entries, command rows, snippet cache,
  and `VACUUM`, so a deleted secret is genuinely not on disk. Test it by
  grepping the raw database file after a `forget`.
- Opt-in encryption at rest (SQLCipher), which is also the moment `export`
  stops being a nicety: an encrypted file isn't greppable, so the open-format
  export is what keeps the ownership promise true.
- `tmem export` / `import`.

**Exit:** a paste-a-token test, performed adversarially, leaves nothing
recoverable in the database file.

---

## Phase 4 — Reuse

*The third pillar. Memory goes back into a live session.*

**Scope**

- `tmem mcp` over stdio: `search_memory`, `get_exchange`, `recent`. Read-only,
  provenance attached to every result. Agents read memory; they never write or
  delete it.
- `tmem tools --schema openai` and `tmem call <tool>`, so a local model behind
  Ollama, llama.cpp, or vLLM reaches the same three tools without MCP.
- `tmem render --prompt-block` for models without reliable tool use — the
  `--json | render` pipe from scenario 3, formalized only as formatting.
- Optional `UserPromptSubmit` automatic recall: **off by default**, capped at 3
  exchanges and ~1500 tokens, and always visibly attributed. Memory injected
  invisibly is indistinguishable from the model hallucinating confidently.

**Exit:** a new session answers from a past exchange, and the user can see
exactly which one and why it was chosen.

---

## Phase 5 — Semantic recall

*Only now, and only if the archive says it's needed.*

By this point there are months of real queries. The question "does keyword
search miss things" has an answer from data instead of intuition.

**Scope**

- `fastembed-rs` / ONNX Runtime with `bge-small-en-v1.5` (384 dims, ~130MB),
  `sqlite-vec` for storage and kNN.
- The model is **not** bundled and **not** downloaded at install. `tmem
  embed --enable` fetches it; until then the tool is smaller and works.
- Reciprocal rank fusion over BM25 and vector ranks, `k=60`. Chosen because it
  needs no score calibration and degrades to plain BM25 when embeddings are
  absent — which means Phase 2's behavior is the graceful-failure path, not a
  legacy mode.
- Backfill embeddings for the existing archive in the background, resumable,
  never blocking a query.

**Exit:** measurably better recall on queries that failed in Phase 2, with no
regression on the ones that worked. If it can't beat BM25 on the author's own
history, it doesn't ship.

---

## Phase 6 — Beyond Claude Code

*Widening capture, once the pipeline is proven against one assistant.*

`tech-stack.md` listed as an open question whether aider, Codex CLI, and
Cursor's CLI persist transcripts in comparably complete form. **Codex CLI is
surveyed and the answer is yes** —
[phases/codex-cli-format.md](phases/codex-cli-format.md). Most terminal coding
agents persist something; the risk isn't availability, it's that each format has
its own silent-failure surface.

**Codex CLI is the first adapter**, ahead of aider. It's structured JSONL rather
than aider's markdown, its `session_meta` supplies `repo`, `branch`, and
`repository_url` outright, and it exercises the parser interface hardest: it has
no per-record identity, so `(session_id, uuid)` doesn't apply, and it ships a
duplicate event stream that double-counts every response if ingested naively.

- Generalize the parser interface first, so each adapter declares its own dedup
  key and its own injected-block vocabulary. Phase 1's interface was designed
  against a sample of one and assumes both are universal. They aren't.
- Then Codex CLI, then aider, then the rest — each with checked-in fixtures.

The PTY wrapper (`tmem run <assistant>`) lands here as the explicitly lossy last
resort — and stays last, because the governing rule doesn't move: **we never
watch the terminal.** Capture happens only from processes with an explicit
adapter. Its real constituency is now clearer: not coding agents, which nearly
all persist, but the plain-REPL tier — `ollama run`, `llama.cpp -i`, `sgpt` —
where there is genuinely nothing on disk.

**Also here, if wanted:** the interactive picker
([cli.md](cli.md)'s third open question) — `ratatui` plus `nucleo` over results,
as a separate mode rather than the default.

---

## What would change this plan

- **Phase 0 finds the transcript incomplete.** Then the tier ordering inverts,
  the PTY wrapper moves to Phase 1, and the project is materially harder.
- **Phase 2 latency doesn't hold at scale.** Then either the archive gets a
  retention policy or the storage decision reopens — but not both at once.
- **Phase 5 shows no gain.** Then embeddings are dropped rather than shipped for
  completeness, and the mission's "fuzzy, semantic" line gets revised to match
  what's true.

The riskiest assumptions are all in Phase 0 and Phase 2, which is where they
belong.
