# term-mem — CLI surface

Status: Phases 1 to 4 and 6 shipped, so searching, browsing, capture control,
deletion, data ownership, reuse and multi-assistant capture are all now *as
implemented* rather than as sketched. Phase 5 (semantic recall) is blocked
rather than built — see [plan.md](plan.md). Anything not yet built is marked
with the phase that owns it.

## Name

The project is **term-mem**. The binary is **`tmem`**.

Two names doing two jobs — a distinctive, greppable, searchable project name and
a short thing to type — is standard practice (`ripgrep`→`rg`,
`fd-find`→`fd`, `the-silver-searcher`→`ag`). `tmem` is an obvious contraction of
the project name, so the two reinforce each other rather than being unrelated
strings to learn.

Rejected: `tm` (two chars, but widely used as a personal tmux alias, and needs
explaining in every README), `mem` (reads like a memory profiler), `recall`
(self-documenting but back to six characters).

**Install-time collision check.** If `tmem` already resolves to something on the
user's `PATH`, `init` says so and offers an alternative. Silently shadowing an
existing command is hostile, however unlikely the conflict.

## Search is the default verb

Anything that isn't a recognized subcommand is a search query:

```
tmem ffmpeg concat filter
```

Search is ~95% of invocations — capture is automatic and configuration happens
once — so it earns the bare form. Most tools can't do this because they have no
dominant verb; this one does.

Multi-word queries need no quoting. Query terms are already parsed, stemmed, and
OR-ed rather than passed raw to the search index, so joining `argv` costs
nothing and removes a ritual from the hot path.

`tmem search <query>` remains available as the explicit form, for scripts and
for queries that collide with a subcommand name.

A query that matches nothing exits `1` and names the browse commands, rather
than printing an empty list — an empty list is indistinguishable from an archive
that had lost the exchange. A query with no searchable terms in it at all
(`tmem search ---`) exits `2`, because that is a different thing from finding
nothing.

**This constrains the subcommand list.** Every reserved word is a query that
behaves surprisingly. Keep the set small, stable, and made of words nobody
searches for.

Phase 4 spent some of that budget: `mcp`, `tools`, `call`, `render` and `recall`
are now reserved, and `tools` and `call` are ordinary English. Phase 6 added
`run`, which is worse than either — but it is the name `tech-stack.md` gives the
PTY tier and `tmem search run` still works. `tmem call` is a
usage error, not a search for the word. `tmem search call` is the escape hatch
and always was — but this is the first time a reserved word has had a meaning,
and the list should not grow again without a reason as good as "the tech stack
names it".

## Command surface

### Searching

```
tmem <query>                    search everything
tmem <query> --in <path>        limit to a directory tree
tmem <query> --since <when>     limit by time
tmem <query> --repo             limit to the current git repo
tmem <query> --json             machine-readable, for piping
tmem <query> -n <count>         how many results (default 20)
```

Metadata filters carry real weight here. Constraining by where and when you were
collapses the search space before the text query runs, and recovers much of what
would otherwise need semantic search.

Flags may come before or after the query terms — `tmem backfill --repo --since
january` is the form scenarios.md types, and it works. The cost is that a query
term beginning with `-` needs `tmem search -- <term>`.

Ranking is BM25 with the command lines weighted well above prose. It orders
results *within* a filtered set; it is not a way of telling two similar
exchanges apart, and where two rows match a single term equally the shorter one
wins. See [phases/phase-2.md](phases/phase-2.md) finding 2 — the filters are the
lever, and the ranking is the part the roadmap expects to replace.

### Browsing

The fallback when recall fails — and it will fail, since a query with no
overlapping terms finds nothing:

```
tmem recent                     latest exchanges
tmem log --in <path>            everything from a directory tree
tmem show <id>                  one exchange, in full
tmem show <id> --session        the surrounding conversation (thread, not file)
```

