# Codex CLI — transcript format survey

Status: findings, 2026-09-05. A pre-Phase-6 probe, run against a real local
archive to answer the open question in [tech-stack.md](../tech-stack.md): *do
other terminal assistants persist transcripts in a form as complete as Claude
Code's?*

**Headline: yes for content, no for identity.** Codex CLI persists complete
assistant responses in structured JSONL, so the tier-1 premise holds for a
second vendor. But its records carry **no per-record identifier and no parent
pointers at all**, which breaks the `(session_id, uuid)` idempotency key the
whole ingest design rests on. That key is a Claude Code detail, not a universal
one, and [plan.md](../plan.md) Phase 1 currently writes it into the schema as if
it were universal.

## Sample

```
~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl
7 session files + 1 archived, 4.0 MB, 2,665 records, 2026-03-01 .. 2026-04-10
```

One entrypoint, one user, seven sessions. **Shape findings below are
trustworthy** — they repeat across every file. **Frequency and volume findings
are not**; the sample is small and one file holds 73 of the 100 user messages.
Same caveat as [phase-0.md](phase-0.md), for the same reason.

## The format

Every record is exactly three keys — `{payload, timestamp, type}` — with a
two-level discriminator: `type` at the top, `payload.type` beneath it. Claude
Code's is flat.

| `type` | count | |
|---|---|---|
| `response_item` | 1508 | the conversation proper |
| `event_msg` | 1031 | UI event stream |
| `turn_context` | 116 | per-turn settings snapshot |
| `session_meta` | 9 | first record of each file |
| `compacted` | 1 | history rewrite |

`payload.type` under those: `token_count` 469, `message` 400,
`function_call_output` 384, `function_call` 384, `agent_message` 274,
`reasoning` 237, `user_message` 86, `task_started` 86, `task_complete` 83,
`custom_tool_call_output` 50, `custom_tool_call` 50, `agent_reasoning` 24,
`item_completed` 6, `web_search_call` 3, `turn_aborted` 2, `context_compacted` 1.

## Findings

### 1. No record identity, and timestamps don't substitute

Every record has the same three top-level keys. There is no `uuid`, no
`parentUuid`, no `id`. `turn_context` carries a `turn_id`, but **no
`response_item` carries one** (checked: 1508 of 1508 lack the field), so turns
cannot be joined to their contents by key — only by position in the file.

Timestamps don't rescue it: **2,186 unique timestamps across 2,665 records.**
Roughly one record in five shares its timestamp with another.

So the Codex adapter's idempotency key must be **positional** —
`(file_path, line_number)`, or a content hash — not `(session_id, uuid)`. That
is a genuinely weaker guarantee: it holds only because the file is append-only.

**This is the finding that changes the design.** The parser interface needs to
let each adapter *declare its own dedup key*, rather than the schema assuming
one shape. Better to learn this from the second adapter than the fifth.

### 2. `event_msg` duplicates `response_item` verbatim

`agent_message` (an `event_msg`) and `message`/`assistant` (a `response_item`)
carry the same text. Compared across a full session, every `agent_message`
matched an assistant message character for character. Counts are close but not
equal — 280 assistant messages against 274 `agent_message` events — so the extra
few are turns whose event never fired (aborted turns, most likely).

**Ingest `response_item` only; ignore `event_msg` entirely.** It is the UI
stream, not the record. A parser that walks records by `payload.type` without
first filtering on the *top-level* `type` double-counts every response — and
because both copies are real text, nothing looks wrong.

Claude Code has no equivalent trap. This one is Codex-specific and silent.

### 3. `message`/`user` is not the same thing as a human prompt

Of 100 user-role messages, 8 are injected: 5 `<environment_context>` blocks
(cwd, shell, date, timezone), 2 `<turn_aborted>`, 1 `<subagent_notification>`.
The `<environment_context>` block is *prepended to the same string* as the real
prompt rather than being a separate record, so the filter here is a **strip**,
not a reject — the opposite of the Claude Code case.

Separately, `role: "developer"` (20 records) holds the permissions/sandbox
system prompt. Excluded.

This generalizes [phase-0.md](phase-0.md) finding 2: a content-shape filter on
prompts is **not** a Claude Code quirk. Two vendors, two different injected-block
vocabularies, same requirement.

### 4. Reasoning is empty on disk — second independent confirmation

All 237 `reasoning` records carry `summary` of length 2 (an empty array) and
`content: null`. Nothing to store.

Phase 0 found the identical thing in Claude Code: 23 `thinking` blocks, all
`{"signature": …, "thinking": ""}`. Two vendors independently withholding
reasoning text from the local transcript upgrades **"don't store reasoning"**
from an observation about one tool to a rule about the category. The
defense-in-depth framing in [tech-stack.md](../tech-stack.md) stays, but it is
now guarding against a much less likely event.

### 5. `session_meta` and `turn_context` are a gift

The first record of every file carries `cwd`, `id`, `timestamp`, `cli_version`,
and a `git` object with `commit_hash`, `branch`, and `repository_url`.

[scenarios.md](../scenarios.md) scenario 2 depends on `repo` and `git_branch`,
and [tech-stack.md](../tech-stack.md) notes these must be resolved *at capture
time* because the checkout may be renamed or gone by the time anyone searches.
Codex hands them over directly, `repository_url` included — which Claude Code
does not provide at all.

`turn_context` additionally snapshots `model`, `cwd`, `approval_policy`,
`sandbox_policy`, and `effort` **per turn**. `cwd` was stable within every file
in this sample, but the field exists per-turn, so the adapter should read it
per-turn rather than once per file.

### 6. Compaction rewrites history rather than marking a boundary

The single `compacted` record carries `{message, replacement_history}` with 59
history items — it *replaces* the preceding conversation rather than annotating
it, as Claude Code's `compact_boundary` does.

The record is appended to the log, so the file stays append-only and the
positional key from finding 1 survives. But an adapter that re-reads a file from
the top after compaction must not treat `replacement_history` as new
conversation content, or it will re-ingest 59 items as fresh exchanges.

Not fully characterized — one instance in the sample. Flagged, not resolved.

### 7. Assembly is many-to-one, as in Claude Code

384 `function_call` / `function_call_output` pairs against 100 user messages.
Assistant messages carry a `phase` field (280 have it, 120 don't) marking
preamble narration versus final answer. The many-to-one folding rule from
[phase-0.md](phase-0.md) finding 9 transfers unchanged; only the field names
differ.

## Verdict

The tier ordering in [tech-stack.md](../tech-stack.md) is confirmed against a
second assistant: the transcript is on disk, complete, and structured, so tier 1
is right and the PTY wrapper stays last.

What changes is the **parser interface**. It was designed against a sample of
one, and two of its assumptions turn out to be Claude-Code-specific:

1. Records have stable identity → they don't; dedup keys are per-adapter.
2. Record types partition cleanly → Codex ships two overlapping streams, and
   the duplicate copy is indistinguishable from the real one by content.

Both fail silently, which is the same property that made Phase 0's findings
worth acting on.

## Carried forward

- **Verify the append-only assumption directly.** Positional dedup is only safe
  if Codex never rewrites a session file in place. Finding 6 suggests it doesn't,
  but that was inferred from one compaction event, not tested.
- **Characterize compaction properly** against a session with several
  compactions.
- **Check whether `turn_id` links to anything reachable.** If some record type
  outside this sample carries it, key-based assembly becomes possible and
  finding 1 softens considerably.
- **No credential scan was run** on this archive. Phase 3 should cover it
  alongside the Claude Code re-scan already queued in [phase-0.md](phase-0.md).
