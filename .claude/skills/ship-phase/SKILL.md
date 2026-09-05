---
name: ship-phase
description: Implement one phase of the term-mem roadmap end to end — branch from main, write code in src/ with unit and integration tests, open a PR back to main, then audit the docs for staleness. Use when asked to ship, implement, build, or start a phase (e.g. "do phase 1", "ship phase 2").
---

# Ship a phase

One roadmap phase, from branch to merged PR. The phases are defined in
`docs/plan.md`; that file and the four docs beside it **are the specification**.
Do not invent scope, and do not silently narrow it.

## Ground rules

These come from the project's own docs and outrank convenience:

- **Capture is irreversible, retrieval is not.** Bugs that lose or corrupt
  captured data are unacceptable; a mediocre ranking function is fine and gets
  replaced later.
- **Nothing leaves the machine.** No telemetry, no network calls, no crash
  reporters, no "anonymized usage data". The only exception in the whole roadmap
  is the opt-in embedding-model download in Phase 5, triggered by an explicit
  `tmem embed --enable`. If a dependency phones home, it doesn't ship.
- **Silent failure is the enemy.** `docs/phases/phase-0.md` found seven parser
  traps, five of which fail silently. Prefer loud errors over best-effort
  guesses everywhere in the ingest path.
- **Every phase ends with something a user can run.** If what you built isn't
  usable on its own, the cut was wrong — say so rather than shipping a stub.

## Step 1 — Read before writing

Read, in this order:

1. `docs/plan.md` — the phase's **Scope**, **Exit**, **Budget**, and
   **Deliberately not** sections. "Deliberately not" is binding: shipping it
   early is a defect, not a bonus.
2. `docs/tech-stack.md` — the schema, the tier model, and the open questions.
   Check whether any open question is *blocking* for this phase; several are
   marked as such.
3. `docs/cli.md` — the exact command surface and output conventions.
4. `docs/scenarios.md` — the three walkthroughs. Phases 2 and 3 name them as
   acceptance tests; they should run verbatim.
5. `docs/phases/*.md` — findings from earlier phases. `phase-0.md` and
   `codex-cli-format.md` contain parser findings that must each become a
   fixture-backed test.

If the phase's scope contradicts a finding in `docs/phases/`, the finding wins —
it is empirical and the plan is not. Flag the contradiction to the user.

## Step 2 — Branch

Branch from an up-to-date `main`:

```
git checkout main && git pull --ff-only 2>/dev/null; git checkout -b phase-<N>/<short-slug>
```

Example: `phase-1/capture-and-browse`. Never commit a phase directly to `main`.

## Step 3 — Implement in `src/`

Rust, per `docs/tech-stack.md`. On the **first** phase, bootstrap the crate:
`cargo init --name tmem`, binary at `src/main.rs`, and add `Cargo.toml`,
`rust-toolchain.toml`, and a `.gitignore` covering `/target`.

Layout — grow it as phases land, don't create empty modules ahead of need:

```
src/
├── main.rs           clap entrypoint, subcommand dispatch
├── cli/              one module per subcommand
├── db/               schema, migrations (refinery), queries
├── capture/
│   ├── adapters/     one per assistant; claude_code.rs first
│   └── watcher.rs    notify-based tailing
├── search/           FTS5, ranking, snippets
├── redact/           pattern + entropy rules (Phase 3)
└── mcp/              stdio server (Phase 4)
tests/                integration tests
tests/fixtures/       recorded, scrubbed transcript samples
```

Hard requirements regardless of phase:

- **Migrations from day one.** Every phase changes the schema; an unversioned
  schema becomes an unupgradable one.
- **Fixtures are scrubbed before they are committed.** They come from real
  transcripts. Read every fixture you add and strip paths, tokens, hostnames,
  and client names. This is not optional and applies even when the source
  archive looks clean.
- **Each adapter declares its own dedup key and its own injected-block
  vocabulary.** `(session_id, uuid)` is Claude Code's key, not a universal one —
  see `docs/phases/codex-cli-format.md`.

