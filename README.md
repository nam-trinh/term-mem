# term-mem

**A local memory layer for terminal AI conversations.** Your assistant already
writes down everything it said. `tmem` keeps it, and gives it back to you
months later, in the terminal, without a browser and without a network.

```console
$ tmem ffmpeg concat
 1.  01HQ8F2K9MQ7X   2026-03-03   ~/talks/pycon-2026
     "I have 4 mp4 files I need to join into one. Same codec, same resolution…"
     → ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4

 2.  01HR1M4P2WB3Z   2026-01-12   ~/src/media-worker
     "why is ffmpeg dropping frames when I concat segments"
     → ffmpeg -f concat -safe 0 -i list.txt -vsync 2 out.mp4
```

Project **term-mem**, binary **`tmem`**. Rust, one SQLite file, no network.

---

## Nothing leaves the machine

This is the constraint everything else is designed around, not a feature bullet.

- **There is no network code in the binary.** No telemetry, no crash reporters,
  no update pings, not anonymised, not opt-in. It is a property you can check
  with `strings` and `lsof`.
- **The archive is one SQLite file** at `~/.local/share/term-mem/memory.db`. You
  can open it with tools you already have.
- **term-mem never watches your terminal.** No keylogger, no shell-wide PTY
  shim, no scraping of scrollback. It reads transcripts that assistants already
  write to disk, for assistants it has an explicit adapter for, and nothing
  else. Run `psql` in the same pane and `psql` is not captured.
- **Deletes are real deletes.** `tmem forget` removes the row, its mined
  commands, its file references and its index entries in one transaction, then
  `VACUUM`s so the text is not recoverable from a free page. There is no
  `deleted = 1` flag on a row that stays greppable on disk. A test reads every
  byte of every file term-mem wrote, the WAL included, and fails if the secret
  is in any of them.
- **Credentials are redacted before they are written**, not after. The archive
  never contains the secret in the first place.
