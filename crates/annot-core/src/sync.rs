//! Anchor creation and drift healing between stored records and working-tree files.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use imara_diff::{Algorithm, Diff, InternedInput};

use crate::model::{Anchor, Record, Status, Ulid};
use crate::store::{Store, StoreError};

// Lines of context hashed on each side of an anchor.
const CTX_LINES: usize = 3;
// Largest internal insertion/deletion tolerated while re-aligning an anchor.
const MAX_INTERNAL_DRIFT: i64 = 2;
const W_POS_BP: u64 = 7_000;
const W_CTX_BP: u64 = 1_500;
const ACCEPT_BP: u64 = 6_000;
const ACCEPT_BP_SHORT: u64 = 8_000;
const SHORT_ANCHOR_LINES: u32 = 2;
const SCAN_OP_BUDGET: u64 = 5_000_000;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// A git-repository handle used to anchor annotations and heal them as files drift.
pub struct Syncer {
    repo: gix::Repository,
    workdir: PathBuf,
}

impl Syncer {
    /// Discovers the enclosing repository upward from `dir`. `Err(Repo)` if there
    /// is no repository, or it is bare (no working directory).
    pub fn open(dir: &Path) -> Result<Syncer, SyncError> {
        let repo = gix::discover(dir).map_err(|e| SyncError::Repo(Box::new(e)))?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| {
                SyncError::Repo(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("bare repository at {} has no working tree", dir.display()),
                )))
            })?
            .to_path_buf();
        Ok(Syncer { repo, workdir })
    }

    /// The repository's working directory.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Builds an anchor for the current working-tree bytes of `rel_path` over the
    /// 1-based inclusive line range `start..=end`. Writes the file content into the
    /// object database so the anchored bytes stay recoverable, and returns the exact
    /// anchored text alongside the anchor.
    pub fn make_anchor(
        &self,
        rel_path: &Path,
        start: u32,
        end: u32,
    ) -> Result<NewAnchor, SyncError> {
        let abs = self.workdir.join(rel_path);
        let bytes = std::fs::read(&abs).map_err(|e| SyncError::Io {
            path: abs,
            source: e,
        })?;
        if is_binary(&bytes) {
            return Err(SyncError::BinaryFile {
                path: rel_path.to_path_buf(),
            });
        }
        let index = FileIndex::build(&bytes);
        let line_count = index.len() as u32;
        if start < 1 || start > end || end > line_count {
            return Err(SyncError::InvalidRange {
                path: rel_path.to_path_buf(),
                start,
                end,
                line_count,
            });
        }
        let oid = self
            .repo
            .write_blob(&bytes)
            .map_err(|e| SyncError::Odb(Box::new(e)))?
            .detach();

        let (a, b) = ((start - 1) as usize, end as usize);
        Ok(NewAnchor {
            anchor: Anchor {
                base_blob: oid.to_hex().to_string(),
                start,
                end,
                line_hashes: index.hex_hashes(a, b),
                ctx_before: hex16(index.ctx_before_at[a]),
                ctx_after: hex16(index.ctx_after_at[b]),
                symbol: None,
            },
            snippet: index.snippet(a, b),
        })
    }

    /// Syncs every record against the current content of `rel_path`, mutating them
    /// in place. Performs no store I/O; `outcomes[i]` describes `records[i]`.
    pub fn sync_file(
        &self,
        rel_path: &Path,
        records: &mut [Record],
    ) -> Result<Vec<SyncOutcome>, SyncError> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let abs = self.workdir.join(rel_path);
        let bytes = match std::fs::read(&abs) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(orphan_all(records, OrphanReason::FileMissing));
            }
            Err(e) => {
                return Err(SyncError::Io {
                    path: abs,
                    source: e,
                })
            }
        };
        if is_binary(&bytes) {
            return Ok(orphan_all(records, OrphanReason::LowConfidence));
        }

        let cur_hex = blob_oid(&bytes)?.to_hex().to_string();
        let mut index: Option<FileIndex<'_>> = None;
        let mut hunk_cache: Vec<(String, Option<Vec<HunkRanges>>)> = Vec::new();
        let mut outcomes = Vec::with_capacity(records.len());

        for record in records.iter_mut() {
            if record.status != Status::Live {
                outcomes.push(SyncOutcome::SkippedNotLive);
                continue;
            }
            if record.anchor.base_blob == cur_hex {
                outcomes.push(SyncOutcome::Unchanged);
                continue;
            }
            if index.is_none() {
                self.repo
                    .write_blob(&bytes)
                    .map_err(|e| SyncError::Odb(Box::new(e)))?;
                index = Some(FileIndex::build(&bytes));
            }
            let index = index.as_ref().expect("index built above");

            if !hunk_cache
                .iter()
                .any(|(blob, _)| blob == &record.anchor.base_blob)
            {
                let hunks = self.hunks_since(&record.anchor.base_blob, &bytes);
                hunk_cache.push((record.anchor.base_blob.clone(), hunks));
            }
            let hunks = hunk_cache
                .iter()
                .find(|(blob, _)| blob == &record.anchor.base_blob)
                .and_then(|(_, hunks)| hunks.as_deref());

            outcomes.push(sync_one(record, index, &cur_hex, hunks));
        }
        Ok(outcomes)
    }

    /// Single-record convenience wrapper over [`Syncer::sync_file`].
    pub fn sync_annotation(
        &self,
        rel_path: &Path,
        record: &mut Record,
    ) -> Result<SyncOutcome, SyncError> {
        let mut outcomes = self.sync_file(rel_path, std::slice::from_mut(record))?;
        Ok(outcomes
            .pop()
            .expect("sync_file yields one outcome per record"))
    }

    // Hunks from the blob named by `base_blob` to `new_bytes`. `None` when the blob
    // is unreadable (pruned, corrupt id, wrong kind), which forces the hash-only path.
    fn hunks_since(&self, base_blob: &str, new_bytes: &[u8]) -> Option<Vec<HunkRanges>> {
        let oid = gix::ObjectId::from_hex(base_blob.as_bytes()).ok()?;
        let object = self.repo.try_find_object(oid).ok()??;
        if object.kind != gix::objs::Kind::Blob {
            return None;
        }
        let input = InternedInput::new(object.data.as_slice(), new_bytes);
        let diff = Diff::compute(Algorithm::Histogram, &input);
        Some(
            diff.hunks()
                .map(|h| HunkRanges {
                    before: h.before,
                    after: h.after,
                })
                .collect(),
        )
    }
}

