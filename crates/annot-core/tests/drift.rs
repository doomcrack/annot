mod common;

use std::path::{Path, PathBuf};

use common::FixtureRepo;

use annot_core::model::{Kind, Record, Status, Ulid};
use annot_core::store::Store;
use annot_core::sync::{self, FileSyncReport, OrphanReason, SyncOutcome, Syncer};

const SRC: &str = "src/lib.rs";
/// `fn encode_payload` in `tests/fixtures/base.rs.txt`, per the fixture README.
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

    /// The `annot add` flow: `source_rel` -> `make_anchor` -> `Record::new` ->
    /// `orig_snippet = Some(snippet)` -> `store.append`.
    fn add(&self, rel: &str, kind: Kind, start: u32, end: u32) -> Record {
        let source = self.source(rel);
        let source_rel = self.store.source_rel(&source).unwrap();
        let new_anchor = self
            .syncer
            .make_anchor(&source_rel, start, end)
            .unwrap_or_else(|e| panic!("make_anchor({rel}, {start}:{end}) failed: {e}"));
        let mut record = Record::new(kind, "note".to_string(), new_anchor.anchor);
        record.orig_snippet = Some(new_anchor.snippet);
        self.store.append(&source, &record).unwrap();
        record
    }
}

/// Lines `start..=end` (1-based inclusive) of a fixture-repo file, `\n`-joined,
/// read directly off disk (never derived by invoking the sync engine).
fn lines_of(repo: &FixtureRepo, rel: &str, start: u32, end: u32) -> String {
    repo.read(rel)
        .lines()
        .skip(start as usize - 1)
        .take((end - start + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_confidence(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-4,
        "confidence {actual} != {expected}"
    );
}

fn outcome_for(report: &FileSyncReport, id: Ulid) -> SyncOutcome {
    report
        .outcomes
        .iter()
        .find(|(rid, _)| *rid == id)
        .unwrap_or_else(|| panic!("no outcome for {id}"))
        .1
        .clone()
}

fn record_for(records: &[Record], id: Ulid) -> &Record {
    records
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no healed record for {id}"))
}

// ---------------------------------------------------------------------------
// The six documented drift scenarios, end to end through the store.
// ---------------------------------------------------------------------------

#[test]
fn scenario_edit_above_persists_the_shift() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let before = f.add(SRC, Kind::Decision, ANCHOR_START, ANCHOR_END);
    let id = before.id;

    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(report.written_back);
    assert_eq!(
        report.outcomes,
        vec![(id, SyncOutcome::Shifted { delta: 5 })]
    );
    let healed = record_for(&report.records, id);
    assert_eq!((healed.anchor.start, healed.anchor.end), (37, 47));
    assert_eq!(healed.anchor.line_hashes, before.anchor.line_hashes);
    assert_eq!(healed.anchor.ctx_before, before.anchor.ctx_before);
    assert_eq!(healed.anchor.ctx_after, before.anchor.ctx_after);
    assert_ne!(healed.anchor.base_blob, before.anchor.base_blob);
    assert_eq!(healed.status, Status::Live);
    assert!(healed.updated_at >= healed.created_at);

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!((reloaded[0].anchor.start, reloaded[0].anchor.end), (37, 47));
    assert_eq!(reloaded[0].anchor.line_hashes, before.anchor.line_hashes);
    assert_eq!(reloaded[0].anchor.base_blob, healed.anchor.base_blob);

    let raw = f.store.read_raw(f.source(SRC)).unwrap();
    assert_eq!(raw.len(), 2);
    assert_eq!(raw[0].id, id);
    assert_eq!(raw[1].id, id);

    let second = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();
    assert_eq!(second.outcomes, vec![(id, SyncOutcome::Unchanged)]);
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);
}

