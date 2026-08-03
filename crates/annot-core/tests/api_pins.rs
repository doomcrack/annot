use std::fs::File;
use std::io::Write as _;

use fs4::{FileExt, TryLockError};
use imara_diff::{Algorithm, Diff, InternedInput};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[test]
fn gix_write_blob_and_find_blob_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let repo = gix::init(dir.path()).unwrap();

    let bytes = b"hello annot spike\n".to_vec();
    let id = repo.write_blob(&bytes).unwrap().detach();

    let blob = repo.find_blob(id).unwrap();
    assert_eq!(blob.data, bytes);

    let computed =
        gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &bytes).unwrap();
    assert_eq!(computed, id);
}

#[test]
fn imara_diff_pure_insertion_hunk_is_zero_width_at_old_position() {
    let before = "a\nb\nc\n";
    let after = "a\nb\nx\nc\n";
    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Histogram, &input);
    let hunks: Vec<_> = diff.hunks().collect();

    assert_eq!(hunks.len(), 1);
    let hunk = &hunks[0];
    assert!(hunk.before.is_empty());
    assert_eq!(hunk.before, 2..2);
    assert_eq!(hunk.after, 2..3);
}

#[test]
fn imara_diff_pure_deletion_hunk_is_zero_width_at_new_position() {
    let before = "a\nb\nx\nc\n";
    let after = "a\nb\nc\n";
    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Histogram, &input);
    let hunks: Vec<_> = diff.hunks().collect();

    assert_eq!(hunks.len(), 1);
    let hunk = &hunks[0];
    assert!(hunk.after.is_empty());
    assert_eq!(hunk.before, 2..3);
    assert_eq!(hunk.after, 2..2);
}

#[test]
fn imara_diff_modification_hunk_has_equal_length_ranges() {
    let before = "a\nb\nc\n";
    let after = "a\nz\nc\n";
    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Histogram, &input);
    let hunks: Vec<_> = diff.hunks().collect();

    assert_eq!(hunks.len(), 1);
    let hunk = &hunks[0];
    assert_eq!(hunk.before, 1..2);
    assert_eq!(hunk.after, 1..2);
}

#[test]
fn fs4_exclusive_try_lock_fails_from_second_handle_while_held() {
    // std::fs::File gained inherent lock_shared/try_lock/unlock methods (stabilized
    // 1.89) that shadow fs4::FileExt's identically-named trait methods under normal
    // method-call syntax. Fully-qualified calls are required to actually exercise fs4.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lockfile");
    let mut file_a = File::create(&path).unwrap();
    file_a.write_all(b"x").unwrap();

    FileExt::lock_shared(&file_a).unwrap();
    FileExt::unlock(&file_a).unwrap();

    FileExt::try_lock(&file_a).unwrap();

    let file_b = File::open(&path).unwrap();
    let err = FileExt::try_lock(&file_b).unwrap_err();
    assert!(matches!(err, TryLockError::WouldBlock));

    FileExt::unlock(&file_a).unwrap();
}

#[derive(Serialize, Deserialize)]
struct Wrapper {
    id: Ulid,
}

#[test]
fn ulid_generate_round_trips_as_26_char_crockford_string_via_json() {
    let id = Ulid::generate();
    let wrapper = Wrapper { id };

    let json = serde_json::to_string(&wrapper).unwrap();
    let encoded = id.to_string();
    assert_eq!(encoded.len(), 26);
    assert_eq!(json, format!(r#"{{"id":"{encoded}"}}"#));

    let decoded: Wrapper = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.id, id);
}
