mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::FixtureRepo;

use annot_core::model::{Kind, Record, Status};
use annot_core::store::Store;
use annot_core::sync::{self, OrphanReason, SyncError, SyncOutcome, Syncer};

const SRC: &str = "src/lib.rs";
// `fn encode_payload` in `tests/fixtures/base.rs.txt`, per the fixture README.
const ANCHOR_START: u32 = 32;
const ANCHOR_END: u32 = 42;

struct Fixture {
    repo: FixtureRepo,
    syncer: Syncer,
    store: Store,
}

impl Fixture {
    fn new() -> Fixture {
        let repo = FixtureRepo::new();
        let syncer = Syncer::open(repo.root()).expect("fixture repo is discoverable");
        let store = Store::open(repo.root()).expect("fixture repo root opens as a store");
        Fixture {
            repo,
            syncer,
            store,
        }
    }

    fn source(&self, rel: &str) -> PathBuf {
        self.store.repo_root().join(rel)
    }

    fn anchor(&self, rel: &str, start: u32, end: u32) -> Record {
        let new_anchor = self
            .syncer
            .make_anchor(Path::new(rel), start, end)
            .unwrap_or_else(|e| panic!("make_anchor({rel}, {start}:{end}) failed: {e}"));
        let mut record = Record::new(Kind::Decision, "note".to_string(), new_anchor.anchor);
        record.orig_snippet = Some(new_anchor.snippet);
        record
    }

    fn sync(&self, rel: &str, record: &mut Record) -> SyncOutcome {
        self.syncer
            .sync_annotation(Path::new(rel), record)
            .unwrap_or_else(|e| panic!("sync_annotation({rel}) failed: {e}"))
    }
}

// Lines `start..=end` (1-based inclusive) of a fixture-repo file, `\n`-joined.
fn lines_of(repo: &FixtureRepo, rel: &str, start: u32, end: u32) -> String {
    repo.read(rel)
        .lines()
        .skip(start as usize - 1)
        .take((end - start + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n")
}

fn numbered_lines(count: u32) -> String {
    (1..=count)
        .map(|i| format!("line {i:02} unique content"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-4,
        "confidence {actual} != {expected}"
    );
}

// ---------------------------------------------------------------------------
// The six documented drift scenarios (tests/fixtures/README.md)
// ---------------------------------------------------------------------------

#[test]
fn scenario_edit_above_shifts_by_five() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    let before = record.clone();

    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    let outcome = f.sync(SRC, &mut record);

    assert_eq!(outcome, SyncOutcome::Shifted { delta: 5 });
    assert_eq!((record.anchor.start, record.anchor.end), (37, 47));
    assert_eq!(record.anchor.line_hashes, before.anchor.line_hashes);
    assert_eq!(record.anchor.ctx_before, before.anchor.ctx_before);
    assert_eq!(record.anchor.ctx_after, before.anchor.ctx_after);
    assert_ne!(record.anchor.base_blob, before.anchor.base_blob);
    assert_eq!(record.orig_snippet, before.orig_snippet);
    assert_eq!(record.status, Status::Live);
    assert!(record.updated_at >= before.updated_at);
}

#[test]
fn scenario_edit_below_refreshes_in_place() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    let before = record.clone();

    f.repo.write_fixture(SRC, "edit_below.rs.txt");
    let outcome = f.sync(SRC, &mut record);

    assert_eq!(outcome, SyncOutcome::Refreshed);
    assert_eq!(
        (record.anchor.start, record.anchor.end),
        (ANCHOR_START, ANCHOR_END)
    );
    assert_eq!(record.anchor.line_hashes, before.anchor.line_hashes);
    assert_eq!(record.anchor.ctx_before, before.anchor.ctx_before);
    assert_ne!(record.anchor.base_blob, before.anchor.base_blob);
    assert_eq!(record.status, Status::Live);
}

#[test]
fn scenario_edit_inside_rematches_and_grows_the_span() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);

    f.repo.write_fixture(SRC, "edit_inside_rematch.rs.txt");
    let outcome = f.sync(SRC, &mut record);

    match outcome {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            assert_eq!((new_start, new_end), (34, 45));
            assert_close(confidence, 1.0);
        }
        other => panic!("expected Rematched 34-45, got {other:?}"),
    }
    assert_eq!((record.anchor.start, record.anchor.end), (34, 45));
    assert_eq!(record.anchor.line_hashes.len(), 12);
    assert_eq!(
        record.orig_snippet.as_deref(),
        Some(lines_of(&f.repo, SRC, 34, 45).as_str())
    );
    assert_eq!(record.status, Status::Live);
}