- **The archive is not encrypted**, and `tmem status` says so in those words
  rather than leaving you to guess. See [Status](#status).

## Install

No published release yet. Build it:

```console
$ cargo build --release
$ install -m 755 target/release/tmem ~/.local/bin/tmem
```

Requires a Rust toolchain (1.82 or newer). SQLite is compiled in — there is
nothing else to install.

## Getting started

```console
$ tmem init --backfill
```

`init` creates the archive, registers a `Stop` hook in Claude Code's
`settings.json`, prints exactly what it is about to start recording, and — with
`--backfill` — imports the transcripts already on your disk, so the tool is
useful on day one instead of in six weeks. Use `--no-hook` to leave the
settings file alone.

From then on capture is automatic and costs about 2 ms at the end of each turn.

```console
$ tmem status         # what is in here, and is it running
$ tmem doctor         # is capture actually wired up
```

## Using it

### Searching

Search is the default verb, because it is most of what anyone types. Multi-word
queries need no quoting.

```console
$ tmem postgres migration lock
$ tmem backfill --repo --since january     # flags may follow the query
$ tmem ffmpeg --json | jq .
```

| Flag | |
| --- | --- |
| `--in <path>` | limit to a directory tree |
| `--since <when>` | `2h`, `7d`, `3w`, `january`, `yesterday`, `2026-03-01` |
| `--repo` | limit to the current git repository |
| `--json` | one record per line, for piping |
| `-n, --limit` | how many results (default 20) |

`tmem search <query>` is the explicit form, for scripts and for queries that
collide with a subcommand name (`tmem search status`).

Ranking is BM25 with the command lines the assistant ran weighted well above
prose. The metadata filters are the lever that does the heavy lifting —
constraining by *where and when you were* collapses the search space far more
than any amount of ranking.

### Browsing

The backstop, for when you cannot reconstruct a single distinctive word — which
is the normal way recall fails:

```console
$ tmem recent
$ tmem log --in ~/src/billing-api --since january
$ tmem show 01HQ8F2K9            # any unambiguous id prefix
$ tmem show 01HQ8F2K9 --session  # the surrounding conversation
```

### Controlling capture

Three scopes, because "off" means different things:

```console
$ tmem pause              # global, until resumed
$ tmem pause 2h           # global, auto-resumes
$ tmem resume
$ tmem ignore ~/work/client-x     # this tree, permanently
$ TMEM=0 claude                   # this invocation only
```

Pause state is always visible in `tmem status`.

### Redaction

Credentials with a recognisable shape — `sk-…`, `ghp_…`, AWS and GCP keys, JWTs,
`Authorization:` headers, PEM blocks — are replaced *before* anything reaches
the disk. The replacement names the rule, the row is flagged, and capture says
so out loud:

```console
$ tmem capture --all
12 new, 0 updated, from 3 transcript(s) (0 unchanged)
  redacted: 1 exchange(s) — github-token, aws-access-key ×2
```

Site-specific shapes go in `~/.config/term-mem/redact.toml`:

```toml
[[rule]]
name = "internal-host"
pattern = '\b[a-z0-9-]+\.corp\.internal\b'
```

A rule that does not compile stops capture rather than quietly not running.

**The entropy fallback is off by default.** It catches high-entropy values that
no pattern rule knows — and measured against a real archive, every single thing
it caught was a filesystem path or a UUID filename. Because mined command lines
are the only copy that exists, a false positive is permanent data loss. Turn it
on with `[entropy] enabled = true` if your archive is shaped differently.

Redaction is prevention, not a guarantee. `forget` is the valve for what it
misses.

### Owning the data

```console
$ tmem export --json > backup.jsonl
$ tmem export --markdown > archive.md
$ tmem export --json --repo > this-project.jsonl
$ tmem import backup.jsonl
```

Export writes the whole archive, not the first page — `-n` is honoured only when
you ask for it. Import is keyed the same way capture is, so it is idempotent, it
redacts on the way in, and it will not resurrect anything you deleted.

### Deleting

The safety valve, for when you realise afterwards that you pasted something you
shouldn't have:

```console
$ tmem forget --last
$ tmem forget 01HQ8F2K9
$ tmem forget --since '18 hours ago'
$ tmem forget --in ~/src/webhooks
```

It confirms before deleting and takes `-y` to skip that. With no terminal to
confirm at — a script, a cron job — it refuses rather than assuming, so `-y` is
how you say you mean it.

## How it works

Claude Code writes one JSONL file per session under `~/.claude/projects/`,
containing the complete text of every response. That file is the source; the
`Stop` hook is only a *trigger* telling `tmem` that a turn finished and where to
read. Both the hook and (later) a file watcher run the same parser against the
same file, keyed for idempotency, so re-running ingest over a transcript is a
no-op.

Two things are derived at write time because they cannot be recovered later:
the assistant's **command lines** are mined out of `tool_use` blocks and the
raw block is discarded, and **file paths** it touched are recorded as paths,
never as contents. Reasoning blocks are never stored.

The parser is the fragile part, and deliberately loud: unknown record types are
skipped and counted rather than being fatal, and anything it cannot attribute
is reported instead of guessed at. Most of the ways this format goes wrong
produce a plausible-looking archive that is quietly incorrect, which is why
[docs/phases/](docs/phases/) exists.

## Status

Phase 2 of the roadmap in [docs/plan.md](docs/plan.md). What works today is
capture, browse, keyword search, capture control, and deletion.

| | |
| --- | --- |
| Phase 0 — prove the premise | done |
| Phase 1 — capture and browse | done |
| Phase 2 — keyword recall | done |
| Phase 3 — redaction, and honest deletion | done (4 of 6 items) |
| Phase 4 — reuse (MCP server, tool schemas) | next |
| Phase 5 — semantic recall, if the archive says it is needed | |
| Phase 6 — Codex CLI, aider, and other assistants | |

**Two things Phase 3 did not ship, stated plainly:**

- **There is no encryption at rest.** SQLCipher works; the key management does
  not. An unattended capture hook needs a key it can read without you, which
  puts the key beside the database it protects — so the feature would stop
  almost nothing while claiming otherwise. The file is an ordinary SQLite
  database and `tmem status` says so.
- **The entropy fallback is off**, for the reason under
  [Redaction](#redaction).

Measured, not assumed: capture costs p95 **2.9 ms** at the turn boundary, and a
cold query against a 100,000-exchange archive returns in p95 **19.3 ms**.

## The documentation is the specification

This project is written docs-first, and they are worth more than the code:

- [docs/mission.md](docs/mission.md) — purpose and non-goals
- [docs/plan.md](docs/plan.md) — the phased roadmap
- [docs/tech-stack.md](docs/tech-stack.md) — schema, capture tiers, open questions
- [docs/cli.md](docs/cli.md) — the command surface
- [docs/scenarios.md](docs/scenarios.md) — three walkthroughs, used as acceptance tests
- [docs/phases/](docs/phases/) — **empirical findings from real archives**, which
  outrank the design docs wherever the two disagree

## Development

```console
$ cargo build && cargo test && cargo clippy --all-targets && cargo fmt --check
$ cargo test --release --test budget -- --nocapture     # the measured budgets
```

`TMEM_HOME`, `TMEM_CLAUDE_PROJECTS` and `TMEM_CLAUDE_SETTINGS` redirect the data
directory and transcript tree, so anything you run by hand can be pointed at a
scratch archive instead of your own.

## License

MIT.
