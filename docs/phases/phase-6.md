# Phase 6 — Findings

Built 2026-09-21 against [plan.md](../plan.md)'s Phase 6 scope: generalize the
parser interface, ingest subagent transcripts, add the Codex CLI adapter, and
land the PTY wrapper as the lossy last resort. This is "widening capture, once
the pipeline is proven against one assistant".

**Headline: the whole scope ships, and the survey that made this phase possible
was wrong about the thing it did not look at.**
[codex-cli-format.md](codex-cli-format.md) characterised Codex's *records* in
detail and got them right. What it never examined was the *tree*, and the tree
holds three JSONL files that are not transcripts — including one that has no
`type` field on any line. A `**/*.jsonl` sweep ingests all of them and produces
a permanent complaint about files that were never conversations. Discovery, not
parsing, was the sharper trap, and it is now the third thing an adapter
declares.

The other surprise is in the opposite direction: Phase 2 guessed that subagent
transcripts might not carry the `isSidechain` shape the parser was built for.
They carry it on every record, 445 of 445. Four phases of "zero sidechain
records" were never a parsing problem at all.

## Scope

Two real local archives, plus a live pty:

```
~/.claude/projects   12 transcripts + 5 subagent files, 9.4 MB, 2026-09-04 .. 2026-09-21
~/.codex/sessions     7 rollout files, 2,675 records,    2026-03-01 .. 2026-04-10
```

Both are one user, one machine, one entrypoint each. **Shape findings repeat
across every file and are trustworthy. Frequency findings are not** — the same
caveat [phase-0.md](phase-0.md) and [codex-cli-format.md](codex-cli-format.md)
carry, for the same reason.

After this phase, ingesting both trees together:

```
135 new, 0 updated, from 19 transcript(s)
claude-code   51 exchanges across 11 threads
codex-cli     84 exchanges across  6 threads
```

Of those 51 Claude Code exchanges, 5 come from subagent transcripts that no
previous phase could see.

## 1. Discovery is the third thing that is not universal

The interface already let an adapter declare its dedup key and its
injected-block vocabulary, both because the Codex survey found them
vendor-specific. Discovery was still a function in the pipeline —
`claude_transcripts(root)`, one level deep — because with one vendor the tree
*was* the design.

Two vendors make it obvious that it is not:

| | Claude Code | Codex CLI |
|---|---|---|
| layout | `<project>/*.jsonl` and `<project>/<session>/subagents/*.jsonl` | `sessions/YYYY/MM/DD/rollout-*.jsonl` |
| depth | 1 and 3 | 4 |
| non-transcript JSONL in the tree | none found | `session_index.jsonl`, a plugin fixture, `archived_sessions/` |

`~/.codex/session_index.jsonl` is the interesting one. Its lines are
`{id, thread_name, updated_at}` — no `type`, no `timestamp`, no `payload`. A
parser handed it reports every line as an unrecognised record type, which is
term-mem's "the format may have moved" warning, fired permanently, about a file
that was never a transcript. The warning is load-bearing (it is how a real
format change announces itself) and this would have trained the user to ignore
it.

So `Adapter` now declares `transcript_root` and `discover`, and the rule is that
**a file discovery returns must be a conversation**, because everything
downstream treats a file it cannot parse as a problem with the archive.

`archived_sessions/` is deliberately left out. It is Codex's own retention
decision, and re-ingesting what a user archived is not obviously wanted.

## 2. The subagent flag was always there; the directory was not

[phase-2.md](phase-2.md) finding 3 recorded that subagent transcripts live in
`<project>/<session>/subagents/agent-*.jsonl`, and speculated: "their root is a
`user` record with `isMeta: true` holding the agent's instructions, so the
inline-`isSidechain` shape the parser was built for may not be the shape that
occurs."

Measured across the local archive:

```
445 records in 5 subagent files
isSidechain: true    445  (100%)
user records         163
  isMeta: true         5  (one per file — the agent's instructions)
  tool_result        148
  toolUseResult       10
```

The flag occurs, universally. The speculation was wrong and the correction is
worth more than the guess: **Phases 0 and 1 both reported "zero sidechain
records" from an archive containing 445 of them**, and that number was read as a
fact about the format rather than a fact about where the parser looked. This is
[CLAUDE.md](../../CLAUDE.md)'s warning about Phase 2 finding 1 happening a
second time, in a different file, three phases later.

What the shape does require is a **mode**, because the same two discriminators
need opposite rules:

| | main transcript | subagent transcript |
|---|---|---|
| `isSidechain: true` | a subagent turn folded into the exchange above | every record; means nothing |
| `isMeta: true` | an injected local-command caveat — reject | the agent's instructions — **this is the prompt** |

A subagent file therefore yields exactly one exchange: the instructions, and
what the agent concluded. That is a good unit, and it is one a user recognises.

One thing that needed no work and is worth recording because it could so easily
have needed a lot: a subagent file carries its **parent's** `session_id`, so
these rows land in the same session as the conversation that spawned them. They
do not merge, because `--session` groups on `thread_id`, and a subagent file's
tree root is its own `isMeta` record. Same session, different thread — which is
exactly what a subagent is. The Phase 0 decision to group on `thread_id` rather
than `session_id` paid for itself again, six phases later.

