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

## Phase 1 — Capture and browse ✅

**Done 2026-09-05 — [phases/phase-1.md](phases/phase-1.md).** Verdict: the scope
ships and the hook budget is met with room (p95 2.35 ms against 5 ms, and flat
in transcript size). Two findings changed the design rather than confirming it.
Phase 0's rule that `<command-name>` records are echoes turns out to reject real
requests — the session that built this phase would have archived nothing — and
unwrapping them exposed a worse trap underneath: a resumed transcript can hold
assistant records whose prompts were never written to it, and the tree walk
hangs them off the nearest `/clear` echo, producing a well-formed row containing
14,870 characters of an unrelated conversation. Those records are now dropped
*and counted*, which is honest but not a resolution. The Exit criterion's
two-week soak is the one part still outstanding.

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
- The parser findings from Phase 0, each with a checked-in fixture. The first
  task is confirming the human-prompt discriminator against a bare-terminal
  session, which Phase 0 could not sample and which the whole ingest hangs on.
  **Resolved without that sample:** a record with `origin.kind: "human"` and no
  `promptSource` at all exists inside the entrypoint Phase 0 *did* sample, which
  settles it — the field cannot gate ingest. See
  [phases/phase-1.md](phases/phase-1.md) finding 1.
- A watermark per session, so ingest is incremental rather than a full reparse.
  **Corrected in flight:** it cannot be a resume offset, because assembly is
  many-to-one over out-of-order records. It is a change detector, and the
  reparse it triggers is affordable only because the hook never does it.
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
boundary is in the user's way by construction. **Measured: p95 2.35 ms**, and
2.14 ms against an 8 MB transcript, because the hook enqueues rather than parses.

---

## Phase 2 — Keyword recall ✅

**Done 2026-09-06 — [phases/phase-2.md](phases/phase-2.md).** Verdict: the scope
ships and the query budget is met with room. The ranking was the easy half. The
hard half was that the archive being ranked was missing 42% of itself: Phase 1's
carried-forward suspicion that resumed sessions omit their prompts was wrong —
the prompts were always in the transcript, and Phase 1's own discriminator was
rejecting them because the editor prepends `<ide_opened_file>` to the record
that carries the question. Fixing it takes the orphan count from 21 records
(19 KB) to zero. Two design claims did not survive: `tech-stack.md`'s
`exchanges_fts` cannot be created as written (finding 4), and scenario 2's "the
first is the one" is contradicted by BM25's length normalisation (finding 2),
which is a defect in the scenario rather than in the ranking.

*Scenario 1 works end to end.*

FTS5 with `porter unicode61`, BM25 ranking, and the extracted-command index that
all three scenarios lean on. No embeddings, no fusion, nothing to configure.

**Scope**

- `exchanges_fts` maintained transactionally with `exchanges` — an index that
  can drift from its table produces results that point at rows that aren't
  there.
- The `commands` FTS column, weighted above prose. This is the single
  highest-leverage ranking decision available and it costs nothing at query
  time. **Extraction itself shipped in Phase 1**, not here: `tool_use` blocks
  are mined and discarded at capture, so commands not extracted at write time
  are unrecoverable. Only the index and the weighting were ever re-derivable.
  See [phases/phase-1.md](phases/phase-1.md) finding 7.
- `tmem <query>` as the default verb, with the `PATH` collision check from
  [cli.md](cli.md).
- `--in`, `--since`, `--repo`, `--json`, `--limit`. Metadata filters run
  *before* the text query, so they collapse the space rather than filtering the
  results.
- Snippets with the matched region highlighted; pipe detection; exit codes
  `0`/`1`/`2`.
- `forget` extended to `--since` and `--in`, and now responsible for the index
  and the derived command rows too.

