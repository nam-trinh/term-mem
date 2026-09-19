# Phase 3 — Findings

Built 2026-09-19 against [plan.md](../plan.md)'s Phase 3 scope: redaction
pre-write, the `redacted` flag, a user rule file, deletion audited end to end,
opt-in encryption at rest, and `export`/`import`. This is "the phase that earns
the privacy claim in the mission".

**Headline: two of the six scope items did not survive contact, in opposite
directions.** The entropy fallback works exactly as specified and had to be
turned off, because on a real archive it was wrong every single time it fired
and each wrong answer destroyed a command line permanently. Encryption at rest
is not shipped at all, because the cipher was never the hard part — the key
management is, and no arrangement of it survives an unattended capture hook
without becoming theatre.

The other four shipped, and the Exit criterion is met for the path it covers.

## Sample

```
6 transcripts, 5.3 MB, 2026-09-02 .. 2026-09-19, 37 exchanges
credential scan:  0 matches for sk-, ghp_, AKIA, JWT, Bearer, PEM, xox-, AIza
                  35 matches for an email address
```

Same machine, same entrypoint, fourth phase running. The standing caveat is now
load-bearing rather than boilerplate: **this archive has never contained a
credential, so nothing here measures whether the rules catch one.** Every
true-positive claim below rests on synthetic fixtures. Every false-positive
number is real.

That asymmetry is the whole of finding 2.

| Exit clause | Result |
| --- | --- |
| a paste-a-token test, adversarially, leaves nothing recoverable | **yes**, for tokens with a known shape — `tests/redaction.rs` greps every byte term-mem wrote |
| …for a secret no rule knows | **only via `forget`**, which is tested the same way and passes |

## 1. Phase 0's credential scan still holds, and the only leak is PII

Phase 0 scanned two days of archive and found nothing. Seventeen days and
5.3 MB later, the result is unchanged: zero credentials of any shape, and 35
email addresses.

The email is the author's own, and it does not arrive by being pasted. It
arrives because the assistant's own context block contains it, which means
**term-mem archives it once per session regardless of what the user does.** That
is the one empirically-attested leak in four phases of real data, and it is not
a credential.

An `email` rule is implemented and **off by default**: a user's own address is
not a secret from them, and redacting every address mangles ordinary discussion
about ordinary mail. `[builtin] email = true` in the rule file turns it on. The
finding worth carrying is smaller and sharper than "add a rule": the tool's own
prompt scaffolding is a source of archived PII, and no amount of pattern
matching addresses the cause.

## 2. The entropy fallback was wrong 38 times out of 38, and ships disabled

This is the phase's real result, and it is a departure from the Scope.

[plan.md](../plan.md) asks for "Gitleaks-style pattern rules plus a
Shannon-entropy fallback". Both are implemented. The fallback ran against the
real archive three times, once per tightening:

| Rule | Hits | True positives | What it actually matched |
| --- | --- | --- | --- |
| entropy, as first written | 38 | 0 | `S=/private/tmp/claude-501/…/4d827016-5b15-…/scratchpad` — absolute paths |
| + reject path-*prefixed* values | 8 | 0 | `f="tests/fixtures/claude_code/finding-09-many-to-one.jsonl"` — relative paths |
| + reject any value containing `/` | 3 | 0 | `f=4d9e79b1-108c-4b7e-b4e1-4545e43765b9.jsonl` — UUID filenames |

Each tightening is curve-fitting to one archive, and the fourth would have been
too. The values are long, mixed-case, digit-bearing and genuinely
high-entropy — a UUID *is* random data. Nothing about the shape distinguishes a
path from a secret, because there is nothing to find.

What turns this from an annoyance into a defect is what a false positive costs.
`tool_use` blocks are mined and discarded at capture ([phase-1.md](phase-1.md)
finding 7), so the mined command line is the only copy there will ever be.
Replacing it with `[redacted:entropy]` is **permanent destruction of captured
content** — and the roadmap's first principle is that capture is irreversible
while retrieval is not. A rule that has never once been right, and whose every
wrong answer is unrecoverable, does not belong on by default.

So it ships **off**, behind `[entropy] enabled = true`, fully implemented and
tested. The pattern rules ship on: they are precise, and across the whole real
archive they fire zero times, which is the correct answer for an archive with no
credentials in it.

**The honest caveat, stated because it cuts the other way:** this archive has
never held a credential, so the false-positive rate is measured and the
true-positive rate is not. If a later archive shows the entropy rule catching
something real, this decision should be revisited — the evidence for it is
one-sided by construction.

## 3. Encryption at rest is not shipped, and the cipher is not why

The dependency works. A scratch build of `rusqlite` with
`bundled-sqlcipher-vendored-openssl` compiles in 57 seconds, and a database
written through it has no plaintext and no `SQLite format 3` header. That was
the easy half and it is done.