#[test]
fn scenario_edit_inside_orphans_on_low_confidence() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let original_snippet = lines_of(&f.repo, SRC, ANCHOR_START, ANCHOR_END);
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    let before = record.clone();

    f.repo.write_fixture(SRC, "edit_inside_orphan.rs.txt");
    let outcome = f.sync(SRC, &mut record);

    assert_eq!(
        outcome,
        SyncOutcome::Orphaned {
            reason: OrphanReason::LowConfidence
        }
    );
    assert_eq!(record.status, Status::Orphaned);
    assert_eq!(
        record.orig_snippet.as_deref(),
        Some(original_snippet.as_str())
    );
    assert_eq!(
        (record.anchor.start, record.anchor.end),
        (ANCHOR_START, ANCHOR_END)
    );
    assert_eq!(record.anchor.base_blob, before.anchor.base_blob);
    assert_eq!(record.anchor.line_hashes, before.anchor.line_hashes);
}

#[test]
fn scenario_rename_orphans_with_file_missing() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let original_snippet = lines_of(&f.repo, SRC, ANCHOR_START, ANCHOR_END);
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    let before = record.clone();

    f.repo.rename(SRC, "src/renamed.rs");
    let outcome = f.sync(SRC, &mut record);

    assert_eq!(
        outcome,
        SyncOutcome::Orphaned {
            reason: OrphanReason::FileMissing
        }
    );
    assert_eq!(record.status, Status::Orphaned);
    assert_eq!(
        record.orig_snippet.as_deref(),
        Some(original_snippet.as_str())
    );
    assert_eq!(record.anchor, before.anchor);
}

#[test]
fn scenario_moved_function_rematches_at_positional_confidence() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    let before = record.clone();

    f.repo.write_fixture(SRC, "moved_function.rs.txt");
    let outcome = f.sync(SRC, &mut record);

    match outcome {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            assert_eq!((new_start, new_end), (12, 22));
            assert_close(confidence, 0.70);
        }
        other => panic!("expected Rematched 12-22, got {other:?}"),
    }
    assert_eq!((record.anchor.start, record.anchor.end), (12, 22));
    // Byte-identical relocation: the line hashes survive, the context hashes do not.
    assert_eq!(record.anchor.line_hashes, before.anchor.line_hashes);
    assert_ne!(record.anchor.ctx_before, before.anchor.ctx_before);
    assert_ne!(record.anchor.ctx_after, before.anchor.ctx_after);
}

// ---------------------------------------------------------------------------
// Remap boundary arithmetic
// ---------------------------------------------------------------------------

#[test]
fn insertion_at_the_anchor_first_line_shifts_it_down() {
    let f = Fixture::new();
    let base = numbered_lines(10);
    f.repo.write(SRC, &base);
    let mut record = f.anchor(SRC, 4, 8);

    let mut lines: Vec<&str> = base.lines().collect();
    lines.insert(3, "inserted just above the anchor");
    f.repo.write(SRC, &(lines.join("\n") + "\n"));

    assert_eq!(f.sync(SRC, &mut record), SyncOutcome::Shifted { delta: 1 });
    assert_eq!((record.anchor.start, record.anchor.end), (5, 9));
}

#[test]
fn insertion_just_past_the_anchor_last_line_is_ignored() {
    let f = Fixture::new();
    let base = numbered_lines(10);
    f.repo.write(SRC, &base);
    let mut record = f.anchor(SRC, 4, 8);

    let mut lines: Vec<&str> = base.lines().collect();
    lines.insert(8, "inserted just below the anchor");
    f.repo.write(SRC, &(lines.join("\n") + "\n"));

    assert_eq!(f.sync(SRC, &mut record), SyncOutcome::Refreshed);
    assert_eq!((record.anchor.start, record.anchor.end), (4, 8));
}

