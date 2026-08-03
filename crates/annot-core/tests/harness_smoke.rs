mod common;

use common::FixtureRepo;

const BASE_LINE_COUNT: usize = 59;

#[test]
fn new_repo_is_discoverable_by_gix_and_workdir_matches_root() {
    let repo = FixtureRepo::new();
    repo.write_fixture("src/lib.rs", "base.rs.txt");

    let discovered = gix::discover(repo.root())
        .unwrap_or_else(|e| panic!("gix::discover({}) failed: {e}", repo.root().display()));
    let workdir = discovered.workdir().unwrap_or_else(|| {
        panic!(
            "discovered repository at {} has no workdir (bare?)",
            repo.root().display()
        )
    });

    let expected = repo
        .root()
        .canonicalize()
        .unwrap_or_else(|e| panic!("failed to canonicalize {}: {e}", repo.root().display()));
    let actual = workdir
        .canonicalize()
        .unwrap_or_else(|e| panic!("failed to canonicalize {}: {e}", workdir.display()));
    assert_eq!(actual, expected);
}

#[test]
fn base_template_has_the_documented_line_count() {
    let repo = FixtureRepo::new();
    repo.write_fixture("src/lib.rs", "base.rs.txt");

    let content = repo.read("src/lib.rs");
    assert_eq!(
        content.lines().count(),
        BASE_LINE_COUNT,
        "tests/fixtures/README.md documents base.rs.txt as {BASE_LINE_COUNT} lines; \
         update the README table (and every scenario's hand-computed line numbers) if this changes"
    );
    assert!(content.contains("fn encode_payload"));
}

#[test]
fn write_read_rename_delete_round_trip() {
    let repo = FixtureRepo::new();

    repo.write("scratch/note.txt", "hello fixture\n");
    assert_eq!(repo.read("scratch/note.txt"), "hello fixture\n");
    assert!(repo.root().join("scratch/note.txt").exists());

    repo.rename("scratch/note.txt", "scratch/moved/note.txt");
    assert!(!repo.root().join("scratch/note.txt").exists());
    assert!(repo.root().join("scratch/moved/note.txt").exists());
    assert_eq!(repo.read("scratch/moved/note.txt"), "hello fixture\n");

    repo.delete("scratch/moved/note.txt");
    assert!(!repo.root().join("scratch/moved/note.txt").exists());
}
