#!/usr/bin/env bash
# PostToolUse(Edit|Write|MultiEdit|NotebookEdit) hook: re-sync annotation
# anchors against the edit that just happened. Fire-and-forget — always
# exits 0, no stdout either way.
set -uo pipefail

command -v jq >/dev/null 2>&1 || exit 0
command -v annot >/dev/null 2>&1 || exit 0

input="$(cat)" || exit 0
# Edit/Write/MultiEdit carry the path as tool_input.file_path; NotebookEdit
# carries it as tool_input.notebook_path instead.
file_path="$(printf '%s' "$input" | jq -r '.tool_input.file_path // .tool_input.notebook_path // empty' 2>/dev/null)" || exit 0
[ -n "$file_path" ] || exit 0

file_dir="$(dirname -- "$file_path")" || exit 0
cd "$file_dir" 2>/dev/null || exit 0

annot sync "$file_path" >/dev/null 2>&1 || true

exit 0