#[test]
fn modification_of_only_the_first_anchor_line_rematches_in_place() {
    let f = Fixture::new();
    let base = numbered_lines(10);
    f.repo.write(SRC, &base);
    let mut record = f.anchor(SRC, 4, 8);

    let mut lines: Vec<String> = base.lines().map(str::to_string).collect();
    lines[3] = "line 04 rewritten beyond recognition".to_string();
    f.repo.write(SRC, &(lines.join("\n") + "\n"));

    match f.sync(SRC, &mut record) {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            assert_eq!((new_start, new_end), (4, 8));
            // 4 of 5 lines + both context hashes.
            assert_close(confidence, 0.86);
        }
        other => panic!("expected Rematched 4-8, got {other:?}"),
    }
    assert_eq!(record.anchor.line_hashes.len(), 5);
}

// ---------------------------------------------------------------------------
// Scoring thresholds and the hash-only path
// ---------------------------------------------------------------------------

// Two anchored lines relocated to a spot where neither context hash survives:
// score is exactly the positional 0.70, which clears the normal bar but not the
// stricter one applied to anchors of two lines or fewer.
fn relocated_block(target_lines: &[&str]) -> (String, String) {
    let head = ["alpha", "beta", "gamma"];
    let tail = ["delta", "epsilon", "zeta"];
    let before: Vec<&str> = head
        .iter()
        .chain(target_lines.iter())
        .chain(tail.iter())
        .copied()
        .collect();
    let after: Vec<&str> = target_lines
        .iter()
        .chain(head.iter())
        .chain(tail.iter())
        .copied()
        .collect();
    (before.join("\n") + "\n", after.join("\n") + "\n")
}

// Relocates `target_lines` to the top of the file and re-matches with the old
// blob made unreachable, so both halves take the same fuzzy path and differ only
// in anchor length.
fn relocation_outcome(target_lines: &[&str]) -> SyncOutcome {
    let f = Fixture::new();
    let (before, after) = relocated_block(target_lines);
    f.repo.write(SRC, &before);
    let mut record = f.anchor(SRC, 4, 3 + target_lines.len() as u32);
    record.anchor.base_blob = "1".repeat(40);
    f.repo.write(SRC, &after);
    f.sync(SRC, &mut record)
}

#[test]
fn short_anchors_need_the_stricter_confidence_bar() {
    assert_eq!(
        relocation_outcome(&["TARGET_ONE", "TARGET_TWO"]),
        SyncOutcome::Orphaned {
            reason: OrphanReason::LowConfidence
        },
        "a 2-line anchor scoring 0.70 must not re-anchor"
    );

    match relocation_outcome(&["TARGET_ONE", "TARGET_TWO", "TARGET_THREE"]) {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            assert_eq!((new_start, new_end), (1, 3));
            assert_close(confidence, 0.70);
        }
        other => panic!("expected the same 0.70 score to re-anchor 3 lines, got {other:?}"),
    }
}

/// A 2-line block that really moves: the diff may model this as a shift or an
/// overlap, but either way the stricter bar must keep it from landing wrong.
#[test]
fn two_line_anchor_survives_a_real_relocation_diff() {
    let f = Fixture::new();
    let (before, after) = relocated_block(&["TARGET_ONE", "TARGET_TWO"]);
    f.repo.write(SRC, &before);
    let mut record = f.anchor(SRC, 4, 5);
    f.repo.write(SRC, &after);

    assert_eq!(
        f.sync(SRC, &mut record),
        SyncOutcome::Orphaned {
            reason: OrphanReason::LowConfidence
        }
    );
    assert_eq!(record.status, Status::Orphaned);
}

#[test]
fn hash_only_path_rematches_when_the_base_blob_is_gone() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    // Well-formed OID that no object database holds.
    record.anchor.base_blob = "1".repeat(40);

    // Content untouched: the blob-equality short circuit cannot fire, so the
    // whole-file scan has to find the anchor exactly where it already is.
    match f.sync(SRC, &mut record) {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            assert_eq!((new_start, new_end), (ANCHOR_START, ANCHOR_END));
            assert_close(confidence, 1.0);
        }
        other => panic!("expected Rematched at the original range, got {other:?}"),
    }

    let mut drifted = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    drifted.anchor.base_blob = "1".repeat(40);
    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    match f.sync(SRC, &mut drifted) {
        SyncOutcome::Rematched {
            new_start, new_end, ..
        } => assert_eq!((new_start, new_end), (37, 47)),
        other => panic!("expected Rematched 37-47 without the old blob, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Record lifecycle
// ---------------------------------------------------------------------------

#[test]
fn unchanged_content_short_circuits_without_touching_the_record() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    let before = record.clone();

    assert_eq!(f.sync(SRC, &mut record), SyncOutcome::Unchanged);
    assert_eq!(record, before);
}

#[test]
fn non_live_records_are_skipped_untouched() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut orphaned = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    orphaned.orphan();
    let mut tombstoned = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    tombstoned.mark_tombstone();
    let before = vec![orphaned.clone(), tombstoned.clone()];

    f.repo.write_fixture(SRC, "edit_inside_orphan.rs.txt");
    let mut records = vec![orphaned, tombstoned];
    let outcomes = f
        .syncer
        .sync_file(Path::new(SRC), &mut records)
        .expect("sync_file over non-live records succeeds");

    assert_eq!(
        outcomes,
        vec![SyncOutcome::SkippedNotLive, SyncOutcome::SkippedNotLive]
    );
    assert_eq!(records, before);
}

