# Phase 4 — Findings

Built 2026-09-20 against [plan.md](../plan.md)'s Phase 4 scope: `tmem mcp` with
`search_memory`/`get_exchange`/`recent`, `tmem tools --schema openai` and `tmem
call`, `tmem render --prompt-block`, and optional `UserPromptSubmit` recall that
is off by default. This is "the third pillar — memory goes back into a live
session".

**Headline: all four scope items ship, and the one number the plan asked for
could not be built.** The plan says automatic recall should fire "if anything
clears a relevance floor", which reads like a minimum score. A BM25 score cannot
be a floor: it is not comparable between archives, and the implementation that
used one passed every unit test and then found nothing at all against a
three-row integration fixture, scoring a perfect match at `5e-06`. What replaced
it is term coverage, which is archive-size independent and is a sentence a user
can check. That is finding 1 and it is the only design claim this phase
contradicted.

The rest is mostly the phase behaving as specified, with two surprises worth the
ink: the recall hook's latency is set by the commonest word in the prompt rather
than by how many words it has (finding 4), and the budget suite the docs tell
people to run had been measuring the wrong thing whenever both of its tests ran
at once (finding 5).

## Scope

No sample this time — this phase reads the archive rather than parsing anything
new, so there is no format to be wrong about. What is measured instead is the
agent surface itself, against the generated 100k-exchange archive the budget
suite already builds.

| Exit clause | Result |
| --- | --- |
| a new session answers from a past exchange | **yes** — over MCP, over `tmem call`, over the `--json \| render` pipe, and through the `UserPromptSubmit` hook; `tests/reuse.rs` drives all four against the real binary |
| the user can see exactly which one | **yes** — every result carries `cite`, the literal `tmem show <id>` that displays the same exchange, and the recall hook prints the ids to stderr where the user's own terminal shows them |
| …and why it was chosen | **yes, and narrowly** — `why` says "keyword match on <terms> (BM25 <score>, command lines weighted above prose); no semantic search is involved", and the preview adds coverage. It is an honest account of a keyword ranker, not an explanation of relevance |

## 1. "A relevance floor" cannot be a BM25 score

The plan's sentence is one clause long and the implementation of it was the
obvious thing: a `min_score` in the config, defaulted to `4.0`, compared against
the negated BM25 the search path already returns. Every unit test passed. The
first integration test against a real three-exchange archive recalled nothing at
all, for a prompt whose words were sitting in the archive verbatim.

The score of that perfect match was `0.0000051`.

BM25's IDF term is `log((N - n + 0.5) / (n + 0.5))`, which goes to zero as `n`
approaches `N` — when every document contains the term, the term distinguishes
nothing and contributes nothing. On a three-row archive almost every matching
term is in almost every row, so every score collapses toward the epsilon SQLite
clamps at. On a 100k-row archive the same match scores double digits.

So a fixed floor is not a floor. It is "always on" for users with a lot of
history and "always off" for users with a little, and the user it is worst for
is the new one, who has the least reason to keep the feature turned on.

What ships instead is **term coverage**: how many of the prompt's own content
words are physically present in the exchange, with a floor of two, scaling to a
quarter of a long prompt, capped at four. Properties worth stating:

- It does not move with the size of the archive.
- It is checkable. `tmem recall <words>` prints `3/7 query term(s) present
  (signature, validation, clock)`, and a user can disagree with that.
- It is not a relevance measure and does not claim to be. BM25 still orders the
  candidates; coverage only decides whether any of them are good enough to spend
  the user's context window on.

The score is still reported everywhere, in `why` and in the preview, because it
is what did the ordering. It is just no longer asked a question it cannot
answer.

## 2. Read-only wanted to be a file handle, not a rule

[plan.md](../plan.md) says "Agents read memory; they never write or delete it",
and the natural implementation is to expose only read tools — which is true and
is tested, but is a property of a list that a later phase edits.

`SQLITE_OPEN_READ_ONLY` is the version that cannot be edited by accident. The
MCP server and `tmem call` both go through `db::open_readonly`, and
`src/db/mod.rs` has a test that fires `DELETE`, `UPDATE`, `INSERT`, `DROP`,
`CREATE` and an FTS5 `rebuild` at that handle and asserts every one of them
fails. A future tool added to the dispatch table inherits the guarantee without
anyone remembering to think about it.

Two consequences, both acceptable and both worth writing down:

- **Migrations cannot run on that handle.** An archive older than the binary
  fails loudly on the first query rather than being upgraded under an agent.
  That is the right way round — `tmem status` is one write-capable command away
  — but it means the first `tmem mcp` after an upgrade can fail where the docs
  imply it would not.