/// A freshly computed anchor together with the text it covers.
#[derive(Debug, Clone)]
pub struct NewAnchor {
    pub anchor: Anchor,
    pub snippet: String,
}

/// What a sync pass did to one record.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncOutcome {
    Unchanged,
    Refreshed,
    Shifted {
        delta: i64,
    },
    Rematched {
        new_start: u32,
        new_end: u32,
        confidence: f32,
    },
    Orphaned {
        reason: OrphanReason,
    },
    SkippedNotLive,
}

/// Why a record lost its anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanReason {
    LowConfidence,
    FileMissing,
}

/// Post-sync state of one annotated file.
#[derive(Debug, Clone)]
pub struct FileSyncReport {
    pub rel_path: PathBuf,
    pub records: Vec<Record>,
    pub outcomes: Vec<(Ulid, SyncOutcome)>,
    pub written_back: bool,
}

/// Post-sync state of a whole tree.
#[derive(Debug, Clone)]
pub struct SyncReport {
    pub files: Vec<FileSyncReport>,
}

/// Loads `rel_path`'s records, syncs them, and appends the mutated ones back.
/// A failed append is reported as `written_back == false`, not an error: the
/// returned records already carry the healed state.
pub fn sync_path(
    syncer: &Syncer,
    store: &Store,
    rel_path: &Path,
) -> Result<FileSyncReport, SyncError> {
    let source = store.repo_root().join(rel_path);
    let rel = store.source_rel(&source)?;
    let mut records = store.load(&source)?;
    let outcomes = syncer.sync_file(&rel, &mut records)?;

    let changed: Vec<Record> = records
        .iter()
        .zip(outcomes.iter())
        .filter(|(_, outcome)| mutates_record(outcome))
        .map(|(record, _)| record.clone())
        .collect();
    let written_back = changed.is_empty() || store.append_all(&source, &changed).is_ok();

    Ok(FileSyncReport {
        rel_path: rel,
        outcomes: records.iter().map(|r| r.id).zip(outcomes).collect(),
        records,
        written_back,
    })
}

