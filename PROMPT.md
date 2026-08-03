# Orchestrator kickoff prompt — `annot`

> Paste everything below this line as the initial task for the orchestrator session.
> Replace MODEL_STRONG / MODEL_FAST with the model strings your Claude Code version accepts.

---

## Role

You are the **orchestrator** for building `annot`, a Rust tool that stores line-anchored,
agent-oriented annotations outside a codebase (in `.annot/`). You plan, decompose,
delegate to subagents via the Task tool, review their output, and integrate. You do NOT
implement large components yourself — your context is reserved for planning, review, and
integration decisions. Small glue fixes (<20 lines) during integration are fine.

## Model routing

Choose the subagent model per task:

- **MODEL_STRONG** (e.g. Opus-tier): anchor/sync algorithm implementation, public API
  design of `annot-core`, code review passes, debugging failed integrations.
- **MODEL_FAST** (e.g. Sonnet-tier): CLI wiring, serde types, test scaffolding, fixtures,
  hook scripts, docs, mechanical refactors.

When in doubt: correctness-critical or design-shaping → MODEL_STRONG; well-specified and
mechanical → MODEL_FAST.

## Project spec (authoritative — pass relevant sections verbatim to subagents)

**Purpose.** Claude Code writes verbose decision-context that is useful to agents but is
slop in a codebase. `annot` stores this context in `.annot/`, anchored to code locations,
injected into agent context at read time, and kept in sync as code drifts.

**Workspace layout.**

```
annot/
  Cargo.toml            # workspace
  crates/
    annot-core/         # lib: storage, anchors, sync engine
    annot-cli/          # bin: thin frontend over annot-core
  hooks/                # Claude Code hook scripts + settings snippet
  skill/                # authoring-side skill (SKILL.md)
  tests/fixtures/       # git repos as fixtures for drift scenarios
```

**Storage.** `.annot/` mirrors the source tree: `src/parser.rs` → `.annot/src/parser.rs.jsonl`.
Append-only JSONL, periodic compaction (`annot compact` merges duplicate ids, drops
tombstones). Record schema:

```json
{
  "id": "<ulid>",
  "kind": "decision | gotcha | todo | history",
  "body": "<text>",
  "anchor": {
    "base_blob": "<git blob oid>",
    "start": 141, "end": 158,
    "line_hashes": ["..."],
    "ctx_before": "<hash>", "ctx_after": "<hash>",
    "symbol": "fn parse_expr"        // optional in MVP
  },
  "status": "live | orphaned | tombstone",
  "orig_snippet": "<stored when orphaned>",
  "created_at": "...", "updated_at": "..."
}
```

**Sync engine (lazy, at read time).**
1. Current blob OID == `base_blob` → positions valid, return.
2. Else diff old blob vs new (imara-diff, blobs via gix). Hunks fully above anchor →
   shift by net delta; fully below → ignore.
3. Hunk overlaps anchor → content re-match using line_hashes with fuzz, weighted by
   ctx hashes. High confidence → re-anchor, rewrite record. Low → `status: orphaned`,
   preserve `orig_snippet`.
4. Write healed anchors back opportunistically.

**CLI surface.** `annot add <file> <start>:<end> --kind K -m "body"`,
`annot get <file> [--format=context|json] [--kinds decision,gotcha] [--max-tokens N]`,
`annot sync [path]`, `annot orphans`, `annot resolve <id> (--reanchor <range> | --drop)`,
`annot compact`. `--format=context` wraps each annotation in
`<annot untrusted="true" id=".." file=".." lines=".." kind="..">` delimiters and enforces
the token cap and kind policy deterministically (never rely on the model for budgeting).

**Concurrency.** Advisory file locks (fs4) around compaction; plain appends need no lock.

**Crates.** gix, imara-diff, serde/serde_json, ulid, clap, fs4, thiserror. anyhow in the
CLI only. No tree-sitter, no MCP in MVP.

**Claude Code integration deliverables.**
- `hooks/settings.json` snippet: PostToolUse(Read) → `annot get <file> --format=context
  --kinds decision,gotcha --max-tokens 800`; PostToolUse(Edit|Write) → `annot sync <file>`.
- `skill/SKILL.md`: authoring discipline — record decision-context via `annot add`
  instead of inline comments; kind selection guide; when an annotation earns promotion
  to a real code comment (stable, human-relevant invariants) vs staying external.

## Workstreams (delegate in this order)

1. **Core types + storage** (MODEL_FAST): record/anchor serde types, JSONL read/append,
   compaction, locking. Owns `annot-core/src/{model,store}.rs`.
2. **Anchor + sync engine** (MODEL_STRONG): blob access via gix, diff remap, fuzzy
   re-match, orphaning. Owns `annot-core/src/sync.rs`. Depends on 1's types — hand it
   the exact type definitions from 1's output.
3. **CLI** (MODEL_FAST): clap commands over annot-core, context formatter with token
   cap. Owns `annot-cli`. Depends on 1 + 2's public APIs.
4. **Drift test suite** (MODEL_STRONG for scenario design, MODEL_FAST for scaffolding):
   fixture repos exercising: edit above anchor, edit below, edit inside (re-matchable),
   edit inside (orphaning), file rename, function moved within file.
5. **Hooks + skill** (MODEL_FAST): scripts, settings snippet, SKILL.md. Depends on 3's
   CLI surface being final.

1 and 4-scaffolding can run in parallel. 2 blocks 3 blocks 5.

## Subagent delegation rules

- Subagents are stateless: every delegation must include the spec sections they need,
  the exact public interfaces they consume, the files they own, and acceptance criteria.
  Never say "as discussed" to a subagent.
- One workstream = one subagent = exclusive file ownership. No two concurrent subagents
  touch the same file.
- Require each subagent to end by running `cargo build -p <crate> && cargo test -p <crate>`
  and reporting results. Reject handoffs with failing builds.
- After workstreams 2 and 3, dispatch a MODEL_STRONG review subagent with a diff-only
  brief: check the sync algorithm against the spec's four steps, check the token cap is
  enforced in code, check orphaning preserves `orig_snippet`.

## Code standards (include in every delegation)

- Comments: minimal, only for non-obvious invariants. NO narrative comments explaining
  what was tried or why a decision was made — that is exactly the slop this tool
  externalizes. Doc comments on public API items only.
- `cargo clippy -- -D warnings` clean. rustfmt default.
- Errors: thiserror enums in annot-core; anyhow at the CLI boundary.

## Definition of done

- `cargo test --workspace` green, clippy clean.
- End-to-end script: init fixture repo → `annot add` → mutate file above/inside anchor →
  `annot get` returns correctly shifted annotation in one case and an orphan in the other.
- Hook snippet verified by manual walkthrough in the report (you cannot run Claude Code
  inside itself — state this limitation rather than faking verification).
- Final report: what was built, deviations from spec (with reasons), known gaps,
  suggested v2 items (tree-sitter symbol anchors, MCP frontend, `annot compact`
  summarization).

Begin by writing a short build plan (sequence + model per workstream), then start
delegating. Do not ask me to confirm the plan unless you find a contradiction in this
spec.

---

## Companion note for CLAUDE.md (optional, add to the repo)

```md
# annot (this repo builds it)
- Orchestrator pattern: plan/delegate/review only; implementation goes to subagents.
- No narrative/slop comments anywhere in this codebase — dogfood the philosophy.
- Spec lives in annot-orchestrator-prompt.md; it is authoritative over memory.
```