**Exit:** scenarios 1 and 2 run verbatim against a real archive. Phase 1 already
carries the browse half of scenario 2 (`log --in`, `show --session`); what is
missing is every line that begins `tmem <query>`. p95 query
latency under 100ms on 100k exchanges — generate the synthetic archive to prove
it rather than waiting to be surprised in year two. **Scenario 1 runs verbatim;
scenario 2 runs verbatim except for one ordering sentence, corrected in
[scenarios.md](scenarios.md) rather than coded around. p95 measured on a
generated 100k-exchange archive — see [phases/phase-2.md](phases/phase-2.md)
finding 5.**

**Deliberately not:** semantic search. If scenario 1 needs embeddings to work,
the tokenizer is wrong and adding vectors would hide that.

---

## Phase 3 — Redaction, and honest deletion ⚠️

**Done 2026-09-19, four of six scope items —
[phases/phase-3.md](phases/phase-3.md).** Verdict: pattern-rule redaction
pre-write, the `redacted` flag and counts, the user rule file, the audited
deletion path and `export`/`import` all ship. Two items did not, in opposite
directions, and both are findings rather than omissions. **The entropy fallback
is implemented and off by default**: against the real archive it fired 38 times
and was wrong 38 times — paths and filenames, never a credential — and because
the raw `tool_use` block is never stored, each wrong answer destroys a command
line permanently, which contradicts this roadmap's first principle. **Encryption
at rest is not implemented**: SQLCipher itself works (verified), but every key
an unattended capture hook can read sits next to the database it protects, and
`status` printing `encrypted yes` for that is the one thing this project exists
not to do. The Exit criterion passes for credentials with a recognisable shape.

*The phase that earns the privacy claim in the mission.*

Scenario 3 argues redaction-on-capture is load-bearing, and Phase 0 will have
produced evidence about what a real archive contains. This phase acts on it.

**Scope**

- Gitleaks-style pattern rules plus a Shannon-entropy fallback, applied
  **pre-write**. A redactor that runs after the insert has already lost.
  **Shipped, with the entropy half off by default** — see
  [phases/phase-3.md](phases/phase-3.md) finding 2. The rule is one line of
  config away; what changed is the default, because a false positive here is
  permanent data loss rather than a bad search result.
- `redacted` flagged on the row, with the count visible in `status` — silent
  redaction leaves the user unable to tell a mangled response from a bad one.
- A user rule file, because internal hostname and ticket-ID shapes are
  site-specific and no shipped ruleset will guess them.
- Deletion audited end to end: row, FTS entries, command rows, snippet cache,
  and `VACUUM`, so a deleted secret is genuinely not on disk. Test it by
  grepping the raw database file after a `forget`. **Partly done: Phase 2 put
  the FTS index behind triggers on `exchanges`, so a delete that reaches the row
  reaches the index, and the grep-the-file test passes today for `--last`,
  `<id>`, `--since` and `--in`.** What Phase 3 adds is the adversarial version
  of it and the artifacts that do not exist yet.
- ~~Opt-in encryption at rest (SQLCipher)~~, which is also the moment `export`
  stops being a nicety: an encrypted file isn't greppable, so the open-format
  export is what keeps the ownership promise true. **Not shipped.** The cipher
  works; the key management does not. An unattended capture hook needs a key it
  can read without a human, which puts the key on the same machine, under the
  same user, in the same directory as the database — so the feature would
  protect against almost nothing while `status` claimed otherwise. Export ships
  regardless, and `status` says plainly that the file is greppable. See
  [phases/phase-3.md](phases/phase-3.md) finding 3.
- `tmem export` / `import`.

**Exit:** a paste-a-token test, performed adversarially, leaves nothing
recoverable in the database file. **Met for a token with a recognisable shape**,
verified by grepping every byte of every file term-mem wrote — the database, the
WAL, and anything beside them. For a secret no rule knows, prevention does not
fire and `forget` is the valve, tested the same way.

---

## Phase 4 — Reuse ✅

