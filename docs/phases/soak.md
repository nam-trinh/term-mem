# The soak

Phase 1's Exit criterion, outstanding since 2026-09-05 and carried forward
through five phases:

> the author runs it against their own daily work for two weeks without losing
> an exchange, duplicating one, or noticing it running.

**Started 2026-09-22.** It had not been started before, and no document said so.
[phase-6.md](phase-6.md) is where that was finally noticed: there was no
`~/.local/share/term-mem/memory.db` on the author's machine at all. Six phases
of features and no day of real use.

This file is the log. It exists so the next gap of this kind is visible in a
document rather than discoverable only by checking the filesystem.

## Day 0 — 2026-09-22

Installed with `cargo install --path .`; `tmem init --backfill`.

```
archive     ~/.local/share/term-mem/memory.db   3.9 MB
exchanges   140          threads 18          commands 842
span        2026-03-01 … 2026-09-22
redacted    5 exchange(s)
capture     ON, Stop hook registered in ~/.claude/settings.json
```

Backfill took 140 exchanges from 20 transcripts across both vendors — 13
Claude Code (including 5 subagent transcripts) and 7 Codex CLI. The Codex half
reaches back to March because that archive predates this project; the Claude
Code half is this project building itself.

`settings.json` gained exactly one key (`hooks`) and nothing else changed;
`permissions`, `model` and `effortLevel` are byte-identical to the backup at
`~/.claude/settings.json.pre-tmem-backup`.

Verified on day 0, against live data rather than fixtures:

- **The hook fires and the exchange lands.** The session that ran the install is
  in the archive, 11 exchanges, including the turn that asked for it.
- **Hook latency 2 ms mean over 20 runs** on a 1,329-record transcript. (A first
  measurement of 23 ms was an artifact of timing the shell and a Python
  subprocess around it, not the hook.)
- **Re-ingest is an upsert on live data**: a drain mid-conversation reported
  `0 new, 1 updated` — the in-progress exchange completing rather than
  duplicating.
- **Redaction fires on real content.** Five exchanges across the backfill
  (`url-credentials`, `github-token`, `slack-token`, `anthropic-key`), and two
  more on the live session.
- **Subagent transcripts are captured**, the Phase 6 fix working on real data
  rather than a fixture.

### What to watch for

The Exit criterion names three failures, and each needs a different check:

- **losing an exchange** — `tmem doctor` reports transcripts on disk with no
  watermark row; that is the direct test.
- **duplicating one** — the count should track the transcripts, and
  `source_key` uniqueness is what prevents it. A duplicated exchange would show
  as two rows with the same prompt and timestamp.
- **noticing it running** — the hook budget is 5 ms and is measured, but the
  thing being tested here is subjective and cannot be asserted.

And two the criterion does not name, both of which this project has now been
bitten by once:

- **prompts that are not prompts.** The IDE-context leak
  ([phase-6.md](phase-6.md) finding 8) was found by reading `tmem recent` and
  noticing the prompts were wrapper text. Worth doing deliberately, now and
  then, because no test detects an injected wrapper nobody has seen before.
- **a format that moved.** `capture` reports unrecognised record types; the
  archive currently reports `pr-link`, which is known. A *new* name appearing
  there is the signal.

## Log

_(Entries go here. A quiet week is worth recording as a quiet week.)_
