# term-mem — CLI surface

Status: Phases 1 and 2 shipped, so searching, browsing, capture control and
deletion are all now *as implemented* rather than as sketched. Data ownership
(`export`/`import`) and everything downstream of it are still design. Anything
not yet built is marked with the phase that owns it.

## Name

The project is **term-mem**. The binary is **`tmem`**.

Two names doing two jobs — a distinctive, greppable, searchable project name and
a short thing to type — is standard practice (`ripgrep`→`rg`,
`fd-find`→`fd`, `the-silver-searcher`→`ag`). `tmem` is an obvious contraction of
the project name, so the two reinforce each other rather than being unrelated
strings to learn.

Rejected: `tm` (two chars, but widely used as a personal tmux alias, and needs
explaining in every README), `mem` (reads like a memory profiler), `recall`
(self-documenting but back to six characters).

**Install-time collision check.** If `tmem` already resolves to something on the
user's `PATH`, `init` says so and offers an alternative. Silently shadowing an
existing command is hostile, however unlikely the conflict.

## Search is the default verb

Anything that isn't a recognized subcommand is a search query:

```
tmem ffmpeg concat filter
```

Search is ~95% of invocations — capture is automatic and configuration happens
once — so it earns the bare form. Most tools can't do this because they have no
dominant verb; this one does.

Multi-word queries need no quoting. Query terms are already parsed, stemmed, and
OR-ed rather than passed raw to the search index, so joining `argv` costs
nothing and removes a ritual from the hot path.

`tmem search <query>` remains available as the explicit form, for scripts and
for queries that collide with a subcommand name.

A query that matches nothing exits `1` and names the browse commands, rather
than printing an empty list — an empty list is indistinguishable from an archive
that had lost the exchange. A query with no searchable terms in it at all
(`tmem search ---`) exits `2`, because that is a different thing from finding
nothing.

**This constrains the subcommand list.** Every reserved word is a query that
behaves surprisingly. Keep the set small, stable, and made of words nobody
searches for.

## Command surface

### Searching

```
tmem <query>                    search everything
tmem <query> --in <path>        limit to a directory tree
tmem <query> --since <when>     limit by time
tmem <query> --repo             limit to the current git repo
tmem <query> --json             machine-readable, for piping
tmem <query> -n <count>         how many results (default 20)
```

Metadata filters carry real weight here. Constraining by where and when you were
collapses the search space before the text query runs, and recovers much of what
would otherwise need semantic search.

Flags may come before or after the query terms — `tmem backfill --repo --since
january` is the form scenarios.md types, and it works. The cost is that a query
term beginning with `-` needs `tmem search -- <term>`.

Ranking is BM25 with the command lines weighted well above prose. It orders
results *within* a filtered set; it is not a way of telling two similar
exchanges apart, and where two rows match a single term equally the shorter one
wins. See [phases/phase-2.md](phases/phase-2.md) finding 2 — the filters are the
lever, and the ranking is the part the roadmap expects to replace.

### Browsing

The fallback when recall fails — and it will fail, since a query with no
overlapping terms finds nothing:

```
tmem recent                     latest exchanges
tmem log --in <path>            everything from a directory tree
tmem show <id>                  one exchange, in full
tmem show <id> --session        the surrounding conversation (thread, not file)
```

`recent`, `log` and search share the metadata filters — `--in`, `--since`,
`--repo`, `--json`, and `-n/--limit` (default 20); `recent` and `log` differ only
in intent and both order newest first. `--in` matches a directory tree exactly,
so `--in ~/src/api` does not also match `~/src/api-legacy`, and it matches a tree
under every name the filesystem gives it — on macOS `/var/folders/…` and
`/private/var/folders/…` are the same directory, and the archive holds whichever
one the assistant recorded. `--since` takes `2h`, `7d`, `3w`,
`2 hours ago`, `today`, `yesterday`, or `2026-03-01`; anything else is an error
rather than a silent fallback to the epoch, which would return the whole archive
and look like it worked.

`show` accepts any unambiguous id prefix, so `tmem show 01M1QM9B` is enough.
`--session` groups on `thread_id`, not `session_id` — `/clear` starts a fresh
conversation inside the same transcript file, and grouping on the file would
merge unrelated threads.

Browsing by time and place rescues a large fraction of failed searches, and it's
an ordinary query rather than a search problem.

### Controlling capture

Three scopes, because "off" means different things:

```
tmem pause                      global, until resumed
tmem pause 2h                   global, auto-resumes
tmem resume
tmem ignore <path>              this tree, permanently
tmem ignore --list / --remove
TMEM=0 <assistant>              this invocation only
```