`recent`, `log` and search share the metadata filters — `--in`, `--since`,
`--repo`, `--json`, and `-n/--limit` (default 20); `recent` and `log` differ only
in intent and both order newest first. `--in` matches a directory tree exactly,
so `--in ~/src/api` does not also match `~/src/api-legacy`, and it matches a tree
under every name the filesystem gives it — on macOS `/var/folders/…` and
`/private/var/folders/…` are the same directory, and the archive holds whichever
one the assistant recorded. `--since` takes `2h`, `7d`, `3w`,
`2 hours ago`, `today`, `yesterday`, or `2026-03-01`; anything else is an error
rather than a silent fallback to the epoch, which would return the whole archive
and look like it worked.

`show` accepts any unambiguous id prefix, so `tmem show 01M1QM9B` is enough.
`--session` groups on `thread_id`, not `session_id` — `/clear` starts a fresh
conversation inside the same transcript file, and grouping on the file would
merge unrelated threads.

Browsing by time and place rescues a large fraction of failed searches, and it's
an ordinary query rather than a search problem.

### Controlling capture

Three scopes, because "off" means different things:

```
tmem pause                      global, until resumed
tmem pause 2h                   global, auto-resumes
tmem resume
tmem ignore <path>              this tree, permanently
tmem ignore --list / --remove
TMEM=0 <assistant>              this invocation only
```

Pause and the ignore list are plain files under the data directory rather than
rows in the database, because the capture hook consults both on every turn and
opening SQLite to ask would not fit the latency budget. They are greppable and
hand-editable, like everything else the user owns.

`ignore` affects capture from that point on; it does not retroactively delete.
The command says so, and points at `forget --in` for that.

**Pause state must be visible.** A user who believes it's recording when it's
paused loses work; one who believes it's paused when it's recording gets a nasty
surprise. Surface it — a prompt segment, a notice on assistant start.

Path-based ignore is the one that sees real use: there's usually one directory
whose contents shouldn't be archived even though everything else should.

### Redaction

Credentials with a recognisable shape are replaced *before* anything is written
— `sk-…`, `ghp_…`, AWS and GCP keys, JWTs, `Authorization:` headers, PEM blocks,
and the rest of the thirteen shipped rules. The replacement names the rule
(`[redacted:github-token]`), the row is flagged, and both `capture` and `status`
count it, because silent redaction leaves the user unable to tell a mangled
response from a bad one.

```
$ tmem capture --all
12 new, 0 updated, from 3 transcript(s) (0 unchanged)
  redacted: 1 exchange(s) — github-token, aws-access-key ×2
```

Site-specific shapes go in `~/.config/term-mem/redact.toml` (`$TMEM_CONFIG_DIR`
redirects it):

```toml
[[rule]]
name = "internal-host"
pattern = '\b[a-z0-9-]+\.corp\.internal\b'

[builtin]
email = true          # off by default

[entropy]
enabled = true        # off by default — see below
min_bits = 4.0
min_len = 20
```

A rule that does not compile is a **fatal** error, not a warning: a redactor the
user believes is running and which silently is not is the worst outcome here.
Because the capture drainer runs detached with its stderr discarded, `tmem
doctor` also loads the ruleset and reports a broken file — otherwise capture
stops and nothing says why.

**The entropy fallback is off by default.** It catches assignment-shaped
high-entropy values that no pattern rule knows, and on a real archive every
single thing it caught was a filesystem path or a UUID filename. Because mined
command lines are the only copy that exists, a false positive is permanent data
loss. [phases/phase-3.md](phases/phase-3.md) finding 2 has the numbers.

Redaction is prevention, not a guarantee. `forget` is the valve for what it
misses.

### Deleting

The real safety valve. People realize *after* the fact that they pasted a
credential or discussed something sensitive:

```
tmem forget --last
tmem forget <id>
tmem forget --since '1 hour ago'
tmem forget --in <path>
```

`--since` and `--in` select a set and cannot be combined with an id or `--last`;
mixing them is an error rather than a guess, because guessing wrong on this
command deletes more than was asked for. The confirmation lists the rows, not a
count — a number is not something a person can check.