/// Syncs every annotated file, optionally restricted to those under `subpath`.
pub fn sync_tree(
    syncer: &Syncer,
    store: &Store,
    subpath: Option<&Path>,
) -> Result<SyncReport, SyncError> {
    let filter = match subpath {
        Some(p) => Some(store.source_rel(store.repo_root().join(p))?),
        None => None,
    };
    let mut files = Vec::new();
    for rel in store.list_annotated_files()? {
        if filter.as_ref().is_some_and(|f| !rel.starts_with(f)) {
            continue;
        }
        files.push(sync_path(syncer, store, &rel)?);
    }
    Ok(SyncReport { files })
}

/// Errors from anchoring and syncing.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("not inside a git repository (or bare repo): {0}")]
    Repo(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("failed to read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid line range {start}:{end} for {path} ({line_count} lines)")]
    InvalidRange {
        path: PathBuf,
        start: u32,
        end: u32,
        line_count: u32,
    },
    #[error("refusing to anchor into binary file {path}")]
    BinaryFile { path: PathBuf },
    #[error("git object database access failed: {0}")]
    Odb(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Clone)]
struct HunkRanges {
    before: std::ops::Range<u32>,
    after: std::ops::Range<u32>,
}

// Line view of one file version: raw lines, per-line hashes, and the context
// hashes for every possible span boundary `0..=n`.
struct FileIndex<'a> {
    lines: Vec<&'a [u8]>,
    line_hashes: Vec<u64>,
    ctx_before_at: Vec<u64>,
    ctx_after_at: Vec<u64>,
}

impl<'a> FileIndex<'a> {
    fn build(bytes: &'a [u8]) -> Self {
        let lines = split_lines(bytes);
        let n = lines.len();
        let line_hashes = lines.iter().map(|l| hash64(&[normalize(l)])).collect();
        let ctx_before_at = (0..=n)
            .map(|p| ctx_hash(&lines[p.saturating_sub(CTX_LINES)..p]))
            .collect();
        let ctx_after_at = (0..=n)
            .map(|p| ctx_hash(&lines[p..(p + CTX_LINES).min(n)]))
            .collect();
        FileIndex {
            lines,
            line_hashes,
            ctx_before_at,
            ctx_after_at,
        }
    }

    fn len(&self) -> usize {
        self.lines.len()
    }

    fn hex_hashes(&self, a: usize, b: usize) -> Vec<String> {
        self.line_hashes[a..b].iter().map(|&h| hex16(h)).collect()
    }