## Step 4 — Tests

Both kinds, always. A phase is not done with one.

**Unit tests** — colocated in `#[cfg(test)]` modules. Cover the parsing and
ranking logic: discriminators, polymorphic fields, boundary conditions.

**Integration tests** — in `tests/`, driving the real binary against a temp
database and committed fixtures. These are what prove the phase's **Exit**
criterion, so write them against the Exit wording directly.

Non-negotiable test cases:

- **Every finding in `docs/phases/*.md` gets a fixture and a test.** A finding
  without a regression test will be re-broken.
- **Idempotency:** re-running ingest over the same transcript is a no-op. Assert
  the row count, not just the absence of an error.
- **Deletion completeness** (from Phase 3 on): after `tmem forget`, grep the raw
  database file on disk for the secret. A `deleted=1` flag is a failure.
- **Budgets are measured, not assumed:** hook latency < 5 ms, and search p95
  < 100 ms at 100k exchanges against a *generated* archive. If a budget is
  missed, report the number — do not quietly relax it.

Run `cargo test`, `cargo clippy --all-targets`, and `cargo fmt --check` before
the PR. Report real results; if something fails, say so with the output.

## Step 5 — PR back to `main`

```
gh pr create --base main --title "Phase <N> — <name>" --body "…"
```

The body states: what shipped against the phase's Scope, evidence the **Exit**
criterion is met, measured numbers for any **Budget**, anything deliberately
left out and why, and any new open question. Link the phase's findings doc.

End the body with:

```
🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

End commit messages with:

```
Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
```

Do not merge the PR. Opening it is where this skill stops.

## Step 6 — Docs staleness audit

Implementation always contradicts the design somewhere. Finding out where is a
deliverable, not cleanup. Work through each doc and report findings even when
nothing needs changing:

- **`docs/plan.md`** — mark the phase done with a one-paragraph verdict linking
  its findings doc, in the style of the Phase 0 entry. If what you learned
  changes a *later* phase's scope, edit that phase now.
- **`docs/tech-stack.md`** — schema drift, decisions that didn't survive
  contact, and open questions. Resolve them by striking through and appending
  **Resolved:** with the answer; add new ones you created.
- **`docs/cli.md`** — every flag, subcommand, exit code, and output convention
  as actually implemented. Divergence here is user-visible.
- **`docs/scenarios.md`** — for phases that claim a scenario, confirm it runs
  verbatim. If it doesn't, that is a finding about the design.
- **`docs/mission.md`** — rarely changes. If a phase makes one of its claims
  untrue, that is a serious finding: raise it rather than editing the mission to
  match the code.
- **`CLAUDE.md`** — the orientation file loaded into *every* session in this
  repo. Create it during the first phase that produces a `src/` tree if it
  doesn't exist yet. Keep it short (~40 lines) and limited to what a fresh
  session would otherwise re-derive or get wrong:
  - the docs are the spec, and the order to read them in;
  - `docs/phases/` holds empirical findings that **outrank** the design docs
    when the two conflict;
  - the privacy premise, because it silently constrains dependency choices;
  - project `term-mem`, binary `tmem`;
  - the build, test, and lint commands, and where fixtures live.

  Update it whenever a phase changes any of those — most often the commands and
  the module layout. **Do not summarize `tech-stack.md` into it.** A second copy
  of the design will drift from the first, and the cost is paid on every
  unrelated turn. Link, don't restate.

Then write **`docs/phases/phase-<N>.md`**, following the structure of
`phase-0.md`: sample or scope up front, honest limits on what the evidence
supports, numbered findings with measurements, a verdict, and a
carried-forward list. Record what was *surprising* — not a changelog of what you
built, which the diff already shows.

Finally, verify every markdown link across `docs/` resolves, and commit the doc
updates onto the same branch so they land with the PR.

## Reporting back

State plainly: what shipped, what the tests prove, measured budget numbers,
what was left out and why, which docs changed, and what is now uncertain. If a
stated requirement wasn't met, lead with that — do not bury it under what
worked.
