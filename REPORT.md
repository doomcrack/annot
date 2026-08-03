# annot — build report

Built per PROMPT.md via orchestrated subagent waves. Final state: `cargo test --workspace`
120 passed / 0 failed, `cargo clippy --workspace --all-targets -- -D warnings` clean,
`cargo fmt --check` clean, `scripts/e2e.sh` ⇒ E2E PASS.

## What was built

- `crates/annot-core` — `model.rs` (Record/Anchor/Kind/Status, serde schema matching the
  spec field-for-field, hand-rolled RFC 3339 ms timestamps), `store.rs` (JSONL mirror
  store under `.annot/`, shared-lock appends, exclusive-lock atomic compaction,
  thiserror `StoreError`), `sync.rs` (blob anchoring with a content-addressed
  `.annot/blobs/` cache + gix ODB read fallback, imara-diff Histogram
  remap, integer basis-point fuzzy re-match 0.70/0.15/0.15 with accept ≥0.60 (≥0.80 for
  ≤2-line anchors), orphaning, opportunistic write-back via `sync_path`/`sync_tree`).
- `crates/annot-cli` — binary `annot`: add/get/sync/orphans/resolve/compact, deterministic
  context formatter (token cap, kind policy, delimiter neutralization), json format.
- `tests/` — api_pins (external-crate API contracts), store/model units, 24 sync-engine
  tests, 9 end-to-end drift/persistence tests, 31 black-box CLI tests, fixture templates
  (`tests/fixtures/`) + `FixtureRepo` harness.
- `hooks/` — PostToolUse scripts (context injection via `hookSpecificOutput.additionalContext`,
  silent-degradation design), a Stop-hook repo-wide sync catch-all, mergeable
  `settings.json` snippet, README with install + limitations. `skill/SKILL.md` —
  authoring discipline, kind guide, promotion policy. Records/anchors round-trip
  unknown JSON fields (`#[serde(flatten)]`), so older binaries cannot destroy newer
  data on `compact`.
- `scripts/e2e.sh` — the Definition-of-done walkthrough (shift + orphan cases).

Two adversarial Opus review passes ran (sync engine; CLI/hooks/DoD). All confirmed
findings were fixed with regression tests: degenerate-anchor panic, an extra accept gate
narrowing the contract's re-match region, `resolve --reanchor` dropping `anchor.symbol`,
context-delimiter spoofing, `sync <dir>` false success, token-budget separator overshoot.

## Deviations from spec (with reasons)

1. **Appends take a shared fs4 lock** (spec: "plain appends need no lock") — user-approved.
   A lockless append racing compaction's rewrite+rename is silently lost; the shared/
   exclusive split closes the window at one syscall per append.
2. **Old content is persisted in a content-addressed cache at `.annot/blobs/<oid>`**
   (initially shipped as loose-blob writes into the git ODB, user-approved; reworked on
   review to keep annot writing only inside its own directory). The old content must be
   retrievable for diffing even when never committed (dirty worktree). The cache is
   gc-independent, written atomically at anchor/heal time, pruned by `annot compact`
   (never when any mirror is malformed), and the git ODB serves as a read-only fallback
   for content that happens to be committed. `base_blob` remains a standard git blob
   SHA-1, so the record schema is unchanged.
3. **`orig_snippet` is captured eagerly** at anchor time and refreshed on heal (spec:
   "stored when orphaned") — the only variant robust to gc-pruned blobs; orphaning still
   preserves it, satisfying the spec's intent.
4. **"Rewrite record" = append a superseding same-id record**; `load` is last-wins,
   `compact` merges. The spec's literal in-place rewrite contradicts its own append-only
   storage mandate.
5. **Rename ⇒ orphan** (`FileMissing`); no mirror relocation in MVP. The spec's 4-step
   sync has no rename mechanism; re-homing is via `resolve --reanchor <file>:<s>:<e>`.
6. **Hook wiring differs from the spec's literal snippet**: current Claude Code docs
   (verified live) do not inject PostToolUse stdout; the scripts wrap `annot get` output
   in `hookSpecificOutput.additionalContext` JSON. Prerequisites: bash + jq. The spec's
   literal command would silently no-op.
7. **`tests/fixtures/` holds text templates** materialized into temp git repos at test
   time; runnable tests live inside the crates (nested `.git` dirs can't be committed;
   a root `tests/` dir is not a cargo target).
8. **Surface extensions**: `add --symbol`; `resolve --reanchor [<file>:]<start>:<end>`
   (file prefix re-homes orphans of renamed files — the spec's file-less grammar could
   not repair its own rename scenario); `resolve` echoes `reanchored/dropped <id>`;
   `compact` reports and exits 1 when malformed mirrors were skipped.
9. **Context output neutralizes `<annot`/`</annot` sequences in bodies** (`&lt;`-escape)
   and quotes in the `file` attribute — a shared `.annot/` mirror must not be able to
   forge an `untrusted="false"` block into model context. Spec was silent on escaping.
10. **DoD "get returns an orphan"** is satisfied via `--format=json` (includes orphans)
    plus `annot orphans`; context format excludes orphans by design (it feeds agent
    context; orphans are unplaceable).
11. **Token cap** = ceil(bytes/4) over the full emitted stream (separators included);
    budget selection is kind-priority (gotcha > decision > todo > history) then
    positional, emission always positional. Deterministic, enforced in code.
12. Repo `CLAUDE.md` references PROMPT.md (the spec's companion text pointed at a
    nonexistent filename). Dev-dependencies `tempfile` + `assert_cmd` approved.

## Known gaps

- Fuzzy re-match models internal drift as ONE contiguous gap (±2 lines): two separate
  insertions inside an anchor undercount matches and tend to orphan rather than
  mis-anchor — conservative by design, but real moves with scattered internal edits
  orphan more than strictly necessary.
- Sync triggers on tools matched by `Edit|Write|MultiEdit|NotebookEdit` plus a Stop-hook
  repo-wide catch-all; a writing tool outside that set drifts only until the next Stop.
- O_APPEND atomicity assumed (local filesystems; NFS caveat).
- "Periodic" compaction is manual (`annot compact`; the Stop hook syncs but does not
  compact).
- `.annot/blobs/` grows with anchors and heals between compactions.
- `symbol` is pass-through only. CRLF files keep a dangling `\r` on the snippet's last
  line. `hunk_cache` is a linear scan (O(n²) for many-annotation files).
  `Syncer::sync_file`/`make_anchor` trust `rel_path` (the CLI path goes through
  `Store::source_rel`).
- Hook-snippet verification is pipe-level smoke testing; a live Claude Code session
  cannot be exercised from inside Claude Code — stated, not faked.

## Suggested v2

tree-sitter symbol anchors (auto-populate + verify `symbol`); MCP frontend;
`annot compact` summarization of history records; rename re-homing via content-similarity
mirror relocation; `--no-heal` read-only mode for CI checkouts; gap tolerance
proportional to anchor size (max(2, k/10)) and a multi-gap alignment model.