## 3. Codex hands over the repository, and we were throwing it away

[codex-cli-format.md](codex-cli-format.md) finding 5 called `session_meta` "a
gift": `cwd`, `branch`, `commit_hash` and `repository_url`, resolved by the
vendor at capture time. The first implementation read all of it and dropped the
repository on the floor, because `ParsedExchange` had no field for it —
`repo` was resolved by the *pipeline*, by walking up from `cwd` looking for a
`.git`.

That is a worse answer and [tech-stack.md](../tech-stack.md) already said so:
"the checkout may be renamed or deleted by the time anyone searches", which is
exactly when old memories matter most. Codex's answer survives that; ours does
not. The integration test caught it as `repo IS NULL` and the fix is a field on
`ParsedExchange` that an adapter may fill and the pipeline falls back from.

`repository_url` is stored as the bare repo name rather than the URL, so
`--repo billing-api` finds the same checkout whichever assistant recorded it.
Seven of the eight `session_meta` records in the sample carry `git`; one carries
`repository_url` alone with no branch or commit, so every field inside it is
individually optional.

## 4. The PTY tier's turn boundaries should not come from the screen

[tech-stack.md](../tech-stack.md) describes the wrapper as teeing the stream
"through an ANSI parser that strips escape sequences and reconstructs turn
boundaries from the prompt pattern the adapter declares" — that is, by watching
the repainting screen for something that looks like a prompt.

That is the hard version of the problem and it is not the one we have. **Because
we own the pty, we can see what the user typed separately from what the program
printed.** A turn boundary is the user pressing Enter. No prompt regex, no
deciding whether a given `>` was a prompt or a quoted character, no per-REPL
pattern to maintain.

What stays lossy is the response, and that part of the design was right: a REPL
redraws, and the bytes between two Enters are a render rather than a document.

Three bugs found while building it, all in the same place — the gap between
"what a terminal does" and "what I assumed a terminal does":

- **`\r` is not an overwrite.** A pty's line discipline turns every `\n` the
  child writes into `\r\n`, so treating carriage return as "erase the current
  line" — which is correct for a spinner — deleted the line before every
  newline, which is all of them. The first live session recorded a screen of
  `"\n\n\n\n>>> "`. The fix is to resolve `\r` by what follows it.
- **The terminal echoes input the program has not read yet.** Piped or pasted
  input puts *every* queued line on screen before the first answer arrives, so
  the next question turns up inside the previous question's response.
  Interactive typing never does this, which is why it survived until a test sent
  two lines at once.
- **A turn cannot close when the next question is typed**, for the same reason.
  The first implementation did, and recorded zero turns from a two-turn session
  because both questions were queued before the program had printed a word.
  Turns close when the **output goes quiet**, which is the same signal a human
  reads off the screen.

The remaining limit is honest and unfixed: a REPL that answers faster than the
quiet window merges two answers into one. Piping a script of questions into
`tmem run` does exactly that. The tool now says so — it compares lines sent
against turns recorded and names the gap — rather than silently keeping one of
two.

## 5. `tmem run` had to be an allowlist, not a flag

The governing rule in [plan.md](../plan.md) is "we never watch the terminal.
Capture happens only from processes with an explicit adapter." A PTY wrapper is
the one feature that can quietly undo that, because a pty around `bash` records
everything.

So `tmem run` takes a name from a table of three REPLs and refuses anything
else, with the reason in the error:

```
$ tmem run bash
tmem: `tmem run bash` is not supported; known REPLs are ollama, llama-cli, sgpt.
  term-mem never watches the terminal — a program needs an explicit adapter,
  and adding one is a code change rather than a flag.
```

The recording is written as a transcript and ingested through the ordinary path,
which is what keeps redaction, the `forgotten` tombstone and idempotency from
needing a third implementation. That decision also produced the phase's only
new format: `tmem-pty` JSONL, the one transcript format term-mem writes itself,
and therefore the only one that cannot change without notice. A file in that
directory without the right header is a hard error rather than a best-effort
parse, because we control the format and a surprise there means something else
is writing to it.

## 6. The record-type allowlist keeps paying, and keeps needing feeding

Phase 0 finding 1's allowlist absorbed two more types this phase. Codex ships
`response.done` (usage accounting) which the survey did not see, and the local
Claude Code archive has grown `pr-link` since Phase 2 — 72 records of it, which
`capture` reports as an unrecognised type.

That report is working as designed and is also the thing finding 1 would have
drowned. Both are now in the known-ignored lists.

## 7. Budgets: unmoved

```
Stop hook latency (60 samples, release):
  12-record transcript     p50 2.13 ms   p95 2.94 ms   max 3.56 ms
  8 MB transcript          p50 2.01 ms   p95 2.15 ms   max 2.24 ms

100,000 exchanges, 139.5 MB:
  common two-term  p50  18.34 ms   p95  18.84 ms
  rare term        p50   3.11 ms   p95   3.23 ms
  filtered         p50  13.55 ms   p95  14.32 ms
  recall short     p50  74.40 ms   p95  75.00 ms   max 83.74 ms
  recall long      p50  51.16 ms   p95  51.77 ms
```