`forget` confirms interactively and takes `-y`/`--yes` to skip that. The prompt
is gated on *stdin* being a terminal, not stdout, so `tmem forget <id> | tee log`
still asks. It deletes the row, its mined commands and its file references in one
transaction, checkpoints the WAL and `VACUUM`s, so the text is not recoverable
from a free page — which an integration test checks by grepping the raw database
file afterwards.

It also records the deleted exchange's dedup key, and only that, so the next
ingest of the same transcript does not put it back. `status` shows the count.
The search index needs no mention here because it is maintained by triggers on
the row: a delete that reaches `exchanges` has already reached `exchanges_fts`.

These are genuine deletes, including from the search index — never a hidden
flag on a row that stays on disk. An integration test reads every byte of every
file in the data directory afterwards, the WAL included, and fails if the secret
is in any of them.

What `forget` does **not** touch is the assistant's own transcript, which is the
user's file and still contains whatever was pasted. The tombstone is what stops
the next ingest putting it back.

### Reuse — feeding memory back

Three routes to the same three read-only tools, because what reaches the model
depends on what the model can do:

```
tmem mcp                        Model Context Protocol over stdio
tmem tools --schema openai      the same tools as JSON Schema
tmem tools --schema mcp         …in MCP's envelope, for a client that wants it
tmem call <tool> --args '{…}'   run one and print its JSON result
tmem call <tool> --args -        …with the arguments on stdin
tmem <query> --json | tmem render --prompt-block
```

The tools are `search_memory` (`query`, `in?`, `since?`, `repo?`, `limit?`),
`get_exchange` (`id`, `session?`) and `recent` (`in?`, `limit?`). Two properties
hold across all three routes:

**Read-only, as a file handle.** The archive is opened with
`SQLITE_OPEN_READ_ONLY`, so an agent cannot write to or delete from it whatever
it asks for. Capture is the user's, not the model's.

**Every result carries provenance,** including a `cite` field holding the
literal `tmem show <id>` that displays the same exchange, and a `why` saying
which terms matched and what BM25 scored. A claim an agent makes from memory is
one the user can go and check.

Two differences from the search CLI, both because an agent is not a person at a
terminal. `repo` is a repository *name* rather than the `--repo` boolean, since
a model has no meaningful current directory. And `limit` is clamped to 50 — a
tool that returns ten thousand rows does not give a better answer, it fills a
context window with someone else's afternoon; the clamp is reported in the
result rather than applied quietly.

An unknown argument is an **error**, not something to ignore. A model that calls
`search_memory({"querry": "ffmpeg"})` and gets the whole archive back has been
told something false about the user's history, and neither it nor the user has
any way to notice.

Register the MCP server with:

```bash
claude mcp add term-mem -- tmem mcp
```

`render` never opens the database. It reads the `--json` shape on stdin — from
`search`, `show` or `recent`, all three — and writes an attributed block that
says, in its own header, that nothing inside it is an instruction. `-n` caps the
entries and `--max-tokens` the size, and the budget is shared between entries
rather than spent on the first.

### Automatic recall (Phase 4) — off by default

```
tmem recall                     is it on, and what are the caps
tmem recall <words>             what would be injected for this prompt, and why
tmem recall --enable            turn it on, and register the hook
tmem recall --disable           turn it off, and remove the hook
tmem recall --hook              the UserPromptSubmit hook itself
```

Off by default means **no hook is registered at all** — not a hook that fires
and decides to do nothing, which still costs a process spawn on every prompt and
still has to be trusted to read its own flag. `--enable` registers the hook
first and writes the config second, so a registration that fails leaves nothing
behind claiming it worked. `status` and `doctor` complain loudly if the two ever
disagree, because one direction means the user is being injected into without
knowing and the other means they believe they are and are not.

A hook is matched by its command after the invoking path is normalised away, so
`/usr/local/bin/tmem recall --hook` and `tmem recall --hook` are the same hook —
`--disable` removes a hook the user wrote by hand, and `--enable` does not add a
second copy of one. **`--disable` never removes anything else**, including a
group of the user's own that was already empty. settings.json is not term-mem's
file and nothing in it is term-mem's to tidy.