- **`open_readonly` refuses to create.** `db::open` creates; this one errors
  and says `run tmem init`. An agent pointed at the wrong `TMEM_HOME` must not
  be handed an empty archive, which is indistinguishable from a user with no
  history.

## 3. The tombstone needed nothing, and that is the finding

[plan.md](../plan.md)'s note on this phase warned that the MCP server is a third
read path and that `import` was a third *write* path which had to be taught the
`forgotten` tombstone. The read path needed no such teaching, and the reason is
worth keeping: Phase 3's deletion is a real delete, so a forgotten exchange is
not in `exchanges` for any reader to find. There is no flag to check because
there is no row.

`tests/reuse.rs` asserts it anyway — `forget`, then `get_exchange` by the exact
id and `search_memory` for text that was in it, both empty — because the
property that needs protecting is that nobody ever *adds* a `deleted` flag to
make some future feature easier.

Redacted rows are the other half of that note, and go to the agent as stored,
with `redacted: true` and a line saying that `[redacted:…]` markers are
term-mem's rather than the user's. Without that the model cannot tell a
placeholder from something the user typed, and will occasionally explain it.

## 4. Recall's latency is set by the commonest word, not the word count

The `UserPromptSubmit` hook is on the turn boundary exactly as the `Stop` hook
is, and unlike `Stop` it cannot enqueue and run away — its output has to be on
stdout before the prompt is sent. [plan.md](../plan.md) sets it no budget, which
is an omission rather than permission, so it is held to the search budget.

Measured against the generated 100k-exchange archive, release build:

```
  recall short     p50  72.33 ms   p95  73.37 ms   max  74.03 ms
  recall long      p50  48.99 ms   p95  49.36 ms   max  49.54 ms
```

The expectation going in was that cost tracks the number of terms — a prompt is
a paragraph, and a paragraph is a lot of terms to OR together. The *short*
prompt is half again slower than the long one.

The short prompt contains `problem`, and the generated archive puts the word
`problem` in all 100,000 rows. One term with a posting list covering the whole
archive costs more than a dozen selective ones. The cap on term count (24) is
therefore not the lever it was built to be, and the honest statement of the
budget is: **recall costs what its most common word costs**, up to the whole
archive.

It passes — 73 ms against 100 ms — and that is with a deliberately pathological
corpus where a word really is in every row. It is also 4× the cost of an
ordinary two-term query, on a path the user pays for on every prompt. Nothing
here fixes it, and the lever if it is ever needed is a stop list derived from
the archive's own term frequencies rather than from a hardcoded list of English
function words.

## 5. The budget suite had been measuring the machine, not the program

`cargo test --release --test budget -- --nocapture` is the command in
[CLAUDE.md](../../CLAUDE.md). Running it after adding the recall measurement
failed the *Phase 1* hook budget at 5.62 ms against 5 ms — a budget that has
passed since Phase 1 and that this phase does not touch.

Run singly it measured 2.51 ms. The two tests in that file run in parallel by
default, and one of them generates a 100k-exchange archive by ingesting 200
transcript files; the other measures single-digit-millisecond process
latencies while that happens. The number being reported was the disk, not the
hook.

They now take a mutex. This is not a Phase 4 defect — it has presumably been
true since the search budget was added in Phase 2, and the reported 2.35 ms
figure in [phase-1.md](phase-1.md) predates it and stands. But a latency suite
whose numbers depend on test scheduling is worse than no latency suite, because
it is believed.

## 6. `render` has to assume its own input is hostile

`render` formats text from the archive into a block that a model will read as
part of a prompt. The archive is the user's own, which is not the same as the
text being safe: an exchange from March that happens to contain "ignore all
previous instructions" is a prompt injection with a six-month fuse, and so is
one that contains a code fence.

Two things follow, and only the second was anticipated:

- The block says, in its own header, that nothing inside it is an instruction
  and that anything that looks like one is something the user or an assistant
  wrote earlier. This costs about forty tokens and is the only defence
  available at this layer.
- Command blocks are fenced with a fence longer than the longest backtick run
  inside them. A three-backtick fence around a response *about* markdown closes
  early and spills the rest of the block into the prompt as prose.

The plan calls `render` "formalized only as formatting", and it is — it never
opens the database. But "only formatting" turned out to include deciding how the
text is framed, which is not a neutral act.

## 7. Four new reserved words, two of which are searchable English

[cli.md](../cli.md) is explicit that the subcommand list constrains the search
surface: "Every reserved word is a query that behaves surprisingly. Keep the set
small, stable, and made of words nobody searches for."