#[test]
fn empty_record_slice_is_a_no_op_even_for_a_missing_file() {
    let f = Fixture::new();
    let outcomes = f
        .syncer
        .sync_file(Path::new("does/not/exist.rs"), &mut [])
        .expect("an empty slice never touches the filesystem");
    assert!(outcomes.is_empty());
}

// ---------------------------------------------------------------------------
// make_anchor edge policies
// ---------------------------------------------------------------------------

#[test]
fn make_anchor_rejects_out_of_bounds_ranges() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let path = Path::new(SRC);

    for (start, end) in [(0, 5), (7, 3), (55, 60)] {
        match f.syncer.make_anchor(path, start, end) {
            Err(SyncError::InvalidRange { line_count, .. }) => assert_eq!(line_count, 59),
            other => panic!("expected InvalidRange for {start}:{end}, got {other:?}"),
        }
    }
    assert!(f.syncer.make_anchor(path, 1, 59).is_ok());
}

#[test]
fn make_anchor_refuses_binary_content() {
    let f = Fixture::new();
    f.repo
        .write("assets/blob.bin", "header\u{0}\u{1}\u{2}payload\n");
    let outcome = f.syncer.make_anchor(Path::new("assets/blob.bin"), 1, 1);
    assert!(
        matches!(outcome, Err(SyncError::BinaryFile { .. })),
        "expected BinaryFile, got {outcome:?}"
    );
}

#[test]
fn make_anchor_on_a_missing_file_is_an_io_error() {
    let f = Fixture::new();
    let outcome = f.syncer.make_anchor(Path::new("nope.rs"), 1, 1);
    assert!(
        matches!(outcome, Err(SyncError::Io { .. })),
        "expected Io, got {outcome:?}"
    );
}

#[test]
fn binary_working_tree_content_orphans_live_records() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    fs::write(f.source(SRC), b"\0\0\0\0binary now\n").unwrap();

    assert_eq!(
        f.sync(SRC, &mut record),
        SyncOutcome::Orphaned {
            reason: OrphanReason::LowConfidence
        }
    );
}

// ---------------------------------------------------------------------------
// Store-facing entry points
// ---------------------------------------------------------------------------

#[test]
fn sync_path_persists_the_heal_and_self_limits() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    let id = record.id;
    f.store.append(f.source(SRC), &record).unwrap();

    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(report.written_back);
    assert_eq!(report.rel_path, PathBuf::from(SRC));
    assert_eq!(
        report.outcomes,
        vec![(id, SyncOutcome::Shifted { delta: 5 })]
    );
    assert_eq!(
        (report.records[0].anchor.start, report.records[0].anchor.end),
        (37, 47)
    );

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!((reloaded[0].anchor.start, reloaded[0].anchor.end), (37, 47));
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);

    let second = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();
    assert_eq!(second.outcomes, vec![(id, SyncOutcome::Unchanged)]);
    assert!(second.written_back);
    assert_eq!(
        f.store.read_raw(f.source(SRC)).unwrap().len(),
        2,
        "an Unchanged sync must not append anything"
    );
}

#[test]
fn sync_path_reports_a_failed_write_back_without_erroring() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    f.store.append(f.source(SRC), &record).unwrap();

    // Wedge the store's lock file so append_all cannot open it.
    let lock = f.store.annot_root().join(".lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();

    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(!report.written_back);
    assert_eq!(
        (report.records[0].anchor.start, report.records[0].anchor.end),
        (37, 47),
        "the healed state is still returned in-memory"
    );
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 1);
}