#[test]
fn scenario_edit_below_persists_the_refresh() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let before = f.add(SRC, Kind::Decision, ANCHOR_START, ANCHOR_END);
    let id = before.id;

    f.repo.write_fixture(SRC, "edit_below.rs.txt");
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(report.written_back);
    assert_eq!(report.outcomes, vec![(id, SyncOutcome::Refreshed)]);
    let healed = record_for(&report.records, id);
    assert_eq!(
        (healed.anchor.start, healed.anchor.end),
        (ANCHOR_START, ANCHOR_END)
    );
    assert_eq!(healed.anchor.line_hashes, before.anchor.line_hashes);
    assert_eq!(healed.anchor.ctx_before, before.anchor.ctx_before);
    assert_ne!(healed.anchor.base_blob, before.anchor.base_blob);
    assert_eq!(healed.status, Status::Live);
    assert!(healed.updated_at >= healed.created_at);

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!(
        (reloaded[0].anchor.start, reloaded[0].anchor.end),
        (ANCHOR_START, ANCHOR_END)
    );
    assert_eq!(reloaded[0].anchor.base_blob, healed.anchor.base_blob);

    let raw = f.store.read_raw(f.source(SRC)).unwrap();
    assert_eq!(raw.len(), 2);
    assert_eq!(raw[0].id, id);
    assert_eq!(raw[1].id, id);

    let second = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();
    assert_eq!(second.outcomes, vec![(id, SyncOutcome::Unchanged)]);
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);
}

#[test]
fn scenario_edit_inside_rematch_persists_the_regrow() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let before = f.add(SRC, Kind::Decision, ANCHOR_START, ANCHOR_END);
    let id = before.id;

    f.repo.write_fixture(SRC, "edit_inside_rematch.rs.txt");
    let expected_snippet = lines_of(&f.repo, SRC, 34, 45);
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(report.written_back);
    match outcome_for(&report, id) {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            assert_eq!((new_start, new_end), (34, 45));
            assert_confidence(confidence, 1.0);
        }
        other => panic!("expected Rematched 34-45, got {other:?}"),
    }
    let healed = record_for(&report.records, id);
    assert_eq!((healed.anchor.start, healed.anchor.end), (34, 45));
    assert_eq!(healed.anchor.line_hashes.len(), 12);
    assert_eq!(
        healed.orig_snippet.as_deref(),
        Some(expected_snippet.as_str())
    );
    assert_eq!(healed.status, Status::Live);
    assert!(healed.updated_at >= healed.created_at);

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!((reloaded[0].anchor.start, reloaded[0].anchor.end), (34, 45));
    assert_eq!(reloaded[0].anchor.line_hashes.len(), 12);
    assert_eq!(
        reloaded[0].orig_snippet.as_deref(),
        Some(expected_snippet.as_str())
    );

    let raw = f.store.read_raw(f.source(SRC)).unwrap();
    assert_eq!(raw.len(), 2);
    assert_eq!(raw[0].id, id);
    assert_eq!(raw[1].id, id);

    let second = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();
    assert_eq!(second.outcomes, vec![(id, SyncOutcome::Unchanged)]);
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);
}

#[test]
fn scenario_edit_inside_orphan_persists_the_orphan() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let original_snippet = lines_of(&f.repo, SRC, ANCHOR_START, ANCHOR_END);
    let before = f.add(SRC, Kind::Decision, ANCHOR_START, ANCHOR_END);
    let id = before.id;

    f.repo.write_fixture(SRC, "edit_inside_orphan.rs.txt");
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(report.written_back);
    assert_eq!(
        report.outcomes,
        vec![(
            id,
            SyncOutcome::Orphaned {
                reason: OrphanReason::LowConfidence
            }
        )]
    );
    let healed = record_for(&report.records, id);
    assert_eq!(healed.status, Status::Orphaned);
    assert_eq!(
        healed.orig_snippet.as_deref(),
        Some(original_snippet.as_str())
    );
    assert_eq!(healed.anchor, before.anchor);
    assert!(healed.updated_at >= healed.created_at);

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].status, Status::Orphaned);
    assert_eq!(
        reloaded[0].orig_snippet.as_deref(),
        Some(original_snippet.as_str())
    );
    assert_eq!(reloaded[0].anchor, before.anchor);

    let raw = f.store.read_raw(f.source(SRC)).unwrap();
    assert_eq!(raw.len(), 2);
    assert_eq!(raw[0].id, id);
    assert_eq!(raw[1].id, id);

    let second = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();
    assert_eq!(second.outcomes, vec![(id, SyncOutcome::SkippedNotLive)]);
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);
}

