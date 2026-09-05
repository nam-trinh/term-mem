# Phase 1 — Findings

Built 2026-09-05 against [plan.md](../plan.md)'s Phase 1 scope. The question the
phase exists to answer is not "does it parse" — Phase 0 settled that — but
whether the parser described in [phase-0.md](phase-0.md) survives being written
down as code and pointed at a real archive.

**Headline: it mostly does, and the two places it doesn't are both places where
the *right* rule loses data.** Phase 0's discriminator list is correct as a list
of things that are not prompts, but two of its entries turn out to sit in front
of real content: rejecting slash commands drops questions the user actually
asked, and rejecting them without also handling unattributable responses turns a
`/clear` echo into a container for 14,870 characters of someone else's answers.
Both fail silently, which is the same property that made Phase 0's findings
worth acting on.

## Sample, and what it can't tell us

```
4 session files, 970 records, 2.4 MB, 2026-09-02 .. 2026-09-05
19 exchanges ingested, 148 KB of database
```

Two more session files than Phase 0 had, from the same machine and the same
entrypoint. So the same caveat applies and is worth restating: **shape findings
below are existence proofs and trustworthy; frequency and volume findings are
not.** Nothing here re-opens the volume question, which still waits for a
code-heavy archive.

The Exit criterion — *two weeks of daily work without losing an exchange,
duplicating one, or noticing it running* — **has not been met, because two weeks
have not elapsed.** What has been done is to make each of its three clauses
mechanically checkable, and to check them:

| Clause | How it is checked | Result |
| --- | --- | --- |
| without losing an exchange | 20 genuine prompts counted independently in `jq`, reconciled against the database row for row | 19 archived, 1 interrupted turn still in flight |
| without duplicating one | re-ingest asserts the *ids*, not just the count; forced full reparse of the real archive | stable at 19 |
| without noticing it running | `tests/budget.rs`, release build, 60 samples | p95 **2.35 ms** against a 5 ms budget |

The two-week soak is the thing still owed, and no amount of unit testing
substitutes for it.

## 1. `promptSource` is optional even inside one entrypoint

[tech-stack.md](../tech-stack.md) marked this **blocking for Phase 1**: Phase 0
saw `promptSource: "sdk"` on every human record and could not tell whether that
was a property of prompts or of the VSCode entrypoint it happened to sample.

It is neither. This project's own archive contains a record with
`origin.kind: "human"`, no `toolUseResult`, no `isMeta` — and **no
`promptSource` field at all**:

```json
{"type":"user","uuid":"d5b08c0b-…","origin":{"kind":"human"},
 "message":{"role":"user","content":"<command-message>ship-phase</command-message>\n<command-name>/ship-phase</command-name>"}}
```

So the field is optional *within* the entrypoint Phase 0 sampled, and the
bare-terminal question never needed answering to settle the design: a gate on
`promptSource` would drop real records today. Phase 0's advice — treat it as a
positive signal, gate on the negative discriminators — is confirmed, and the
implementation goes one step further and does not read the field at all. A
positive signal that cannot gate anything is not worth the line of code.

**This closes the blocking open question.** The bare-terminal sample is still
worth having, but for a different reason: to check whether some *other* field
moves, not this one.

## 2. Slash commands are requests, not echoes

Phase 0 finding 2 counted `<command-name>` records with the editor telemetry, as
"slash-command echo (`/clear`, `/compact`)". Both examples in that sample were
conversation-control commands, and generalising from them is wrong.

`/ship-phase` — the command that produced this document — reaches the transcript
in exactly the same wrapper. Under Phase 0's rule, **the session that built
Phase 1 archives zero exchanges.** That is not a cosmetic loss: it is the
governing principle of the roadmap ("capture is irreversible") failing on the
first day.

The fix is to unwrap rather than reject, and to deny by *name* the commands that
genuinely aren't requests — `/clear`, `/compact`, `/resume`, `/exit`, `/quit`,
`/undo`. A denylist is unsatisfying, and the first implementation tried to avoid
it: `/clear` draws no response, so the rule that discards unanswered prompts
should discard it for free. **That was wrong, and finding 3 is why.**

## 3. A `/clear` echo will happily absorb someone else's conversation

The most surprising thing in the phase, and the reason finding 2 needs a
denylist after all.

A resumed transcript can contain assistant records whose **human prompt was
never written to that file**. In this archive, `0a36b825….jsonl` opens with a
`<local-command-caveat>` root, then a `/clear` echo, then nineteen assistant
records — 16,401 characters of real answers — whose prompts are simply absent.

Walking the conversation tree to attribute those records (which
[phase-0.md](phase-0.md) finding 8 requires, since children precede parents)
sends every one of them past the missing prompts and up to the nearest
prompt-shaped ancestor. That ancestor is the `/clear` echo. The result:

```
prompt:   /clear
response: I'll read the docs first. … (14,870 characters)
```

An archive row that looks entirely well-formed and is completely wrong. Both
available behaviours are lossy, and the choice between them is the phase's one
real design decision:

- **Reject `/clear`** → the records become orphans and are dropped *silently*.
- **Accept `/clear`** → the records are kept, presented as the answer to a
  question nobody asked.

Neither is acceptable as it stands, so the implementation does the third thing:
drops them **and counts them**, and `tmem capture` says so.

```
19 new, 0 updated, from 4 transcript(s)
  skipped: 0 unparsable, 0 unknown-type, 2 prompt(s) with no response, 2 API error(s)
  note: 19 assistant record(s) (16401 chars) had no prompt in the transcript
        and were dropped rather than misattributed
```

