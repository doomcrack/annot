# annot Claude Code hooks

Two `PostToolUse` hooks that wire `annot` into a Claude Code session:

- `annot-context.sh` — after a `Read`, injects live `decision`/`gotcha`
  annotations for the file just read as `additionalContext`.
- `annot-sync.sh` — after an `Edit` or `Write`, re-syncs that file's
  annotation anchors against the change (fire-and-forget, no output).

## Install

1. Build the binary and put it on `PATH`:

   ```sh
   cargo build -p annot-cli
   ln -sf "$(pwd)/target/debug/annot" /usr/local/bin/annot   # or add target/debug to PATH
   ```

2. Make sure `jq` is on `PATH` too (`brew install jq` / `apt install jq`).

3. Merge the contents of `hooks/settings.json` into your project's
   `.claude/settings.json` (or `~/.claude/settings.json` for a user-wide
   install). It is a **snippet**, not a full settings file — if you already
   have a `hooks.PostToolUse` array, append these two matcher entries to it
   rather than overwriting the file. It references the hook scripts via
   `${CLAUDE_PROJECT_DIR}/hooks/...`, so keep this `hooks/` directory at the
   project root, or adjust the paths.

4. Make the scripts executable if the permission bit didn't survive the copy:

   ```sh
   chmod +x hooks/annot-context.sh hooks/annot-sync.sh
   ```

## Verify it's working

Manual walkthrough inside a Claude Code session, in a git repo that has at
least one `annot add`-ed annotation:

1. Ask Claude to read the annotated file. Its next turn should show
   awareness of the `decision`/`gotcha` content without you having pasted it
   — that content arrived via `additionalContext`, not the file body.
2. Ask Claude to edit a line above the annotated range. Then run
   `annot get <file> --format=json` yourself and confirm the anchor's
   `start`/`end` shifted to match — that proves `annot-sync.sh` fired on the
   edit.

### One-liner smoke test (no live session needed)

This is what actually exercises the pipe Claude Code uses, without needing
Claude Code itself:

```sh
FILE=/abs/path/to/annotated_file.rs
jq -n --arg fp "$FILE" '{tool_input:{file_path:$fp}}' \
  | hooks/annot-context.sh | jq .
```

Non-empty JSON with a populated `.hookSpecificOutput.additionalContext` means
the pipeline works end to end (jq found, annot found, repo discovered,
annotations found). Empty stdout means one of those links is missing —
see Limitations below for how to tell which.

## Limitations

- **Only `Read`/`Edit`/`Write` are wired.** Any writing tool not literally
  named `Edit` or `Write` (e.g. `NotebookEdit`, and `MultiEdit` on Claude
  Code versions that expose it) never triggers `annot-sync.sh` — nor do
  custom MCP tools, `Bash` redirection, etc. — so annotations on files
  touched only through those paths can drift until something else (a later
  `Read`, or a manual `annot sync`) resyncs them.
- **PostToolUse fires after the tool runs.** The very first `Read` of a
  file in a session gets its context injected on schedule, but a hook can
  never affect the tool call that triggered it — there is no way to warn
  Claude about a file's annotations *before* it reads the file for the
  first time.
- **Silent degradation is deliberate but opaque.** Both scripts exit 0 and
  print nothing on every failure path (missing `annot`/`jq` on `PATH`, file
  outside a git repo, `annot get`/`annot sync` erroring, no matching
  annotations). This is required so a broken install never breaks a Claude
  Code session — but it also means "no annotations ever show up" and
  "everything is fine, this file just has none" look identical from inside
  the session. Use the smoke test above to tell them apart when in doubt.
- **No verification inside a live Claude Code session was performed** as
  part of building this integration — Claude Code cannot run inside
  itself. The pipe-level smoke test above (fabricated `PostToolUse` stdin
  through the scripts, asserted with `jq`) is the verification that exists;
  it exercises the exact stdin shape and stdout contract the docs specify,
  but not Claude Code's actual hook dispatch.
