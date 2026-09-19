# term-mem

A local memory layer for terminal AI conversations. Project **term-mem**, binary
**`tmem`**. Rust, one SQLite file, no network.

## The docs are the spec

Read in this order: [mission.md](docs/mission.md) (purpose and non-goals) →
[plan.md](docs/plan.md) (the phased roadmap; each phase's **Scope**, **Exit**,
**Budget** and **Deliberately not** are binding) → [tech-stack.md](docs/tech-stack.md)
(schema, capture tiers, open questions — check whether one is *blocking* for
your phase) → [cli.md](docs/cli.md) (command surface, output conventions) →
[scenarios.md](docs/scenarios.md) (acceptance tests for Phases 2 and 3).

**[docs/phases/](docs/phases/) outranks all of them.** Those are empirical
findings from real archives. Where a finding and a design doc disagree, the
finding wins and the doc is what gets corrected — flag the contradiction rather
than quietly coding to one side.

## The premise, which constrains dependencies

*Nothing leaves the machine.* No telemetry, no crash reporters, no update pings.
A dependency that opens a socket does not ship. The one planned exception in the
roadmap is the opt-in model download in Phase 5, behind `tmem embed --enable`.

- **Capture is irreversible, retrieval is not.** Losing or corrupting a captured
  exchange is unacceptable; a mediocre ranking function is fine and gets
  replaced. Anything mined at capture (commands, file paths) is unrecoverable
  later — the raw `tool_use` block is never stored. This is also why redaction
  defaults are conservative: a false positive is permanent data loss, not a bad
  result.
- **Silent failure is the enemy.** Ingest prefers a loud error, or a
  counted-and-reported skip, over a best-effort guess. Most traps found so far
  produce a plausible-looking archive that is wrong. A counter is not immunity:
  Phase 2 finding 1 is a case where the honest report of what ingest dropped was
  read as a fact about the format for a whole phase.

## Build, test, lint

```
cargo build && cargo test && cargo clippy --all-targets && cargo fmt --check
cargo test --release --test budget -- --nocapture    # measured budgets
```

The budget suite is release-only, slow, and internally serialised: it measures
the `Stop` hook at the turn boundary (< 5 ms), a cold query against a generated
100k-exchange archive (p95 < 100 ms), and the `UserPromptSubmit` recall hook
against the same archive and the same 100 ms. It builds that archive by
ingesting transcripts through the real parser. Its two tests take a mutex —
running them concurrently measures the disk rather than the program. Unit tests sit beside the code; `tests/` drives the real binary against
a temp database, and `tests/search.rs` runs scenarios 1 and 2 verbatim. Fixtures are in `tests/fixtures/<adapter>/` — real record *shapes*,
synthetic content, one per finding in `docs/phases/`. `TMEM_HOME`,
`TMEM_CLAUDE_PROJECTS` and `TMEM_CLAUDE_SETTINGS` redirect the data directory
and transcript tree; use them for anything run by hand.

## Layout

`src/main.rs` dispatch · `src/cli/` one module per subcommand · `src/db/` schema
and forward-only refinery migrations · `src/capture/` ingest, hook queue, and
`adapters/` · `src/search/` FTS5 match building and BM25 ranking ·
`src/redact/` pre-write pattern rules and the user rule file ·
`src/mcp/` the three agent tools and a hand-rolled JSON-RPC stdio server ·
`src/output.rs` pipe detection, exit codes, formatting.

`exchanges_fts` is maintained by triggers on `exchanges`, not by the write path.
Anything that changes a row updates the index without knowing it exists.

**Every write path redacts and honours the `forgotten` tombstone.** There are
two — transcript ingest and `tmem import` — and a third would need both again.

**Every agent-facing read goes through `db::open_readonly`.** Read-only is the
open flags, not the tool list: `src/mcp/` defines the three tools once and MCP,
`tmem tools` and `tmem call` are envelopes over it. Results carry `cite` (the
`tmem show` that proves them) and `why`. An unknown tool argument is an error,
never a wider search.

**Automatic recall is off by default, and that means no hook is registered** —
`tmem recall --enable` writes both the config and the `UserPromptSubmit` entry,
and `doctor` fails if they disagree. Whether anything is injected is decided by
term coverage, never by a BM25 score; see
[phase-4.md](docs/phases/phase-4.md) finding 1 before reaching for one.

Adapters declare their own dedup key and injected-block vocabulary; neither is
universal — see `src/capture/adapters/mod.rs` and
[codex-cli-format.md](docs/phases/codex-cli-format.md).