The hard half is the key, and it does not work:

- **The capture hook runs unattended on every turn.** It enqueues rather than
  opening SQLite, but the drainer it spawns does open the database, detached and
  with nobody at the keyboard. Whatever key that process can read, it reads
  without a human.
- **So the key must sit where an unattended process can reach it** — a file in
  the data directory, or an environment variable. Both live on the same machine,
  under the same user, as the database.
- **Which means the encryption protects against exactly one thing**: the `.db`
  file being separated from its key. A backup of `~/.local/share/term-mem/`
  takes both. Cloud sync of that directory takes both. Local compromise takes
  both.

The remaining threat model — a copied file, a stolen disk imaged without the
keyring — is real but narrow, and a keyfile beside the database does not even
cover it. An OS keychain would, and is platform-specific work with a per-open
latency cost that the 5 ms hook budget has an opinion about.

The alternative was to ship it anyway and let `status` print `encrypted yes`.
That is the specific thing this project exists not to do. A claim the user
cannot check, protecting against a threat the design does not actually stop, is
worse than no feature.

**What ships instead is the half that was contingent on it.** plan.md says
encryption "is also the moment `export` stops being a nicety: an encrypted file
isn't greppable, so the open-format export is what keeps the ownership promise
true." With no encryption, the file *is* greppable — so `status` says so, in
those words, rather than leaving the line blank:

```
  encrypted   no    (the file is readable with sqlite3 and grep — `tmem export` if you want it elsewhere)
```

## 4. `import` is a second door, and `forget` has to hold it shut

`export`/`import` adds the first new write path since Phase 1, and it arrives in
the same phase as "deletion audited end to end". The two meet immediately: a
user who exports, forgets something, and re-imports the backup would otherwise
undo the delete.

The `forgotten` tombstone table already existed for the transcript case
([phase-1.md](phase-1.md) finding 9), and import consults it. A restored backup
comes back minus anything the user deleted, and says so:

```
imported 18 exchange(s)
  1 left out because you forgot them — `tmem status` counts these
```

Two smaller decisions fell out of it:

- **Import redacts on the way in.** It is a write path, and pre-write means
  every write path. An export may predate a rule the user has since added.
- **`export` ignores the browse default of 20.** `-n` is honoured when given
  explicitly, but "export" defaulting to the first page would hand someone a
  twentieth of their history and call it a backup. This required making `limit`
  an `Option` so the code can tell "the user asked for 20" from "nobody said".

## 5. The deletion test had to grep the directory, not the file

plan.md says "grepping the raw database file after a `forget`". The file is not
enough. SQLite in WAL mode keeps recently-written pages in `memory.db-wal`, a
separate file, and a secret sitting there is a secret on disk by any honest
reading.

`forget` already checkpointed the WAL before vacuuming, so this was correct
before it was tested — but the test as specified would have passed without
checking. The test now reads **every byte of every file in the data directory**
and fails if the secret appears in any of them.

One thing the test explicitly does not cover: the source transcript. The
assistant's own `~/.claude/projects/*.jsonl` still contains whatever was pasted,
and `tmem forget` neither does nor should delete the user's other files. The
tombstone is what stops the next ingest re-importing it. This is worth stating
because "tmem forget removed the secret" is true of term-mem's archive and false
of the machine.

## 6. Budgets: unmoved

```
Stop hook (60 samples, release):
  12-record transcript     p50 2.17 ms   p95 2.65 ms
  8 MB transcript          p50 2.07 ms   p95 2.87 ms      (budget 5 ms)

Search, 100,000 exchanges:
  common two-term          p50 18.39 ms  p95 19.32 ms
  rare term                p50  3.07 ms  p95  3.18 ms
  filtered                 p50 13.55 ms  p95 13.81 ms     (budget 100 ms)
```

Both are release numbers, and as of this phase only a release build asserts on
them: `cargo test` still runs the measurement and prints it, but a debug binary
is ~3x slower — search p95 ~40 ms and hook p95 ~4.6 ms against a 5 ms
budget — so enforcing there was a gate that failed whenever the machine was
busy. It failed once on `main` immediately after this phase merged, which is
how it was found.

Redaction is in the ingest path, not the hook, so the turn boundary is
untouched by construction. Its own cost shows up in the budget suite's archive
build: 12 s before this phase, 15 s after, for 100,000 exchanges — roughly
**30 µs per exchange** for thirteen pattern rules. No budget covers ingest
throughput, and on this evidence none needs to.

## 7. A review found five more, and the first one broke the Exit criterion