This is the "prefer loud errors over best-effort guesses" rule paying for
itself. It is also **not a resolution**: 16 KB of real answers are still not in
the archive, and finding out where those prompts went is carried forward.

## 4. A retried prompt is the API-error trap wearing a different hat

Phase 0 finding 6 says to skip `isApiErrorMessage: true`. Phase 0 did not notice
that the same records create a *duplicate-prompt* problem, which
`(session_id, uuid)` cannot catch:

```
18:02:35  user  0867d7c3  parent=null   "I want to create a terminal memory app…"  (389 chars)
18:03:15  user  531e6b7e  parent=220b…  "I want to create a terminal memory app…"  (389 chars)
```

Two records, identical text, different uuids, forty seconds apart. `220b6d33` in
between is the OAuth-failure record. The user's message was re-sent after the
error, and dedup on record identity sees two distinct records because they *are*
two distinct records.

The pleasing part: **no new rule is needed.** Skipping the API error leaves the
first prompt with zero assistant records, and the rule that refuses to write an
exchange with no response drops it. Two traps, one fix. It is worth writing down
because the obvious alternative — deduplicating on prompt text — would be wrong:
asking the same question twice in a month is legitimate, and an archive that
silently keeps only one of them is lying about the user's history.

## 5. The watermark cannot be a seek offset

[plan.md](../plan.md) asks for "a watermark per session, so ingest is
incremental rather than a full reparse", which reads like a byte offset to
resume from.

It cannot be. Phase 0 finding 8 (records precede their parents) and finding 9
(assembly is many-to-one) between them mean a correct exchange cannot be
assembled from a suffix of the file: the prompt an appended assistant record
belongs to is behind any offset you would resume from.

So the watermark degrades to a **change detector** — `(bytes, mtime)` per file,
which lets a sweep skip an untouched transcript for the price of one `stat`, and
forces a whole-file reparse of anything that moved. The idempotent upsert is
what makes that safe, and it is also what lets a mid-turn ingest *complete* the
row it already wrote rather than adding a second one.

The cost is real but small: reparsing an 8 MB transcript is work the hook never
does, because the hook only enqueues.

## 6. Two more record types, three months on

Phase 0 counted eleven top-level record types. This archive has thirteen —
`cost-state` and `bridge-session` are new. Nothing broke, because the parser
allowlists what it handles rather than matching on what it knows.

That is finding 1 of Phase 0 working exactly as intended, and it is the cheapest
insurance in the codebase. The report now also counts records of a type in
*neither* list, so a fourteenth type is visible rather than merely survivable.

## 7. Command extraction is capture-time, and cannot wait for Phase 2

[plan.md](../plan.md) lists "command extraction into its own table" under
Phase 2. That placement is not safe.

`tool_use` blocks are mined and discarded at capture — the raw block never
reaches the database, by design ([tech-stack.md](../tech-stack.md): a 3.2× size
win). If Phase 1 stores exchanges without extracting commands, the `Bash`
invocations in every exchange captured before Phase 2 ships are **gone**, and no
later phase can recover them.

So extraction lands here. What stays in Phase 2 is the part that is genuinely
re-derivable: the `exchanges_fts` column, the BM25 weighting, and the ranking.
The real archive yields 71 commands from 19 exchanges, so the table is not
hypothetical.

## 8. Budget: measured, and flat

```
Stop hook latency (60 samples, release build):
  12-record transcript     p50 2.03 ms   p95 2.35 ms   max 2.63 ms
  8 MB transcript          p50 1.94 ms   p95 2.14 ms   max 2.53 ms
```

Under the 5 ms budget with room, and — the property that actually matters —
**flat in the size of the transcript the hook points at.** Most of the 2 ms is
process start; the hook's own work is a `stat` of the pause file, a JSON parse
of a small payload, and one `write` plus `rename`.

The queue design is what buys this, and it is worth being explicit that the 5 ms
number would not survive parsing in the hook: a full reparse of the 8 MB file
takes far longer than the budget, which is precisely why finding 5's whole-file
reparse is affordable only off the turn boundary.

## Verdict

**Phase 1 ships, with the Exit criterion partly outstanding.** The three
mechanically checkable clauses hold; the two-week soak does not exist yet and
cannot be manufactured.

The scope is complete as written — schema under migrations, idempotent ingest,
watermarks, `init`/`status`/`doctor`/`recent`/`log`/`show`,
`pause`/`resume`/`ignore`/`TMEM=0`, `forget --last | <id>` — plus command
extraction pulled forward from Phase 2 for the reason in finding 7.

What was surprising, in order:

1. That the correct rule from Phase 0 (reject `<command-name>` records) would
   have archived nothing at all from the session that implemented it.
2. That a conversation-control echo can silently inherit a conversation.
3. That the API-error skip and the duplicate-prompt problem are the same trap,
   fixed by the same line.
4. That the watermark cannot be an offset — the plan's phrasing assumes a
   streaming parser the format does not permit.

## Carried forward

- **The two-week soak.** Nothing here substitutes for it, and it is the only
  part of the Exit criterion still open.
- **Where the 16 KB of orphaned responses came from.** They are reported, not
  recovered. If resumed sessions routinely omit their prompts, that is a hole in
  the capture premise and belongs in front of Phase 2, not behind it.
- **`isSidechain` is still unverified against a real session.** The fixture
  covers the rule (fold into the parent, never open an exchange); this archive
  contains zero sidechain records, exactly as Phase 0 found.
- **A bare-terminal sample** is still unsampled — no longer blocking, since
  finding 1 settled the discriminator without it, but the entrypoint remains
  untested.
- **The volume question** is untouched: 148 KB for four days of documentation
  work says nothing about a code-heavy archive, and Phase 3's redaction rules
  still need that scan.
