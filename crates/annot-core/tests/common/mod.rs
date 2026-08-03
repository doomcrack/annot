#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Materializes a throwaway git repository for drift tests: a `tempfile::TempDir`
/// initialized via `git init`, plus helpers to populate it from the plain-text
/// templates in `tests/fixtures/`. Pure repo builder — no dependency on
/// annot-core's model/store/sync.
pub struct FixtureRepo {
    dir: tempfile::TempDir,
    root: PathBuf,
}

impl FixtureRepo {
    /// Creates a tempdir and runs `git init` inside it with a fixed default
    /// branch name, so tests never depend on the ambient git config.
    pub fn new() -> FixtureRepo {
        let dir = tempfile::tempdir()
            .unwrap_or_else(|e| panic!("FixtureRepo::new: failed to create tempdir: {e}"));
        let root = dir.path().to_path_buf();

        let output = Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "--quiet"])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|e| {
                panic!(
                    "FixtureRepo::new: failed to spawn `git init` in {}: {e}",
                    root.display()
                )
            });
        if !output.status.success() {
            panic!(
                "FixtureRepo::new: `git init` failed in {} with status {}\nstderr: {}",
                root.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
        }

        FixtureRepo { dir, root }
    }

    /// Absolute path to the repository's working directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes `content` to `rel` (repo-relative), creating parent directories
    /// as needed. Overwrites any existing file.
    pub fn write(&self, rel: &str, content: &str) {
        let target = self.root.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!(
                    "FixtureRepo::write: failed to create parent dirs for {}: {e}",
                    target.display()
                )
            });
        }
        fs::write(&target, content).unwrap_or_else(|e| {
            panic!(
                "FixtureRepo::write: failed to write {}: {e}",
                target.display()
            )
        });
    }

    /// Copies `tests/fixtures/<template>` (workspace-root-relative) to `rel`
    /// inside the fixture repo. `template` is a filename in that directory,
    /// e.g. `"base.rs.txt"`.
    pub fn write_fixture(&self, rel: &str, template: &str) {
        let template_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(template);
        let content = fs::read_to_string(&template_path).unwrap_or_else(|e| {
            panic!(
                "FixtureRepo::write_fixture: failed to read template {}: {e}",
                template_path.display()
            )
        });
        self.write(rel, &content);
    }

    /// Reads `rel` (repo-relative) as a UTF-8 string.
    pub fn read(&self, rel: &str) -> String {
        let target = self.root.join(rel);
        fs::read_to_string(&target).unwrap_or_else(|e| {
            panic!(
                "FixtureRepo::read: failed to read {}: {e}",
                target.display()
            )
        })
    }

    /// Renames `from` to `to` (both repo-relative), creating parent
    /// directories for `to` as needed.
    pub fn rename(&self, from: &str, to: &str) {
        let from_path = self.root.join(from);
        let to_path = self.root.join(to);
        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!(
                    "FixtureRepo::rename: failed to create parent dirs for {}: {e}",
                    to_path.display()
                )
            });
        }
        fs::rename(&from_path, &to_path).unwrap_or_else(|e| {
            panic!(
                "FixtureRepo::rename: failed to rename {} to {}: {e}",
                from_path.display(),
                to_path.display()
            )
        });
    }

    /// Deletes `rel` (repo-relative).
    pub fn delete(&self, rel: &str) {
        let target = self.root.join(rel);
        fs::remove_file(&target).unwrap_or_else(|e| {
            panic!(
                "FixtureRepo::delete: failed to delete {}: {e}",
                target.display()
            )
        });
    }
}

impl Default for FixtureRepo {
    fn default() -> Self {
        Self::new()
    }
}