1. **Mined file paths were not redacted.** `redact_exchange` covered the
   prompt, the response and the command lines — and said so in a comment
   claiming it covered "every field that carries text". It did not cover
   `ex.files`. A `Read` of `/home/dev/secrets/ghp_….pem` put the credential in
   `file_refs`, in the raw database bytes and in every export, with `redacted`
   left at 0. Verified against the built binary before fixing.

   Finding 5 above congratulates this phase for testing the *directory* rather
   than the file. It did — and then the Exit criterion failed anyway, on a
   field the test never fed a secret into. Widening where you look does not
   help if you never put the secret there.

2. **`url-credentials` matched its own replacement.** Its password class
   allowed `:`, so `[redacted:url-credentials]` matched the rule that produced
   it. The text converged, so the output was right, but `scrub` rewrote the
   same bytes until the 10,000-iteration guard tripped — ten thousand full-text
   regex scans per exchange on the ingest path, and a report reading `×10001`.
   Fixed in the pattern; `scrub` also gained a guard, because a *user* rule can
   do the same and no shipped regex controls those.

3. **The tests read the developer's real `~/.config/term-mem/redact.toml`.**
   `Env::cmd()` set `TMEM_HOME` and the transcript paths but not
   `TMEM_CONFIG_DIR` or `HOME`. A local `[entropy] enabled = true` failed one
   test and made several others pass for the wrong reason. The suite that
   proved this phase depended on the machine it ran on.

4. **`doctor` said "capture looks healthy" while capture was dead.** A rule
   file that will not compile aborts ingest, which is correct — but the drainer
   is detached with its stderr discarded, so the user sees capture stop and
   nothing tell them why. `doctor` now loads the ruleset and reports it. This
   is the same defect as [phase-1.md](phase-1.md) finding 9's item 8, which was
   `doctor` printing a problem and then declaring health, one release later in a
   different check.

5. **`status` claimed a file it could not read was plaintext.**
   `encryption_status` read sixteen bytes and `unwrap_or(true)` on failure, so
   an unreadable archive printed `encrypted no (readable with sqlite3 and
   grep)` — a statement about bytes it had just failed to read. In a phase whose
   entire subject is not overclaiming about what is on disk.

Each fix has a regression test confirmed to fail against the unfixed code. Two
of the first drafts did not: an integration test for the self-matching rule
passed because the *pattern* fix alone covered it, and one for `status` passed
because `status` opens the database before it reaches the header check, so a
corrupt file failed earlier. Both were replaced with unit tests that reach the
defect. A test that cannot fail is worse than no test, because it is counted —
the lesson of [phase-2.md](phase-2.md) finding 8, arrived at again by a
different road.

## Verdict

**Phase 3 ships four of six scope items, after a review pass that found five
more defects — one of which broke the Exit criterion this document had already
claimed was met (finding 7).** The two unshipped items are findings rather than
omissions. Pattern-rule redaction pre-write, the `redacted` flag
with counts in `capture` and `status`, the user rule file, the audited deletion
path, and `export`/`import` are all in. The entropy fallback is implemented and
off. Encryption at rest is not implemented.

The Exit criterion — "a paste-a-token test, performed adversarially, leaves
nothing recoverable in the database file" — **passes for a token with a
recognisable shape**, verified by grepping every byte term-mem wrote. For a
secret no rule knows, prevention does not fire and `forget` is the valve, which
is what plan.md says it is for; that path is tested the same way and passes.

What was surprising, in order:

1. That the entropy fallback's first contact with a real archive was 38 false
   positives and zero true ones, and that three rounds of tightening never once
   found a credential — because there was none to find.
2. That a redaction false positive is permanent data loss, which makes
   "redact aggressively by default" contradict the roadmap's first principle.
   The two rules were written eight weeks apart and nothing had put them in the
   same sentence before.
3. That SQLCipher was the easy part, and that every key arrangement an
   unattended hook can use puts the key next to the thing it protects.
4. That the one leak four phases of real data actually show is an email address
   the tool's own context block injects, which no redaction rule addresses the
   cause of.
5. That `export` inheriting the browse default of 20 would have silently handed
   someone a twentieth of their archive as a backup.
6. That the deletion test was widened to read the whole data directory and the
   Exit criterion still failed — on a field no test had put a secret into
   (finding 7).

## Carried forward

- **Encryption at rest**, unshipped, with the key-management analysis above. It
  needs an OS keychain path and a decision about what the hook does when the key
  is unavailable — refusing to capture is data loss, capturing unencrypted is a
  lie. Neither is obviously right and the docs do not choose.
- **The entropy rule's true-positive rate is still unmeasured**, and cannot be
  measured on an archive with no credentials in it. A deliberate test against a
  synthetic corpus of real-shaped secrets would settle whether the rule earns
  being on by default.
- **The tool's own context block is a PII source.** The `email` rule treats the
  symptom.
- **The two-week soak** from Phase 1, still outstanding, and now with redaction
  in the write path.
- **The volume question**, untouched for a fourth phase.
- **No test runs the terminal formatting path** — carried from
  [phase-2.md](phase-2.md), still true.
