# term-mem — Scenarios

Status: scenarios 1 and 2 are Phase 2's acceptance tests and now run as
integration tests against the real binary (`tests/search.rs`); scenario 3's
delete-and-reuse path is Phase 3's, in `tests/redaction.rs`, and its *reuse*
half is Phase 4's, in `tests/reuse.rs`. **Scenario 1 runs verbatim, ranking
included. Scenario 2 runs verbatim except for one sentence, corrected in place
below.** Scenario 3 now runs end to end.

Two gaps surfaced from being executed rather than read: `--since january` needs
month-name parsing, which [cli.md](cli.md) never specified and which no
duration-suffix parser would have provided (Phase 1), and scenario 2's ranking
claim turns out to be an intuition nothing had checked (Phase 2). Scenarios
earning their keep as acceptance tests, as intended.

Three end-to-end walkthroughs — a conversation happens,
term-mem captures it, and weeks later the user gets it back. These exist to
pressure-test the design in [mission.md](mission.md) and
[cli.md](cli.md): if a scenario needs something the CLI doesn't have, that's a
finding.

Storage shapes shown below are illustrative. What matters is which fields the
retrieval path actually depends on.

All three scenarios say "the assistant" and mean Claude Code, because that is
what existed when they were written. As of Phase 6 the archive holds Codex CLI
and subagent transcripts too, and every line below runs the same — which is the
point of the adapter interface and is worth stating rather than assuming. The
one place a second vendor *changes* a scenario is scenario 2: Codex records
`repo` and `git_branch` in the transcript, so `--repo` still resolves after the
checkout Marcus was working in has been renamed. For a Claude Code row it does
not, because the repository is recovered by walking up from `cwd` for a `.git`
that may no longer be there.

---

## Scenario 1 — The half-remembered incantation

*The base case. Keyword recall of a command that worked once.*

### The conversation

March 3rd. Priya is stitching together screen recordings for a conference talk
and needs to concatenate four `.mp4` files without re-encoding. She asks her
assistant in `~/talks/pycon-2026`:

```
> I have 4 mp4 files I need to join into one. Same codec, same resolution.
> Don't want to re-encode, it takes forever and the quality drops.
```

The assistant explains the concat demuxer, has her write a `files.txt`, and
gives her:

```
ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4
```

A follow-up: `-safe 0` is needed because the paths are absolute. She runs it, it
works, she moves on. The pane is closed by the end of the week.

### What term-mem captures

Capture happens on exchange boundaries — a completed prompt/response pair — not
on session end, so an abandoned terminal doesn't lose the work. Two rows, both
tagged with the same `session_id`:

```json
{
  "id": "01HQ8F2K9",
  "session_id": "s_4b1e",
  "ts": "2026-03-03T14:22:07Z",
  "cwd": "/Users/priya/talks/pycon-2026",
  "repo": null,
  "assistant": "claude-code",
  "prompt": "I have 4 mp4 files I need to join into one. Same codec, same resolution. Don't want to re-encode...",
  "response": "…ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4…",
  "commands": ["ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4"]
}
```

Two things get derived at write time, because doing them at query time is too
slow:

- **A tokenized index over prompt and response.** Stemmed and stopworded, so
  `joining` and `join` collapse, and so does `concatenate`/`concat`.
- **Extracted command lines**, indexed separately and weighted higher. This is
  the highest-signal region of a response and the thing users are most often
  actually looking for.

### The retrieval

Late May. Different laptop directory, different project, no memory of when this
happened. She remembers two words:

```
$ tmem ffmpeg concat
```

Terms are stemmed and OR-ed, then ranked. The March exchange ranks first: both
terms hit, and one of them hits inside an extracted command line.

```
1.  01HQ8F2K9   2026-03-03   ~/talks/pycon-2026
    "I have 4 mp4 files I need to join into one…"
    → ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4

2.  01HR1M4P2   2026-01-12   ~/src/media-worker
    "why is ffmpeg dropping frames when I concat segments"
    → ffmpeg -f concat -safe 0 -i list.txt -vsync 2 out.mp4
```

Snippets, not transcripts — the second result's response was 200 lines. She
wants the reasoning behind `-safe 0`, so:

```
$ tmem show 01HQ8F2K9
```

Total elapsed: about four seconds, and she never left the terminal or opened a
browser. This is the 95% case, and it's deliberately the boring one.