#[test]
fn scenario_rename_orphans_via_sync_tree() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let original_snippet = lines_of(&f.repo, SRC, ANCHOR_START, ANCHOR_END);
    let before = f.add(SRC, Kind::Decision, ANCHOR_START, ANCHOR_END);
    let id = before.id;

    f.repo.rename(SRC, "src/renamed.rs");
    let report = sync::sync_tree(&f.syncer, &f.store, None).unwrap();

    assert_eq!(report.files.len(), 1);
    let file_report = &report.files[0];
    assert!(file_report.written_back);
    assert_eq!(file_report.rel_path, PathBuf::from(SRC));
    assert_eq!(
        file_report.outcomes,
        vec![(
            id,
            SyncOutcome::Orphaned {
                reason: OrphanReason::FileMissing
            }
        )]
    );
    let healed = record_for(&file_report.records, id);
    assert_eq!(healed.status, Status::Orphaned);
    assert_eq!(
        healed.orig_snippet.as_deref(),
        Some(original_snippet.as_str())
    );
    assert_eq!(healed.anchor, before.anchor);

    let old_mirror = f.store.annot_path(f.source(SRC)).unwrap();
    assert!(old_mirror.exists(), "old mirror file must survive a rename");

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].status, Status::Orphaned);
    assert_eq!(reloaded[0].anchor, before.anchor);

    let raw = f.store.read_raw(f.source(SRC)).unwrap();
    assert_eq!(raw.len(), 2);
    assert_eq!(raw[0].id, id);
    assert_eq!(raw[1].id, id);

    let second = sync::sync_tree(&f.syncer, &f.store, None).unwrap();
    assert_eq!(
        second.files[0].outcomes,
        vec![(id, SyncOutcome::SkippedNotLive)]
    );
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);
}

#[test]
fn scenario_moved_function_persists_the_rematch() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let before = f.add(SRC, Kind::Decision, ANCHOR_START, ANCHOR_END);
    let id = before.id;

    f.repo.write_fixture(SRC, "moved_function.rs.txt");
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(report.written_back);
    match outcome_for(&report, id) {
        SyncOutcome::Rematched {
            new_start,
            new_end,
            confidence,
        } => {
            assert_eq!((new_start, new_end), (12, 22));
            assert_confidence(confidence, 0.70);
        }
        other => panic!("expected Rematched 12-22, got {other:?}"),
    }
    let healed = record_for(&report.records, id);
    assert_eq!((healed.anchor.start, healed.anchor.end), (12, 22));
    assert_eq!(healed.anchor.line_hashes, before.anchor.line_hashes);
    assert_ne!(healed.anchor.ctx_before, before.anchor.ctx_before);
    assert_ne!(healed.anchor.ctx_after, before.anchor.ctx_after);
    assert_eq!(healed.status, Status::Live);
    assert!(healed.updated_at >= healed.created_at);

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!((reloaded[0].anchor.start, reloaded[0].anchor.end), (12, 22));
    assert_eq!(reloaded[0].anchor.line_hashes, before.anchor.line_hashes);

    let raw = f.store.read_raw(f.source(SRC)).unwrap();
    assert_eq!(raw.len(), 2);
    assert_eq!(raw[0].id, id);
    assert_eq!(raw[1].id, id);

    let second = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();
    assert_eq!(second.outcomes, vec![(id, SyncOutcome::Unchanged)]);
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// Cross-cutting store lifecycle behavior.
// ---------------------------------------------------------------------------

