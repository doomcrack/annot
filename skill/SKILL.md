---
name: annot-authoring
description: Use when about to leave decision rationale, a non-obvious hazard, deferred work, or historical context in a codebase during or after editing code — records it as an anchored annot annotation instead of a narrative code comment, and covers picking the right kind, when to promote an annotation into a real comment, and maintaining annotations as code drifts (orphans, reanchoring, compaction).
---

# annot authoring

`annot` stores agent-oriented context — the stuff that's useful to an agent
working in this codebase but is slop when left as a comment for human
readers — outside the source tree, anchored to line ranges, and re-injected
at read time. That's the whole point of the tool: **narrative comments
explaining what you tried, why you chose an approach, or what to watch out
for are exactly the noise this tool exists to externalize.** If you catch
yourself about to write `// we do it this way because...` or
`// NOTE: careful, this breaks if...` in code, that's an `annot add`, not a
comment.

## Installation

This skill directory isn't picked up automatically — copy or symlink it into
the target repo as `.claude/skills/annot-authoring/SKILL.md`, per Claude
Code's skill conventions:

```sh
mkdir -p .claude/skills
ln -s /path/to/annot/skill .claude/skills/annot-authoring
```

## Kind selection

Four kinds, each with a distinct job. Pick the narrowest one that fits.

- **`decision`** — why this approach was chosen over the alternatives.
  Example: `annot add src/parser.rs 40:78 --kind decision -m "Recursive
  descent over a Pratt parser: precedence table would be premature here,
  the grammar has 4 operators total."`

- **`gotcha`** — a non-obvious hazard or invariant an agent must not trip
  over; something that will cause a bug if violated silently. Example:
  `annot add src/cache.rs 12:20 --kind gotcha -m "evict() must run inside
  the same lock acquisition as insert() — releasing between them lets a
  reader observe a torn state."`

- **`todo`** — known, deliberately deferred work, distinct from a hazard —
  nothing breaks today, but this is incomplete. Example: `annot add
  src/auth.rs 88:88 --kind todo -m "token refresh doesn't handle clock skew
  across nodes yet; fine for single-node deploys only."`

- **`history`** — what changed and why it will matter to someone (or some
  agent) reading this code later, when the change itself isn't visible in
  the diff anymore. Example: `annot add src/retry.rs 5:30 --kind history -m
  "Backoff base went from 100ms to 500ms after the 2024-06 incident where
  the old value caused a thundering herd against a cold cache."`

## Promotion: when context earns a real code comment

Default to `annot add`. Promote to an actual `//` comment only when the
content is:

- **stable** — unlikely to churn with the next few edits (an `annot`
  survives drift better than a comment survives a human forgetting to
  update it, but a comment survives an agent-less read of the file);
- **human-relevant** — every human reader of this code needs it, not just
  an agent operating on it; and
- **an invariant, not a rationale** — "this must hold" reads better as a
  comment than "we chose this because." Rationale belongs in `annot`
  history even after promotion — don't delete the `annot`, just also add
  the comment.

Concretely: a public API's safety contract, a `SAFETY:` comment on unsafe
code, a legal/license notice — promote. "We tried X first but it was too
slow" — stays an `annot`, forever; nobody reading the code six months from
now needs the rejected alternative in their face on every scroll.

## Workflow

```sh
# Record why, right after making the call — anchor the range you're
# actually justifying, not the whole function.
annot add src/sync.rs 55:90 --kind decision \
  -m "Blob-oid fast path before diffing: avoids running imara-diff on
      every read when nothing changed." --symbol "fn make_anchor"

# Read them back the way the PostToolUse(Read) hook does. Each annotation is
# wrapped in an <annot ...>...</annot> block; any "<annot" or "</annot"
# sequence inside a body is neutralized to an "&lt;"-escape first, so an
# annotation's own text can never forge a delimiter and inject a fake block.
annot get src/sync.rs --format=context --kinds decision,gotcha --max-tokens 800

# Full JSON (e.g. to inspect anchor state, not just render text).
annot get src/sync.rs --format=json

# After an edit, resync explicitly (the PostToolUse(Edit|Write) hook does
# this automatically for Edit/Write — do it by hand after anything the
# hooks don't cover, e.g. a Bash-driven codemod or NotebookEdit).
annot sync src/sync.rs
```

## Maintenance

Anchors drift when code changes faster than the fuzzy re-match can follow,
or a hunk overlaps an anchor with too little confidence. Check periodically:

```sh
annot orphans
```

Each orphan keeps its `orig_snippet` so you can see what it used to anchor.
Resolve each one:

```sh
# Re-home it after the code moved but the annotation is still valid.
annot resolve <id> --reanchor 120:145              # same file, new range
annot resolve <id> --reanchor src/new_home.rs:12:30 # moved to another file

# Or it's genuinely stale — drop it (tombstones, doesn't hard-delete).
annot resolve <id> --drop
```

The `file:` prefix on `--reanchor` is how you re-home an annotation after a
rename or a move — without it, the range applies to the record's existing
file. Like every other file argument, that path is resolved cwd-relative
(or absolute) — `src/new_home.rs` above is relative to wherever you run
`annot`, not to the repo root.

Run `annot compact` occasionally (merges duplicate ids left behind by
resolve/append churn, drops tombstones) to keep the `.annot/` mirror lean —
it's append-only JSONL day to day, so it only grows until compacted.

## Caveat: `git gc` degrades anchor healing, never deletes annotations

`annot add`/`resolve` write the anchored file's full content into the git
object database as a loose blob, purely so the sync engine can diff old vs.
new content precisely on the next read. That blob is unreferenced by any
commit or ref, which makes it exactly the kind of object `git gc` prunes.
If it gets pruned, re-anchoring after drift falls back to weaker signal
(the stored line hashes and context hashes alone, no full diff) — lower
confidence, more likely to orphan instead of cleanly re-matching. The
annotation's own text (`body`, `orig_snippet`) lives in the `.annot/`
JSONL mirror, not in the git object database, so `git gc` can never lose
the annotation itself — worst case it orphans and you `--reanchor` it by
hand.
