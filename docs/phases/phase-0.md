# Phase 0 — Findings

Run on 2026-09-05 against the local Claude Code archive, per
[plan.md](../plan.md). The question Phase 0 exists to answer: *does the transcript
contain what we need, and what does a real archive weigh?*

**Headline: the premise holds — responses are on disk in structured form — but
the parser sketched in [tech-stack.md](../tech-stack.md) would have silently
produced a badly wrong archive.** Six defects found, five of them silent.

## Sample, and what it can't tell us

```
2 project directories, 2 session files, 214 records, 456 KB, ~2 days
```

This is small, single-machine, and single-entrypoint — every session came from
the VSCode extension, none from a bare terminal. So:

- **Shape findings below are trustworthy** (they're existence proofs: a record
  shape that appears once will appear again).
- **Frequency and volume findings are not.** The extrapolation at the bottom is
  labelled as the guess it is.
- **One finding is explicitly at risk from the sampling** — the human-prompt
  discriminator, flagged below.

`isSidechain: true` appears **zero** times, so the subagent-attribution rule in
`tech-stack.md` is unverified rather than confirmed.

## 1. There are eleven record types, not three

```
62 assistant   41 user      32 queue-operation   16 attachment
14 file-history-snapshot    12 atis-latch        12 ai-title
10 last-prompt              8 mode                5 file-history-delta
 2 system
```

Most carry no `uuid` and no `message`. A parser that switches on `.type` with a
`user`/`assistant`/`_` match, or that assumes `.message` exists, breaks on 40%
of lines. **Filter with an allowlist and skip the rest**, so an unknown twelfth
type added next release is ignored rather than fatal.

## 2. Most `user` records are not user prompts

This is the serious one. Of 41 `user` records:

| What it actually is | Count | Discriminator |
| --- | --- | --- |
| Tool results fed back to the model | 24 | `toolUseResult != null` |
| Genuine typed prompts | ~11 | see below |
| IDE noise (`<ide_opened_file>`) | 3 | content wrapper tag only |
| Slash-command echo (`/clear`, `/compact`) | 3 | content wrapper tag only |
| Local-command caveat | 2 | `isMeta: true` |
| Injected compaction summary | 1 | `isCompactSummary: true` |

A naive `type == "user" → prompt` rule ingests **roughly three times more rows
than there are prompts**, and the extra rows are the worst possible content:
tool output, editor telemetry, and a 14,872-character conversation summary
stored as if the user had typed it.

**The discriminator that works on this sample:**

```
type == "user"
  && toolUseResult == null
  && isMeta != true
  && isCompactSummary != true
  && promptSource == "sdk" && origin.kind == "human"
  && content does not match ^<(ide_opened_file|command-name|command-message
                              |local-command-(caveat|stdout))>
```

The last clause is not optional. Three `<ide_opened_file>` records carry
`promptSource: "sdk"` and `origin.kind: "human"` — the metadata says human, the
content is editor telemetry. **Metadata alone is insufficient; a content-shape
filter is required.**

**At risk from the sample:** `promptSource: "sdk"` may be an artifact of the
VSCode entrypoint. A bare-terminal session may use a different value or omit the
field, in which case this filter captures nothing. Treat `promptSource` as a
*positive* signal, not a required one, and gate on the negative discriminators —
which are load-bearing and entrypoint-independent.

## 3. `message.content` is polymorphic

Sometimes a string, sometimes an array of typed blocks. Both occur on `user`
records in this sample. Anything that indexes `.message.content[0].text` works
on most records and panics on the rest.

## 4. The parent chain breaks at compaction

The `compact_boundary` record:

```json
{ "type": "system", "subtype": "compact_boundary",
  "parentUuid": null,
  "logicalParentUuid": "c8cd04b2-…",
  "compactMetadata": { "trigger": "manual", "preTokens": …, "postTokens": … } }
```

`parentUuid` is **null** — the thread is severed — and the real link moves to
`logicalParentUuid`. `tmem show --session`, specified in [cli.md](../cli.md) as a
`parentUuid` walk, would stop dead at the compaction boundary and show the user
the tail of a conversation while reporting it as the whole thing. Silently.

**Fix: walk `parentUuid ?? logicalParentUuid`.** Scenario 2 depends on this —
Marcus's multi-turn decision thread is exactly the kind that outlives a compact.

