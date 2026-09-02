# term-mem — Mission

## The problem

Terminal AI assistants are amnesiac. Every session starts from zero. The answer
that unblocked you three weeks ago — the exact `ffmpeg` incantation, the reason
you chose one migration strategy over another, the debugging path that finally
worked — is gone the moment the pane closes or the scrollback rolls over.

People compensate badly: screenshots, scratch files, pasting answers into notes
apps, or simply asking the same question again and getting a slightly different
answer. The knowledge exists, it just isn't retrievable.

## What term-mem is

term-mem is a local memory layer for terminal AI conversations. It captures the
questions you ask and the answers you get, stores them in a database that lives
on your own machine, and makes that history searchable and recallable later.

Three things, in order of importance:

1. **Capture** — every exchange (prompt, response, and the context that makes it
   meaningful: working directory, timestamp, which tool answered) is recorded
   without you having to remember to record it.
2. **Recall** — you can find a past exchange by what you remember about it, not
   by where it was filed. Fuzzy, semantic, and plain keyword search over your
   own history.
3. **Reuse** — retrieved memory can be fed back into a new conversation, so the
   assistant starts with what you already established rather than re-deriving it.

## Principles

**Local and private, without qualification.** The database is a file on the
user's disk. No account, no sync service, no telemetry, no "anonymized usage
data." Nothing leaves the machine unless the user explicitly exports it. This is
not a default that can be flipped later — it's the premise of the product. A
terminal history contains API keys, internal hostnames, client names, and
half-formed thinking; it is among the most sensitive data a developer produces.

**The user owns the data, in a form they can actually use.** Plain, documented
storage the user can open, query, back up, grep, or delete with ordinary tools.
No proprietary lock-in, no opaque blob. Deleting a memory means it's gone.

**Invisible until wanted.** Capture must cost nothing in attention or latency.
If remembering requires a ritual, people stop doing it and the archive dies. The
tool should feel like the terminal already worked this way.

**Recall beats storage.** An archive nobody can search is a landfill. Design
decisions favor retrieval quality over completeness of capture.

**Terminal-native.** Keyboard-driven, pipe-friendly, composable with the tools
already in the user's workflow. It reads and writes text; it does not want to be
a web app.

## Who it's for

Developers, sysadmins, data folks, and researchers who live in a terminal and
use an AI assistant there — and who have already had the experience of knowing
they solved this exact problem before, and not being able to find how.

## What success looks like

A user, months in, types a half-remembered fragment of a question and gets back
the conversation that answered it — along with enough context to know why they
asked. Over time the archive becomes a personal, private record of how they
actually think and work: something no hosted service holds a copy of.

## Explicit non-goals

- **Not a team or shared knowledge base.** Single-user, single-machine by
  design. Collaboration features would compromise the privacy premise.
- **Not a cloud service.** No hosted component, no sync backend, no accounts.
- **Not an AI assistant itself.** term-mem remembers conversations; it does not
  replace the tool having them.
- **Not a general note-taking app.** Its subject is AI conversations in the
  terminal, not arbitrary user content.
- **Not a shell history replacement.** Commands you ran are a different thing
  from questions you asked; `atuin` and friends already do that well.

## Open questions

Deliberately unresolved at this stage — they shape the design but don't belong
in the mission:

- How conversations are captured across different assistants and terminal setups.
- Where the line sits between automatic capture and user curation.
- How much of recall can work offline and locally versus needing embeddings.
- What secret-redaction on capture should look like, given the sensitivity above.
