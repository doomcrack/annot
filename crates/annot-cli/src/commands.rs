use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use annot_core::model::{Kind, Record, Status, Ulid};
use annot_core::store::Store;
use annot_core::sync::{self, Syncer};

use crate::cli::Command;
use crate::context;

pub fn run(command: Command) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let syncer = Syncer::open(&cwd)?;
    let store = Store::open(syncer.workdir())?;

    match command {
        Command::Add {
            file,
            range,
            kind,
            body,
            symbol,
        } => add(&syncer, &store, &file, &range, &kind, body, symbol),
        Command::Get {
            file,
            format,
            kinds,
            max_tokens,
        } => get(&syncer, &store, &file, &format, kinds, max_tokens),
        Command::Sync { path } => sync_cmd(&syncer, &store, path),
        Command::Orphans => orphans(&store),
        Command::Resolve { id, reanchor, drop } => resolve(&syncer, &store, &id, reanchor, drop),
        Command::Compact => compact(&store),
    }
}

fn abs(store: &Store, rel: &Path) -> PathBuf {
    store.repo_root().join(rel)
}

fn add(
    syncer: &Syncer,
    store: &Store,
    file: &Path,
    range: &str,
    kind: &str,
    body: String,
    symbol: Option<String>,
) -> Result<()> {
    let kind: Kind = kind.parse()?;
    let (start, end) = parse_range(range)?;
    let rel = store.source_rel(file)?;
    let new_anchor = syncer.make_anchor(&rel, start, end)?;
    let mut record = Record::new(kind, body, new_anchor.anchor);
    record.orig_snippet = Some(new_anchor.snippet);
    record.anchor.symbol = symbol;
    store.append(abs(store, &rel), &record)?;
    println!("{}", record.id);
    Ok(())
}

fn parse_range(raw: &str) -> Result<(u32, u32)> {
    let (a, b) = raw
        .split_once(':')
        .ok_or_else(|| anyhow!("range must be START:END, got {raw:?}"))?;
    let start: u32 = a
        .parse()
        .with_context(|| format!("invalid start in range {raw:?}"))?;
    let end: u32 = b
        .parse()
        .with_context(|| format!("invalid end in range {raw:?}"))?;
    Ok((start, end))
}

fn get(
    syncer: &Syncer,
    store: &Store,
    file: &Path,
    format: &str,
    kinds: Option<String>,
    max_tokens: Option<usize>,
) -> Result<()> {
    let rel = store.source_rel(file)?;
    let source_abs = abs(store, &rel);
    if !source_abs.exists() && !store.annot_path(&source_abs)?.exists() {
        bail!("no such file or annotations: {}", rel.display());
    }
    let records = match sync::sync_path(syncer, store, &rel) {
        Ok(report) => report.records,
        Err(err) => {
            eprintln!(
                "warning: sync failed for {}: {err}; showing possibly stale data",
                rel.display()
            );
            store.load(abs(store, &rel))?
        }
    };

    match format {
        "context" => {
            let kind_filter = kinds.as_deref().map(parse_kinds).transpose()?;
            let file_rel = rel.display().to_string();
            let out = context::render(&file_rel, &records, kind_filter.as_deref(), max_tokens);
            print!("{out}");
        }
        "json" => {
            if max_tokens.is_some() {
                eprintln!("warning: --max-tokens is ignored for --format=json");
            }
            println!("{}", serde_json::to_string(&records)?);
        }
        other => bail!("unknown format {other:?} (expected context|json)"),
    }
    Ok(())
}

fn parse_kinds(raw: &str) -> Result<Vec<Kind>> {
    let kinds: Vec<Kind> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<Kind>().map_err(anyhow::Error::from))
        .collect::<Result<_>>()?;
    if kinds.is_empty() {
        bail!("--kinds requires at least one of decision,gotcha,todo,history");
    }
    Ok(kinds)
}