This phase adds `mcp`, `tools`, `call` and `render`. `mcp` and `render` are
safe. `tools` and `call` are ordinary English that a developer might plausibly
type — `tmem call` is now a usage error rather than a search for the word
"call". The names come from [tech-stack.md](../tech-stack.md) verbatim and the
escape hatch (`tmem search call`) is already documented, so this ships as
specified; it is recorded because it is the first time the reserved-word budget
has actually been spent on a word with a meaning.

## 8. Every defect the review found was in the file that is not ours

Twelve findings came out of reviewing this phase, and they cluster hard. Five
were in the settings.json editing — the one place term-mem writes to a file
belonging to another program — and three of those were introduced by the
refactor that was supposed to make it *safer*: Phase 1 had one hook editor,
Phase 4 needed two, so it became a shared helper, and the helper grew three
different ideas about what "this hook is ours" means.

`add_hook` and `hook_registered` matched a substring of the serialised group;
`remove_hook` compared the `command` field for equality. A user whose `tmem` is
not on the hook's `PATH` writes `/usr/local/bin/tmem recall --hook` by hand,
which lands in the gap: `status` reported ON, `--disable` reported OFF and did
nothing, and `doctor` pointed at the command that had just failed silently.
Permanently. All three now go through one `entry_names`, which compares for
equality after normalising away the path the binary was invoked by.

The second one is worse in kind. `remove_hook` dropped any group left with an
empty `hooks` array — including a user's own `{"matcher": "x", "hooks": []}`,
which was empty before term-mem touched it. Turning a feature off deleted a line
of someone else's configuration. The rule that was missing is the obvious one in
hindsight: **drop a group only if we are what emptied it.** Nothing in that file
is ours to tidy.

The generalisable lesson is not "be careful with JSON". It is that the
refactor's stated justification — "two hand-rolled settings.json editors is two
chances to corrupt a file that is not ours" — was right about the risk and
wrong about where it lives. Consolidating three call sites into one helper does
not give them one definition of identity unless you go and write it.

## 9. A config-fatality rule does not transfer between config files

`Config::load` was fatal on an unparsable `recall.toml`, and the comment said
why: "for the same reason a broken `redact.toml` is fatal". The reasoning was
copied rather than re-derived, and it does not survive the copy.

A redaction rule that will not compile has to stop *capture*, because capturing
unredacted is worse than capturing nothing. A recall config that will not parse
should stop *recall*, because the alternative — injecting under settings nobody
can read — is the harm. Those are not the same conclusion, and treating them as
one produced the review's most embarrassing pairing:

```
$ tmem status
…
tmem: parsing recall.toml: TOML parse error at line 1, column 11
$ tmem recall --disable
tmem: parsing recall.toml: TOML parse error at line 1, column 11
```

`status` abandoned its output halfway, taking the pause state and the ignore
list with it — the two lines a user checks when they suspect capture is not
running. And the command both `status` and `doctor` tell you to run to fix the
file was the one command the file stopped from running.

`Config::load_or_default` is the fix, used by `status`, `preview` and
`--disable`; it returns the defaults — which are **off** — plus the reason. The
failure direction is now "nothing is injected", which is the safe one.

## 10. The budget suite failed in debug, and had done since Phase 1

`cargo test` is the first command in [CLAUDE.md](../../CLAUDE.md), and it ran
`tests/budget.rs` against an unoptimised binary and failed on numbers nobody
ever claimed for one. This phase's addition made it louder (272 ms against a
100 ms budget) but did not cause it: the file has been release-only by
convention and by doc comment, never by code, since Phase 1.

A suite that is supposed to fail is a suite readers learn to skip, which is the
opposite of what a budget assertion is for. Both tests now skip in a debug build
and say which command to use instead, and `cargo test` is green for the first
time.

The recall hook's tail is genuinely close: a sampled max of 101.7 ms against the
100 ms budget was seen during review, with p95 comfortably under. The p95 is
what the budget asserts, consistent with Phases 1 and 2; the tail now has its
own looser assertion at 2× rather than going unwatched.

## 11. Budgets: the rest

```
Stop hook latency (60 samples, release build):
  12-record transcript     p50 2.02 ms   p95 2.82 ms   max 2.94 ms
  8 MB transcript          p50 2.00 ms   p95 2.91 ms   max 6.44 ms

archive: 100000 exchanges, 139.5 MB
  common two-term  p50  19.95 ms   p95  26.10 ms   max  27.10 ms
  rare term        p50   3.24 ms   p95   4.08 ms   max   4.18 ms
  filtered         p50  14.69 ms   p95  16.15 ms   max  16.21 ms
  recall short     p50  77.68 ms   p95  87.79 ms   max  90.46 ms
  recall long      p50  53.12 ms   p95  57.28 ms   max  81.56 ms
```