    fn snippet(&self, a: usize, b: usize) -> String {
        self.lines[a..b]
            .iter()
            .map(|l| String::from_utf8_lossy(l))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn sync_one(
    record: &mut Record,
    index: &FileIndex<'_>,
    cur_hex: &str,
    hunks: Option<&[HunkRanges]>,
) -> SyncOutcome {
    let anchor = &record.anchor;
    let degenerate =
        anchor.start == 0 || anchor.start > anchor.end || anchor.line_hashes.is_empty();
    if degenerate {
        record.orphan();
        return SyncOutcome::Orphaned {
            reason: OrphanReason::LowConfidence,
        };
    }

    let a = record.anchor.start.saturating_sub(1) as usize;
    let b = record.anchor.end as usize;
    let well_formed = b > a && !record.anchor.line_hashes.is_empty();

    let expected = match hunks {
        Some(hunks) if well_formed => {
            let (delta, overlap) = classify_hunks(hunks, a as u32, b as u32);
            let (new_a, new_b) = (a as i64 + delta, b as i64 + delta);
            if !overlap && new_a >= 0 && new_b <= index.len() as i64 {
                heal(record, index, cur_hex, new_a as usize, new_b as usize);
                return if delta == 0 {
                    SyncOutcome::Refreshed
                } else {
                    SyncOutcome::Shifted { delta }
                };
            }
            new_a
        }
        // Hash-only path: the old blob is gone, so the old position is the best hint.
        _ => a as i64,
    };

    match rematch(&record.anchor, index, expected) {
        Some(m) => {
            heal(record, index, cur_hex, m.start, m.start + m.len);
            SyncOutcome::Rematched {
                new_start: (m.start + 1) as u32,
                new_end: (m.start + m.len) as u32,
                confidence: m.score_bp as f32 / 10_000.0,
            }
        }
        None => {
            record.orphan();
            SyncOutcome::Orphaned {
                reason: OrphanReason::LowConfidence,
            }
        }
    }
}

// Rebases a record onto `[a, b)` of the current content, refreshing every
// content-derived anchor field so the next sync short-circuits as `Unchanged`.
fn heal(record: &mut Record, index: &FileIndex<'_>, cur_hex: &str, a: usize, b: usize) {
    record.anchor.base_blob = cur_hex.to_string();
    record.anchor.start = (a + 1) as u32;
    record.anchor.end = b as u32;
    record.anchor.line_hashes = index.hex_hashes(a, b);
    record.anchor.ctx_before = hex16(index.ctx_before_at[a]);
    record.anchor.ctx_after = hex16(index.ctx_after_at[b]);
    record.orig_snippet = Some(index.snippet(a, b));
    record.touch();
}

fn orphan_all(records: &mut [Record], reason: OrphanReason) -> Vec<SyncOutcome> {
    records
        .iter_mut()
        .map(|record| {
            if record.status != Status::Live {
                SyncOutcome::SkippedNotLive
            } else {
                record.orphan();
                SyncOutcome::Orphaned { reason }
            }
        })
        .collect()
}

fn mutates_record(outcome: &SyncOutcome) -> bool {
    matches!(
        outcome,
        SyncOutcome::Refreshed
            | SyncOutcome::Shifted { .. }
            | SyncOutcome::Rematched { .. }
            | SyncOutcome::Orphaned { .. }
    )
}

// Net line delta contributed by hunks entirely above `[a, b)`, plus whether any
// hunk touches the span. An empty before-range is an insertion point `p`: `p <= a`
// pushes the anchor down, `p >= b` is below it, anything between is an overlap.
fn classify_hunks(hunks: &[HunkRanges], a: u32, b: u32) -> (i64, bool) {
    let mut delta = 0i64;
    let mut overlap = false;
    for hunk in hunks {
        if hunk.before.end <= a {
            delta += i64::from(hunk.after.end - hunk.after.start)
                - i64::from(hunk.before.end - hunk.before.start);
        } else if hunk.before.start < b {
            overlap = true;
        }
    }
    (delta, overlap)
}

struct MatchResult {
    start: usize,
    len: usize,
    score_bp: u64,
}

// Deterministic candidate order (ties resolved by scan order): highest score,
// then closest to the expected position, then the tightest window, then the
// earliest start.
type ScoreKey = (u64, Reverse<i64>, Reverse<i64>, Reverse<usize>);

// Whole-file candidate scan for the anchor's lines, tolerating up to
// [`MAX_INTERNAL_DRIFT`] inserted or deleted lines inside the span.
fn rematch(anchor: &Anchor, index: &FileIndex<'_>, expected: i64) -> Option<MatchResult> {
    let k = anchor.line_hashes.len();
    let n = index.line_hashes.len();
    if k == 0 || n == 0 || (n as u64).saturating_mul(k as u64) > SCAN_OP_BUDGET {
        return None;
    }
    let wanted: Vec<Option<u64>> = anchor.line_hashes.iter().map(|h| parse_hash(h)).collect();
    let ctx_before = parse_hash(&anchor.ctx_before);
    let ctx_after = parse_hash(&anchor.ctx_after);
    let accept = if k <= SHORT_ANCHOR_LINES as usize {
        ACCEPT_BP_SHORT
    } else {
        ACCEPT_BP
    };

    let mut prefix = vec![0usize; k + 1];
    let mut suffix = vec![0usize; k + 1];
    let mut best: Option<(ScoreKey, MatchResult)> = None;

    for d in -MAX_INTERNAL_DRIFT..=MAX_INTERNAL_DRIFT {
        let span = k as i64 + d;
        if span < 1 || span > n as i64 {
            continue;
        }
        let span = span as usize;
        for start in 0..=(n - span) {
            let matched = best_alignment(
                &wanted,
                &index.line_hashes,
                start,
                d,
                &mut prefix,
                &mut suffix,
            );
            let score = W_POS_BP * matched as u64 / k as u64
                + ctx_bonus(index.ctx_before_at[start], ctx_before)
                + ctx_bonus(index.ctx_after_at[start + span], ctx_after);
            let key = (
                score,
                Reverse((start as i64 - expected).abs()),
                Reverse(d.abs()),
                Reverse(start),
            );
            if best.as_ref().is_none_or(|(best_key, _)| key > *best_key) {
                best = Some((
                    key,
                    MatchResult {
                        start,
                        len: span,
                        score_bp: score,
                    },
                ));
            }
        }
    }

    best.map(|(_, m)| m).filter(|m| m.score_bp >= accept)
}

fn ctx_bonus(actual: u64, wanted: Option<u64>) -> u64 {
    if wanted == Some(actual) {
        W_CTX_BP
    } else {
        0
    }
}

// Anchor lines matched inside the window `[start, start + k + d)`, allowing one
// internal split where `d` lines were inserted (`d > 0`) or deleted (`d < 0`).
fn best_alignment(
    wanted: &[Option<u64>],
    actual: &[u64],
    start: usize,
    d: i64,
    prefix: &mut [usize],
    suffix: &mut [usize],
) -> usize {
    let k = wanted.len();
    if d == 0 {
        return (0..k)
            .filter(|&i| wanted[i] == Some(actual[start + i]))
            .count();
    }
    let head_len = if d > 0 { k } else { k - (-d) as usize };
    prefix[0] = 0;
    for g in 1..=head_len {
        prefix[g] = prefix[g - 1] + usize::from(wanted[g - 1] == Some(actual[start + g - 1]));
    }
    suffix[head_len] = 0;
    let gap = d.unsigned_abs() as usize;
    for g in (0..head_len).rev() {
        let matched = if d > 0 {
            wanted[g] == Some(actual[start + g + gap])
        } else {
            wanted[g + gap] == Some(actual[start + g])
        };
        suffix[g] = suffix[g + 1] + usize::from(matched);
    }
    (0..=head_len)
        .map(|g| prefix[g] + suffix[g])
        .max()
        .unwrap_or(0)
}

fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

fn normalize(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && line[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &line[..end]
}

// Raw SHA-1 (no git object header) of `parts` joined with `\n`, truncated to its
// leading 64 bits. Zero parts hashes the empty input.
fn hash64(parts: &[&[u8]]) -> u64 {
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update(b"\n");
        }
        hasher.update(part);
    }
    match hasher.try_finalize() {
        Ok(id) => u64::from_be_bytes(
            id.as_bytes()[..8]
                .try_into()
                .expect("sha1 digests are 20 bytes"),
        ),
        Err(_) => 0,
    }
}

fn ctx_hash(lines: &[&[u8]]) -> u64 {
    let normalized: Vec<&[u8]> = lines.iter().map(|l| normalize(l)).collect();
    hash64(&normalized)
}

fn hex16(hash: u64) -> String {
    format!("{hash:016x}")
}

fn parse_hash(hex: &str) -> Option<u64> {
    if hex.len() != 16 {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

fn blob_oid(bytes: &[u8]) -> Result<gix::ObjectId, SyncError> {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, bytes)
        .map_err(|e| SyncError::Odb(Box::new(e)))
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(before: std::ops::Range<u32>, after: std::ops::Range<u32>) -> HunkRanges {
        HunkRanges { before, after }
    }

    fn lines_of(text: &str) -> Vec<String> {
        split_lines(text.as_bytes())
            .iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect()
    }

    #[test]
    fn split_lines_drops_only_the_trailing_newline_segment() {
        assert!(lines_of("").is_empty());
        assert_eq!(lines_of("a\n"), ["a"]);
        assert_eq!(lines_of("a"), ["a"]);
        assert_eq!(lines_of("a\nb\n"), ["a", "b"]);
        assert_eq!(lines_of("a\n\n"), ["a", ""]);
        assert_eq!(lines_of("\n"), [""]);
    }

    #[test]
    fn hashes_are_16_hex_and_ignore_trailing_whitespace() {
        let plain = hash64(&[normalize(b"let x = 1;")]);
        assert_eq!(plain, hash64(&[normalize(b"let x = 1;  \t")]));
        assert_eq!(plain, hash64(&[normalize(b"let x = 1;\r")]));
        assert_ne!(plain, hash64(&[normalize(b"let x = 2;")]));

        let hex = hex16(plain);
        assert_eq!(hex.len(), 16);
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        assert_eq!(parse_hash(&hex), Some(plain));
        assert_eq!(parse_hash("nope"), None);
    }

    #[test]
    fn zero_line_context_hashes_the_empty_input() {
        let empty: [&[u8]; 0] = [];
        assert_eq!(ctx_hash(&[]), hash64(&empty));
        assert_ne!(ctx_hash(&[]), hash64(&[b"".as_slice(), b"".as_slice()]));
    }

    #[test]
    fn file_index_context_covers_both_file_edges() {
        let index = FileIndex::build(b"l1\nl2\nl3\nl4\nl5\n");
        assert_eq!(index.len(), 5);
        assert_eq!(index.ctx_before_at[0], ctx_hash(&[]));
        assert_eq!(index.ctx_after_at[5], ctx_hash(&[]));
        assert_eq!(
            index.ctx_before_at[4],
            ctx_hash(&[b"l2".as_slice(), b"l3", b"l4"])
        );
        assert_eq!(
            index.ctx_after_at[1],
            ctx_hash(&[b"l2".as_slice(), b"l3", b"l4"])
        );
        assert_eq!(index.snippet(1, 3), "l2\nl3");
        assert_eq!(index.hex_hashes(1, 3).len(), 2);
    }

    #[test]
    fn classify_hunks_boundary_cases() {
        // Anchor [3, 8) in old coordinates.
        let (a, b) = (3, 8);

        // Insertion exactly at the anchor's first line shifts it down.
        assert_eq!(classify_hunks(&[hunk(3..3, 3..5)], a, b), (2, false));
        // Insertion exactly after the last anchor line is below.
        assert_eq!(classify_hunks(&[hunk(8..8, 8..9)], a, b), (0, false));
        // Insertion strictly inside overlaps.
        assert!(classify_hunks(&[hunk(5..5, 5..6)], a, b).1);
        // Non-empty hunk ending exactly at the anchor start is above.
        assert_eq!(classify_hunks(&[hunk(1..3, 1..2)], a, b), (-1, false));
        // Non-empty hunk starting exactly at the anchor end is below.
        assert_eq!(classify_hunks(&[hunk(8..10, 8..14)], a, b), (0, false));
        // A hunk covering only the anchor's first line overlaps.
        assert!(classify_hunks(&[hunk(3..4, 3..4)], a, b).1);
        // Deltas accumulate across every above-hunk.
        assert_eq!(
            classify_hunks(
                &[hunk(0..0, 0..3), hunk(1..2, 4..4), hunk(9..9, 11..12)],
                a,
                b
            ),
            (2, false)
        );
    }

    #[test]
    fn best_alignment_tolerates_one_internal_gap() {
        let wanted: Vec<Option<u64>> = (0..5).map(Some).collect();
        let mut prefix = vec![0usize; wanted.len() + 1];
        let mut suffix = vec![0usize; wanted.len() + 1];

        let exact = [0u64, 1, 2, 3, 4];
        assert_eq!(
            best_alignment(&wanted, &exact, 0, 0, &mut prefix, &mut suffix),
            5
        );

        // One line inserted in the middle: all 5 anchor lines still align.
        let inserted = [0u64, 1, 99, 2, 3, 4];
        assert_eq!(
            best_alignment(&wanted, &inserted, 0, 1, &mut prefix, &mut suffix),
            5
        );
        assert_eq!(
            best_alignment(&wanted, &inserted, 0, 0, &mut prefix, &mut suffix),
            2
        );

        // One anchor line deleted: the remaining 4 align across the gap.
        let deleted = [0u64, 1, 3, 4];
        assert_eq!(
            best_alignment(&wanted, &deleted, 0, -1, &mut prefix, &mut suffix),
            4
        );

        // An unparseable stored hash can never match.
        let mut broken = wanted.clone();
        broken[2] = None;
        assert_eq!(
            best_alignment(&broken, &exact, 0, 0, &mut prefix, &mut suffix),
            4
        );
    }

    #[test]
    fn rematch_prefers_the_tighter_window_on_a_score_tie() {
        // At s=0 both d=0 and d=+1 recover all 3 anchor lines; the tie must go to d=0.
        let index = FileIndex::build(b"a\nb\nc\nd\ne\nf\n");
        let anchor = Anchor {
            base_blob: "0".repeat(40),
            start: 1,
            end: 3,
            line_hashes: index.hex_hashes(0, 3),
            ctx_before: hex16(0xdead_beef_dead_beef),
            ctx_after: hex16(0xdead_beef_dead_beef),
            symbol: None,
        };
        let m = rematch(&anchor, &index, 0).expect("positional-only match is accepted");
        assert_eq!((m.start, m.len, m.score_bp), (0, 3, W_POS_BP));
    }

    #[test]
    fn rematch_holds_short_anchors_to_the_strict_bar() {
        let index = FileIndex::build(b"a\nb\nc\nd\ne\nf\n");
        let two_line = Anchor {
            base_blob: "0".repeat(40),
            start: 1,
            end: 2,
            line_hashes: index.hex_hashes(0, 2),
            ctx_before: hex16(0xdead_beef_dead_beef),
            ctx_after: hex16(0xdead_beef_dead_beef),
            symbol: None,
        };
        assert!(rematch(&two_line, &index, 0).is_none());

        let mut with_ctx = two_line.clone();
        with_ctx.ctx_before = hex16(index.ctx_before_at[0]);
        with_ctx.ctx_after = hex16(index.ctx_after_at[2]);
        let m = rematch(&with_ctx, &index, 0).expect("full positional + context clears 0.80");
        assert_eq!(m.score_bp, W_POS_BP + 2 * W_CTX_BP);
    }

    #[test]
    fn rematch_bails_out_over_the_op_budget() {
        let mut text = String::new();
        for i in 0..40 {
            text.push_str(&format!("line {i}\n"));
        }
        let index = FileIndex::build(text.as_bytes());
        let anchor = Anchor {
            base_blob: "0".repeat(40),
            start: 1,
            end: 3,
            line_hashes: vec![hex16(0); (SCAN_OP_BUDGET as usize / index.len()) + 1],
            ctx_before: hex16(0),
            ctx_after: hex16(0),
            symbol: None,
        };
        assert!(rematch(&anchor, &index, 0).is_none());
    }

    #[test]
    fn binary_sniff_only_reads_the_first_8kib() {
        let mut late_nul = vec![b'a'; BINARY_SNIFF_BYTES];
        late_nul.push(0);
        assert!(!is_binary(&late_nul));

        let mut early_nul = vec![b'a'; 16];
        early_nul.push(0);
        assert!(is_binary(&early_nul));
        assert!(!is_binary(b""));
    }
}
