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

The Exit criterion names three failures. Each needs a different check, and the
first two are less well covered than they look.

**Losing an exchange.** `tmem doctor` reports transcripts with no watermark row
— but that only catches a *whole file* never ingested, plus one that has grown
since. It cannot see an exchange dropped **inside** a file that was ingested,
which is the loss mode [phase-1.md](phase-1.md) actually recorded: orphaned
assistant records, unusable prompts, unparsable lines. Those have counters, and
`capture` prints them:

```
2 new, 1 updated, from 2 transcript(s) (19 unchanged)
  skipped: 0 unparsable, 45 unknown-type, 1 prompt(s) with no response, …
```

**And on the automatic path nobody sees that line.** The hook spawns the drainer
with `--quiet` and its stderr pointed at `/dev/null`, which is deliberate — the
turn boundary is not the place for a report — but it means the counters exist
and are discarded on every real capture. So the check is to run
`tmem capture --all` **by hand**, periodically, and read the `skipped` line. A
non-zero `unparsable` or `unusable` is the signal; `prompt(s) with no response`
is usually just a turn still in flight.

*That the loss counters are invisible during normal operation is itself a
finding, and arguably a defect. It is recorded here rather than fixed because
what to do about it — a notice in `status`, a counter in the database, or
nothing — is a design question the soak is better placed to answer than a guess
is.*

**Duplicating one.** The key is `(assistant, session_id, source_key)`, not
`source_key` alone, and the database enforces it — so an exact repeat cannot
happen. The realistic duplication mode is the same *content* arriving under a
**different** key: a Codex session file rewritten in place would shift every
positional key (the append-only assumption
[codex-cli-format.md](codex-cli-format.md) carries forward as unverified), and a
changed session id would do the same for either vendor.

The detector is identical prompt **and** identical response under different
keys:

```sql
SELECT prompt, response, COUNT(*) FROM exchanges
GROUP BY prompt, response
HAVING COUNT(DISTINCT assistant || '|' || session_id || '|' || source_key) > 1;
```

Not prompt text alone. [phase-1.md](phase-1.md) finding 4 rejects prompt-text
matching as a dedup *rule* because asking the same question twice is legitimate
history — and on day 0 this archive already has **six** repeated prompts and
**zero** repeated prompt-and-response pairs, so the naive version would start
with six false positives. Timestamps are no help either: Codex has 2,186 unique
ones across 2,665 records.

Even the query above is a signal to investigate rather than a verdict — re-asking
and getting the same answer is possible.

**Noticing it running.** The hook budget is 5 ms and is measured, but what is
being tested here is subjective and cannot be asserted. Day 0: 2 ms mean.

And two the criterion does not name, both of which this project has now been
bitten by once:

- **prompts that are not prompts.** The IDE-context leak
  ([phase-6.md](phase-6.md) finding 8) was found by reading `tmem recent` and
  noticing the prompts were wrapper text. Worth doing deliberately, now and
  then, because no test detects an injected wrapper nobody has seen before.
- **a format that moved.** `capture` reports unrecognised record types; the
  archive currently reports `pr-link`, which is known. A *new* name appearing
  there is the signal — and it arrives on the same discarded stderr as the loss
  counters, so it needs the same by-hand run to be seen.

## Log

### 2026-09-22 — CI turned out to be gated on a scan that never ran

Merging was blocked with *"Waiting for Code Scanning results. Code Scanning may
not be configured for the target branch."* The cause was a repository ruleset,
`main-protection`, carrying a `code_scanning` rule requiring CodeQL — while
CodeQL had never been configured. The required result had no producer, so the
gate could never resolve, for any PR.

Two things worth keeping:

- **CodeQL's default setup does not support Rust.** The API's `GET
  default-setup` reports `languages: ["actions", "rust"]`, which reads like
  support and is not — it is a list of languages *detected* in the repository.
  `PATCH` rejects `rust` outright. So scanning here covers the workflow YAML and
  nothing else, and the ruleset is satisfied by a scan that never looks at the
  program. That is worth knowing before anyone trusts the badge.
- Its first run flagged a real one anyway: `rust.yml` declared no `permissions`,
  so CI inherited the repository default token scope it never used. Fixed.

### 2026-09-22 — a flaky test, found by CI rather than by the suite

`a_live_pty_session_records_the_turns_a_user_typed` failed once in six CI runs
and passed on re-run. It drove the PTY tier with piped stdin, which is the path
the tier documents as lossy: whether the quiescence timer closed the turn before
the child exited was a race, so the assertion was on a coin flip.

Rewritten to type the input with a pause past the quiet window — the case the
feature actually supports — and it then failed *deterministically*, which is how
a second real bug surfaced: the echo of the next question arrives while the
current turn is still open, so it lands on the **end** of the answer.
`clean_response` trimmed echoes only from the front, leaving responses ending
`>>> bye`. Now trimmed from both ends. Fifteen consecutive local runs, green.

The lossy path still has a test; it asserts what that path promises — that turns
may merge and the command says so — instead of asserting a race.