Nothing in this phase touches the query path, and the third adapter costs the
`--all` sweep one `read_dir` on a tree that usually does not exist.

## 8. Six defects, and the two methods that found disjoint sets of them

The review of this phase found five bugs by reading the diff. Pointing the
finished tool at a real archive and reading the output found a sixth that the
review did not, and could not easily have: **16 of 84 Codex prompts carried an
unstripped IDE context block**, the largest 6,387 characters of `## Open tabs:`
stored as if the user had typed it.

That one is worth its own paragraph because of what it is. It is the *fourth*
time this project has met "injected content is prepended to the record that
carries the question" — Phase 0 finding 2, Phase 2 finding 1, the Codex survey's
finding 3 — and the first time the wrapper was not angle brackets. The stripper
knew a vocabulary of tags. This block is markdown:

```text
# Context from my IDE setup:
## Open tabs:
- README.md: README.md
## My request for Codex:
Spawn a subagent to explore this repo.
```

The lesson the earlier three findings kept stating was "injected-block
vocabularies are per-adapter". The lesson they were actually teaching is
narrower and was missed three times: **the wrapper's *shape* is not stable
either, even within one vendor.** Only the position is — always prepended,
always with the real question as the tail.

The other three verified findings were all in code written this phase, and two
of them corrupt an archive rather than merely annoying a user:

- **A reply glued to the previous exchange.** A skipped Codex user record left
  the current-exchange pointer where it was, so the answer to the skipped
  question was appended to the row above it. `question ONE` ended up owning
  `answer TWO`, silently, uncounted. Phase 0 called this failure class out by
  name: "most traps found so far produce a plausible-looking archive that is
  wrong."
- **Every non-ASCII prompt mangled.** `line.push(b as char)` in the PTY tier
  treats each byte as a codepoint, so `concaténer` became `concatÃ©ner` — and
  the damage compounds, because the mangled prompt no longer matches the
  correctly-decoded terminal echo, so the echo is not stripped and the answer
  is filed under the wrong question.
- **`tmem run` recorded while paused**, after printing "capture is paused —
  running without recording". [cli.md](../cli.md) names this exact failure:
  "one who believes it's paused when it's recording gets a nasty surprise."
  Writing that warning was not the same as honouring it, and the fix is a
  genuinely separate no-record code path rather than a flag — "paused" has to
  mean nothing was written, and the way to be sure is for there to be no code
  that can write.

## Verdict

**The scope ships.** The interface generalizes and has two more implementations
to prove it; subagent transcripts are captured for the first time; Codex CLI
ingests with both of its silent traps under regression test; and the PTY tier
exists, refuses to be a terminal recorder, and reports its own losses.

The thing to take from this phase is narrower than "we added adapters". It is
that **the second implementation is where an interface's assumptions become
visible, and the assumptions that hurt are the ones nobody wrote down.** Dedup
keys and injected blocks were known to be vendor-specific because a survey went
looking. Discovery was not, because it never occurred to anyone that "where the
files are" was a design decision — it was just the shape of one tree. The cost
of finding out was one integration test; the cost of not finding out would have
been a permanent false alarm in the one warning that tells a user the format
moved.

## Carried forward

- **The injected-block stripper is still a list of known shapes.** Two
  vocabularies and two syntaxes now, found one at a time, each after it had
  already polluted an archive. Nothing detects an unrecognised wrapper; the
  only signal is a human reading prompts and noticing they are not prompts.
- **`archived_sessions/` is not ingested**, deliberately. If a user's Codex
  retention settings move sessions there, term-mem stops seeing them, and
  nothing says so.
- **The Codex append-only assumption is still inferred, not tested.**
  [codex-cli-format.md](codex-cli-format.md) carried this forward and it
  survives: positional dedup is only safe while Codex never rewrites a session
  file in place, and one compaction event is all the evidence there is.
- **Compaction is handled by ignoring it.** A `compacted` record's
  `replacement_history` is skipped so it cannot be re-ingested as new
  conversation, which is correct and is not the same as understanding what it
  means for the turns around it.
- **The PTY tier merges turns a REPL answers faster than the quiet window.**
  Reported rather than hidden; unfixed. The lever would be per-REPL knowledge of
  what a finished answer looks like, which is the prompt-pattern approach this
  phase deliberately avoided.
- **aider is not surveyed.** [plan.md](../plan.md) names it next and its
  markdown format has none of the structure both JSONL vendors provide.
- **The interactive picker** ([cli.md](../cli.md)'s third open question) was
  listed as "also here, if wanted". Not built, not wanted yet.
- **The two-week soak** from Phase 1, still outstanding — and now known to be
  outstanding because term-mem has never been installed on the author's machine:
  there is no `~/.local/share/term-mem/memory.db`. Six phases of features and no
  day of real use. That is the largest carried-forward item in this project and
  it has been invisible because no doc recorded it.
- **Phase 5 is still blocked** on the same absence, for the same reason. See
  [plan.md](../plan.md).
