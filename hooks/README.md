# annot Claude Code hooks

Three hooks that wire `annot` into a Claude Code session:

- `annot-context.sh` — `PostToolUse(Read)`: injects live `decision`/`gotcha`
  annotations for the file just read as `additionalContext`.
- `annot-sync.sh` — `PostToolUse(Edit|Write|MultiEdit|NotebookEdit)`:
  re-syncs that file's annotation anchors against the change
  (fire-and-forget, no output).
- `annot-sync-all.sh` — `Stop`: repo-wide safety net. Runs once at the end
  of every turn and re-syncs every annotated file in the repo, catching
  drift from any write the matcher above didn't (fire-and-forget, no
  output). See Limitations below for exactly what it covers.

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
   have a `hooks.PostToolUse` or `hooks.Stop` array, append these entries to
   it rather than overwriting the file. It references the hook scripts via
   `${CLAUDE_PROJECT_DIR}/hooks/...`, so keep this `hooks/` directory at the
   project root, or adjust the paths.

4. Make the scripts executable if the permission bit didn't survive the copy:

   ```sh
   chmod +x hooks/annot-context.sh hooks/annot-sync.sh hooks/annot-sync-all.sh
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
3. To see the `Stop` catch-all specifically: drift an annotation through a
   path the matcher doesn't cover (e.g. edit the file via `Bash`/`sed`
   instead of `Edit`/`Write`), let Claude finish its turn, then check
   `annot get <file> --format=json` again — `annot-sync-all.sh` should have
   healed it even though no `PostToolUse` hook fired for that write.

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

The `Stop` hook can be smoke-tested the same way, minus the `jq` assertion
(it produces no output on success):

```sh
CLAUDE_PROJECT_DIR=/abs/path/to/repo hooks/annot-sync-all.sh </dev/null
echo "exit: $?"   # should be 0 regardless of whether anything needed syncing
```

## Behavior notes

- **Context lands right after the file body, in the same turn.** `PostToolUse`
  fires after the tool runs, so the very first `Read` of a file gets its
  `additionalContext` injected immediately after that tool result — the
  model reads the file body and then, still in the same turn, the
  `decision`/`gotcha` annotations for it. That's the natural read order
  (code, then annotations about the code), not a gap to work around.

## Limitations

- **The `PostToolUse` matcher only catches tools literally named
  `Edit`/`Write`/`MultiEdit`/`NotebookEdit`.** Custom MCP tools, `Bash`
  redirection/codemods, or a future tool name not yet added to the matcher
  all bypass `annot-sync.sh`. This used to mean such drift could persist
  indefinitely; now the `Stop` hook (`annot-sync-all.sh`) runs a repo-wide
  `annot sync` at the end of every turn, so the residual exposure window is
  only: a tool that (a) writes files, (b) isn't named in the matcher, and
  (c) only until the turn's `Stop` event fires — at most one turn's worth
  of drift, not indefinite.
- **Silent degradation is deliberate but opaque.** All three scripts exit 0
  and print nothing on every failure path (missing `annot`/`jq` on `PATH`,
  file or directory outside a git repo, `annot get`/`annot sync` erroring,
  no matching annotations). This is required so a broken install never
  breaks a Claude Code session — but it also means "no annotations ever
  show up" and "everything is fine, this file just has none" look
  identical from inside the session. Use the smoke tests above to tell
  them apart when in doubt.
- **No verification inside a live Claude Code session was performed** as
  part of building this integration — Claude Code cannot run inside
  itself. The pipe-level smoke tests above (fabricated `PostToolUse`/`Stop`
  stdin through the scripts) are the verification that exists; they
  exercise the exact stdin shape and stdout contract the docs specify, but
  not Claude Code's actual hook dispatch.
