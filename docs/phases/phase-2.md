# Phase 2 — Findings

Built 2026-09-06 against [plan.md](../plan.md)'s Phase 2 scope: FTS5, BM25, the
weighted command index, `tmem <query>` as the default verb, and the metadata
filters. The phase exists to make the archive answerable.

**Headline: the ranking was the easy half.** The hard half was that the archive
being ranked was missing a fifth of itself, for a reason Phase 1 had already
seen and mis-diagnosed. Phase 1 carried forward a suspicion that resumed
sessions lose their prompts and called it "a hole in the capture premise". It is
not. The prompts were always in the transcript; the parser was throwing them
away, and the 19 KB of unattributable responses it dutifully reported were the
downstream symptom of its own rejection rule.

## Sample

```
5 transcripts (+1 subagent transcript, see finding 3), 1,591 records, 2026-09-02 .. 2026-09-06
before this phase:  24 exchanges, 21 orphaned assistant records (19,192 chars)
after  this phase:  34 exchanges,  0 orphaned assistant records
```

Same machine and same entrypoint as Phases 0 and 1, so the standing caveat
stands: **shape findings are existence proofs and trustworthy; frequency and
volume findings are not.** The volume question is still untouched and still
waiting on a code-heavy archive before Phase 3's redaction rules are designed
against it.

The Exit criterion has two clauses and they came out differently:

| Clause | Result |
| --- | --- |
| scenario 1 runs verbatim | **yes** — every command, and the documented ranking |
| scenario 2 runs verbatim | **partly** — the commands and the result counts hold; "the first is the one" does not. Finding 2 |
| p95 under 100 ms at 100k exchanges | **yes** — worst case 17.87 ms, finding 5 |

## 1. The prompts of a "resumed session" were never missing

[phase-1.md](phase-1.md) finding 3 found a transcript that opened with a
`/clear` echo followed by nineteen assistant records "whose prompts are simply
absent", concluded that a resumed transcript can omit them, and carried the
question forward as possibly "a hole in the tier-1 premise".

The prompts are in the file. Here is the head of that same transcript, which
Phase 1 read as evidence of loss:

```
[1] user   isMeta=true   <local-command-caveat>Caveat: The messages below…
[2] user                 <command-name>/clear</command-name>…
[3] system
[6] user                 <ide_opened_file>The user opened the file …/cli.md…</ide_opened_file>
                         + "read the docs and tell me what phase 1 should…"   ← the prompt
```

Record 6 is *two text blocks*: the editor's telemetry, then the question. Phase
1's discriminator asked whether a record **starts with** an injected tag and
rejected the whole record if so — correct for a record that is nothing but
telemetry, and wrong for the far more common one that is telemetry followed by a
question. The question was dropped; its answers then had no prompt up their
parent chain; the orphan counter, working exactly as designed, reported them.

**Every one of the 21 orphans in this archive was this.** Fixing the
discriminator to *strip* injected blocks rather than reject the record takes the
orphan count to zero and the archive from 24 exchanges to 34 — a 42% increase in
a four-day sample, on the day search was supposed to start ranking it.

Three things worth carrying:

- **This is the same bug as Phase 1 finding 2, one tag along.** That finding
  fixed `<command-name>` by unwrapping rather than rejecting, and left the other
  nine tags in `INJECTED_TAGS` rejecting whole records. A fix applied to the
  instance rather than the class.
- **The loud counter is what made it findable, and it still pointed the wrong
  way.** "N records had no prompt in the transcript" is a true statement about
  the file that invites a false conclusion about the format. A report that named
  what it *rejected* alongside what it could not attribute would have closed
  this in Phase 1.
- **An unclosed injected tag is now left alone rather than swallowing the rest
  of the record.** "why does `<ide_opened_file>` appear here?" is a question
  someone asked. A stray tag in a stored prompt is cosmetic; a truncated prompt
  is not, and capture is the irreversible half.

## 2. Scenario 2's ranking claim does not survive BM25