**What this scenario tests:** that plain keyword search over a well-tokenized
index carries the common path, with no embeddings involved. If this case needs
semantic search, the index is wrong. *(It does not: this scenario runs verbatim
as of Phase 2, ranking included. Note that the ordering depends on the second
result's response genuinely being 200 lines — BM25 normalises by length, so the
detail this scenario mentions in passing is what puts the March exchange first.)*

---

## Scenario 2 — "Why did we decide that?"

*Recall of reasoning rather than a command, rescued by metadata filters.*

### The conversation

January. Marcus is midway through a Postgres migration on the `billing-api`
repo and talks through whether to backfill a new `tenant_id` column in one
transaction or in batches. The exchange runs long: table size, lock duration,
what happens to replicas, why the online-schema-change tooling isn't worth
introducing for one column. They land on batched backfill with a checkpoint
table.

No command is the artifact here. The decision is.

### What term-mem captures

Same row shape, but the fields that will matter are different:

```json
{
  "id": "01HN7Q0X4",
  "session_id": "s_9c2a",
  "ts": "2026-01-19T09:41:55Z",
  "cwd": "/Users/marcus/src/billing-api",
  "repo": "billing-api",
  "git_branch": "migrate-tenant-id",
  "assistant": "claude-code",
  "prompt": "should I backfill tenant_id in one transaction or batch it…",
  "response": "…(long)…"
}
```

`repo` and `git_branch` are resolved at capture time, not derived from `cwd`
later — the checkout may be gone or renamed by the time anyone searches.

### The retrieval

Six weeks on. A reviewer asks Marcus why the migration has a checkpoint table.
He remembers the conversation, but not a single distinctive word from it. A bare
`tmem backfill` returns eleven results across four repos — everything from a
Redis warm-up script to an unrelated analytics job.

This is the failure mode the mission predicts: recall by content fails when you
can't reconstruct the content. So he recalls by *where and when* he was instead:

```
$ tmem backfill --repo --since january
```

`--repo` resolves from the current checkout and collapses the space to
`billing-api`; `--since` cuts it further. Two results.

~~The first is the one.~~ **Corrected in Phase 2: it is the second.** Both
results contain `backfill` exactly once, in the prompt, so nothing separates
them but BM25's length normalisation — which prefers the short follow-up ("how
long will the backfill take on staging") over the long reasoning this scenario
exists to recall. No column weighting reaches it, because the match is in the
same column for both. See [phases/phase-2.md](phases/phase-2.md) finding 2.

That the sentence was wrong matters less than what it was doing there: it
described what a search engine *feels* like rather than anything that had been
measured, in a document used as an acceptance test. The rest of the scenario is
unaffected, and is the part it says it is testing — eleven results became two,
and two is a set Marcus reads in full.

If even that had failed, the browse path is the backstop:

```
$ tmem log --in ~/src/billing-api --since january
```

Which is an ordinary ordered scan, not a search problem — and it puts every
January exchange from that repo in front of him to skim. *(This line runs as of
Phase 1.)*

He then wants the whole thread, because the decision was arrived at across
several turns:

```
$ tmem show 01HN7Q0X4 --session
```

He pastes the two relevant paragraphs into the PR description.

**What this scenario tests:** that metadata filters do the heavy lifting the CLI
doc claims. It also makes the case for `--in` defaulting to global rather than
the current directory — Marcus was in a worktree elsewhere when he first tried,
and a silent implicit `--in .` would have returned nothing and looked like the
archive had lost the exchange.

---

## Scenario 3 — The paste you regret, and the reuse that pays off

*Deletion as a first-class path, then recall-and-reuse.*

### The conversation

A Thursday afternoon. Dana is debugging a failing webhook handler and, to get a
useful answer fast, pastes a full request log into the assistant — headers
included. One of those headers is a live `Authorization: Bearer` token for a
staging service.

The assistant diagnoses the problem (a clock-skew tolerance on signature
validation set to zero) and suggests a fix.

Ten minutes later Dana realizes what's in the scrollback — and, now, in the
archive.

### The delete path

```
$ tmem forget --last
```

