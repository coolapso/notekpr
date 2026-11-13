---
name: independent-review
description: Have notekpr changes reviewed by an agent of another model family before they're committed — the maintainer's procedure. Use when designing anything that touches keys, trust, encryption, signatures or what the server learns (review the design before writing code), and before committing a feature, a larger fix, or anything touching sync, keys or the editor (code review, then a cryptography review where it applies). Not for small fixes, docs, tests or mechanical changes.
---

# Independent review

Before a change is merged, the maintainer has it reviewed by an agent of
another model family than the one that wrote it: its blind spots differ,
and it isn't anchored on the author's reasoning. This is the maintainer's
procedure. Contributors don't need to run it (the maintainer does, for
their changes too); they may if they have the means.

Reviews are capped: one pass, a second only to re-check the fix of
something serious from the first. Nothing is ever 100 % done: fix what is
critical and reasonable, record the rest, move on.

## When

| What | Review | When |
|---|---|---|
| A change to keys, trust, encryption, signatures, the membership log, authorization, or what the server (or other people) can learn | **Design review**: the plan — the protocol and security docs as drafted, the flows, what each party signs and checks | Before writing the code |
| A feature, a larger fix, anything touching sync, keys or the editor | **Code review** of the change | Before committing |
| The changes in the first row | **Cryptography review** of the change, against [docs/security.md](../../../docs/security.md) | After the code review's fixes, before committing (at the latest before the release that ships it) |

Not for small fixes, docs, tests, formatting or other mechanical changes.

## Who reviews

Never the model that wrote the change. As set up now:

| Review | Reviewer | Effort |
|---|---|---|
| Code | Codex, `gpt-6.1-sol` | `high` |
| Design, cryptography | Codex, `gpt-daybreak-blue-latest` (Daybreak Blue) | `xhigh` |

The maintainer may name others; this table changes with them.

**Starting a review**: call the Codex CLI from your harness's shell. Write
the brief (below) to a file outside the repository, then start one new
session per review, a second pass included; never `codex exec resume` an
earlier one:

```sh
command codex exec -C <repository path> \
  -m gpt-6.1-sol -c model_reasoning_effort=high \
  -s read-only \
  -o "${TMPDIR:-/tmp}/notekpr-code-review.md" \
  - < <brief file>
```

For a design or cryptography review, use `-m gpt-daybreak-blue-latest -c
model_reasoning_effort=xhigh` and name the output file after the review.

- `command` skips a shell alias that adds flags, such as one that changes
  the sandbox. `-s read-only` lets the reviewer read files and run
  commands, but not change anything.
- Run it in the background: a review takes minutes, longer than a
  foreground command may run. If your harness tells you when a background
  command ends, don't poll. The findings are in the `-o` file.
- Work on (tests, docs) meanwhile. A review on code you are still changing
  reads a moving target, so start it once the change is nearly final.

If `codex` is missing or not signed in (`codex login status`), say so to
the maintainer instead of reviewing your own work.

## The brief

The reviewer knows only what the brief says. Give it the code and the
promises, not your conclusions — an agent handed a hypothesis tends to
confirm it.

- Where: the repository path, and how to see the change (`git diff` plus
  untracked files, or a commit range).
- First read AGENTS.md (the hard rules) and, for design and cryptography,
  docs/security.md.
- **Read-only and private**: no edits, commits, pushes, issues, PRs,
  comments or advisories; findings go back to you only (SECURITY.md).
- What changed, in a few lines, and where: the files and the spec sections
  (docs/protocol.md, docs/security.md).
- What to look for, as questions: can a malicious server, a stranger or a
  collaborator do what the docs say they can't? What leaks? What breaks
  for existing data, older apps, agents? Where do web and CLI differ?
- One pass (two at most); report the most critical and reasonable findings
  and stop.
- For each finding: severity, `file:line`, a concrete scenario, a fix, and
  a reproduction where it can (a failing test, or steps).
- Also say what was checked and found to hold.

## After

1. Check each finding: reproduce it, or follow it through the code. A
   reviewer's mistakes read as confidently as its findings.
2. Fix what is critical and reasonable now, with a test; correct docs that
   claim more than holds.
3. What isn't fixed now:
   - **A security finding** (a way around what docs/security.md promises)
     goes, with its fix plan, in the private TODO's security section
     (`TODO:` in `AGENTS.local.md`) — never an issue, a commit message or
     anything public before its fix is released (SECURITY.md). Commit
     messages stay neutral.
   - Anything else becomes a roadmap issue when you work for the maintainer.
   - A choice that changes an agreed design is the maintainer's: ask.
4. In your report to the maintainer: who reviewed, how many passes, what
   each found, what you fixed and what you left (and where).