## 5. One session file can hold several conversations

Root count per file (records with `parentUuid == null`):

```
6b0aff94….jsonl  roots=1
0a36b825….jsonl  roots=2
```

`/clear` starts a fresh tree **in the same file, under the same `sessionId`**.
So `session_id` is a *file* identifier, not a conversation identifier. Grouping
`--session` by `session_id` merges unrelated conversations.

**Fix: derive a `thread_id` at ingest** by walking to the tree root, and group
on that. `session_id` stays as metadata.

## 6. Failed turns are stored as assistant messages

```json
{ "type": "assistant", "isApiErrorMessage": true,
  "message": {"content": [{"type":"text",
    "text":"Failed to authenticate: OAuth session expired…"}]} }
```

Indistinguishable from a real response without checking the flag. Skip on
`isApiErrorMessage == true`.

## 7. Two design decisions get evidence

**`thinking` blocks are already empty on disk.** All 23 have
`{"signature": "…", "thinking": ""}` — zero characters of text. The
`tech-stack.md` rule ("thinking blocks are not stored") turns out to be right
for a different reason than the one given: there's nothing there to store. Keep
the rule as defense-in-depth, since this is version-dependent behavior, but the
sensitive-surface argument for it doesn't currently apply.

**Mining `tool_use` instead of storing it is a 3.2× size win.** Assistant
content by block type:

```
tool_use  62,799 chars   (69%)
text      27,935 chars   (31%)
thinking       0 chars
```

Tool calls are more than twice the response prose. Storing them whole would
make the archive mostly `Edit` payloads and `Bash` invocations of `jq`. Tool
names in sample: `Bash` 20, `Write` 5, `Edit` 3, `Read` 2 — so the `commands`
table gets real material from `Bash`, which is the highest-signal source the
scenarios lean on.

## 8. Records are not written in parent order

Children appear before their parents in both files, though **no dangling
references survive to end-of-file** (every `parentUuid` resolves). A streaming
parser must therefore buffer unresolved references rather than treating a
forward reference as corruption — and thread assembly is a second pass, not
something done inline.

## 9. Exchange assembly is many-to-one

~11 human prompts produced 62 assistant records — roughly **6 assistant records
per prompt**, because each tool-use round trip emits another record. The
`exchanges` schema has one `response` column, so ingest must fold every
assistant record between two human prompts into a single row: concatenate the
`text` blocks, route `tool_use` to `commands`, drop the rest.

## 10. Volume, held loosely

456 KB for ~2 days of doc-writing work. Naively that's ~7 MB/month, ~85 MB/year
— comfortable for SQLite, and comfortable for a full-table BM25 scan. But the
sample is two days of one workload, and heavy code sessions with large `Read`
and `Edit` payloads will skew far higher. **The 100k-exchange latency target in
`tech-stack.md` stands as the thing to design against**; this number is too
weak to move it.

Since we mine rather than store `tool_use` (finding 7), stored bytes should land
well under the raw transcript size.

## 11. Credential scan: clean, and unrepresentative

Zero matches for `sk-…`, `ghp_…`, `AKIA…`, JWTs, `Bearer` tokens, PEM blocks, or
assignment-shaped secrets across the whole archive.

**This is not evidence that redaction is unnecessary.** It is two days of
writing markdown; no credential was in scope to leak. What the scan *did* surface
is that PII arrives without anyone pasting it — an email address appears in the
archive purely because it was in the session's ambient context. Phase 3 keeps
its scope, and the entropy fallback stays, because the pattern rules had nothing
to prove here.

## Verdict

Phase 0 passes: **the transcript is a sufficient source and the tier ordering in
`tech-stack.md` is correct.** No architectural change.

The correction is to the parser's difficulty. It was described as reading
`type: "user"` and `type: "assistant"` records; it is actually a filtered,
two-pass, many-to-one assembly with six documented traps, five of which fail
silently. That is a Phase 1 scope increase, not a redesign — and it is exactly
the kind of thing Phase 0 was meant to find before it became a year of quietly
corrupt archive.

**Carried forward:**

- Verify the human-prompt discriminator against a bare-terminal session
  (blocking for Phase 1).
- `isSidechain` handling remains unverified — needs a session that spawns
  subagents.
- Re-run this scan against a real code-heavy archive before Phase 3 sets its
  redaction rules.