Pause and the ignore list are plain files under the data directory rather than
rows in the database, because the capture hook consults both on every turn and
opening SQLite to ask would not fit the latency budget. They are greppable and
hand-editable, like everything else the user owns.

`ignore` affects capture from that point on; it does not retroactively delete.
The command says so, and points at `forget --in` for that.

**Pause state must be visible.** A user who believes it's recording when it's
paused loses work; one who believes it's paused when it's recording gets a nasty
surprise. Surface it — a prompt segment, a notice on assistant start.

Path-based ignore is the one that sees real use: there's usually one directory
whose contents shouldn't be archived even though everything else should.

### Deleting

The real safety valve. People realize *after* the fact that they pasted a
credential or discussed something sensitive:

```
tmem forget --last
tmem forget <id>
tmem forget --since '1 hour ago'
tmem forget --in <path>
```

`--since` and `--in` select a set and cannot be combined with an id or `--last`;
mixing them is an error rather than a guess, because guessing wrong on this
command deletes more than was asked for. The confirmation lists the rows, not a
count — a number is not something a person can check.

`forget` confirms interactively and takes `-y`/`--yes` to skip that. The prompt
is gated on *stdin* being a terminal, not stdout, so `tmem forget <id> | tee log`
still asks. It deletes the row, its mined commands and its file references in one
transaction, checkpoints the WAL and `VACUUM`s, so the text is not recoverable
from a free page — which an integration test checks by grepping the raw database
file afterwards.

It also records the deleted exchange's dedup key, and only that, so the next
ingest of the same transcript does not put it back. `status` shows the count.
The search index needs no mention here because it is maintained by triggers on
the row: a delete that reaches `exchanges` has already reached `exchanges_fts`.

These are genuine deletes, including from the search index — never a hidden
flag on a row that stays on disk.

### Data ownership

*Phase 3.*

```
tmem export --json | --markdown
tmem import <path>
```

Export is the concrete form of the mission's ownership promise, and matters more
if the database is encrypted at rest — an encrypted file isn't greppable, so a
guaranteed open-format export is what keeps the data genuinely the user's.

### Setup

```
tmem init                       create the archive and wire up capture
tmem init --backfill            also import the transcripts already on disk
tmem init --no-hook             do not touch Claude Code's settings.json
tmem status                     paused? encrypted? how many exchanges?
tmem doctor                     is capture actually wired up?
tmem capture --hook <assistant> the Stop hook itself; reads its payload on stdin
tmem capture --drain            process whatever the hook queued
tmem capture --path <file>      ingest one transcript, synchronously
tmem capture --all              ingest every transcript on disk
```

`init` edits the `Stop` hooks in `~/.claude/settings.json` in place, preserving
everything else in the file, and is idempotent. It does not ask the encryption
question yet — encryption is Phase 3 — and `status` says so rather than leaving
the field blank.

The `capture` verb is the one addition to the surface this document sketched. It
is not really a user command; it is the hook's entrypoint, and it is documented
because `doctor` names it and because `--path` is how anyone debugs an adapter.
`--hook` writes a queue entry and spawns a drainer rather than parsing anything,
which is what keeps the turn boundary under 5 ms.

`init` creates the database, wires up capture, asks the encryption question, and
prints what it is about to start recording. A tool that silently begins
archiving everything you type is one people uninstall in anger.

`init` also offers to **import existing assistant transcripts** where they're
available on disk. Starting with months of searchable history rather than an
empty database is the difference between proving the tool on day one and asking
for faith for six weeks.

## Output conventions

- **Human by default, machine on request.** Results are a scannable list —
  the prompt as the title, a highlighted snippet of the match. `--json` for
  piping.
- **Detect a pipe.** No color, no pager, one record per line when stdout isn't a
  terminal.
- **Exit codes carry meaning.** `0` found, `1` nothing found, `2` error — so
  `tmem <query> || ...` works in a script. `doctor` uses `2` for "capture is not
  wired up", and `status` uses `2` for "no archive yet".
- **Snippets, not transcripts.** A response can be hundreds of lines; a result
  list of full responses is unusable. Show the matched region, expand on demand.

## Open questions

- ~~Whether `--in` should default to the current directory when inside a known
  repo, or always default to global.~~ **Resolved: global.**
  [scenarios.md](scenarios.md) argues an implicit `--in .` breaks scenario 2 and
  breaks it silently, and Phase 1 implemented it that way. `--repo` is the
  opt-in for "here".
- Whether recall-and-reuse (feeding a past exchange back into a live session) is
  a `tmem` subcommand or belongs entirely to the assistant-side integration.
- Whether an interactive picker (fuzzy-select over results) is core or a
  separate mode.