[scenarios.md](../scenarios.md) ends scenario 2 with "Two results. The first is
the one." Built as an archive — eleven exchanges matching `backfill` across four
repos, two of them in `billing-api` in January — the first half holds exactly
and the second does not:

```
$ tmem backfill --repo --since january
1.  how long will the backfill take on staging       ← short follow-up
2.  should I backfill tenant_id in one transaction   ← the decision
```

Both candidates contain `backfill` exactly once, in the prompt. Nothing else
distinguishes them *except* length, and BM25 normalises by document length, so
the two-line follow-up outranks the long reasoning. Scenario 2 is specifically
about recalling long reasoning, which makes this the least convenient possible
place for the effect to show up.

No weighting fixes it. `commands ≫ prompt > response` is a *column* weighting,
and here the match is in the same column for both rows; FTS5's `bm25()` exposes
no `b` parameter to soften the normalisation. A ranking function that preferred
longer documents would be worse on scenario 1, where the 200-line response is
the one that must rank second.

**What the scenario actually tests still passes.** Its own stated purpose is
"that metadata filters do the heavy lifting the CLI doc claims", and they do:
eleven results become two, which is a set a person reads in full. The correction
belongs in the scenario, and [scenarios.md](../scenarios.md) now carries it —
the ordering was written as an intuition about what a search engine feels like,
not as a claim anything had verified.

## 3. Subagent transcripts are separate files, and they are not ingested

Both earlier phases carried "`isSidechain` is unverified — this archive contains
zero sidechain records". That is true of the files the parser walks, and
misleading. Sidechain records exist; they live somewhere else:

```
~/.claude/projects/<project>/<session-uuid>/subagents/agent-<id>.jsonl
    52 records, isSidechain: true on every one
    18 user, 31 assistant, 3 attachment
```