**Done 2026-09-20 — [phases/phase-4.md](phases/phase-4.md).** Verdict: all four
scope items ship and the Exit criterion is met on every path — MCP, `tools`/
`call`, the `--json | render` pipe, and the opt-in hook. One design claim did
not survive: this document asks automatic recall to fire when something "clears
a relevance floor", and a BM25 score cannot be one. Its IDF term collapses when
every row contains the word, so a perfect match scores `5e-06` on a three-row
archive and double digits on a 100k one — the implementation passed its unit
tests and then recalled nothing at all from a real fixture. **Term coverage
replaced it**: two of the prompt's content words present, scaling to a quarter
of a long prompt, capped at four. Archive-size independent, and a sentence a
user can disagree with. The other surprise is the hook's latency, which tracks
the commonest word in the prompt rather than how many words it has (73 ms p95 at
100k exchanges, against the 100 ms search budget) — see
[phases/phase-4.md](phases/phase-4.md) finding 4.

*The third pillar. Memory goes back into a live session.*

**Note from Phase 3:** the MCP server is a third read path onto the archive, and
`import` was the third *write* path — both had to be taught the `forgotten`
tombstone. Anything added here that reads exchanges must decide what it does
about redacted rows, and "show them as they are stored" is the answer unless
something argues otherwise.

**Scope**

- `tmem mcp` over stdio: `search_memory`, `get_exchange`, `recent`. Read-only,
  provenance attached to every result. Agents read memory; they never write or
  delete it. **Shipped, with read-only as a file handle rather than a rule** —
  `SQLITE_OPEN_READ_ONLY`, so a tool added later inherits the guarantee without
  anyone remembering to. The JSON-RPC is hand-rolled: the maintained MCP crates
  bring an async runtime and HTTP/SSE transports into a binary whose whole pitch
  is that it opens no sockets.
- `tmem tools --schema openai` and `tmem call <tool>`, so a local model behind
  Ollama, llama.cpp, or vLLM reaches the same three tools without MCP.
  **Shipped**, from the same tool definitions, so the two envelopes cannot come
  to disagree about the archive.
- `tmem render --prompt-block` for models without reliable tool use — the
  `--json | render` pipe from scenario 3, formalized only as formatting.
  **Shipped, and "only formatting" turned out to include deciding how the text
  is framed** — the block declares itself as reference material rather than
  instructions, because the user's own history is a prompt-injection channel
  with a six-month fuse. See [phases/phase-4.md](phases/phase-4.md) finding 6.
- Optional `UserPromptSubmit` automatic recall: **off by default**, capped at 3
  exchanges and ~1500 tokens, and always visibly attributed. Memory injected
  invisibly is indistinguishable from the model hallucinating confidently.
  **Shipped. Off by default means no hook is registered at all** — `tmem recall
  --enable` is what writes it, because a hook that fires and declines still
  costs a process spawn on every prompt and still has to be trusted to read its
  own flag. The relevance floor is the one thing that changed shape; finding 1.

**Exit:** a new session answers from a past exchange, and the user can see
exactly which one and why it was chosen. **Met on all four paths, tested in
`tests/reuse.rs` against the real binary. The "why" is met narrowly and
honestly**: the tool reports which terms matched, what BM25 scored, and how many
of the prompt's words are present. For a keyword ranker that is the whole of
the answer, and finding 1 is what happens when you try to build a threshold on
top of it.

**Budget:** none was stated, which was an omission — the recall hook sits on the
turn boundary exactly as the `Stop` hook does, and unlike that one it cannot
enqueue and run away. It is held to the 100 ms search budget and **measured at
73 ms p95 on 100k exchanges**.

---

## Phase 5 — Semantic recall ⛔

**Blocked, and skipped in favour of Phase 6 on 2026-09-21.** Its Exit criterion
is "measurably better recall on queries that failed in Phase 2 … if it can't
beat BM25 on the author's own history, it doesn't ship." There is no such
history: term-mem has never been run for real, so there is no archive, and no
query log to say which queries failed. Building it would mean shipping
embeddings against a gate nothing can pass or fail, which is precisely the
"shipped for completeness" outcome *What would change this plan* says to avoid.
**What unblocks it is use, not code** — the soak, and then the opt-in query log
named below.

*Only now, and only if the archive says it's needed.*

