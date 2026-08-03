#!/usr/bin/env bash
# End-to-end script for `annot`:
#   init fixture repo -> annot add -> mutate file above/inside anchor ->
#   annot get returns correctly shifted annotation in one case and an
#   orphan in the other.
#
# Self-contained: no dependency on tests/fixtures. Safe to run repeatedly.
set -euo pipefail

# --- locate the workspace from the script's own location -------------------
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." &>/dev/null && pwd)"

# --- tiny assertion framework ------------------------------------------------
ok() {
    printf 'ok: %s\n' "$1"
}

fail() {
    local what="$1" actual="$2"
    printf 'FAIL: %s\n' "$what"
    printf -- '--- actual output ---\n%s\n---------------------\n' "$actual"
    exit 1
}

assert_contains() {
    local what="$1" haystack="$2" needle="$3"
    if printf '%s' "$haystack" | grep -qF -- "$needle"; then
        ok "$what"
    else
        fail "$what (expected output to contain: $needle)" "$haystack"
    fi
}

assert_not_contains() {
    local what="$1" haystack="$2" needle="$3"
    if printf '%s' "$haystack" | grep -qF -- "$needle"; then
        fail "$what (expected output NOT to contain: $needle)" "$haystack"
    else
        ok "$what"
    fi
}

assert_jq_true() {
    local what="$1" json="$2" filter="$3"
    local result
    result="$(printf '%s' "$json" | jq -r "$filter" 2>&1)" || fail "$what (jq failed to evaluate)" "$result"
    if [[ "$result" == "true" ]]; then
        ok "$what"
    else
        fail "$what (jq \"$filter\" -> \"$result\")" "$json"
    fi
}

# --- prerequisites -----------------------------------------------------------
if ! command -v jq >/dev/null 2>&1; then
    echo "FAIL: this script requires jq on PATH, but it was not found" >&2
    exit 1
fi

cd "$REPO_ROOT"
cargo build -p annot-cli --quiet
export PATH="$REPO_ROOT/target/debug:$PATH"

if ! command -v annot >/dev/null 2>&1; then
    echo "FAIL: annot binary not found on PATH after cargo build -p annot-cli" >&2
    exit 1
fi

# --- fixture repo --------------------------------------------------------------
TMP_DIR="$(mktemp -d)"
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

cd "$TMP_DIR"
git init -q .

# =============================================================================
# CASE A: mutate ABOVE the anchor -> annotation shifts and stays live.
# =============================================================================
SHIFT_FILE="shift_target.rs"
cat > "$SHIFT_FILE" <<'EOF'
// shift-fixture line 1
// shift-fixture line 2
// shift-fixture line 3
// shift-fixture line 4
// shift-fixture line 5
// shift-fixture line 6
// shift-fixture line 7
// shift-fixture anchor start
// shift-fixture anchor middle
// shift-fixture anchor end
// shift-fixture line 11
// shift-fixture line 12
// shift-fixture line 13
// shift-fixture line 14
// shift-fixture line 15
// shift-fixture line 16
// shift-fixture line 17
// shift-fixture line 18
// shift-fixture line 19
// shift-fixture line 20
EOF

SHIFT_BODY="why the shift-fixture anchor exists"
SHIFT_ID="$(annot add "$SHIFT_FILE" 8:10 --kind decision -m "$SHIFT_BODY")"
ok "annot add on shift-fixture returned an id ($SHIFT_ID)"

# Insert 3 new lines ABOVE the anchor (at the top of the file).
{
    printf '// inserted above 1\n// inserted above 2\n// inserted above 3\n'
    cat "$SHIFT_FILE"
} > "$SHIFT_FILE.tmp"
mv "$SHIFT_FILE.tmp" "$SHIFT_FILE"

SHIFT_OUT="$(annot get "$SHIFT_FILE" --format=context)"
assert_contains "shifted annotation reports lines=\"11-13\"" "$SHIFT_OUT" 'lines="11-13"'
assert_contains "shifted annotation retains its body text" "$SHIFT_OUT" "$SHIFT_BODY"

# =============================================================================
# CASE B: rewrite the file beyond recognition -> annotation orphans.
# =============================================================================
ORPHAN_FILE="orphan_target.rs"
cat > "$ORPHAN_FILE" <<'EOF'
// orphan-fixture line 1
// orphan-fixture line 2
// orphan-fixture line 3
// orphan-fixture line 4
// orphan-fixture line 5
// orphan-fixture line 6
// orphan-fixture line 7
// orphan-fixture anchor start
// orphan-fixture anchor middle
// orphan-fixture anchor end
// orphan-fixture line 11
// orphan-fixture line 12
// orphan-fixture line 13
// orphan-fixture line 14
// orphan-fixture line 15
// orphan-fixture line 16
// orphan-fixture line 17
// orphan-fixture line 18
// orphan-fixture line 19
// orphan-fixture line 20
EOF

ORPHAN_BODY="explanation that will be orphaned"
ORPHAN_ID="$(annot add "$ORPHAN_FILE" 8:10 --kind gotcha -m "$ORPHAN_BODY")"
ok "annot add on orphan-fixture returned an id ($ORPHAN_ID)"

# Rewrite every line so nothing in the anchored region (or its surrounding
# context) is recognizable any more.
cat > "$ORPHAN_FILE" <<'EOF'
totally rewritten content 1
totally rewritten content 2
totally rewritten content 3
totally rewritten content 4
totally rewritten content 5
totally rewritten content 6
totally rewritten content 7
totally rewritten content 8
totally rewritten content 9
totally rewritten content 10
totally rewritten content 11
totally rewritten content 12
totally rewritten content 13
totally rewritten content 14
totally rewritten content 15
totally rewritten content 16
totally rewritten content 17
totally rewritten content 18
totally rewritten content 19
totally rewritten content 20
EOF

ORPHAN_CTX="$(annot get "$ORPHAN_FILE" --format=context)"
assert_not_contains "orphaned annotation is absent from --format=context" "$ORPHAN_CTX" "$ORPHAN_ID"

ORPHAN_JSON="$(annot get "$ORPHAN_FILE" --format=json)"
assert_jq_true "orphaned record has status \"orphaned\"" "$ORPHAN_JSON" \
    ".[] | select(.id==\"$ORPHAN_ID\") | .status == \"orphaned\""
assert_jq_true "orphaned record retains a non-null orig_snippet" "$ORPHAN_JSON" \
    ".[] | select(.id==\"$ORPHAN_ID\") | .orig_snippet != null"

ORPHANS_OUT="$(annot orphans)"
assert_contains "annot orphans lists the orphaned id" "$ORPHANS_OUT" "$ORPHAN_ID"

echo "E2E PASS"
