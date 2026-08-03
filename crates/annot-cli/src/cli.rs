use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "annot",
    version,
    about = "Line-anchored annotations outside your codebase"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Add a new annotation anchored to a line range.
    Add {
        /// Source file to anchor into (cwd-relative or absolute).
        file: PathBuf,
        /// 1-based inclusive line range as START:END.
        range: String,
        /// Annotation kind: decision | gotcha | todo | history.
        #[arg(long)]
        kind: String,
        /// Annotation body text.
        #[arg(short = 'm')]
        body: String,
        /// Optional symbol name (e.g. "fn parse_expr").
        #[arg(long)]
        symbol: Option<String>,
    },
    /// Read annotations for a file, syncing them against drift first.
    Get {
        /// Source file to read annotations for (cwd-relative or absolute).
        file: PathBuf,
        /// Output format: context | json.
        #[arg(long, default_value = "context")]
        format: String,
        /// Comma-separated kind filter, e.g. decision,gotcha.
        #[arg(long)]
        kinds: Option<String>,
        /// Token budget for --format=context (ignored, with a warning, for json).
        #[arg(long)]
        max_tokens: Option<usize>,
    },
    /// Sync annotations against working-tree drift.
    Sync {
        /// A file or directory to sync (cwd-relative or absolute); omit to
        /// sync every annotated file in the repo.
        path: Option<PathBuf>,
    },
    /// List every orphaned annotation across the repo.
    Orphans,
    /// Resolve an annotation by reanchoring it to a new location or dropping it.
    Resolve {
        /// The annotation's ulid.
        id: String,
        /// Reanchor to [<file>:]<start>:<end> (file is cwd-relative or
        /// absolute; defaults to the record's own file).
        #[arg(long, value_name = "[FILE:]START:END", conflicts_with = "drop")]
        reanchor: Option<String>,
        /// Drop the annotation (tombstone it) instead of reanchoring.
        #[arg(long, conflicts_with = "reanchor")]
        drop: bool,
    },
    /// Compact every annotation mirror: merge duplicate ids, drop tombstones.
    Compact,
}