#[test]
fn sync_tree_covers_every_mirror_and_honors_a_subpath() {
    let f = Fixture::new();
    f.repo.write_fixture("src/a.rs", "base.rs.txt");
    f.repo.write_fixture("src/b.rs", "base.rs.txt");
    f.repo.write_fixture("other/c.rs", "base.rs.txt");
    for rel in ["src/a.rs", "src/b.rs", "other/c.rs"] {
        let record = f.anchor(rel, ANCHOR_START, ANCHOR_END);
        f.store.append(f.source(rel), &record).unwrap();
    }

    f.repo.write_fixture("src/a.rs", "edit_above.rs.txt");
    f.repo.delete("src/b.rs");

    let report = sync::sync_tree(&f.syncer, &f.store, None).unwrap();
    let outcomes: Vec<(PathBuf, SyncOutcome)> = report
        .files
        .iter()
        .map(|file| (file.rel_path.clone(), file.outcomes[0].1.clone()))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            (PathBuf::from("other/c.rs"), SyncOutcome::Unchanged),
            (PathBuf::from("src/a.rs"), SyncOutcome::Shifted { delta: 5 }),
            (
                PathBuf::from("src/b.rs"),
                SyncOutcome::Orphaned {
                    reason: OrphanReason::FileMissing
                }
            ),
        ]
    );
    assert!(report.files.iter().all(|file| file.written_back));

    let scoped = sync::sync_tree(&f.syncer, &f.store, Some(Path::new("src"))).unwrap();
    assert_eq!(
        scoped
            .files
            .iter()
            .map(|file| file.rel_path.clone())
            .collect::<Vec<_>>(),
        vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]
    );
}

// ---------------------------------------------------------------------------
// Malformed anchors and the accept-score contract
// ---------------------------------------------------------------------------

#[test]
fn degenerate_anchor_orphans_instead_of_panicking() {
    // start > end: `line_count()`'s `end - start + 1` would underflow.
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.anchor(SRC, ANCHOR_START, ANCHOR_END);
    record.anchor.start = 5;
    record.anchor.end = 3;

    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    assert_eq!(
        f.sync(SRC, &mut record),
        SyncOutcome::Orphaned {
            reason: OrphanReason::LowConfidence
        }
    );
    assert_eq!(record.status, Status::Orphaned);

    // start: 0, end: u32::MAX: `line_count()` would overflow past `u32::MAX`.
    let f2 = Fixture::new();
    f2.repo.write_fixture(SRC, "base.rs.txt");
    let mut record2 = f2.anchor(SRC, ANCHOR_START, ANCHOR_END);
    record2.anchor.start = 0;
    record2.anchor.end = u32::MAX;

    f2.repo.write_fixture(SRC, "edit_above.rs.txt");
    assert_eq!(
        f2.sync(SRC, &mut record2),
        SyncOutcome::Orphaned {
            reason: OrphanReason::LowConfidence
        }
    );
    assert_eq!(record2.status, Status::Orphaned);
}

#[test]
fn contract_score_accepts_sub_majority_matches() {
    // 3 lines of context, a 9-line anchor, 3 more lines of context.
    let f = Fixture::new();
    let base = numbered_lines(15);
    f.repo.write(SRC, &base);
    let mut record = f.anchor(SRC, 4, 12);

    // Rewrite 5 of the 9 anchor lines in place: same file length, both context
    // blocks untouched, so the diff hunk overlaps the anchor and forces the
    // fuzzy path instead of the delta-shift heal.
    let mut lines: Vec<String> = base.lines().map(str::to_string).collect();
    for line in lines.iter_mut().take(8).skip(3) {
        *line = format!("{line} REWRITTEN");
    }
    f.repo.write(SRC, &(lines.join("\n") + "\n"));

    match f.sync(SRC, &mut record) {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            // 4 of 9 anchor lines still match (a sub-majority the old `2 *
            // matched < k` pre-filter would have rejected outright), plus
            // both context hashes: 7000*4/9 + 3000 = 6111 bp.
            assert_eq!((new_start, new_end), (4, 12));
            assert_eq!((confidence * 10_000.0).round() as i32, 6111);
        }
        other => panic!("expected Rematched 4-12 at 0.6111, got {other:?}"),
    }
    assert_eq!(record.status, Status::Live);
}
