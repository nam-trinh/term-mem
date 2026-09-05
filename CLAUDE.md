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
  later — the raw `tool_use` block is never stored.
- **Silent failure is the enemy.** Ingest prefers a loud error, or a
  counted-and-reported skip, over a best-effort guess. Most traps found so far
  produce a plausible-looking archive that is wrong.

## Build, test, lint

```
cargo build && cargo test && cargo clippy --all-targets && cargo fmt --check
cargo test --release --test budget -- --nocapture    # the measured hook budget
```

Unit tests sit beside the code; `tests/` drives the real binary against a temp
database. Fixtures are in `tests/fixtures/<adapter>/` — real record *shapes*,
synthetic content, one per finding in `docs/phases/`. `TMEM_HOME`,
`TMEM_CLAUDE_PROJECTS` and `TMEM_CLAUDE_SETTINGS` redirect the data directory
and transcript tree; use them for anything run by hand.

## Layout

`src/main.rs` dispatch · `src/cli/` one module per subcommand · `src/db/` schema
and forward-only refinery migrations · `src/capture/` ingest, hook queue, and
`adapters/` · `src/output.rs` pipe detection, exit codes, formatting.

Adapters declare their own dedup key and injected-block vocabulary; neither is
universal — see `src/capture/adapters/mod.rs` and
[codex-cli-format.md](docs/phases/codex-cli-format.md).
