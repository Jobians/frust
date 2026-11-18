#!/usr/bin/env bash
# Run `cargo deny check advisories` against every tracked Cargo.lock, using the
# root deny.toml for all of them: cargo-deny reads <cwd>/deny.toml by default and
# this script runs from the repository root. No `--config` flag is passed because
# its position differs between cargo-deny releases (after `check` up to 0.19,
# before it from 0.20). The root workspace and each standalone
# workspace (excluded from the root Cargo.toml) have their own lockfile, so a
# single run from the root would miss the standalone ones.
#
# Usage: scripts/ci/advisories.sh   (run from anywhere; it cd's to the repo root)
# Exits non-zero if any workspace fails, after listing the failed workspaces.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

lockfiles=()
while IFS= read -r lock; do
  lockfiles+=("$lock")
done < <(git ls-files '*Cargo.lock')

if [ "${#lockfiles[@]}" -eq 0 ]; then
  echo "error: no tracked Cargo.lock found" >&2
  exit 1
fi

failed=()
for lock in "${lockfiles[@]}"; do
  dir="$(dirname "$lock")"
  if [ ! -f "$dir/Cargo.toml" ]; then
    echo "error: $lock has no $dir/Cargo.toml next to it" >&2
    failed+=("$dir (no Cargo.toml)")
    continue
  fi
  echo "=== advisories: $dir ==="
  if ! cargo deny --manifest-path "$dir/Cargo.toml" check advisories; then
    failed+=("$dir")
  fi
done

echo
if [ "${#failed[@]}" -gt 0 ]; then
  echo "advisories failed in ${#failed[@]} of ${#lockfiles[@]} workspaces:" >&2
  printf '  %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "advisories ok in all ${#lockfiles[@]} workspaces"
