# Drift test fixtures

Plain-text templates for `crates/annot-core/tests/drift.rs` (WS4b). Each scenario
test materializes a fresh git repo via `FixtureRepo` (see
`crates/annot-core/tests/common/mod.rs`), writes `base.rs.txt` as the source file,
anchors an annotation at the canonical range below, then overwrites the source
file with the scenario's AFTER template (or renames it, for scenario 5) and runs
`sync_file`.

## Base file

`base.rs.txt` is 59 lines, every content line textually unique across the file
(the only repeats are blank lines and bare `}` / `    }` brace lines, which are
unavoidable in Rust and never coincide within a single anchor span). Function
layout:

| Symbol | Lines |
|---|---|
| `struct Header` | 6-10 |
| `fn compute_checksum` | 12-19 |
| `fn parse_header` | 21-30 |
| `fn encode_payload` (**canonical anchor target**) | 32-42 |
| `fn validate_footer` | 44-51 |
| `fn build_lookup_table` | 53-59 |

**Canonical anchor**: every scenario anchors `fn encode_payload`, `base.rs.txt`
lines **32-42 inclusive** (11 lines: the `fn` line through its closing `}`).

## Scenario table

| # | Scenario | Template file | Canonical anchor range (base.rs.txt, 1-based incl.) | Expected sync outcome |
|---|---|---|---|---|
| 1 | edit above | `edit_above.rs.txt` | 32-42 | `Shifted { delta: +5 }` — new range **37-47**, `line_hashes` byte-identical to original, `base_blob` healed to the new content OID |
| 2 | edit below | `edit_below.rs.txt` | 32-42 | `Refreshed` — range unchanged, still **32-42**, `line_hashes`/`ctx_before` byte-identical; `base_blob` healed to the new content OID (opportunistic heal so the next sync short-circuits at step 1) |
| 3 | edit inside, re-matchable | `edit_inside_rematch.rs.txt` | 32-42 | `Rematched { new_start: 34, new_end: 45, .. }` — new range **34-45** (12 lines: all 11 original anchor lines present in order plus the 1 newly inserted line, healed into the range); high confidence (all 11 line_hashes recovered ⇒ ~1.0, well above 0.60) |
| 4 | edit inside, orphaning | `edit_inside_orphan.rs.txt` | 32-42 | `Orphaned(LowConfidence)` — anchor fields retain the last known **32-42**; `orig_snippet == Some(<original lines 32..=42 of base.rs.txt, \n-joined>)`; best whole-file fuzzy score is context-only (~0.30, since all 11 original lines are replaced with globally-unique new text and only the unavoidable brace lines could coincidentally match), safely under the 0.60 accept bar |
| 5 | file rename | *(none — `FixtureRepo::rename` of the base source file)* | 32-42 | `Orphaned(FileMissing)` — MVP contract: missing source orphans all live records for it; sidecar file still exists; `orig_snippet` preserved from the stored value |
| 6 | function moved within file | `moved_function.rs.txt` | 32-42 | `Rematched { new_start: 12, new_end: 22, .. }` — new range **12-22** (11 lines, byte-identical content, total file length unchanged at 59); confidence ~0.70 (0.70 positional weight from 11/11 line_hashes matching; `ctx_before`/`ctx_after` do NOT match — surrounding functions differ — contributing 0), still above the 0.60 accept bar |

## Per-template notes

- **`edit_above.rs.txt`**: inserts a 5-line comment block after line 1
  (`use std::collections::HashMap;`), a single clean insertion hunk fully above
  line 32. Nothing else changes.
- **`edit_below.rs.txt`**: rewrites `fn validate_footer` and `fn build_lookup_table`
  bodies (different literals/logic, same function signatures) and appends a new
  trailing `fn checksum_is_valid`. All changes start at line 44, strictly below
  line 42; lines 1-42 are byte-identical to `base.rs.txt`.
- **`edit_inside_rematch.rs.txt`**: two independent insertion hunks — 2 lines
  inserted after line 1 (comment), and 1 line (`debug_assert!(...)`) inserted
  inside `fn encode_payload` between the original `let mut rolling = key;` and
  `for &byte in payload {` lines. All 11 original anchor lines are still present,
  in original order, unmodified; the internal insertion overlaps the anchor's
  mapped range so sync must take the fuzzy re-match path rather than a pure
  shift, proving search-at-a-distance (the expected-position hint from the
  2-line above-shift is only a tie-break, not the final answer).
- **`edit_inside_orphan.rs.txt`**: `fn encode_payload` is replaced wholesale by
  an unrelated `fn compress_stream` (different name, different body, zero
  shared line content with the original 11 lines). Surrounding lines (1-31,
  44+ shifted by the block's new size) are otherwise untouched, so
  `ctx_before`/`ctx_after` of the *old* anchor position still resolve against
  real neighboring lines but the positional (line_hashes) component is zero.
- **`moved_function.rs.txt`**: `fn encode_payload` (byte-identical, 11 lines)
  is relocated from position 3 (after `struct Header`, before
  `fn compute_checksum`) to position 1 (immediately after `struct Header`).
  Total file length is unchanged (59 lines): one deletion hunk at the old
  site, one insertion hunk at the new site, net zero.
- **rename** (scenario 5): no template — the test renames the materialized
  `base.rs.txt` source file on disk (e.g. via `FixtureRepo::rename`) and syncs
  the *original* path, which is now missing.

All templates were diffed against `base.rs.txt` and re-verified with `cat -n`
after construction; the line numbers above are read off those diffs, not
computed by hand from the source text.