A `recall.toml` that will not parse turns recall **off** and says so in `status`
and `doctor`; it does not stop either of them, and it does not stop `--disable`,
which rewrites the file and is therefore also the repair. This is deliberately
unlike `redact.toml`, which is fatal: a redactor the user believes is running
and silently is not is the worst outcome there, whereas here the worst outcome
is injecting under settings nobody can read.

When it is on, at most **3 exchanges and ~1500 tokens** are prepended, and the
user sees a line naming the ids in their own terminal as well as the attribution
inside the block. Settings live in `recall.toml` in the data directory.

Whether anything is injected is decided by **term coverage** — at least two of
the prompt's content words physically present in the exchange, scaling to a
quarter of a long prompt, capped at four — and not by a score.
[phases/phase-4.md](phases/phase-4.md) finding 1 has the numbers; the short
version is that BM25 scores are not comparable between archives, so a score
floor is "always on" or "always off" depending on how much history the user has.
Exchanges from the session doing the asking are never replayed back to it.

`tmem recall <words>` prints exactly what the hook would inject, and the
coverage and score for each, so the setting can be tuned against a real archive
instead of guessed at.

### Assistants (Phase 6)

Capture is not Claude-Code-only. Each adapter declares three things the others
cannot assume: where its transcripts live, how it identifies a record for
idempotency, and which injected blocks to strip out of a prompt.

| Adapter | Transcripts | Dedup key | Env override |
| --- | --- | --- | --- |
| `claude-code` | `~/.claude/projects/<project>/*.jsonl` and `<session>/subagents/*.jsonl` | the record `uuid` | `TMEM_CLAUDE_PROJECTS` |
| `codex-cli` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | `@<line>` — no record carries an id | `TMEM_CODEX_SESSIONS` |
| `pty` | what `tmem run` recorded, under the data directory | `@<line>` | — |

`tmem capture --all` sweeps all of them. A tree that does not exist is normal
and `doctor` reports it as a note rather than a problem — most machines have one
assistant installed, not three.

**Subagent transcripts** are captured as of Phase 6. Each one becomes a single
exchange: the instructions the parent session gave the agent, and what the agent
concluded. They carry the parent's session id but their own `thread_id`, so
`tmem show <id> --session` shows the agent's conversation rather than merging it
into the one that spawned it.

`codex-cli` rows carry `repo` and `git_branch` straight from the transcript
rather than from a filesystem walk, so they still resolve after the checkout is
renamed or deleted.

### Recording a REPL (Phase 6)

```
tmem run                        list the REPLs this can record
tmem run <repl> [args…]         run it under a pty and record the turns
```

The last tier and the worst one. It exists for `ollama run`, `llama.cpp -i` and
`sgpt` — the plain-REPL tier, where there is genuinely nothing on disk. Every
coding agent surveyed so far writes a transcript, and for those this path is
strictly worse than the adapter that reads it.

**It takes a name from a fixed list and refuses everything else.** That is the
CLI expression of the rule in [mission.md](mission.md) and
[plan.md](plan.md): term-mem never watches your terminal, and `tmem run bash` is
not an oversight to be fixed.

```
$ tmem run bash
tmem: `tmem run bash` is not supported; known REPLs are ollama, llama-cli, sgpt.
  term-mem never watches the terminal — a program needs an explicit adapter,
  and adding one is a code change rather than a flag.
```

What it records is lossy and says so. Turn boundaries are reliable — they come
from what you typed, not from parsing the screen — but the response is a render
of a repainting terminal. When a REPL answers faster than the turn detector can
separate two answers, the command reports the gap rather than quietly keeping
one of two.

### Data ownership

```
tmem export --json | --markdown
tmem export --json --in <path> --since <when> --repo
tmem import <path>
```

Export is the concrete form of the mission's ownership promise. It writes to
stdout, so it pipes; closing the pipe early (`| head`) is not an error.

**Export ignores the browse default of 20.** `-n` is honoured when given
explicitly, but a backup that silently contains the first page of an archive is
worse than no backup. The other filters work, so `export --repo` is a
per-project extract.

