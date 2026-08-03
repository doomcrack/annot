#!/usr/bin/env bash
# Stop hook: repo-wide safety-net sync. Fires once at the end of every turn
# (Stop has no matcher — this always runs), so it catches drift from any
# write that the PostToolUse(Edit|Write|MultiEdit|NotebookEdit) matcher
# missed: custom MCP tools, Bash redirection/codemods, tool names not yet
# wired into the matcher, etc. Fire-and-forget — always exits 0, no stdout
# either way, and never blocks the stop.
set -uo pipefail

# Stdin carries the Stop event payload (session_id, transcript_path, ...);
# this hook doesn't need any of it, just drains the pipe.
cat >/dev/null 2>&1 || true

repo_dir="${CLAUDE_PROJECT_DIR:-$PWD}"

command -v annot >/dev/null 2>&1 || exit 0
cd "$repo_dir" 2>/dev/null || exit 0
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0

annot sync >/dev/null 2>&1 || true

exit 0