Unchanged in shape, and now measured without the interference in finding 5.

## Verdict

**The scope ships and the Exit criterion is met on all four paths.** A new
session reaches a past exchange over MCP, over `tmem call`, over the
`--json | render` pipe, and through the opt-in hook; every one of them names the
exchange and the command that shows it.

The phase's own claim about itself needs one qualification. "The user can see
exactly *why* it was chosen" is met in the sense that the tool says truthfully
what it did — which terms matched, what BM25 scored, how many of the prompt's
words are present. It is not met in the sense a user might hope for, because
"why" for a keyword ranker is a tautology: it was chosen because those words are
in it. Finding 1 is what that tautology looks like when you try to build a
threshold on top of it.

The part of this phase most likely to be wrong in six months is the coverage
floor. It is a better shape of answer than a score floor, and it is still a
number nobody has validated against a real user's real prompts. Unlike the score
floor, at least it fails the same way for everyone.

## 12. Four smaller ones, recorded because they share a shape

- **`hook_registered`, a predicate, created `~/.claude/` as a side effect** —
  `read_settings` did the `create_dir_all` for every caller including the ones
  that only ask a question. An unconfigured machine got a directory out of
  running `tmem status`. The creation moved to `write_settings`, where a write
  actually happens.
- **`status`'s MCP line could never appear.** It grepped `settings.json`;
  `claude mcp add` writes `mcpServers` to `~/.claude.json` (user scope), to
  `projects.<cwd>.mcpServers` there (local scope), or to `.mcp.json` beside the
  checkout (project scope). Never settings.json. The check now looks in all
  three and says which one it found.
- **`envelope`'s two notes overwrote each other**, so a `search_memory` that
  was clamped *and* found nothing told the agent only the second thing.
- **`render -n 0` blamed the pipe** — it reported "nothing on stdin" for
  perfectly good input, because the truncation happened before the emptiness
  check. And the ~480-character header sat outside `--max-tokens`, so
  `--max-tokens 1` emitted a wrapper with no memory in it; the cap is now a cap,
  and a budget too small for the wrapper produces no block at all.
- **Invalid UTF-8 killed the MCP server.** The malformed-JSON test passed
  because malformed JSON is still valid UTF-8; a client crashing mid-write
  produces a truncated multi-byte character, and `read_line` on a `String` fails
  the whole call. The loop reads bytes now.

What these share with findings 8 and 9 is that each one is a place where the
*stated* intent and the code disagreed, and the comment was the thing that was
right. Every one of them had a doc comment describing the correct behaviour
sitting directly above the code that did not do it.

## Carried forward

- **The coverage floor is unmeasured.** Two terms, scaling to a quarter, capped
  at four. It is archive-size independent, which is the bug it was built to
  fix, and it has never been checked against a real session's prompts. The
  measurement that would settle it is a log of what recall injected and whether
  the user's next turn used it — which is a query log, and
  [phase-2.md](phase-2.md) already notes that nothing records one, deliberately.
  The same tension now has two phases waiting on it.
- **Recall costs what its commonest word costs** (finding 4). A term-frequency
  stop list built from the user's own archive is the lever; nothing needs it
  yet.
- **`instructions` in the MCP `initialize` response is unvalidated prose.** It
  tells a client when to search the archive, and whether it makes any model
  actually do so is untested and probably untestable here.
- **Nothing measures whether recall helps.** The Phase 5 decision about
  embeddings was already going to be made on memory rather than data; this
  phase adds a second feature in the same position.
- **The two-week soak** from Phase 1, still outstanding, now with a hook on the
  prompt boundary as well as the turn boundary.
- **The volume question**, untouched for a fifth phase.
- **No test runs the terminal formatting path** — carried from
  [phase-2.md](phase-2.md) and [phase-3.md](phase-3.md), still true. Phase 4
  adds no terminal formatting, so it neither helps nor worsens it.
- **The recall hook's tail sits close to the budget** — p95 88 ms, a sampled max
  of 101.7 ms against 100 ms. Finding 4 names the lever if it is ever needed.
- **Nothing tests term-mem against a real Claude Code settings.json.** Every
  hook test writes the file itself, so the shapes under test are the shapes this
  project imagined. Five review findings lived in that file, and two were about
  shapes a real user writes and these tests did not.
