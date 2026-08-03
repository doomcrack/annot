#!/usr/bin/env bash
# PostToolUse(Read) hook: inject live decision/gotcha annotations for the file
# just read. Every failure mode below degrades to a silent, zero-exit no-op —
# a hook must never surface an error into the session.
set -uo pipefail

command -v jq >/dev/null 2>&1 || exit 0
command -v annot >/dev/null 2>&1 || exit 0

input="$(cat)" || exit 0
file_path="$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty' 2>/dev/null)" || exit 0
[ -n "$file_path" ] || exit 0

file_dir="$(dirname -- "$file_path")" || exit 0
cd "$file_dir" 2>/dev/null || exit 0

output="$(annot get "$file_path" --format=context --kinds decision,gotcha --max-tokens 800 2>/dev/null)" || exit 0
[ -n "$output" ] || exit 0

context_json="$(printf '%s' "$output" | jq -Rs '.')" || exit 0
jq -n --argjson ctx "$context_json" \
  '{hookSpecificOutput: {hookEventName: "PostToolUse", additionalContext: $ctx}}' 2>/dev/null || exit 0

exit 0