#[test]
fn compaction_after_heal_merges_the_superseding_record() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let before = f.add(SRC, Kind::Decision, ANCHOR_START, ANCHOR_END);
    let id = before.id;

    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();
    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);

    let stats = f.store.compact_file(f.source(SRC)).unwrap();
    assert_eq!(stats.files_compacted, 1);
    assert_eq!(stats.files_removed, 0);
    assert_eq!(stats.records_before, 2);
    assert_eq!(stats.records_after, 1);
    assert_eq!(stats.duplicates_merged, 1);
    assert_eq!(stats.tombstones_dropped, 0);

    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 1);
    let loaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, id);
    assert_eq!((loaded[0].anchor.start, loaded[0].anchor.end), (37, 47));
}

#[test]
fn tombstone_lifecycle_removes_the_mirror_on_compaction() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");
    let mut record = f.add(SRC, Kind::Todo, ANCHOR_START, ANCHOR_END);
    record.mark_tombstone();
    f.store.append(f.source(SRC), &record).unwrap();

    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 2);
    assert_eq!(f.store.load(f.source(SRC)).unwrap(), Vec::new());

    let stats = f.store.compact_file(f.source(SRC)).unwrap();
    assert_eq!(stats.files_removed, 1);
    assert_eq!(stats.records_after, 0);

    let mirror = f.store.annot_path(f.source(SRC)).unwrap();
    assert!(!mirror.exists());
}

#[test]
fn multi_record_file_heals_every_live_record_in_one_append_all() {
    let f = Fixture::new();
    f.repo.write_fixture(SRC, "base.rs.txt");

    let decision = f.add(SRC, Kind::Decision, 32, 42); // fn encode_payload
    let gotcha = f.add(SRC, Kind::Gotcha, 6, 10); // struct Header
    let todo = f.add(SRC, Kind::Todo, 1, 1); // above the insertion point

    assert_eq!(f.store.read_raw(f.source(SRC)).unwrap().len(), 3);

    f.repo.write_fixture(SRC, "edit_above.rs.txt");
    let report = sync::sync_path(&f.syncer, &f.store, Path::new(SRC)).unwrap();

    assert!(report.written_back);
    assert_eq!(
        outcome_for(&report, decision.id),
        SyncOutcome::Shifted { delta: 5 }
    );
    assert_eq!(
        outcome_for(&report, gotcha.id),
        SyncOutcome::Shifted { delta: 5 }
    );
    assert_eq!(outcome_for(&report, todo.id), SyncOutcome::Refreshed);

    let healed_decision = record_for(&report.records, decision.id);
    let healed_gotcha = record_for(&report.records, gotcha.id);
    let healed_todo = record_for(&report.records, todo.id);
    assert_eq!(
        (healed_decision.anchor.start, healed_decision.anchor.end),
        (37, 47)
    );
    assert_eq!(
        (healed_gotcha.anchor.start, healed_gotcha.anchor.end),
        (11, 15)
    );
    assert_eq!((healed_todo.anchor.start, healed_todo.anchor.end), (1, 1));

    let raw = f.store.read_raw(f.source(SRC)).unwrap();
    assert_eq!(raw.len(), 6);

    let reloaded = f.store.load(f.source(SRC)).unwrap();
    assert_eq!(reloaded.len(), 3);
    let reloaded_decision = record_for(&reloaded, decision.id);
    let reloaded_gotcha = record_for(&reloaded, gotcha.id);
    let reloaded_todo = record_for(&reloaded, todo.id);
    assert_eq!(
        (reloaded_decision.anchor.start, reloaded_decision.anchor.end),
        (37, 47)
    );
    assert_eq!(
        (reloaded_gotcha.anchor.start, reloaded_gotcha.anchor.end),
        (11, 15)
    );
    assert_eq!(
        (reloaded_todo.anchor.start, reloaded_todo.anchor.end),
        (1, 1)
    );
}
