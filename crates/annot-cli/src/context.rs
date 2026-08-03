use std::cmp::Ordering;

use annot_core::model::{Kind, Record, Status};

const ELLIPSIS: &str = "\u{2026}";

pub fn render(
    file_rel: &str,
    records: &[Record],
    kinds: Option<&[Kind]>,
    max_tokens: Option<usize>,
) -> String {
    let mut candidates: Vec<&Record> = records
        .iter()
        .filter(|r| r.status == Status::Live)
        .filter(|r| kinds.is_none_or(|ks| ks.contains(&r.kind)))
        .collect();

    if candidates.is_empty() {
        return String::new();
    }

    let Some(budget) = max_tokens else {
        return join_positional(&candidates, file_rel);
    };

    candidates.sort_by(priority_order);
    let first = candidates[0];
    let first_block = format_block(first, file_rel);
    // +1 reserves the trailing newline emitted by `join_positional`.
    if (first_block.len() + 1).div_ceil(4) > budget {
        return match truncated_block(first, file_rel, budget) {
            Some(block) => format!("{block}\n"),
            None => String::new(),
        };
    }

    let mut selected: Vec<&Record> = Vec::new();
    let mut running = 0usize;
    for r in &candidates {
        let block_len = format_block(r, file_rel).len();
        // Each block after the first costs an extra "\n\n" separator; the
        // trailing "\n" at the very end is reserved via the constant +1.
        let sep = if selected.is_empty() { 0 } else { 2 };
        let cost = block_len + sep;
        if (running + cost + 1).div_ceil(4) > budget {
            break;
        }
        running += cost;
        selected.push(r);
    }

    join_positional(&selected, file_rel)
}

fn kind_priority(kind: Kind) -> u8 {
    match kind {
        Kind::Gotcha => 0,
        Kind::Decision => 1,
        Kind::Todo => 2,
        Kind::History => 3,
    }
}

fn priority_order(a: &&Record, b: &&Record) -> Ordering {
    kind_priority(a.kind)
        .cmp(&kind_priority(b.kind))
        .then_with(|| a.anchor.start.cmp(&b.anchor.start))
        .then_with(|| a.id.cmp(&b.id))
}

fn positional_order(a: &&Record, b: &&Record) -> Ordering {
    a.anchor
        .start
        .cmp(&b.anchor.start)
        .then_with(|| a.id.cmp(&b.id))
}

fn join_positional(records: &[&Record], file_rel: &str) -> String {
    if records.is_empty() {
        return String::new();
    }
    let mut sorted = records.to_vec();
    sorted.sort_by(positional_order);
    let blocks: Vec<String> = sorted.iter().map(|r| format_block(r, file_rel)).collect();
    format!("{}\n", blocks.join("\n\n"))
}

fn format_block(record: &Record, file_rel: &str) -> String {
    format!(
        "<annot untrusted=\"true\" id=\"{}\" file=\"{}\" lines=\"{}-{}\" kind=\"{}\">\n{}\n</annot>",
        record.id,
        escape_attr(file_rel),
        record.anchor.start,
        record.anchor.end,
        record.kind,
        escape_body(&record.body)
    )
}

/// Neutralizes delimiter-token sequences in an annotation body so an untrusted
/// body can never forge a `<annot ...>`/`</annot>` block boundary. The two
/// patterns are disjoint (`<annot` is never a substring of `</annot`), so
/// application order doesn't matter.
fn escape_body(body: &str) -> String {
    body.replace("<annot", "&lt;annot")
        .replace("</annot", "&lt;/annot")
}

/// Escapes `"` for safe interpolation into a double-quoted attribute value.
fn escape_attr(s: &str) -> String {
    s.replace('"', "&quot;")
}

/// Truncates `record`'s (post-escaping) body bytewise (at a char boundary) so the
/// whole block, plus the trailing newline the caller appends, fits within
/// `budget` tokens (`budget * 4` bytes). `None` if even an empty body wouldn't fit.
fn truncated_block(record: &Record, file_rel: &str, budget: usize) -> Option<String> {
    let budget_bytes = budget.saturating_mul(4);
    let prefix = format!(
        "<annot untrusted=\"true\" id=\"{}\" file=\"{}\" lines=\"{}-{}\" kind=\"{}\">\n",
        record.id,
        escape_attr(file_rel),
        record.anchor.start,
        record.anchor.end,
        record.kind
    );
    let suffix = "\n</annot>";
    // +1 reserves the trailing newline the caller appends after this block.
    let overhead = prefix.len() + suffix.len() + ELLIPSIS.len() + 1;
    if budget_bytes < overhead {
        return None;
    }
    let avail_for_body = budget_bytes - overhead;
    let escaped_body = escape_body(&record.body);
    let body = truncate_at_char_boundary(&escaped_body, avail_for_body);
    Some(format!("{prefix}{body}{ELLIPSIS}{suffix}"))
}

fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
