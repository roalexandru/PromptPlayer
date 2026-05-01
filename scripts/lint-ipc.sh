#!/usr/bin/env bash
# Forbid raw `invoke(...)` calls anywhere except the IPC façade itself.
#
# Why: the picker had a regression where it called `invoke("ipc_list_prompts")`
# with a stale string-literal command name that no longer existed after the
# command-rename refactor. tauri-specta's generated `commands.*` bindings
# would have caught the rename at compile time — but raw `invoke()` bypasses
# them. This lint forces every IPC call to go through `$lib/ipc`, which
# re-exports the type-checked bindings.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Files allowed to use raw invoke():
#   - src/lib/ipc.ts          — the façade itself, but it doesn't call invoke directly any more
#   - src/lib/ipc.gen.ts      — the auto-generated tauri-specta bindings
ALLOWED='^src/lib/(ipc\.ts|ipc\.gen\.ts)$'

# Anywhere else under src/, look for `invoke(` or `invoke<...>(`.
HITS=$(
  find src -type f \( -name '*.ts' -o -name '*.svelte' \) \
    | grep -Ev "$ALLOWED" \
    | xargs grep -l -E 'invoke[[:space:]]*[<(]' 2>/dev/null \
    || true
)

if [[ -n "$HITS" ]]; then
  echo "✗ Raw invoke() calls found outside src/lib/ipc{,gen}.ts:" >&2
  echo "" >&2
  for f in $HITS; do
    grep -nE 'invoke[[:space:]]*[<(]' "$f" | sed "s|^|  $f:|" >&2
  done
  echo "" >&2
  echo "Use the typed wrappers in \`\$lib/ipc\` instead. tauri-specta's" >&2
  echo "generated bindings catch command renames at TS compile time;" >&2
  echo "string-literal command names do not." >&2
  exit 1
fi

echo "✓ no raw invoke() calls outside src/lib/ipc{,gen}.ts"