fn sync_cmd(syncer: &Syncer, store: &Store, path: Option<PathBuf>) -> Result<()> {
    let report = match path {
        Some(p) => {
            let rel = store.source_rel(&p)?;
            let source_abs = abs(store, &rel);
            if source_abs.is_dir() {
                let subpath = if rel.as_os_str().is_empty() {
                    None
                } else {
                    Some(rel.as_path())
                };
                sync::sync_tree(syncer, store, subpath)?
            } else if source_abs.exists() || store.annot_path(&source_abs)?.exists() {
                let file_report = sync::sync_path(syncer, store, &rel)?;
                sync::SyncReport {
                    files: vec![file_report],
                }
            } else {
                bail!("no such file or annotations: {}", rel.display());
            }
        }
        None => sync::sync_tree(syncer, store, None)?,
    };

    let mut unchanged = 0usize;
    let mut shifted = 0usize;
    let mut reanchored = 0usize;
    let mut orphaned = 0usize;
    for file in &report.files {
        for (_, outcome) in &file.outcomes {
            match outcome {
                sync::SyncOutcome::Unchanged | sync::SyncOutcome::Refreshed => unchanged += 1,
                sync::SyncOutcome::Shifted { .. } => shifted += 1,
                sync::SyncOutcome::Rematched { .. } => reanchored += 1,
                sync::SyncOutcome::Orphaned { .. } => orphaned += 1,
                sync::SyncOutcome::SkippedNotLive => {}
            }
        }
    }

    println!(
        "synced {} file(s): {unchanged} unchanged, {shifted} shifted, {reanchored} reanchored, {orphaned} orphaned",
        report.files.len(),
    );
    Ok(())
}

fn orphans(store: &Store) -> Result<()> {
    let mut rows: Vec<(PathBuf, Record)> = Vec::new();
    for rel in store.list_annotated_files()? {
        let records = store.load(abs(store, &rel))?;
        for record in records.into_iter().filter(|r| r.status == Status::Orphaned) {
            rows.push((rel.clone(), record));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));

    for (rel, record) in rows {
        let snippet_line = record
            .orig_snippet
            .as_deref()
            .unwrap_or(record.body.as_str())
            .lines()
            .next()
            .unwrap_or("");
        println!(
            "{}  {}:{}-{}  {snippet_line}",
            record.id,
            rel.display(),
            record.anchor.start,
            record.anchor.end,
        );
    }
    Ok(())
}

fn resolve(
    syncer: &Syncer,
    store: &Store,
    id: &str,
    reanchor: Option<String>,
    drop: bool,
) -> Result<()> {
    let ulid = Ulid::from_string(id).with_context(|| format!("invalid annotation id {id:?}"))?;
    let (old_rel, mut record) = store
        .find(ulid)?
        .ok_or_else(|| anyhow!("no annotation found with id {id}"))?;

    if drop {
        record.mark_tombstone();
        store.append(abs(store, &old_rel), &record)?;
        println!("dropped {}", record.id);
        return Ok(());
    }

    let target_raw = reanchor.ok_or_else(|| anyhow!("resolve requires --reanchor or --drop"))?;
    let (file_part, start, end) = parse_reanchor_target(&target_raw)?;
    let target_rel = match file_part {
        Some(f) => store.source_rel(f)?,
        None => old_rel.clone(),
    };

    let original = record.clone();
    let symbol = record.anchor.symbol.clone();
    let new_anchor = syncer.make_anchor(&target_rel, start, end)?;
    record.status = Status::Live;
    record.anchor = new_anchor.anchor;
    record.anchor.symbol = symbol;
    record.orig_snippet = Some(new_anchor.snippet);
    record.touch();
    store.append(abs(store, &target_rel), &record)?;

    if target_rel != old_rel {
        let mut stale = original;
        stale.mark_tombstone();
        store.append(abs(store, &old_rel), &stale)?;
    }
    println!("reanchored {}", record.id);
    Ok(())
}

fn parse_reanchor_target(raw: &str) -> Result<(Option<String>, u32, u32)> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() < 2 {
        bail!("--reanchor must be [<file>:]<start>:<end>, got {raw:?}");
    }
    let end_str = parts[parts.len() - 1];
    let start_str = parts[parts.len() - 2];
    let start: u32 = start_str
        .parse()
        .with_context(|| format!("invalid start in --reanchor {raw:?}"))?;
    let end: u32 = end_str
        .parse()
        .with_context(|| format!("invalid end in --reanchor {raw:?}"))?;
    let file_part = if parts.len() > 2 {
        Some(parts[..parts.len() - 2].join(":"))
    } else {
        None
    };
    Ok((file_part, start, end))
}

fn compact(store: &Store) -> Result<()> {
    let stats = store.compact_all()?;
    let mut line = format!(
        "compacted {} file(s): {} kept, {} dropped",
        stats.files_compacted,
        stats.records_after,
        stats.duplicates_merged + stats.tombstones_dropped
    );
    if stats.files_skipped > 0 {
        line.push_str(&format!(", {} skipped (malformed)", stats.files_skipped));
    }
    println!("{line}");
    if stats.files_skipped > 0 {
        std::process::exit(1);
    }
    Ok(())
}