By this point there are months of real queries. The question "does keyword
search miss things" has an answer from data instead of intuition.

**Except that nothing records one.** [phases/phase-2.md](phases/phase-2.md)
noted that no query is logged and that this is deliberate; Phase 4 added a
second decision waiting on the same absent data, because whether automatic
recall's coverage floor is right is also a question only a log could answer. If
this phase is to be decided on evidence rather than memory, an opt-in query log
has to land before it — and it is exactly the kind of feature this project
should be suspicious of, which is why it is named here rather than assumed.

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

## Phase 6 — Beyond Claude Code ✅

**Done 2026-09-21 — [phases/phase-6.md](phases/phase-6.md).** Verdict: the whole
scope ships — the interface generalizes, subagent transcripts are captured for
the first time, Codex CLI ingests with both of its silent traps under regression
test, and `tmem run` exists as an allowlist rather than a terminal recorder. Two
things the design did not predict. **Discovery is a third thing that is not
universal**: `~/.codex` holds three JSONL files that are not transcripts, and a
`**/*.jsonl` sweep turns the "format may have moved" warning into a permanent
false alarm — so an adapter now declares where its files are as well as how to
read them. And **Phase 2's guess about subagent transcripts was wrong in the
useful direction**: `isSidechain` is on all 445 records, so the four phases of
"zero sidechain records" were never a parsing problem, only a directory nobody
walked into.

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
  **Shipped, and it turned out to be three things rather than two** — discovery
  is per-adapter too, for reasons the survey could not have found because it
  read records rather than directories. `ParsedExchange` also gained `repo`, so
  an adapter that is *told* the repository beats the pipeline walking the
  filesystem for a `.git`: Codex supplies it and the filesystem answer is only
  right while the checkout is still where it was.
- **Subagent transcripts, which are a Claude Code adapter gap rather than a new
  vendor.** Phase 2 found them in `<project>/<session>/subagents/agent-*.jsonl`
  — a directory nothing looks in, which is why Phases 0 and 1 both recorded
  "zero sidechain records". Their root is a `user` record with `isMeta: true`
  holding the agent's instructions, so the inline-`isSidechain` shape the parser
  was built for may not be the shape that occurs. See
  [phases/phase-2.md](phases/phase-2.md) finding 3.
- Then Codex CLI, then aider, then the rest — each with checked-in fixtures.

The PTY wrapper (`tmem run <assistant>`) lands here as the explicitly lossy last
resort — and stays last, because the governing rule doesn't move: **we never
watch the terminal.** Capture happens only from processes with an explicit
adapter. Its real constituency is now clearer: not coding agents, which nearly
all persist, but the plain-REPL tier — `ollama run`, `llama.cpp -i`, `sgpt` —
where there is genuinely nothing on disk.

**Shipped, against an allowlist of three REPLs** — the rule above needed a CLI
expression, and `tmem run bash` refusing is it. One design change:
[tech-stack.md](tech-stack.md) has the wrapper reconstructing turn boundaries
from a prompt pattern by watching the repainting screen, and that is the hard
version of a problem we do not have. Owning the pty means seeing what the user
*typed* separately from what the program printed, so a turn boundary is the user
pressing Enter. The response stays lossy, and the tool now reports when it drops
a turn rather than quietly keeping one of two. See
[phases/phase-6.md](phases/phase-6.md) finding 4.

**Also here, if wanted:** the interactive picker
([cli.md](cli.md)'s third open question) — `ratatui` plus `nucleo` over results,
as a separate mode rather than the default. **Not built.** It was the one
optional item and nothing in six phases has wanted it.

**Not done, and it is the largest thing outstanding in this project: nobody has
ever used term-mem.** There is no `~/.local/share/term-mem/memory.db` on the
author's machine. Phase 1's Exit criterion — "the author runs it against their
own daily work for two weeks" — has been carried forward for five phases, and
this is the phase that found out why: it was never started. Six phases of
features, no day of real use, and no doc recorded the gap. See
[phases/phase-6.md](phases/phase-6.md).

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