`claude_transcripts()` reads `<project>/*.jsonl` and does not descend, so these
files are skipped — silently, and by accident rather than by decision. The
outcome is *approximately* what [tech-stack.md](../tech-stack.md) wants
("captured, but attributed to the parent exchange, so a search doesn't return
five near-identical rows for one question"), because a search does not return
them. But nothing is attributed to the parent either: the subagent's work is
absent, not folded in.

The shape of the file also breaks the assumption underneath the design. Its root
record is a *`user` record with `isMeta: true`* carrying the agent's
instructions — a prompt that no human typed. `isSidechain` on inline records is
the case the parser was built for, and it may no longer be the case that occurs.

Left as-is for Phase 6, which owns the adapter interface, but recorded here
because "we verified there are no sidechains" was never true and both prior
phases said it.

## 4. FTS5's external content needs a column the schema does not have

[tech-stack.md](../tech-stack.md) specifies:

```sql
CREATE VIRTUAL TABLE exchanges_fts USING fts5(
  prompt, response, commands, content='exchanges', tokenize='porter unicode61');
```

This cannot be created. `content='exchanges'` makes every indexed column a
lookup into `exchanges`, and `commands` is a *table*, not a column of that row.
The three ways out are unequal:

- A standalone (non-external-content) FTS5 table keeps its own copy of every
  prompt and response — a second copy of the largest thing in the file.
- A contentless table (`content=''`) stores no text and therefore cannot produce
  `snippet()`, which is the feature the result list is made of.
- Denormalise the mined command lines onto the row as `exchanges.commands_text`
  and index that. Small (commands are short and already derived at capture),
  keeps the index external-content, keeps snippets.

The third shipped. The `commands` table stays authoritative and the column is a
projection of it written in the same transaction.

Related, and worth more than it costs: **the index is maintained by triggers,
not by application code.** [plan.md](../plan.md) asks for it to be "maintained
transactionally with `exchanges`", and a trigger is the only version of that
which a future write path cannot forget. It also made `forget` correct for free
— deleting the row deletes the index entry, so the Phase 3 obligation to grep
the file after a delete already passes for the bulk selectors added here.

## 5. Budget: measured

```
100,000 exchanges, 139.6 MB, built by ingesting 200 generated transcripts (12 s)

  common two-term  p50 17.09 ms   p95 17.87 ms   max 28.36 ms
  rare term        p50  2.96 ms   p95  3.07 ms   max  3.07 ms
  filtered         p50 12.38 ms   p95 15.41 ms   max 21.38 ms
```

Under the 100 ms budget by 5×, measured against the code as shipped — an
earlier run of the same test on the pre-review code gave a worst p95 of 28.12 ms,
so run-to-run variance on a laptop is a larger effect here than anything in the
diff, and neither number is near the budget. Cold-process measurement, which is what a user
pays: fork, open the database, run the migration check, plan, match, rank,
snippet, print. The archive is built by ingesting generated transcripts through
the real parser rather than by inserting rows behind it, because the index is
maintained on the write path and a fixture that bypassed it would be measuring a
different program.

The hook budget is unaffected and still holds: p95 **2.92 ms** on a small
transcript and **2.14 ms** on an 8 MB one, against 5 ms — still flat in the size
of the transcript the hook points at, which is the property that matters. One
earlier run showed a 32 ms outlier in `max` on the 8 MB case that the p95 does
not see; it did not reproduce, and it is recorded here rather than dropped.

**A footnote that was nearly a wasted afternoon.** The first version of this
measurement took an estimated fifty minutes to build its archive, at 0% CPU.
`resolve_repo` walks from `cwd` to the filesystem root looking for `.git`, once
per exchange — and the generator used a plausible-looking `/home/dev/src/...`,
which on macOS routes every one of those stats through the automounter. Two
things came out of it: the fixture now uses paths under the temp home, and
ingest **caches the repo lookup per `cwd`**, which is one resolution per
transcript instead of five hundred. Build time went from ~50 minutes to 14
seconds. The cache is a real improvement — `init --backfill` over months of
history pays that cost for every exchange — and it was found by a budget test
measuring the wrong thing, which is an argument for having one.

## 6. `--in` matched nothing when the path had two spellings

Found by a test, not by a user, which is the only reason it is a footnote.
`--in` canonicalised the path it was given; capture stores `cwd` exactly as the
transcript spells it. On macOS `/var/…` and `/private/var/…` name the same
directory, and the two never compared equal — so `--in` returned nothing and
looked precisely like an archive that had lost the exchange.

This is the second time this flag has failed in this exact way: Phase 1's review
found `--in` silently matching nothing for any path containing `*`, `?` or `[`.
Both bugs are the same shape — a path transformed on one side of a comparison
and not the other — and the fix is now to match *any* spelling of the tree
rather than to pick the right one.

## 7. A review found four more, and one of them was the phase's own headline

Phase 1 ended with the observation that "the tests that mattered were the ones
written from the promise, not from the code". Phase 2 shipped 91 passing tests,
clean lints and a met budget, and a review of the PR found four defects. The
list is shorter than Phase 1's eleven, and one entry is worse than any of them.

1. **The injected-block fix did not apply to any archive that already existed.**
   Ingest's "unchanged" fast path compared the response text and the *counts* of
   derived rows — never the prompt. Finding 1 is a change to how prompts are
   extracted, so every row written by a Phase 1 binary would have kept its
   unstripped prompt through every subsequent re-ingest, forever. The headline
   fix of the phase reached only exchanges captured after it, and the phase's
   own test suite re-ingested nothing that predated the change.

   This is [phase-1.md](phase-1.md) finding 9's third defect exactly — "a
   cautious optimisation defeating the phase's governing principle" — one field
   over, in a comparison whose comment explains why the *previous* version of
   the same mistake was wrong. The fix compares everything the row stores. The
   parse has already happened and the row is already in hand; comparing all of
   it costs nothing worth having.

2. **`highlight()` aborted the process on non-ASCII.** It searched a lowercased
   copy of the line using byte offsets from the original, which desynchronise
   the moment a character's lowercase form differs in byte length — `İ`
   (U+0130) lowercases to two chars. A captured command containing one made
   `tmem <query>` panic with exit **101**, which is not one of the three exit
   codes [cli.md](../cli.md) promises for `tmem <query> || …`. Terminal path
   only, which is why nothing in a suite that captures stdout saw it.

3. **Truncation cut between an opening highlight marker and its close**, leaving
   `\x1b[1;33m` unmatched and the user's terminal bold yellow after the process
   exited. Rendering now counts visible characters and closes what it opened in
   the same pass.

4. **`--json` leaked the raw FTS5 sentinels** (`\u0001`, `\u0002`) into the
   snippet — into the pipe scenario 3 feeds to another assistant. The test
   written to prevent exactly this asserted `!stdout.contains('\u{1}')` on the
   raw output, and `serde_json` had already escaped the control character to six
   printable ones, so the assertion sailed over the bug it existed to catch.

The pattern worth carrying forward is narrower than Phase 1's and sharper: **two
of these four are tests that ran the right scenario through the wrong surface.**
The JSON test checked the bytes before decoding; the whole suite drives the
binary through a pipe, so no test has ever executed the branch that formats for
a terminal. A test that cannot reach the code it is named after is worse than no
test, because it is counted.

Each fix now has a regression test named after the failure, and each was
confirmed to fail against the unfixed code before being kept — including the
truncation sweep, whose first version padded past the cut and passed against the
bug.

## Verdict

**Phase 2 ships, after a review pass that found four more defects** — one of
which meant the capture fix this phase is named for applied to no archive that
already existed (finding 7). Scope is complete as written: `exchanges_fts` maintained
transactionally, the weighted `commands` column, `tmem <query>` as the default
verb with the `PATH` collision check already in `init`, `--in`/`--since`/
`--repo`/`--json`/`--limit`, highlighted snippets, pipe detection, exit codes,
and `forget` extended to `--since` and `--in`.

Scenario 1 runs verbatim. Scenario 2 runs verbatim except for one ordering
sentence, which finding 2 argues is a defect in the sentence.

What was surprising, in order:

1. That Phase 1's carried-forward "hole in the capture premise" was Phase 1's
   own rejection rule, and that its loud, honest counter pointed at the format
   instead of at the parser.
2. That the fix for Phase 1 finding 2 was applied to one tag out of ten, and
   that nothing in a green test suite noticed the other nine.
3. That BM25's length normalisation quietly contradicts a scenario the docs use
   as an acceptance test — and helps the other one.
4. That the schema in `tech-stack.md` could not be executed as written, having
   been read many times by then.
5. That the sidechain records both earlier phases recorded as absent were in a
   directory nothing looks in.
6. That the phase's headline fix reached no existing archive, guarded by a
   comment explaining the previous version of the same mistake (finding 7).

## Carried forward

- **The two-week soak**, still the outstanding clause of Phase 1's Exit
  criterion, and now with a materially different parser underneath it. The
  capture fix in finding 1 is the kind of change the soak exists to catch.
- **Subagent transcripts** (finding 3): not ingested, and the `isSidechain`
  design was written for an inline shape that may not occur. Phase 6 owns it.
- **A `pr-link` record type**, new since Phase 1 and the only unrecognised type
  in the archive. Fourteen top-level types now (`assistant`, `user`,
  `attachment`, `ai-title`, `atis-latch`, `last-prompt`, `queue-operation`,
  `bridge-session`, `mode`, `file-history-snapshot`, `system`,
  `file-history-delta`, `pr-link`, `cost-state`) against Phase 0's eleven. The
  allowlist keeps absorbing them, which is Phase 0 finding 1 continuing to pay.
- **The volume question**, untouched for a third phase. 1.3 MB for five days of
  documentation work still says nothing about a code-heavy archive, and Phase 3
  needs that scan.
- **No test has ever run the terminal formatting path.** Every integration test
  drives the binary through a pipe, so `print_hits`, `highlight` and the escape
  handling are covered only by unit tests calling them with `tty: true`. Two of
  the four review findings lived there. A pty harness is the obvious answer and
  is not written.
- **Ranking quality has no measurement.** The weights (`8 / 2 / 1`) are a
  judgement. Phase 5's premise is that months of real queries will say whether
  keyword search misses things; nothing currently records a query, and
  deliberately so — but that means the Phase 5 decision will be made on memory
  rather than data unless something changes.
