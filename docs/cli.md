# term-mem — CLI surface

Status: design sketch. The naming and the shape of the common path are decided;
individual flags are provisional and expected to move.

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
```

Metadata filters carry real weight here. Constraining by where and when you were
collapses the search space before the text query runs, and recovers much of what
would otherwise need semantic search.

### Browsing

The fallback when recall fails — and it will fail, since a query with no
overlapping terms finds nothing:

```
tmem recent                     latest exchanges
tmem log --in <path>            everything from a directory tree
tmem show <id>                  one exchange, in full
tmem show <id> --session        the surrounding conversation (thread, not file)
```

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

These are genuine deletes, including from the search index — never a hidden
flag on a row that stays on disk.

### Data ownership

```
tmem export --json | --markdown
tmem import <path>
```

Export is the concrete form of the mission's ownership promise, and matters more
if the database is encrypted at rest — an encrypted file isn't greppable, so a
guaranteed open-format export is what keeps the data genuinely the user's.

### Setup

```
tmem init
tmem status                     paused? encrypted? how many exchanges?
tmem doctor                     is capture actually wired up?
```

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
  `tmem <query> || ...` works in a script.
- **Snippets, not transcripts.** A response can be hundreds of lines; a result
  list of full responses is unusable. Show the matched region, expand on demand.

## Open questions

- Whether `--in` should default to the current directory when inside a known
  repo, or always default to global.
- Whether recall-and-reuse (feeding a past exchange back into a live session) is
  a `tmem` subcommand or belongs entirely to the assistant-side integration.
- Whether an interactive picker (fuzzy-select over results) is core or a
  separate mode.
