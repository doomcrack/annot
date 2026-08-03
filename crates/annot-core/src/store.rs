//! JSONL-backed annotation storage, mirroring the source tree under `.annot/`.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use fs4::FileExt;

use crate::model::{Record, Status, Ulid};

/// Directory holding all annotation mirrors, relative to the repo root.
pub const ANNOT_DIR: &str = ".annot";
/// Advisory lock file name, relative to [`ANNOT_DIR`].
pub const LOCK_FILE: &str = ".lock";

/// A JSONL annotation store rooted at a git repository's working directory.
#[derive(Debug)]
pub struct Store {
    repo_root: PathBuf,
    annot_root: PathBuf,
}

impl Store {
    /// Discovers the enclosing git repository from `start` via `gix::discover`
    /// and opens its working directory. `Err(RepoNotFound)` if there is no
    /// repository, or it is bare (no working directory).
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, StoreError> {
        let start = start.as_ref();
        let repo = gix::discover(start).map_err(|_| StoreError::RepoNotFound {
            start: start.to_path_buf(),
        })?;
        let workdir = repo.workdir().ok_or_else(|| StoreError::RepoNotFound {
            start: start.to_path_buf(),
        })?;
        Self::open(workdir.to_path_buf())
    }

    /// Opens a store trusting `repo_root` directly (must exist and be a directory).
    pub fn open(repo_root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let raw = repo_root.into();
        let canonical = fs::canonicalize(&raw).map_err(|e| StoreError::Io {
            path: raw.clone(),
            source: e,
        })?;
        if !canonical.is_dir() {
            return Err(StoreError::Io {
                path: raw,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "repo_root is not a directory",
                ),
            });
        }
        let annot_root = canonical.join(ANNOT_DIR);
        Ok(Self {
            repo_root: canonical,
            annot_root,
        })
    }

    /// The repository's working directory.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// `<repo_root>/.annot`.
    pub fn annot_root(&self) -> &Path {
        &self.annot_root
    }

    /// Maps `source` to a repo-relative, lexically normalized path. Relative
    /// inputs are resolved against the current directory. `Err(OutsideRepo)`
    /// if the result escapes `repo_root`; `Err(ReservedPath)` if it points
    /// inside `.annot/`.
    pub fn source_rel(&self, source: impl AsRef<Path>) -> Result<PathBuf, StoreError> {
        let source = source.as_ref();
        let absolute = if source.is_absolute() {
            source.to_path_buf()
        } else {
            let cwd = std::env::current_dir().map_err(|e| StoreError::Io {
                path: source.to_path_buf(),
                source: e,
            })?;
            cwd.join(source)
        };
        let normalized = normalize_lexically(&absolute);
        let rel = normalized
            .strip_prefix(&self.repo_root)
            .map_err(|_| StoreError::OutsideRepo {
                path: normalized.clone(),
                root: self.repo_root.clone(),
            })?
            .to_path_buf();
        if rel.components().next() == Some(Component::Normal(std::ffi::OsStr::new(ANNOT_DIR))) {
            return Err(StoreError::ReservedPath { path: rel });
        }
        Ok(rel)
    }

    /// Mirror path for `source`: `<repo_root>/.annot/<source_rel>` with
    /// `.jsonl` appended to the file name.
    pub fn annot_path(&self, source: impl AsRef<Path>) -> Result<PathBuf, StoreError> {
        let rel = self.source_rel(source)?;
        Ok(self.mirror_path_for_rel(&rel))
    }

    /// Appends `record` to `source`'s mirror under a blocking shared lock.
    pub fn append(&self, source: impl AsRef<Path>, record: &Record) -> Result<(), StoreError> {
        self.append_all(source, std::slice::from_ref(record))
    }

    /// Appends `records` to `source`'s mirror in one shared-lock hold.
    pub fn append_all(
        &self,
        source: impl AsRef<Path>,
        records: &[Record],
    ) -> Result<(), StoreError> {
        let path = self.annot_path(source)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| StoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let _guard = LockGuard::shared(&self.lock_path())?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        for record in records {
            let mut line = serde_json::to_string(record).expect("Record always serializes");
            line.push('\n');
            file.write_all(line.as_bytes())
                .map_err(|e| StoreError::Io {
                    path: path.clone(),
                    source: e,
                })?;
        }
        Ok(())
    }

    /// Raw log for `source` in file order, duplicates and tombstones included.
    /// Missing mirror/`.annot` => `Ok(vec![])`. A torn unterminated trailing
    /// line is skipped; a malformed interior line is `Err(MalformedLine)`.
    pub fn read_raw(&self, source: impl AsRef<Path>) -> Result<Vec<Record>, StoreError> {
        let path = self.annot_path(source)?;
        self.read_raw_at(&path)
    }

    /// Compacted view for `source`: last record per id wins, ids whose
    /// winner is `Tombstone` are removed, orphaned records are kept, result
    /// sorted by id ascending.
    pub fn load(&self, source: impl AsRef<Path>) -> Result<Vec<Record>, StoreError> {
        let raw = self.read_raw(source)?;
        Ok(Self::fold_last_wins(raw))
    }

    /// Scans every mirror for `id`. Returns its repo-relative source path
    /// and winning record.
    pub fn find(&self, id: Ulid) -> Result<Option<(PathBuf, Record)>, StoreError> {
        for rel in self.list_annotated_files()? {
            let mirror_path = self.mirror_path_for_rel(&rel);
            let records = Self::fold_last_wins(self.read_raw_at(&mirror_path)?);
            if let Some(record) = records.into_iter().find(|r| r.id == id) {
                return Ok(Some((rel, record)));
            }
        }
        Ok(None)
    }

    /// Sorted repo-relative source paths that currently have a mirror file.
    pub fn list_annotated_files(&self) -> Result<Vec<PathBuf>, StoreError> {
        let mut mirrors = self.walk_mirror_files()?;
        mirrors.sort();
        let mut out: Vec<PathBuf> = mirrors
            .iter()
            .filter_map(|m| self.source_rel_from_mirror(m))
            .collect();
        out.sort();
        Ok(out)
    }

    /// Compacts `source`'s mirror under an exclusive lock (default retry
    /// budget): atomic rewrite via same-directory temp file + fsync + rename.
    pub fn compact_file(&self, source: impl AsRef<Path>) -> Result<CompactStats, StoreError> {
        let mirror_path = self.annot_path(source)?;
        let opts = CompactOptions::default();
        let _guard = LockGuard::exclusive(&self.lock_path(), &opts)?;
        self.compact_one(&mirror_path)
    }

    /// Compacts every mirror file (default retry budget).
    pub fn compact_all(&self) -> Result<CompactStats, StoreError> {
        self.compact_all_opts(&CompactOptions::default())
    }

    /// Compacts every mirror file, tuning exclusive-lock acquisition via `opts`.
    pub fn compact_all_opts(&self, opts: &CompactOptions) -> Result<CompactStats, StoreError> {
        let _guard = LockGuard::exclusive(&self.lock_path(), opts)?;
        let mut total = CompactStats::default();
        for mirror_path in self.walk_mirror_files()? {
            match self.compact_one(&mirror_path) {
                Ok(stats) => total.absorb(stats),
                Err(StoreError::MalformedLine { .. }) => total.files_skipped += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }

    fn lock_path(&self) -> PathBuf {
        self.annot_root.join(LOCK_FILE)
    }

    fn mirror_path_for_rel(&self, rel: &Path) -> PathBuf {
        let mut file_name = rel
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        file_name.push(".jsonl");
        let mut path = self.annot_root.clone();
        if let Some(parent) = rel.parent() {
            path.push(parent);
        }
        path.push(file_name);
        path
    }

    fn source_rel_from_mirror(&self, mirror: &Path) -> Option<PathBuf> {
        let rel = mirror.strip_prefix(&self.annot_root).ok()?;
        let stem = rel.file_stem()?;
        Some(rel.with_file_name(stem))
    }

    fn walk_mirror_files(&self) -> Result<Vec<PathBuf>, StoreError> {
        let mut out = Vec::new();
        if !self.annot_root.is_dir() {
            return Ok(out);
        }
        let mut stack = vec![self.annot_root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = fs::read_dir(&dir).map_err(|e| StoreError::Io {
                path: dir.clone(),
                source: e,
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| StoreError::Io {
                    path: dir.clone(),
                    source: e,
                })?;
                let path = entry.path();
                let file_type = entry.file_type().map_err(|e| StoreError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "jsonl")
                {
                    out.push(path);
                }
            }
        }
        Ok(out)
    }

    fn read_raw_at(&self, path: &Path) -> Result<Vec<Record>, StoreError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(StoreError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let ends_with_newline = text.ends_with('\n');
        let mut lines: Vec<&str> = text.split('\n').collect();
        if ends_with_newline {
            lines.pop();
        }
        let last_index = lines.len().saturating_sub(1);
        let mut records = Vec::with_capacity(lines.len());
        for (i, raw_line) in lines.iter().enumerate() {
            let content = raw_line.trim_end();
            if content.is_empty() {
                continue;
            }
            match serde_json::from_str::<Record>(content) {
                Ok(record) => records.push(record),
                Err(e) => {
                    let is_torn_tail = i == last_index && !ends_with_newline;
                    if is_torn_tail {
                        continue;
                    }
                    return Err(StoreError::MalformedLine {
                        path: path.to_path_buf(),
                        line: i + 1,
                        source: e,
                    });
                }
            }
        }
        Ok(records)
    }

    fn fold_last_wins(raw: Vec<Record>) -> Vec<Record> {
        let mut map: HashMap<Ulid, Record> = HashMap::new();
        for record in raw {
            map.insert(record.id, record);
        }
        let mut out: Vec<Record> = map
            .into_values()
            .filter(|r| r.status != Status::Tombstone)
            .collect();
        out.sort_by_key(|r| r.id);
        out
    }

    fn compact_one(&self, mirror_path: &Path) -> Result<CompactStats, StoreError> {
        if !mirror_path.exists() {
            return Ok(CompactStats::default());
        }
        let raw = self.read_raw_at(mirror_path)?;
        let before = raw.len();
        let mut map: HashMap<Ulid, Record> = HashMap::new();
        for record in raw {
            map.insert(record.id, record);
        }
        let duplicates_merged = before - map.len();
        let mut tombstones_dropped = 0usize;
        let mut survivors: Vec<Record> = Vec::with_capacity(map.len());
        for record in map.into_values() {
            if record.status == Status::Tombstone {
                tombstones_dropped += 1;
            } else {
                survivors.push(record);
            }
        }
        survivors.sort_by_key(|r| r.id);
        let after = survivors.len();

        let files_removed = if survivors.is_empty() {
            fs::remove_file(mirror_path).map_err(|e| StoreError::Io {
                path: mirror_path.to_path_buf(),
                source: e,
            })?;
            self.prune_empty_parents(mirror_path);
            1
        } else {
            self.write_mirror_atomic(mirror_path, &survivors)?;
            0
        };

        Ok(CompactStats {
            files_compacted: 1,
            files_removed,
            files_skipped: 0,
            records_before: before,
            records_after: after,
            duplicates_merged,
            tombstones_dropped,
        })
    }

    fn write_mirror_atomic(
        &self,
        mirror_path: &Path,
        survivors: &[Record],
    ) -> Result<(), StoreError> {
        let parent = mirror_path.parent().ok_or_else(|| StoreError::Io {
            path: mirror_path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "mirror path has no parent",
            ),
        })?;
        let tmp_name = format!(
            "{}.tmp-{}",
            mirror_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("mirror"),
            std::process::id()
        );
        let tmp_path = parent.join(tmp_name);
        {
            let mut file = File::create(&tmp_path).map_err(|e| StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;
            for record in survivors {
                let mut line = serde_json::to_string(record).expect("Record always serializes");
                line.push('\n');
                file.write_all(line.as_bytes())
                    .map_err(|e| StoreError::Io {
                        path: tmp_path.clone(),
                        source: e,
                    })?;
            }
            file.sync_all().map_err(|e| StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;
        }
        fs::rename(&tmp_path, mirror_path).map_err(|e| StoreError::Io {
            path: mirror_path.to_path_buf(),
            source: e,
        })?;
        #[cfg(unix)]
        {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    fn prune_empty_parents(&self, mirror_path: &Path) {
        let mut dir = mirror_path.parent();
        while let Some(d) = dir {
            if d == self.annot_root || !d.starts_with(&self.annot_root) {
                break;
            }
            match fs::read_dir(d) {
                Ok(mut entries) => {
                    if entries.next().is_some() || fs::remove_dir(d).is_err() {
                        break;
                    }
                    dir = d.parent();
                }
                Err(_) => break,
            }
        }
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out.into_iter().collect()
}

struct LockGuard {
    file: File,
}

impl LockGuard {
    fn shared(path: &Path) -> Result<Self, StoreError> {
        let file = open_lock_file(path)?;
        FileExt::lock_shared(&file).map_err(|e| StoreError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Ok(Self { file })
    }

    fn exclusive(path: &Path, opts: &CompactOptions) -> Result<Self, StoreError> {
        let file = open_lock_file(path)?;
        let mut attempt = 0u32;
        loop {
            match FileExt::try_lock(&file) {
                Ok(()) => return Ok(Self { file }),
                Err(fs4::TryLockError::WouldBlock) => {
                    attempt += 1;
                    if attempt >= opts.lock_retries {
                        return Err(StoreError::LockBusy);
                    }
                    std::thread::sleep(opts.lock_backoff);
                }
                Err(fs4::TryLockError::Error(e)) => {
                    return Err(StoreError::Io {
                        path: path.to_path_buf(),
                        source: e,
                    });
                }
            }
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn open_lock_file(path: &Path) -> Result<File, StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| StoreError::Io {
            path: path.to_path_buf(),
            source: e,
        })
}

/// Tuning knobs for exclusive-lock acquisition during compaction.
#[derive(Debug, Clone)]
pub struct CompactOptions {
    pub lock_retries: u32,
    pub lock_backoff: Duration,
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self {
            lock_retries: 40,
            lock_backoff: Duration::from_millis(50),
        }
    }
}

/// Outcome of a compaction pass, aggregate across files for `compact_all`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompactStats {
    pub files_compacted: usize,
    pub files_removed: usize,
    pub files_skipped: usize,
    pub records_before: usize,
    pub records_after: usize,
    pub duplicates_merged: usize,
    pub tombstones_dropped: usize,
}

impl CompactStats {
    /// Accumulates `other` into `self`, field by field.
    pub fn absorb(&mut self, other: CompactStats) {
        self.files_compacted += other.files_compacted;
        self.files_removed += other.files_removed;
        self.files_skipped += other.files_skipped;
        self.records_before += other.records_before;
        self.records_after += other.records_after;
        self.duplicates_merged += other.duplicates_merged;
        self.tombstones_dropped += other.tombstones_dropped;
    }
}

/// Errors from [`Store`] operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not inside a git repository (searched upward from {start})")]
    RepoNotFound { start: PathBuf },
    #[error("path {path} is outside the repository root {root}")]
    OutsideRepo { path: PathBuf, root: PathBuf },
    #[error("path {path} is inside the reserved {ANNOT_DIR} directory")]
    ReservedPath { path: PathBuf },
    #[error("malformed record at {path}:{line}")]
    MalformedLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("annotation store is locked by another annot process")]
    LockBusy,
    #[error("io error on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Anchor, Kind};
    use std::sync::Mutex;

    static CWD_GUARD: Mutex<()> = Mutex::new(());

    fn new_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_path_buf()).unwrap();
        (store, dir)
    }

    fn anchor() -> Anchor {
        Anchor {
            base_blob: "b".repeat(40),
            start: 1,
            end: 1,
            line_hashes: vec!["0000000000000000".to_string()],
            ctx_before: String::new(),
            ctx_after: String::new(),
            symbol: None,
        }
    }

    fn record(body: &str) -> Record {
        Record::new(Kind::Decision, body.to_string(), anchor())
    }

    #[test]
    fn mirror_path_mapping() {
        let (store, _dir) = new_store();
        assert_eq!(
            store
                .annot_path(store.repo_root().join("src/parser.rs"))
                .unwrap(),
            store.annot_root().join("src/parser.rs.jsonl")
        );
        assert_eq!(
            store
                .annot_path(store.repo_root().join("Makefile"))
                .unwrap(),
            store.annot_root().join("Makefile.jsonl")
        );
        assert_eq!(
            store.annot_path(store.repo_root().join(".env")).unwrap(),
            store.annot_root().join(".env.jsonl")
        );
        assert_eq!(
            store
                .annot_path(store.repo_root().join("a/b/c/deep.rs"))
                .unwrap(),
            store.annot_root().join("a/b/c/deep.rs.jsonl")
        );
    }

    #[test]
    fn discover_finds_repo_via_git_init() {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(dir.path())
            .status()
            .unwrap();
        assert!(status.success());

        let store = Store::discover(dir.path()).unwrap();
        let expected = fs::canonicalize(dir.path()).unwrap();
        assert_eq!(store.repo_root(), expected.as_path());
    }

    #[test]
    fn discover_fails_outside_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = Store::discover(dir.path()).unwrap_err();
        assert!(matches!(err, StoreError::RepoNotFound { .. }));
    }

    #[test]
    fn source_rel_rejects_escape_and_reserved() {
        let (store, _dir) = new_store();

        let outside = store.repo_root().parent().unwrap().join("elsewhere.rs");
        let err = store.source_rel(&outside).unwrap_err();
        assert!(matches!(err, StoreError::OutsideRepo { .. }));

        let escaped = store.repo_root().join("../escaped.rs");
        let err = store.source_rel(&escaped).unwrap_err();
        assert!(matches!(err, StoreError::OutsideRepo { .. }));

        let reserved = store.repo_root().join(".annot/foo.rs");
        let err = store.source_rel(&reserved).unwrap_err();
        assert!(matches!(err, StoreError::ReservedPath { .. }));

        let dotdot_relative_err = store.source_rel("../x").unwrap_err();
        assert!(matches!(
            dotdot_relative_err,
            StoreError::OutsideRepo { .. }
        ));

        let nested = store.repo_root().join("a/../b/./c.rs");
        assert_eq!(store.source_rel(&nested).unwrap(), PathBuf::from("b/c.rs"));
    }

    #[test]
    fn source_rel_resolves_relative_against_nested_cwd() {
        let _guard = CWD_GUARD.lock().unwrap();
        let (store, _dir) = new_store();
        let nested = store.repo_root().join("src/nested");
        fs::create_dir_all(&nested).unwrap();
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&nested).unwrap();
        let result = store.source_rel("thing.rs");
        std::env::set_current_dir(original_cwd).unwrap();
        assert_eq!(result.unwrap(), PathBuf::from("src/nested/thing.rs"));
    }

    #[test]
    fn append_creates_tree_and_trailing_newline() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");
        store.append(&source, &record("first")).unwrap();
        let mirror = store.annot_path(&source).unwrap();
        let bytes = fs::read(&mirror).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
    }

    #[test]
    fn append_then_read_raw_order_preserved() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");
        let records: Vec<Record> = (0..5).map(|i| record(&format!("r{i}"))).collect();
        store.append_all(&source, &records).unwrap();
        let raw = store.read_raw(&source).unwrap();
        assert_eq!(raw.len(), 5);
        for (r, expected) in raw.iter().zip(records.iter()) {
            assert_eq!(r.id, expected.id);
        }
    }

    #[test]
    fn missing_file_and_missing_annot_dir_read_empty() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/never.rs");
        assert_eq!(store.read_raw(&source).unwrap(), Vec::new());
        assert_eq!(store.load(&source).unwrap(), Vec::new());
    }

    #[test]
    fn load_last_wins_and_drops_tombstones() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");

        let mut a = record("a-v1");
        let a_id = a.id;
        store.append(&source, &a).unwrap();
        a.body = "a-v2".to_string();
        a.touch();
        store.append(&source, &a).unwrap();

        let b = record("b-live");
        let b_id = b.id;
        store.append(&source, &b).unwrap();

        let mut c = record("c-to-tombstone");
        let c_id = c.id;
        store.append(&source, &c).unwrap();
        c.mark_tombstone();
        store.append(&source, &c).unwrap();

        let raw = store.read_raw(&source).unwrap();
        assert_eq!(raw.len(), 5);

        let loaded = store.load(&source).unwrap();
        assert_eq!(loaded.len(), 2);
        let a_loaded = loaded.iter().find(|r| r.id == a_id).unwrap();
        assert_eq!(a_loaded.body, "a-v2");
        assert!(loaded.iter().any(|r| r.id == b_id));
        assert!(!loaded.iter().any(|r| r.id == c_id));

        let ids: Vec<Ulid> = loaded.iter().map(|r| r.id).collect();
        let mut ascending = ids.clone();
        ascending.sort();
        assert_eq!(
            ids, ascending,
            "load() must return records sorted by id ascending"
        );
    }

    #[test]
    fn malformed_interior_line_errors_with_line_number() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");
        store.append(&source, &record("ok1")).unwrap();
        let mirror = store.annot_path(&source).unwrap();
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&mirror).unwrap();
            writeln!(f, "not json").unwrap();
        }
        store.append(&source, &record("ok2")).unwrap();

        let err = store.read_raw(&source).unwrap_err();
        match err {
            StoreError::MalformedLine { line, .. } => assert_eq!(line, 2),
            other => panic!("expected MalformedLine, got {other:?}"),
        }
    }

    #[test]
    fn torn_trailing_line_skipped() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");
        store.append(&source, &record("ok1")).unwrap();
        let mirror = store.annot_path(&source).unwrap();
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&mirror).unwrap();
            write!(f, "{{\"id\":\"truncated").unwrap();
        }
        let raw = store.read_raw(&source).unwrap();
        assert_eq!(raw.len(), 1);
    }

    #[test]
    fn crlf_and_blank_lines_tolerated() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");
        let mirror = store.annot_path(&source).unwrap();
        fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        let r = record("crlf");
        let json = serde_json::to_string(&r).unwrap();
        let content = format!("\r\n{json}\r\n\r\n");
        fs::write(&mirror, content).unwrap();
        let raw = store.read_raw(&source).unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].id, r.id);
    }

    #[test]
    fn list_annotated_files_and_find_by_id() {
        let (store, _dir) = new_store();
        let a_src = store.repo_root().join("src/a.rs");
        let b_src = store.repo_root().join("z/b.rs");
        let a_rec = record("a");
        let b_rec = record("b");
        store.append(&a_src, &a_rec).unwrap();
        store.append(&b_src, &b_rec).unwrap();

        let files = store.list_annotated_files().unwrap();
        assert_eq!(
            files,
            vec![PathBuf::from("src/a.rs"), PathBuf::from("z/b.rs")]
        );

        let (found_path, found_record) = store.find(a_rec.id).unwrap().unwrap();
        assert_eq!(found_path, PathBuf::from("src/a.rs"));
        assert_eq!(found_record.id, a_rec.id);

        assert!(store.find(Ulid::generate()).unwrap().is_none());
    }

    #[test]
    fn compact_merges_dups_drops_tombstones_sorted_idempotent() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");

        let mut a = record("a-v1");
        store.append(&source, &a).unwrap();
        a.body = "a-v2".to_string();
        a.touch();
        store.append(&source, &a).unwrap();

        let b = record("b-live");
        store.append(&source, &b).unwrap();

        let mut c = record("c");
        store.append(&source, &c).unwrap();
        c.mark_tombstone();
        store.append(&source, &c).unwrap();

        let stats = store.compact_file(&source).unwrap();
        assert_eq!(stats.files_compacted, 1);
        assert_eq!(stats.files_removed, 0);
        assert_eq!(stats.files_skipped, 0);
        assert_eq!(stats.records_before, 5);
        assert_eq!(stats.records_after, 2);
        assert_eq!(stats.duplicates_merged, 2);
        assert_eq!(stats.tombstones_dropped, 1);

        let loaded = store.load(&source).unwrap();
        assert_eq!(loaded.len(), 2);
        let mut sorted = loaded.clone();
        sorted.sort_by_key(|r| r.id);
        assert_eq!(loaded, sorted);

        let second = store.compact_file(&source).unwrap();
        assert_eq!(second.files_compacted, 1);
        assert_eq!(second.records_before, 2);
        assert_eq!(second.records_after, 2);
        assert_eq!(second.duplicates_merged, 0);
        assert_eq!(second.tombstones_dropped, 0);
    }

    #[test]
    fn compact_deletes_empty_mirror() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");
        let mut r = record("gone");
        store.append(&source, &r).unwrap();
        r.mark_tombstone();
        store.append(&source, &r).unwrap();

        let stats = store.compact_file(&source).unwrap();
        assert_eq!(stats.files_removed, 1);
        assert_eq!(stats.records_after, 0);
        let mirror = store.annot_path(&source).unwrap();
        assert!(!mirror.exists());
        assert_eq!(store.load(&source).unwrap(), Vec::new());
    }

    #[test]
    fn compact_refuses_malformed_interior() {
        let (store, _dir) = new_store();
        let source = store.repo_root().join("src/lib.rs");
        store.append(&source, &record("ok1")).unwrap();
        let mirror = store.annot_path(&source).unwrap();
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&mirror).unwrap();
            writeln!(f, "not json").unwrap();
        }
        store.append(&source, &record("ok2")).unwrap();

        let before_bytes = fs::read(&mirror).unwrap();
        let err = store.compact_file(&source).unwrap_err();
        assert!(matches!(err, StoreError::MalformedLine { .. }));
        let after_bytes = fs::read(&mirror).unwrap();
        assert_eq!(before_bytes, after_bytes);
    }

    #[test]
    fn compact_all_skips_malformed_and_processes_rest() {
        let (store, _dir) = new_store();
        let bad_source = store.repo_root().join("src/bad.rs");
        let good_source = store.repo_root().join("src/good.rs");

        store.append(&bad_source, &record("ok")).unwrap();
        let bad_mirror = store.annot_path(&bad_source).unwrap();
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&bad_mirror).unwrap();
            writeln!(f, "not json").unwrap();
        }

        let mut good = record("g-v1");
        store.append(&good_source, &good).unwrap();
        good.body = "g-v2".to_string();
        good.touch();
        store.append(&good_source, &good).unwrap();

        let stats = store.compact_all().unwrap();
        assert_eq!(stats.files_skipped, 1);
        assert_eq!(stats.files_compacted, 1);
        assert_eq!(stats.duplicates_merged, 1);

        let good_loaded = store.load(&good_source).unwrap();
        assert_eq!(good_loaded.len(), 1);
        assert_eq!(good_loaded[0].body, "g-v2");
    }

    #[test]
    fn compact_lock_contention_returns_lock_busy() {
        let (store, _dir) = new_store();
        fs::create_dir_all(store.annot_root()).unwrap();
        let lock_path = store.annot_root().join(LOCK_FILE);
        let holder = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        FileExt::lock(&holder).unwrap();

        let opts = CompactOptions {
            lock_retries: 2,
            lock_backoff: Duration::from_millis(10),
        };
        let err = store.compact_all_opts(&opts).unwrap_err();
        assert!(matches!(err, StoreError::LockBusy));

        FileExt::unlock(&holder).unwrap();
    }
}