The point of the CLI doc's promise is that this is a genuine delete: the row is
removed, the index entries for it are removed, and any derived artifact
(extracted commands, embeddings, snippet cache) goes with it. Not a `deleted=1`
flag on a row that stays greppable on disk. A tool that keeps a "deleted" secret
around is worse than one that never stored it, because the user now believes
they're safe.

If she'd only noticed the next morning:

```
$ tmem forget --since '18 hours ago'    # blunt, and correct
$ tmem forget --in ~/src/webhooks       # everything from that tree
```

Blunt is the right default for a safety valve. Precision is a nice-to-have;
certainty is the requirement.

Redaction-on-capture would have prevented this — a capture-time filter for
things shaped like bearer tokens and API keys. The mission leaves that open, and
this scenario is the argument for closing it: `forget` is a valve for what
redaction misses, not a substitute for having it.

**As of Phase 3 it does prevent it**, and the sharper version of the lesson is
worth keeping. Dana's paste is an `Authorization: Bearer` header — a shape a
rule knows — so it never reaches the disk and the ten minutes of dread do not
happen. But the scenario's framing ("a capture-time filter") quietly assumes the
filter is free. It is not: a false positive replaces a mined command line, which
is the only copy that will ever exist. That is why the aggressive half of the
filter ships disabled. See [phases/phase-3.md](phases/phase-3.md) finding 2.

The `forget --last` path below is therefore the *fallback*, and it is still the
one tested adversarially — by grepping every byte term-mem wrote.

### The reuse

The diagnosis itself was good and Dana wants to keep it, so she re-asks with the
log omitted, and that clean exchange is what stays in the archive.

Three months later, a different service, the same class of bug. She half
remembers having seen it:

```
$ tmem signature validation clock skew
```

One strong hit. Rather than reading it and retyping the conclusion, she feeds it
back:

```
$ tmem show 01HS3D8N1 --json | <assistant>
```

The new session starts with the established diagnosis in context instead of
re-deriving it from scratch. This is the mission's third pillar, and note that
in this form it needs nothing from `tmem` beyond `--json` and a pipe — which is
one answer to the open question in `cli.md` about whether reuse is a subcommand.
Terminal-native composition may already cover it.

**As of Phase 4 it does, and three other routes do too.** The pipe still works
as printed. Beside it: an agent with the MCP server registered finds the
exchange itself, without Dana having to remember she had the conversation; a
local model behind Ollama reaches the same three tools through `tmem tools
--schema openai` and `tmem call`; and `tmem <query> --json | tmem render
--prompt-block` turns the same JSON into a block to prepend for a model that
cannot call tools at all.

What the scenario's framing quietly assumes is that Dana pipes *raw JSON* into
an assistant and it does something sensible with it. That is the part `render`
exists for, and building it surfaced something this scenario does not mention:
the block has to say that it is reference material rather than instructions. The
archive is Dana's own, but an exchange from March that happens to contain
"ignore all previous instructions" is still a prompt injection with a six-month
fuse. See [phases/phase-4.md](phases/phase-4.md) finding 6.

**What this scenario tests:** that delete is complete and cheap to invoke under
stress, and that reuse can be a pipe rather than a feature. *(Both hold as of
Phase 4. The pipe is the minimum, not the ceiling — but nothing that was added
on top of it made the pipe unnecessary.)*

---

## What these scenarios surface

Read together, three things the current design should decide:

1. **Extracted commands deserve their own index and their own weight.** All
   three scenarios lean on it, and it's the difference between ranking the right
   exchange first and ranking it seventh. *(Shipped in Phase 2 at `8 / 2 / 1`
   against prompt and response. It is a column weight, so it separates a
   command match from a prose match and nothing else — scenario 2's two results
   both match in `prompt`, and the weighting has no opinion about them.)*
2. **`--in` should default to global.** Scenario 2 breaks under an implicit
   `--in .`, and it breaks silently, which is the worst way to break.
3. **Redaction-on-capture is load-bearing, not a refinement.** Scenario 3's
   valve works, but it depends on the user noticing. Most won't. *(Shipped in
   Phase 3 for shapes a rule recognises. What the scenario did not anticipate is
   that the filter has a cost in the other direction — see the note above.)*

A fourth, which only appeared once reuse was built: **an exchange fed back into
a session is text a model reads as part of its prompt, and the archive is not a
trusted input just because it is the user's own.** None of these three scenarios
says so, and all three end in something being pasted or piped into an
assistant.