JSON is one record per line — the same shape `--json` produces everywhere else,
plus `source_key` — and is what `import` reads. Markdown is for people and is
deliberately not re-importable; a format that is both pretty and lossless is
neither.

`import` is the second door into the database and behaves like the first: it is
keyed on the same `(assistant, session_id, source_key)`, so importing twice is a
no-op; it **redacts on the way in**, because an export may predate a rule the
user has since added; and it **respects `forget`** — an exchange the user
deleted does not come back, and the command says how many it left out. One
unreadable line costs that line, not the import.

This mattered more when encryption was expected to land here. It didn't — see
[phases/phase-3.md](phases/phase-3.md) finding 3 — so the archive is still an
ordinary SQLite file the user can open with `sqlite3`, and `status` says so.

### Setup

```
tmem init                       create the archive and wire up capture
tmem init --backfill            also import the transcripts already on disk
tmem init --no-hook             do not touch Claude Code's settings.json
tmem status                     paused? encrypted? how many exchanges?
tmem doctor                     is capture actually wired up?
tmem capture --hook <assistant> the Stop hook itself; reads its payload on stdin
tmem capture --drain            process whatever the hook queued
tmem capture --path <file>      ingest one transcript, synchronously
tmem capture --path <f> --assistant <name>   …when the filename cannot say whose
tmem capture --all              ingest every transcript, for every assistant
```

`init` edits the `Stop` hooks in `~/.claude/settings.json` in place, preserving
everything else in the file, and is idempotent. It does not ask an encryption
question, because there is no encryption to ask about; `status` states that the
file is readable with `sqlite3` and `grep` rather than leaving the field blank.

The `capture` verb is the one addition to the surface this document sketched. It
is not really a user command; it is the hook's entrypoint, and it is documented
because `doctor` names it and because `--path` is how anyone debugs an adapter.
`--hook` writes a queue entry and spawns a drainer rather than parsing anything,
which is what keeps the turn boundary under 5 ms.

`init` creates the database, wires up capture, and prints what it is about to
start recording. A tool that silently begins
archiving everything you type is one people uninstall in anger.

`init` also offers to **import existing assistant transcripts** where they're
available on disk. Starting with months of searchable history rather than an
empty database is the difference between proving the tool on day one and asking
for faith for six weeks.

## Output conventions

- **Human by default, machine on request.** Results are a scannable list —
  the prompt as the title, a highlighted snippet of the match. `--json` for
  piping.
- **Detect a pipe.** No color, no pager, one record per line when stdout isn't a
  terminal.
- **Exit codes carry meaning.** `0` found, `1` nothing found, `2` error — so
  `tmem <query> || ...` works in a script. `doctor` uses `2` for "capture is not
  wired up", and `status` uses `2` for "no archive yet". `call` and `recall`
  follow the same three, and `call` prints its JSON result either way, so a
  caller can read the answer *and* branch on the code.
- **Snippets, not transcripts.** A response can be hundreds of lines; a result
  list of full responses is unusable. Show the matched region, expand on demand.

## Open questions

- ~~Whether `--in` should default to the current directory when inside a known
  repo, or always default to global.~~ **Resolved: global.**
  [scenarios.md](scenarios.md) argues an implicit `--in .` breaks scenario 2 and
  breaks it silently, and Phase 1 implemented it that way. `--repo` is the
  opt-in for "here".
- ~~Whether recall-and-reuse (feeding a past exchange back into a live session)
  is a `tmem` subcommand or belongs entirely to the assistant-side
  integration.~~ **Resolved: mostly neither, as
  [tech-stack.md](tech-stack.md) predicted.** MCP carries the agentic case and
  `--json` on a pipe carries the manual one. The two subcommands that did land
  are not a third retrieval path: `render` never opens the database, and
  `recall` is the hook's entrypoint rather than something a person types in
  anger. `tools` and `call` are the MCP surface for clients that do not speak
  MCP, which is an envelope rather than a feature.
- Whether an interactive picker (fuzzy-select over results) is core or a
  separate mode.
